use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

use super::{
    CameraConfig, CameraEntry, MAX_CAMERA_METADATA_WORKERS, ServerState, probe_onvif_camera,
};

const PENDING_MAX: usize = 128;

#[derive(Default)]
pub(super) struct Queue {
    state: Mutex<Pending>,
    idle: Condvar,
}

#[derive(Default)]
struct Pending {
    entries: VecDeque<CameraEntry>,
    workers: usize,
}

pub(super) fn enqueue(state: &ServerState, configs: Vec<CameraConfig>) {
    let mut pending = state
        .camera_metadata
        .state
        .lock()
        .expect("camera metadata queue is not poisoned");
    for config in configs {
        let Some(entry) = state.camera(&config.ip.to_string()) else {
            continue;
        };
        if let Some(queued) = pending
            .entries
            .iter_mut()
            .find(|queued| queued.info.id == entry.info.id)
        {
            *queued = entry;
        } else if pending.entries.len() < PENDING_MAX {
            pending.entries.push_back(entry);
        } else {
            tracing::warn!(ip = %config.ip, "camera metadata queue is full");
        }
    }
    let available = MAX_CAMERA_METADATA_WORKERS - pending.workers;
    for _ in 0..available.min(pending.entries.len()) {
        let worker = state.clone();
        pending.workers += 1;
        let spawn = std::thread::Builder::new()
            .name("camera-metadata".to_owned())
            .spawn(move || run(&worker));
        if let Err(error) = spawn {
            pending.workers -= 1;
            tracing::warn!(%error, "camera metadata worker could not start");
        }
    }
}

fn run(state: &ServerState) {
    loop {
        let entry = {
            let mut pending = state
                .camera_metadata
                .state
                .lock()
                .expect("camera metadata queue is not poisoned");
            let Some(entry) = pending.entries.pop_front() else {
                pending.workers -= 1;
                state.camera_metadata.idle.notify_all();
                return;
            };
            entry
        };
        if !current(state, &entry) {
            continue;
        }
        match probe_onvif_camera(&entry.configuration) {
            Ok(probe) => state.apply_camera_metadata(&entry, &probe),
            Err(_) => {
                tracing::debug!(ip = %entry.configuration.ip, "configured camera did not provide ONVIF metadata");
            }
        }
        if current(state, &entry) {
            state.probe_hikvision_capabilities(entry.configuration.ip);
        }
    }
}

fn current(state: &ServerState, entry: &CameraEntry) -> bool {
    state.camera(&entry.info.id).is_some_and(|current| {
        std::sync::Arc::ptr_eq(&current.control_revision, &entry.control_revision)
    })
}

#[cfg(test)]
impl Queue {
    pub(super) fn wait_for_idle(&self, timeout: std::time::Duration) -> bool {
        let (pending, _) = self
            .idle
            .wait_timeout_while(
                self.state
                    .lock()
                    .expect("camera metadata queue is not poisoned"),
                timeout,
                |state| state.workers > 0 || !state.entries.is_empty(),
            )
            .expect("camera metadata queue is not poisoned");
        pending.workers == 0 && pending.entries.is_empty()
    }
}
