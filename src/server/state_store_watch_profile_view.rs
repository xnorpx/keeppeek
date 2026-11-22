use super::*;
use crate::{keeppeek::CameraProfileIds, profiles::RuntimeProfileStatus};
use std::collections::BTreeSet;

enum Visibility {
    Administrator,
    Restricted(Option<BTreeSet<String>>),
}

struct ScopedObservations {
    source: Arc<RuntimeProfileStatus>,
    status: RuntimeProfileStatus,
    generation: Option<String>,
}

pub(super) struct SessionView {
    visibility: Visibility,
    observed: Option<ScopedObservations>,
}

impl SessionView {
    pub(super) fn new(
        state: &ServerState,
        id: SessionId,
        identities: Option<&CameraProfileIds>,
    ) -> Self {
        let principal = state
            .api_session_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .map(|session| session.principal.clone());
        let visibility = match principal {
            Some(principal) if principal.role == AccessRole::Administrator => {
                Visibility::Administrator
            }
            principal => Visibility::Restricted(principal.as_ref().and_then(|principal| {
                profiles::visible_profile_camera_ids(state, principal, identities)
            })),
        };
        Self {
            visibility,
            observed: None,
        }
    }

    fn observed_entry(
        &mut self,
        source: &Arc<RuntimeProfileStatus>,
        administrator: Option<&StoredEntry>,
        key: &str,
    ) -> Option<StoredEntry> {
        let visible = match &self.visibility {
            Visibility::Administrator => {
                return administrator
                    .filter(|entry| {
                        entry.value.encoded_len() <= profiles::PROFILE_DOCUMENT_BYTES_MAX
                    })
                    .cloned();
            }
            Visibility::Restricted(visible) => visible.as_ref()?,
        };
        if self
            .observed
            .as_ref()
            .is_none_or(|cached| !Arc::ptr_eq(&cached.source, source))
        {
            let scoped = profiles::observed::scoped_status(source, visible);
            let generation = profiles::observed::generation(&scoped).ok();
            self.observed = Some(ScopedObservations {
                source: Arc::clone(source),
                status: scoped,
                generation,
            });
        }
        let scoped = self.observed.as_ref()?;
        if let Some(index) = profiles::observed::part_index(key) {
            profiles::observed::part_entry(&scoped.status, index, scoped.generation.as_ref()?).ok()
        } else {
            profiles::observed::entry_for_key(&scoped.status, key).ok()
        }
    }

    pub(super) fn project(&mut self, event: &WatchEvent) -> WatchEvent {
        let kind = match &event.kind {
            WatchEventKind::ProfileObservation {
                status,
                administrator,
            } => self
                .observed_entry(status, administrator.as_ref(), &event.key)
                .map_or(WatchEventKind::ResnapshotRequired, WatchEventKind::Put),
            WatchEventKind::ProfileHeader {
                entry,
                observations,
            } => {
                let count = observations
                    .as_ref()
                    .and_then(|states| match &self.visibility {
                        Visibility::Administrator => states.count(None),
                        Visibility::Restricted(visible) => visible
                            .as_ref()
                            .and_then(|visible| states.count(Some(visible))),
                    });
                let mut entry = entry.clone();
                profiles::observed::write_header_count(count, &mut entry);
                WatchEventKind::Put(entry)
            }
            WatchEventKind::Put(entry) if is_profile_status_event(event) => {
                let mut entry = entry.clone();
                if let Visibility::Restricted(ids) = &self.visibility {
                    profiles::observed::filter_status_rows(ids.as_ref(), &mut entry);
                }
                WatchEventKind::Put(entry)
            }
            _ => return event.clone(),
        };
        WatchEvent {
            namespace: event.namespace.clone(),
            key: event.key.clone(),
            revision: event.revision,
            kind,
        }
    }
}

pub(super) fn needs_projection(event: &WatchEvent) -> bool {
    is_profile_status_event(event) || matches!(event.kind, WatchEventKind::ProfileHeader { .. })
}

pub(super) struct Projection {
    pub(super) event: WatchEvent,
    pub(super) fingerprint: Option<[u8; 32]>,
}

impl Projection {
    pub(super) fn new(event: WatchEvent) -> Self {
        let fingerprint = if event.namespace == profiles::NAMESPACE
            && let WatchEventKind::Put(entry) = &event.kind
        {
            Some(profile_fingerprint(entry))
        } else {
            None
        };
        Self { event, fingerprint }
    }

    pub(super) fn visible_revision(
        &self,
        watch: &mut Watch,
        revisions: &mut HashMap<SessionId, u64>,
    ) -> Option<u64> {
        if self.event.namespace != profiles::NAMESPACE {
            return Some(self.event.revision);
        }
        if let Some(fingerprint) = self.fingerprint {
            if watch.profile_fingerprint == Some(fingerprint) {
                return None;
            }
            watch.profile_fingerprint = Some(fingerprint);
        }
        let Some(session) = &watch.profile_session else {
            return Some(self.event.revision);
        };
        // The watch lock serializes each session's snapshot and visible publications.
        Some(*revisions.entry(watch.session_id).or_insert_with(|| {
            let next = session
                .profile_watch_revision
                .load(Ordering::Relaxed)
                .max(self.event.revision)
                .checked_add(1)
                .expect("profile watch revision space exhausted");
            session
                .profile_watch_revision
                .store(next, Ordering::Relaxed);
            next
        }))
    }
}

pub(super) struct Batch {
    identities: Option<CameraProfileIds>,
    scoped: HashMap<(SessionId, usize), Projection>,
}

impl Batch {
    pub(super) fn new(
        registry: &WatchRegistry,
        state: &ServerState,
        events: &[WatchEvent],
        has_profiles: bool,
    ) -> Self {
        let needs_identities = has_profiles
            && state
                .api_session_owners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
                .any(|owner| owner.principal.role != AccessRole::Administrator);
        let identities = needs_identities
            .then(|| profiles::camera_profile_ids(state).ok())
            .flatten();
        let scoped = if has_profiles {
            prepare(registry, state, events, identities.as_ref())
        } else {
            HashMap::new()
        };
        Self { identities, scoped }
    }

    pub(super) fn for_session<'a>(
        &'a mut self,
        state: &ServerState,
        id: SessionId,
        index: usize,
        event: &WatchEvent,
        unscoped: &'a mut Option<Projection>,
    ) -> &'a Projection {
        if !needs_projection(event) {
            return unscoped.get_or_insert_with(|| Projection::new(event.clone()));
        }
        self.scoped.entry((id, index)).or_insert_with(|| {
            // A watch registered after preparation still needs the session's camera scope.
            Projection::new(SessionView::new(state, id, self.identities.as_ref()).project(event))
        })
    }
}

/// Prepare each watched key once per session, without holding the watch registry lock.
fn prepare(
    registry: &WatchRegistry,
    state: &ServerState,
    events: &[WatchEvent],
    identities: Option<&CameraProfileIds>,
) -> HashMap<(SessionId, usize), Projection> {
    let mut targets = HashMap::<SessionId, BTreeSet<String>>::new();
    for watch in registry.lock().values() {
        if watch.namespace == profiles::NAMESPACE && profiles::viewer_watch_key(&watch.key_prefix) {
            targets
                .entry(watch.session_id)
                .or_default()
                .insert(watch.key_prefix.clone());
        }
    }
    let mut prepared = HashMap::new();
    for (session, keys) in targets {
        let mut view = None;
        for (index, event) in events.iter().enumerate() {
            if needs_projection(event) && keys.contains(&event.key) {
                let projected = view
                    .get_or_insert_with(|| SessionView::new(state, session, identities))
                    .project(event);
                prepared.insert((session, index), Projection::new(projected));
            }
        }
    }
    prepared
}
