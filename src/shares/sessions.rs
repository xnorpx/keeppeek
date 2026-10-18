use super::{model::*, receipts::Receipts, record::Record};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};
use uuid::Uuid;

const RESERVATION_LIFETIME: Duration = Duration::from_secs(30);
const MAX_TICKETS: usize = 8;
const MAX_SESSION_TICKETS: usize = 2;

pub(super) struct Session {
    pub grant_id: Uuid,
    pub revision: u64,
    pub context: SessionContext,
    pub expires_at_ms: i64,
    pub deadline: Instant,
    pub stream_committed: bool,
}

#[derive(Clone, Copy)]
pub(super) struct Delivery {
    pub session_id: Uuid,
    pub grant_id: Uuid,
    pub mode: Mode,
    pub committed: bool,
    pub reservation_deadline: Instant,
}

#[derive(Default)]
pub(super) struct Sessions {
    pub records: HashMap<Uuid, Session>,
    pub receipts: Receipts,
    pub deliveries: HashMap<Uuid, Delivery>,
}

impl Sessions {
    pub fn expire(&mut self, clock: Clock) {
        self.receipts.expire(clock);
        self.records.retain(|_, session| {
            clock.monotonic < session.deadline && clock.unix_ms < session.expires_at_ms
        });
        self.deliveries.retain(|_, delivery| {
            self.records.contains_key(&delivery.session_id)
                && (delivery.committed || clock.monotonic < delivery.reservation_deadline)
        });
    }

    pub fn issue(
        &mut self,
        record: &Record,
        context: &SessionContext,
        clock: Clock,
    ) -> Result<IssuedSession, Error> {
        if self.records.len() >= MAX_SESSIONS
            || self
                .records
                .values()
                .filter(|session| session.grant_id == record.intent.request_id)
                .count()
                >= MAX_GRANT_SESSIONS
        {
            return Err(Error::Capacity);
        }
        let lifetime_ms = SESSION_LIFETIME_MS.min(record.intent.expires_at_ms - clock.unix_ms);
        let lifetime =
            Duration::from_millis(u64::try_from(lifetime_ms).map_err(|_| Error::Denied)?);
        let id = Uuid::new_v4();
        let handle = super::Secret::issue();
        let session = Session {
            grant_id: record.intent.request_id,
            revision: record.revision,
            context: context.clone(),
            expires_at_ms: clock
                .unix_ms
                .checked_add(lifetime_ms)
                .ok_or(Error::Denied)?,
            deadline: clock.monotonic.checked_add(lifetime).ok_or(Error::Denied)?,
            stream_committed: false,
        };
        self.receipts
            .issue(id, &handle, record, context, clock, &self.records)?;
        assert!(
            self.records.insert(id, session).is_none(),
            "guest session UUID collision"
        );
        Ok(IssuedSession { id, handle })
    }

    pub fn lookup(&self, handle: &str, context: &SessionContext) -> Result<Uuid, Error> {
        let (id, _) = self.receipts.lookup(handle, None, context)?;
        self.records
            .contains_key(&id)
            .then_some(id)
            .ok_or(Error::Denied)
    }
    pub fn reserve(
        &mut self,
        session_id: Uuid,
        record: &Record,
        mode: Mode,
        clock: Clock,
    ) -> Result<Ticket, Error> {
        self.capacity(session_id)?;
        self.quota(session_id, record, mode)?;
        let id = Uuid::new_v4();
        let deadline = clock
            .monotonic
            .checked_add(RESERVATION_LIFETIME)
            .ok_or(Error::Denied)?;
        let delivery = Delivery {
            session_id,
            grant_id: record.intent.request_id,
            mode,
            committed: false,
            reservation_deadline: deadline,
        };
        assert!(
            self.deliveries.insert(id, delivery).is_none(),
            "guest delivery UUID collision"
        );
        Ok(Ticket { id, session_id })
    }

    fn capacity(&self, session_id: Uuid) -> Result<(), Error> {
        if self.deliveries.len() >= MAX_TICKETS
            || self
                .deliveries
                .values()
                .filter(|delivery| delivery.session_id == session_id)
                .count()
                >= MAX_SESSION_TICKETS
        {
            Err(Error::Capacity)
        } else {
            Ok(())
        }
    }

    fn quota(&self, session_id: Uuid, record: &Record, mode: Mode) -> Result<(), Error> {
        let session = self.records.get(&session_id).ok_or(Error::Denied)?;
        match mode {
            Mode::Stream if session.stream_committed => Ok(()),
            Mode::Stream => {
                let pending: HashSet<_> = self
                    .deliveries
                    .values()
                    .filter(|delivery| {
                        delivery.grant_id == record.intent.request_id
                            && delivery.mode == Mode::Stream
                            && !delivery.committed
                            && self
                                .records
                                .get(&delivery.session_id)
                                .is_some_and(|s| !s.stream_committed)
                    })
                    .map(|delivery| delivery.session_id)
                    .collect();
                if pending.contains(&session_id) {
                    return Ok(());
                }
                remaining(record.views, pending.len(), record.intent.max_views)
            }
            Mode::Download => {
                let pending = self
                    .deliveries
                    .values()
                    .filter(|delivery| {
                        delivery.grant_id == record.intent.request_id
                            && delivery.mode == Mode::Download
                            && !delivery.committed
                    })
                    .count();
                remaining(record.downloads, pending, record.intent.max_downloads)
            }
        }
    }

    pub fn delivery(&self, ticket: &Ticket) -> Result<Delivery, Error> {
        self.deliveries
            .get(&ticket.id)
            .copied()
            .filter(|delivery| delivery.session_id == ticket.session_id)
            .ok_or(Error::Denied)
    }
}

fn remaining(committed: u32, pending: usize, maximum: Option<u32>) -> Result<(), Error> {
    let pending = u32::try_from(pending).map_err(|_| Error::Capacity)?;
    let used = committed.checked_add(pending).ok_or(Error::Denied)?;
    if used < maximum.unwrap_or(u32::MAX) {
        Ok(())
    } else {
        Err(Error::Denied)
    }
}
