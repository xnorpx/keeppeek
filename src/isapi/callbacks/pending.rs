use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

use super::{Fence, KeepPeekEvent, Permit, Tracker};

pub(super) struct Pending {
    pub hash: Option<[u8; 32]>,
    tracker: Tracker,
    changes: Vec<KeepPeekEvent>,
    offset: usize,
    ack: Option<Receiver<usize>>,
    unknown: bool,
    fence: Option<Fence>,
    _permit: Option<Permit>,
}

impl Pending {
    pub const fn new(
        hash: Option<[u8; 32]>,
        tracker: Tracker,
        changes: Vec<KeepPeekEvent>,
        permit: Option<Permit>,
    ) -> Self {
        Self {
            hash,
            tracker,
            changes,
            offset: 0,
            ack: None,
            unknown: false,
            fence: None,
            _permit: permit,
        }
    }
    pub fn fenced(mut self, fence: Fence) -> Self {
        self.fence = Some(fence);
        self
    }
}

pub(super) struct State {
    generation: u64,
    pub tracker: Tracker,
    pub pending: Option<Pending>,
    seen: VecDeque<([u8; 32], Instant)>,
}

impl State {
    pub const fn new(tracker: Tracker) -> Self {
        Self {
            generation: 1,
            tracker,
            pending: None,
            seen: VecDeque::new(),
        }
    }

    pub fn seen(&mut self, hash: [u8; 32], now: Instant) -> bool {
        self.seen.retain(|(_, received)| {
            now.saturating_duration_since(*received) < Duration::from_secs(300)
        });
        self.seen.iter().any(|(previous, _)| *previous == hash)
    }

    pub fn reconcile(&mut self) {
        if let Some(pending) = self.pending.as_mut()
            && let Some(ack) = &pending.ack
        {
            match ack.try_recv() {
                Ok(committed) => {
                    assert!(committed <= pending.changes.len() - pending.offset);
                    pending.offset += committed;
                    pending.ack = None;
                }
                Err(TryRecvError::Disconnected) => {
                    pending.unknown = true;
                    pending.ack = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        self.finish();
    }

    fn finish(&mut self) {
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.offset == pending.changes.len())
        {
            let pending = self.pending.take().expect("completed callback exists");
            self.tracker = pending.tracker;
            if let Some(hash) = pending.hash {
                if self.seen.len() >= 1024 {
                    self.seen.pop_front();
                }
                self.seen.push_back((hash, Instant::now()));
            }
        }
    }

    pub fn submit(&mut self, tx: &SyncSender<KeepPeekEvent>) -> anyhow::Result<()> {
        self.finish();
        let Some(pending) = self.pending.as_mut() else {
            return Ok(());
        };
        anyhow::ensure!(
            !pending.unknown,
            "ISAPI callback commit outcome is unknown; automatic retry is blocked"
        );
        if pending.ack.is_some() {
            return Ok(());
        }
        let (reply, ack) = mpsc::sync_channel(1);
        tx.try_send(KeepPeekEvent::IsapiBatch {
            fence: pending.fence.clone(),
            changes: pending.changes[pending.offset..].to_vec(),
            reply,
        })
        .map_err(|_| anyhow::anyhow!("ISAPI callback commit queue is unavailable"))?;
        pending.ack = Some(ack);
        Ok(())
    }

    pub fn wait(&mut self, timeout: Duration) {
        if let Some(pending) = self.pending.as_mut()
            && let Some(ack) = &pending.ack
            && let Ok(committed) = ack.recv_timeout(timeout)
        {
            assert!(committed <= pending.changes.len() - pending.offset);
            pending.offset += committed;
            pending.ack = None;
        }
        self.finish();
    }

    pub fn synchronize(
        &mut self,
        generation: u64,
        ip: std::net::IpAddr,
        channel: u32,
        generic_motion: bool,
    ) -> bool {
        if generation < self.generation {
            return false;
        }
        if generation == self.generation {
            return true;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.ack.is_some() || pending.unknown)
        {
            return false;
        }
        let mut endings = std::collections::BTreeMap::new();
        let mut changes = self.tracker.disconnect();
        if let Some(mut pending) = self.pending.take() {
            changes.extend(pending.tracker.disconnect());
            changes.extend(
                pending
                    .changes
                    .into_iter()
                    .skip(pending.offset)
                    .filter(|change| matches!(change, KeepPeekEvent::TimelineEventEnded { .. })),
            );
        }
        for change in changes {
            if let KeepPeekEvent::TimelineEventEnded { id, end_time_ms } = change {
                endings
                    .entry(id)
                    .and_modify(|previous: &mut i64| *previous = (*previous).max(end_time_ms))
                    .or_insert(end_time_ms);
            }
        }
        let tracker = Tracker::new(ip, channel, generic_motion);
        let changes = endings
            .into_iter()
            .map(|(id, end_time_ms)| KeepPeekEvent::TimelineEventEnded { id, end_time_ms })
            .collect::<Vec<_>>();
        self.tracker = tracker.clone();
        self.generation = generation;
        self.pending = (!changes.is_empty()).then(|| Pending::new(None, tracker, changes, None));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_changes_preserve_uncommitted_endings() {
        let ip = "127.0.0.1".parse().unwrap();
        let tracker = Tracker::new(ip, 1, false);
        let mut state = State::new(tracker.clone());
        state.pending = Some(Pending::new(
            None,
            tracker,
            vec![KeepPeekEvent::TimelineEventEnded {
                id: "committed-start".to_owned(),
                end_time_ms: 2000,
            }],
            None,
        ));
        assert!(state.synchronize(2, ip, 1, false));
        assert!(state.synchronize(3, ip, 1, false));
        let (tx, rx) = mpsc::sync_channel(1);
        state.submit(&tx).unwrap();
        let KeepPeekEvent::IsapiBatch { changes, .. } =
            rx.recv_timeout(Duration::from_millis(100)).unwrap()
        else {
            panic!("expected retained clear")
        };
        let [KeepPeekEvent::TimelineEventEnded { id, end_time_ms }] = changes.as_slice() else {
            panic!("expected one retained clear")
        };
        assert_eq!(id, "committed-start");
        assert_eq!(*end_time_ms, 2000);
    }

    #[test]
    fn old_uploads_cannot_roll_callback_state_back_to_an_earlier_generation() {
        let ip = "127.0.0.1".parse().unwrap();
        let mut state = State::new(Tracker::new(ip, 1, false));
        assert!(state.synchronize(3, ip, 1, false));
        assert!(!state.synchronize(2, ip, 1, false));
        assert_eq!(state.generation, 3);
    }
}
