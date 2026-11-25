use super::{ApiSessionControl, ChannelId, ControlEnvelope, Rtc, control_envelope};
use crate::api::proto::{CameraInfo, CameraUpdated, Notification, notification};
use prost::Message as _;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

const BATCH_SIZE: usize = 8;
pub(super) const BATCH_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(super) struct CameraUpdates {
    state: Mutex<Pending>,
    has_work: AtomicBool,
}

#[derive(Default)]
struct Pending {
    // One pending ID per camera in the viewer's last authorized inventory.
    visible: HashMap<String, CameraChange>,
    queue: VecDeque<String>,
    changed_while_opening: Option<bool>,
}

#[derive(Default)]
struct CameraChange {
    queued: bool,
    changed: bool,
}

impl Pending {
    fn mark(&mut self, id: &str) {
        if let Some(change) = self.visible.get_mut(id) {
            change.changed = true;
            if !change.queued {
                change.queued = true;
                self.queue.push_back(id.to_owned());
            }
        }
    }
}

impl CameraUpdates {
    pub(super) fn has_work(&self) -> bool {
        self.has_work.load(Ordering::Acquire)
    }

    pub(super) fn begin_initial_snapshot(&self) {
        self.state.lock().unwrap().changed_while_opening = Some(false);
    }

    pub(super) fn initial_snapshot(&self, cameras: &[CameraInfo]) {
        let mut state = self.state.lock().unwrap();
        let raced = state.changed_while_opening.take().unwrap_or(false);
        state.visible.clear();
        state.queue.clear();
        for camera in cameras {
            state
                .visible
                .insert(camera.source_id.clone(), CameraChange::default());
            if raced {
                state.mark(&camera.source_id);
            }
        }
        self.has_work
            .store(!state.queue.is_empty(), Ordering::Release);
    }

    pub(super) fn refresh_snapshot(&self, cameras: &[CameraInfo]) {
        let mut state = self.state.lock().unwrap();
        let ids: HashSet<_> = cameras
            .iter()
            .map(|camera| camera.source_id.as_str())
            .collect();
        state.visible.retain(|id, _| ids.contains(id.as_str()));
        state.queue.retain(|id| ids.contains(id.as_str()));
        for camera in cameras {
            let id = &camera.source_id;
            // A separately built full inventory may predate a delta we already sent.
            if state.visible.get(id).is_none_or(|change| change.changed) {
                state.visible.entry(id.clone()).or_default();
                state.mark(id);
            }
        }
        self.has_work
            .store(!state.queue.is_empty(), Ordering::Release);
    }

    pub(super) fn mark(&self, id: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        if let Some(changed) = &mut state.changed_while_opening {
            *changed = true;
        }
        let was_empty = state.queue.is_empty();
        state.mark(id);
        let pending = !state.queue.is_empty();
        self.has_work.store(pending, Ordering::Release);
        was_empty && pending
    }

    fn pop(&self) -> Option<String> {
        let mut state = self.state.lock().unwrap();
        let id = state.queue.pop_front()?;
        state
            .visible
            .get_mut(&id)
            .expect("queued camera is visible")
            .queued = false;
        self.has_work
            .store(!state.queue.is_empty(), Ordering::Release);
        Some(id)
    }

    fn write_batch(
        &self,
        mut project: impl FnMut(&str) -> Option<CameraInfo>,
        mut write: impl FnMut(&[u8]) -> anyhow::Result<bool>,
    ) -> anyhow::Result<bool> {
        let mut wrote = false;
        for _ in 0..BATCH_SIZE {
            let Some(source_id) = self.pop() else { break };
            let Some(camera) = project(&source_id) else {
                continue;
            };
            let payload = ControlEnvelope {
                message: Some(control_envelope::Message::Notification(Notification {
                    event: Some(notification::Event::CameraUpdated(CameraUpdated {
                        source_id: source_id.clone(),
                        camera: Some(camera),
                    })),
                })),
            };
            anyhow::ensure!(
                payload.encoded_len() <= super::MAX_CONTROL_MESSAGE_BYTES,
                "camera capability update exceeds the control message limit"
            );
            if !write(&payload.encode_to_vec())? {
                // Keep only the ID. Retrying rechecks the viewer and fetches the latest metadata.
                self.mark(&source_id);
                break;
            }
            wrote = true;
        }
        Ok(wrote)
    }
}

impl ApiSessionControl {
    pub(super) fn write_camera_updates(
        &self,
        rtc: &mut Rtc,
        channel_id: ChannelId,
    ) -> anyhow::Result<bool> {
        let handler = self
            .control_handler
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(Weak::upgrade);
        self.camera_updates.write_batch(
            |id| handler.as_ref()?.camera_capability(self.session_id, id),
            |bytes| {
                let mut channel = rtc.channel(channel_id).ok_or_else(|| {
                    anyhow::anyhow!("WebRTC control channel disappeared before camera update")
                })?;
                Ok(channel.write(true, bytes)?)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(id: &str) -> CameraInfo {
        CameraInfo {
            source_id: id.to_owned(),
            ..Default::default()
        }
    }

    fn delta(bytes: &[u8]) -> CameraUpdated {
        let control_envelope::Message::Notification(notice) =
            ControlEnvelope::decode(bytes).unwrap().message.unwrap()
        else {
            panic!("missing notification")
        };
        let notification::Event::CameraUpdated(update) = notice.event.unwrap() else {
            panic!("expected a single-camera change")
        };
        update
    }

    #[test]
    fn two_discovery_results_for_127_cameras_are_bounded_and_delivered_after_backpressure() {
        let updates = CameraUpdates::default();
        let cameras: Vec<_> = (0..127).map(|i| camera(&format!("camera-{i}"))).collect();
        updates.begin_initial_snapshot();
        updates.initial_snapshot(&cameras);
        for _ in 0..2 {
            for cam in &cameras {
                updates.mark(&cam.source_id);
            }
        }
        assert!(!updates.mark("not-in-this-session"));
        assert_eq!(updates.state.lock().unwrap().queue.len(), 127);
        assert!(
            !updates
                .write_batch(|id| Some(camera(id)), |_| Ok(false))
                .unwrap()
        );
        let mut emitted = HashSet::new();
        while updates.has_work() {
            let before = emitted.len();
            updates
                .write_batch(
                    |id| {
                        Some(CameraInfo {
                            display_name: "current".into(),
                            ..camera(id)
                        })
                    },
                    |bytes| {
                        let update = delta(bytes);
                        let info = update.camera.unwrap();
                        assert_eq!(info.source_id, update.source_id);
                        assert_eq!(info.display_name, "current");
                        assert!(emitted.insert(info.source_id));
                        Ok(true)
                    },
                )
                .unwrap();
            assert!((1..=BATCH_SIZE).contains(&(emitted.len() - before)));
        }
        assert_eq!(emitted.len(), 127);
    }

    #[test]
    fn changes_during_snapshot_or_send_converge_and_withdrawn_access_drops_the_retry() {
        let updates = CameraUpdates::default();
        updates.begin_initial_snapshot();
        updates.mark("front");
        updates.initial_snapshot(&[camera("front"), camera("back")]);
        let mut sent = Vec::new();
        updates
            .write_batch(
                |id| Some(camera(id)),
                |bytes| {
                    let id = delta(bytes).source_id;
                    if sent.is_empty() {
                        updates.mark(&id);
                    }
                    sent.push(id);
                    Ok(true)
                },
            )
            .unwrap();
        assert_eq!(sent, ["front", "back", "front"]);
        updates.mark("front");
        assert!(
            !updates
                .write_batch(|id| Some(camera(id)), |_| Ok(false))
                .unwrap()
        );
        let mut authorized_checks = 0;
        updates
            .write_batch(
                |_| {
                    authorized_checks += 1;
                    None
                },
                |_| panic!("revoked camera was sent"),
            )
            .unwrap();
        assert_eq!(authorized_checks, 1);
        updates.refresh_snapshot(&[camera("back"), camera("new")]);
        assert!(!updates.mark("front"));
        let mut sent = Vec::new();
        updates
            .write_batch(
                |id| Some(camera(id)),
                |bytes| {
                    sent.push(delta(bytes).source_id);
                    Ok(true)
                },
            )
            .unwrap();
        assert_eq!(sent, ["back", "new"]);
    }
}
