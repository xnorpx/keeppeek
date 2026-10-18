mod status;

use super::{
    Access, Clock, CreateGrant, Error, ExportBinding, GrantSummary, IssuedGrant, IssuedSession,
    IssuerBinding, Mode, SessionContext, Ticket,
    database::Database,
    model::{MAX_ACTIVE_GRANTS, MAX_CREATOR_GRANTS, MAX_GRANT_RECORDS},
    record::{Record, bounded_text, valid_issuer, validate_intent},
    secret::{digest, valid},
    sessions::Sessions,
};
use std::{fmt, path::Path};
use uuid::Uuid;

/// Serializes one runtime's durable grants and bounded ephemeral guest reservations.
///
/// The database has one owner, enforced by a private sidecar lock. Restart invalidates
/// sessions and pending reservations; committed views/downloads are never refunded.
/// Callers must commit delivery after validating/buffering the first media chunk and
/// immediately before returning its first positive read. A crash at that boundary may
/// consume an undelivered view, but cannot refund one whose bytes were returned.
pub struct Store {
    database: Database,
    sessions: Sessions,
    last_clock: Option<Clock>,
}

impl fmt::Debug for Store {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GuestGrantStore")
            .field("sessions", &self.sessions.records.len())
            .field("deliveries", &self.sessions.deliveries.len())
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Opens a database inside an explicitly dedicated private parent directory.
    /// The existing ancestor must be supported by the repository private-directory checks.
    pub fn open(path: &Path, now_ms: i64) -> Result<Self, Error> {
        Ok(Self {
            database: Database::open(path, now_ms)?,
            sessions: Sessions::default(),
            last_clock: None,
        })
    }

    pub fn create(&mut self, intent: CreateGrant, clock: Clock) -> Result<IssuedGrant, Error> {
        self.tick(clock)?;
        validate_intent(&intent, clock.unix_ms)?;
        let record = Record {
            intent,
            revision: 1,
            created_at_ms: clock.unix_ms,
            revoked_at_ms: None,
            revoke_reason: None,
            views: 0,
            downloads: 0,
            last_access_at_ms: None,
        };
        let token = super::Secret::issue();
        self.database.transaction(clock.unix_ms, |database| {
            database.prune(clock.unix_ms)?;
            if database.get(record.intent.request_id)?.is_some() {
                return Err(Error::Conflict);
            }
            // ponytail: creation scans at most 4096 records; an index is useful only beyond this ceiling.
            let records = database.all()?;
            let active: Vec<_> = records
                .iter()
                .filter(|record| record.active(clock.unix_ms))
                .collect();
            if records.len() >= MAX_GRANT_RECORDS
                || active.len() >= MAX_ACTIVE_GRANTS
                || active
                    .iter()
                    .filter(|other| {
                        other.intent.issuer.creator_id() == record.intent.issuer.creator_id()
                    })
                    .count()
                    >= MAX_CREATOR_GRANTS
            {
                return Err(Error::Capacity);
            }
            database.insert(&record, &token.hash())
        })?;
        Ok(IssuedGrant {
            grant: record.summary(),
            token,
        })
    }

    pub fn list(
        &mut self,
        issuer: &IssuerBinding,
        clock: Clock,
    ) -> Result<Vec<GrantSummary>, Error> {
        self.tick(clock)?;
        if !valid_issuer(issuer) {
            return Err(Error::Invalid);
        }
        Ok(self
            .database
            .all()?
            .into_iter()
            .filter(|record| record.intent.issuer.creator_id() == issuer.creator_id())
            .map(|record| record.summary())
            .collect())
    }

    /// The caller authorizes revocation for this grant's creator before invoking this method.
    pub fn revoke(
        &mut self,
        id: Uuid,
        revision: u64,
        reason: &str,
        clock: Clock,
    ) -> Result<GrantSummary, Error> {
        self.tick(clock)?;
        if !bounded_text(reason, 256, false) {
            return Err(Error::Invalid);
        }
        let revoked = self.database.transaction(clock.unix_ms, |database| {
            let mut record = database.get(id)?.ok_or(Error::Denied)?;
            if record.revision != revision {
                return Err(Error::Conflict);
            }
            if record.revoked_at_ms.is_none() {
                record.revoked_at_ms = Some(clock.unix_ms);
                record.revoke_reason = Some(reason.to_owned());
                record.revision = record.revision.checked_add(1).ok_or(Error::Storage)?;
                database.save(&record)?;
            }
            Ok(record.summary())
        })?;
        self.sessions
            .records
            .retain(|_, session| session.grant_id != id);
        self.sessions
            .deliveries
            .retain(|_, delivery| delivery.grant_id != id);
        Ok(revoked)
    }

    pub fn exchange(
        &mut self,
        token: &str,
        context: &SessionContext,
        clock: Clock,
    ) -> Result<IssuedSession, Error> {
        self.tick(clock)?;
        if !valid(token) || !valid_context(context) {
            return Err(Error::Denied);
        }
        let record = self
            .database
            .by_hash(&digest(token))?
            .ok_or(Error::Denied)?;
        self.require_active(&record, clock)?;
        self.database.observe(clock.unix_ms)?;
        self.sessions.issue(&record, context, clock)
    }

    pub fn begin_delivery(
        &mut self,
        handle: &str,
        context: &SessionContext,
        binding: &ExportBinding,
        mode: Mode,
        clock: Clock,
    ) -> Result<Ticket, Error> {
        self.tick(clock)?;
        let session_id = self.sessions.lookup(handle, context)?;
        let record = self.session_record(session_id, context, binding, clock)?;
        permitted(&record, mode)?;
        self.database.observe(clock.unix_ms)?;
        self.sessions.reserve(session_id, &record, mode, clock)
    }

    /// Commits exactly once immediately before the first positive media read is returned.
    pub fn commit_delivery(
        &mut self,
        ticket: &Ticket,
        context: &SessionContext,
        binding: &ExportBinding,
        clock: Clock,
    ) -> Result<Access, Error> {
        self.tick(clock)?;
        let delivery = self.sessions.delivery(ticket)?;
        let record = self.session_record(delivery.session_id, context, binding, clock)?;
        permitted(&record, delivery.mode)?;
        if delivery.committed {
            return self.access(ticket, record, delivery.mode);
        }
        let session = self
            .sessions
            .records
            .get(&delivery.session_id)
            .ok_or(Error::Denied)?;
        let count_stream = delivery.mode == Mode::Stream && !session.stream_committed;
        let updated = self.database.transaction(clock.unix_ms, |database| {
            let mut current = database
                .get(record.intent.request_id)?
                .ok_or(Error::Denied)?;
            if current.revision != record.revision
                || !current.active(clock.unix_ms)
                || current.intent.binding != *binding
            {
                return Err(Error::Denied);
            }
            permitted(&current, delivery.mode)?;
            if count_stream {
                current.views = increment(current.views, current.intent.max_views)?;
            }
            if delivery.mode == Mode::Download {
                current.downloads = increment(current.downloads, current.intent.max_downloads)?;
            }
            current.last_access_at_ms = Some(clock.unix_ms);
            database.save(&current)?;
            Ok(current)
        })?;
        let active = self
            .sessions
            .deliveries
            .get_mut(&ticket.id)
            .expect("committed ticket remains reserved");
        active.committed = true;
        if delivery.mode == Mode::Stream {
            self.sessions
                .records
                .get_mut(&delivery.session_id)
                .expect("committed session remains present")
                .stream_committed = true;
        }
        self.access(ticket, updated, delivery.mode)
    }

    /// Rechecks a counted transfer without changing counters or writing healthy chunks to disk.
    pub fn authorize(
        &mut self,
        ticket: &Ticket,
        context: &SessionContext,
        binding: &ExportBinding,
        clock: Clock,
    ) -> Result<Access, Error> {
        self.tick(clock)?;
        let delivery = self.sessions.delivery(ticket)?;
        if !delivery.committed {
            return Err(Error::Denied);
        }
        let record = self.session_record(delivery.session_id, context, binding, clock)?;
        permitted(&record, delivery.mode)?;
        self.access(ticket, record, delivery.mode)
    }

    pub fn finish(&mut self, ticket: Ticket) {
        if self
            .sessions
            .deliveries
            .get(&ticket.id)
            .is_some_and(|delivery| delivery.session_id == ticket.session_id)
        {
            self.sessions.deliveries.remove(&ticket.id);
        }
    }

    /// Purges at most 256 terminal records whose 30-day audit lifetime has ended.
    pub fn purge(&mut self, clock: Clock) -> Result<u64, Error> {
        self.tick(clock)?;
        self.database
            .transaction(clock.unix_ms, |database| database.prune(clock.unix_ms))
    }

    fn tick(&mut self, clock: Clock) -> Result<(), Error> {
        self.database.healthy()?;
        if clock.unix_ms < self.database.high_water_ms()
            || self.last_clock.is_some_and(|last| {
                clock.unix_ms < last.unix_ms || clock.monotonic < last.monotonic
            })
        {
            return Err(Error::Denied);
        }
        self.last_clock = Some(clock);
        if self
            .sessions
            .records
            .values()
            .any(|session| clock.unix_ms >= session.expires_at_ms)
        {
            self.database.observe(clock.unix_ms)?;
        }
        self.sessions.expire(clock);
        Ok(())
    }

    fn require_active(&self, record: &Record, clock: Clock) -> Result<(), Error> {
        if record.active(clock.unix_ms) {
            return Ok(());
        }
        // Persist terminal observations so a later wall-clock rollback cannot resurrect access.
        self.database.observe(clock.unix_ms)?;
        Err(Error::Denied)
    }

    fn session_record(
        &self,
        id: Uuid,
        context: &SessionContext,
        binding: &ExportBinding,
        clock: Clock,
    ) -> Result<Record, Error> {
        let session = self.sessions.records.get(&id).ok_or(Error::Denied)?;
        if session.context != *context {
            return Err(Error::Denied);
        }
        let record = self.database.get(session.grant_id)?.ok_or(Error::Denied)?;
        self.require_active(&record, clock)?;
        if record.revision != session.revision || record.intent.binding != *binding {
            return Err(Error::Denied);
        }
        Ok(record)
    }

    fn access(&self, ticket: &Ticket, record: Record, mode: Mode) -> Result<Access, Error> {
        let session = self
            .sessions
            .records
            .get(&ticket.session_id)
            .ok_or(Error::Denied)?;
        Ok(Access {
            binding: record.intent.binding,
            mode,
            expires_at_ms: session.expires_at_ms.min(record.intent.expires_at_ms),
        })
    }

    #[cfg(test)]
    pub fn fail_next_commit(&self) {
        self.database.fail_next_commit();
    }

    #[cfg(test)]
    pub fn fail_next_checkpoint(&self) {
        self.database.fail_next_checkpoint();
    }

    #[cfg(test)]
    pub fn checkpoint(&self) -> Result<(), Error> {
        self.database.checkpoint()
    }
}

fn valid_context(context: &SessionContext) -> bool {
    bounded_text(&context.origin, 512, false) && context.policy_revision > 0
}

const fn permitted(record: &Record, mode: Mode) -> Result<(), Error> {
    match mode {
        Mode::Stream if record.intent.allow_stream => Ok(()),
        Mode::Download if record.intent.allow_download => Ok(()),
        Mode::Stream | Mode::Download => Err(Error::Denied),
    }
}

fn increment(current: u32, maximum: Option<u32>) -> Result<u32, Error> {
    if current >= maximum.unwrap_or(u32::MAX) {
        return Err(Error::Denied);
    }
    current.checked_add(1).ok_or(Error::Denied)
}
