//! Delivers verified GOP bytes after the ready control response has reached the transport.
use super::*;

pub(in crate::server) struct Delivery {
    pub(super) op: Operation,
    pub(super) plan: Plan,
    pub(super) snapshot: Snapshot,
    pub(super) source: String,
}

impl Delivery {
    fn authorized(&self) -> bool {
        camera_access::for_session(&self.op.state, self.op.key.0)
            .and_then(|access| camera_access::require_camera(&access, &self.source))
            .is_ok()
    }

    pub(super) fn deliver(
        self,
        deadline: Instant,
        mut send: impl FnMut(OutboundDataMessage) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        let cancelled = || self.op.cancelled();
        let work = crate::storage::archive::target::Work {
            deadline,
            cancelled: &cancelled,
        };
        anyhow::ensure!(self.authorized(), "frame step authorization changed");
        let mut messages = self
            .plan
            .messages(&work)
            .map_err(|_| anyhow::anyhow!("frame step media changed or timed out"))?;
        // Keep the catalog lease through complete byte verification. No chunk or token
        // is committed if reading the GOP changed its original witness or stream interval.
        self.snapshot
            .check(&self.op, &self.plan.selected, deadline)
            .map_err(|_| anyhow::anyhow!("frame step source changed"))?;
        let last = messages
            .pop()
            .ok_or_else(|| anyhow::anyhow!("empty frame step transfer"))?;
        for message in messages {
            work.check()?;
            anyhow::ensure!(self.authorized(), "frame step authorization changed");
            send(message)?;
        }
        work.check()?;
        anyhow::ensure!(self.authorized(), "frame step authorization changed");
        {
            let mut cursors = self
                .op
                .state
                .stored_media_cursors
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let c = cursors
                .get_mut(&self.op.key)
                .filter(|c| self.op.current(c))
                .ok_or_else(|| anyhow::anyhow!("frame step cursor changed"))?;
            // The last chunk permits the client to display and immediately step again.
            // Publish its verified candidate token first, without holding the cursor lock during I/O.
            let previous = c.frames.pending.replace(self.plan.selected.clone());
            c.frames.active = None;
            drop(cursors);
            if let Err(error) = send(last) {
                let mut cursors = self
                    .op
                    .state
                    .stored_media_cursors
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(c) = cursors.get_mut(&self.op.key)
                    && c.incarnation == self.op.incarnation
                    && c.generation == self.op.generation
                    && c.frames
                        .pending
                        .as_ref()
                        .is_some_and(|s| s.token == self.plan.selected.token)
                {
                    c.frames.pending = previous;
                }
                return Err(error);
            }
        }
        Ok(())
    }

    pub(in crate::server) fn after_send(self: Box<Self>) -> PostSendAction {
        Box::new(move || {
            // The ready reply has already been sent. Queue delay before this callback
            // cannot shorten the separate transfer and decode window.
            let deadline = Instant::now() + DECODE_WINDOW;
            if let Err(error) = std::thread::Builder::new()
                .name("stored-frame-step".into())
                .spawn(move || {
                    let transport = self.op.state.webrtc.clone();
                    let session = self.op.key.0;
                    let cancelled = self.op.queued.clone();
                    if self
                        .deliver(deadline, |message| {
                            transport.enqueue_api_data_with_deadline(
                                session,
                                message,
                                cancelled.clone(),
                                Some(deadline),
                            )
                        })
                        .is_err()
                    {
                        cancelled.store(true, Ordering::Release);
                    }
                })
            {
                tracing::warn!(%error,"could not start frame step transfer");
            }
        })
    }
}
