use super::{
    Clock, Error, SessionContext,
    model::{MAX_SESSIONS, SESSION_LIFETIME_MS},
    record::Record,
    secret::{Secret, digest, valid},
    sessions::Session,
};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use uuid::Uuid;

const MAX_RECEIPTS: usize = 16;
const _: () = assert!(MAX_RECEIPTS > MAX_SESSIONS);

pub(super) struct Receipt {
    pub handle_hash: [u8; 32],
    pub context: SessionContext,
    pub grant_id: Uuid,
    pub initial_revision: u64,
    pub deadline: Instant,
}

#[derive(Default)]
pub(super) struct Receipts {
    records: HashMap<Uuid, Receipt>,
}

impl Receipts {
    pub fn issue(
        &mut self,
        id: Uuid,
        handle: &Secret,
        record: &Record,
        context: &SessionContext,
        clock: Clock,
        active: &HashMap<Uuid, Session>,
    ) -> Result<(), Error> {
        let deadline = clock
            .monotonic
            .checked_add(Duration::from_millis(
                u64::try_from(SESSION_LIFETIME_MS).expect("session lifetime is positive"),
            ))
            .ok_or(Error::Denied)?;
        if self.records.len() >= MAX_RECEIPTS {
            let oldest = self
                .records
                .iter()
                .filter(|(id, _)| !active.contains_key(id))
                .min_by_key(|(id, receipt)| (receipt.deadline, **id))
                .map(|(id, _)| *id)
                .ok_or(Error::Capacity)?;
            self.records.remove(&oldest);
        }
        let receipt = Receipt {
            handle_hash: handle.hash(),
            context: context.clone(),
            grant_id: record.intent.request_id,
            initial_revision: record.revision,
            deadline,
        };
        assert!(
            self.records.insert(id, receipt).is_none(),
            "guest receipt UUID collision"
        );
        Ok(())
    }

    pub fn lookup(
        &self,
        handle: &str,
        expected_id: Option<Uuid>,
        context: &SessionContext,
    ) -> Result<(Uuid, &Receipt), Error> {
        if !valid(handle) {
            return Err(Error::Denied);
        }
        let hash = digest(handle);
        self.records
            .iter()
            .find(|(id, receipt)| {
                bool::from(receipt.handle_hash.ct_eq(&hash))
                    && receipt.context == *context
                    && expected_id.is_none_or(|expected| expected == **id)
            })
            .map(|(id, receipt)| (*id, receipt))
            .ok_or(Error::Denied)
    }

    pub fn expire(&mut self, clock: Clock) {
        self.records
            .retain(|_, receipt| clock.monotonic < receipt.deadline);
    }

    pub fn remove(&mut self, id: Uuid) {
        self.records.remove(&id);
    }
}
