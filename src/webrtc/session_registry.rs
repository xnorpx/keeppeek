use super::{ApiSessionControl, SessionId};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
    thread::JoinHandle,
};

#[derive(Default)]
pub(super) struct SessionRegistry {
    api_sessions: Mutex<HashMap<SessionId, Arc<ApiSessionControl>>>,
    threads: Mutex<Vec<SessionThread>>,
    session_ids: Mutex<HashSet<SessionId>>,
}

struct SessionThread {
    session_id: SessionId,
    handle: JoinHandle<()>,
}

impl SessionRegistry {
    pub(super) fn reserve_id(&self) -> SessionId {
        let mut session_ids = self
            .session_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            let session_id = SessionId(rand::random());
            if session_id.0 != 0 && session_ids.insert(session_id) {
                return session_id;
            }
        }
    }

    pub(super) fn release_id(&self, session_id: SessionId) {
        self.session_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id);
    }

    pub(super) fn contains_api(&self, session_id: SessionId) -> bool {
        self.api_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains_key(&session_id)
    }

    pub(super) fn api_control(&self, session_id: SessionId) -> Option<Arc<ApiSessionControl>> {
        self.api_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&session_id)
            .cloned()
    }

    pub(super) fn insert_api(&self, session_id: SessionId, control: Arc<ApiSessionControl>) {
        self.api_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(session_id, control);
    }

    pub(super) fn remove_api(&self, session_id: SessionId) -> Option<Arc<ApiSessionControl>> {
        self.api_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&session_id)
    }

    pub(super) fn api_controls(&self) -> Vec<Arc<ApiSessionControl>> {
        self.api_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    pub(super) fn active_api_ids(&self) -> HashSet<SessionId> {
        self.api_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .copied()
            .collect()
    }

    pub(super) fn clear_api(&self) {
        self.api_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    pub(super) fn push_thread(&self, session_id: SessionId, handle: JoinHandle<()>) {
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(SessionThread { session_id, handle });
    }

    pub(super) fn reap_finished(&self) {
        let finished = {
            let mut threads = self
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut active = Vec::with_capacity(threads.len());
            let mut finished = Vec::new();
            for thread in std::mem::take(&mut *threads) {
                if thread.handle.is_finished() {
                    finished.push(thread);
                } else {
                    active.push(thread);
                }
            }
            *threads = active;
            finished
        };
        for thread in finished {
            let session_id = thread.session_id;
            if thread.handle.join().is_err() {
                tracing::warn!(%session_id, "WebRTC session thread panicked");
            }
            self.release_id(session_id);
        }
    }

    pub(super) fn join_thread(&self, session_id: SessionId) {
        let thread = {
            let mut threads = self
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            threads
                .iter()
                .position(|thread| thread.session_id == session_id)
                .map(|index| threads.swap_remove(index))
        };
        if let Some(thread) = thread
            && thread.handle.join().is_err()
        {
            tracing::warn!(%session_id, "WebRTC session thread panicked");
        }
        self.release_id(session_id);
    }

    pub(super) fn join_all_threads(&self) {
        let threads = std::mem::take(
            &mut *self
                .threads
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for thread in threads {
            if thread.handle.join().is_err() {
                tracing::warn!(session_id = %thread.session_id, "WebRTC session thread panicked");
            }
        }
        self.session_ids
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    #[cfg(test)]
    pub(super) fn threads_are_empty(&self) -> bool {
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }
}
