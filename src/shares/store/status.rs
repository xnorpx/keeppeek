use super::*;
use crate::shares::{BoundSession, MediaDeadline};
use std::collections::HashSet;

impl Store {
    /// Invalidates runtime authority during policy rotation without refunding durable delivery.
    pub fn invalidate_sessions(&mut self) {
        self.sessions = Sessions::default();
    }

    /// Reads only the grant authenticated by this fixed-lifetime, non-media receipt.
    pub fn inspect_session(
        &mut self,
        handle: &str,
        expected_id: Option<Uuid>,
        context: &SessionContext,
        clock: Clock,
    ) -> Result<BoundSession, Error> {
        self.tick(clock)?;
        let (session_id, receipt) = self
            .sessions
            .receipts
            .lookup(handle, expected_id, context)?;
        let record = self.database.get(receipt.grant_id)?.ok_or(Error::Denied)?;
        if !record.active(clock.unix_ms) {
            self.database.observe(clock.unix_ms)?;
        }
        let session = self.sessions.records.get(&session_id);
        let media_active = session.is_some_and(|session| {
            if session.revision != receipt.initial_revision {
                return false;
            }
            record.revision == session.revision && record.active(clock.unix_ms)
        });
        Ok(BoundSession {
            session_id,
            grant: record.summary(),
            media_active,
            media_deadline: session
                .filter(|_| media_active)
                .map(|session| MediaDeadline {
                    expires_at_ms: session.expires_at_ms,
                    monotonic: session.deadline,
                }),
            stream_committed: media_active
                && session.is_some_and(|session| session.stream_committed),
        })
    }

    pub fn end_session(
        &mut self,
        handle: &str,
        id: Uuid,
        context: &SessionContext,
        clock: Clock,
    ) -> Result<(), Error> {
        self.tick(clock)?;
        self.sessions.receipts.lookup(handle, Some(id), context)?;
        self.sessions.receipts.remove(id);
        self.sessions.records.remove(&id);
        self.sessions
            .deliveries
            .retain(|_, delivery| delivery.session_id != id);
        Ok(())
    }

    /// The caller must authorize management access before exposing this sanitized snapshot.
    pub fn get(&mut self, id: Uuid, clock: Clock) -> Result<Option<GrantSummary>, Error> {
        self.tick(clock)?;
        Ok(self.database.get(id)?.map(|record| record.summary()))
    }

    /// Returns at most 4096 sanitized records for caller-authorized pagination or revocation.
    pub fn all(&mut self, clock: Clock) -> Result<Vec<GrantSummary>, Error> {
        self.tick(clock)?;
        Ok(self
            .database
            .all()?
            .into_iter()
            .map(|record| record.summary())
            .collect())
    }

    /// Atomically revokes a bounded set after the caller authorizes every selected grant.
    pub fn revoke_batch(
        &mut self,
        expected: &[(Uuid, u64)],
        reason: &str,
        clock: Clock,
    ) -> Result<Vec<GrantSummary>, Error> {
        self.tick(clock)?;
        if expected.len() > MAX_ACTIVE_GRANTS {
            return Err(Error::Invalid);
        }
        let ids: HashSet<_> = expected.iter().map(|(id, _)| *id).collect();
        if ids.len() != expected.len()
            || expected
                .iter()
                .any(|(id, revision)| id.is_nil() || *revision == 0)
            || !bounded_text(reason, 256, false)
        {
            return Err(Error::Invalid);
        }
        if expected.is_empty() {
            return Ok(Vec::new());
        }
        let revoked = self.database.transaction(clock.unix_ms, |database| {
            let mut revoked = Vec::with_capacity(expected.len());
            for (id, revision) in expected {
                let mut record = database.get(*id)?.ok_or(Error::Denied)?;
                if record.revision != *revision {
                    return Err(Error::Conflict);
                }
                if record.revoked_at_ms.is_none() {
                    record.revoked_at_ms = Some(clock.unix_ms);
                    record.revoke_reason = Some(reason.to_owned());
                    record.revision = record.revision.checked_add(1).ok_or(Error::Storage)?;
                    database.save(&record)?;
                }
                revoked.push(record.summary());
            }
            Ok(revoked)
        })?;
        self.sessions
            .records
            .retain(|_, session| !ids.contains(&session.grant_id));
        self.sessions
            .deliveries
            .retain(|_, delivery| !ids.contains(&delivery.grant_id));
        Ok(revoked)
    }
}
