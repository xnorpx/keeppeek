use super::*;
use crate::profiles::RuntimeProfileStatus;

pub(super) const PART_COUNT: usize =
    crate::profiles::MAX_PROFILE_TARGETS.div_ceil(PROFILE_PART_TARGETS);
// Eight documents can represent 256 configured cameras plus every retained missing
// target in a 127-camera profile, within the existing per-session watch budget.
const MAX_OBSERVED_CAMERAS: usize = 256 + crate::profiles::MAX_PROFILE_TARGETS;
const COMPACT_PART_SCHEMA: &str = "keeppeek.runtime-profiles.observed.v2";
const PART_SCHEMA: &str = "keeppeek.runtime-profiles.observed.v1";

pub(in crate::server) fn part_index(key: &str) -> Option<usize> {
    let suffix = key.strip_prefix("observed/")?;
    if (suffix.len() > 1 && suffix.starts_with('0')) || !suffix.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    suffix
        .parse::<usize>()
        .ok()
        .filter(|&index| index < PART_COUNT)
}

pub(in crate::server) fn generation(status: &RuntimeProfileStatus) -> Result<String, Error> {
    if status.cameras.len() > MAX_OBSERVED_CAMERAS {
        return Err(Error::Invalid(Invalid::ValueTooLarge));
    }
    // Worker observations can change without changing the desired profile revision.
    static BOOT_ID: std::sync::OnceLock<uuid::Uuid> = std::sync::OnceLock::new();
    let bytes = serde_json::to_vec(status).map_err(|error| Error::Storage(error.to_string()))?;
    let digest = Sha256::new()
        .chain_update(BOOT_ID.get_or_init(uuid::Uuid::new_v4).as_bytes())
        .chain_update(status.definition_revision.unwrap_or_default().to_be_bytes())
        .chain_update(bytes);
    hex_digest(digest)
}

#[derive(Default)]
pub(in crate::server) struct GenerationCache(
    std::sync::Mutex<Option<(RuntimeProfileStatus, std::sync::Arc<str>)>>,
);

impl GenerationCache {
    fn for_status(&self, status: &RuntimeProfileStatus) -> Result<std::sync::Arc<str>, Error> {
        let mut cached = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((previous, generation)) = cached.as_ref()
            && previous == status
        {
            return Ok(generation.clone());
        }
        // One snapshot covers a multipart read without retaining a row set for every viewer.
        let generation: std::sync::Arc<str> = generation(status)?.into();
        *cached = Some((status.clone(), generation.clone()));
        Ok(generation)
    }
}

pub(in crate::server) fn part_entry(
    status: &RuntimeProfileStatus,
    index: usize,
    generation: &str,
) -> Result<StoredEntry, Error> {
    let width = PROFILE_PART_TARGETS.max(status.cameras.len().div_ceil(PART_COUNT));
    let cameras: BTreeMap<_, _> = status
        .cameras
        .iter()
        .skip(index * width)
        .take(width)
        .map(|(id, application)| {
            let mut application = application.clone();
            if let Some(reason) = &mut application.reason {
                reason.truncate(reason.floor_char_boundary(96));
            }
            (id.clone(), application)
        })
        .collect();
    let mut entry = status_entry(RuntimeProfileStatus {
        revision: status.revision,
        active_profile_id: status.active_profile_id.clone(),
        definition_revision: status.definition_revision,
        cameras: cameras.clone(),
    })
    .map_err(|error| Error::Storage(error.to_string()))?;
    entry.key = format!("observed/{index}");
    entry.schema = PART_SCHEMA.to_owned();
    entry.value.fields.insert(
        "definition_revision".to_owned(),
        Value {
            kind: Some(
                status
                    .definition_revision
                    .map_or(Kind::NullValue(0), |revision| {
                        Kind::StringValue(revision.to_string())
                    }),
            ),
        },
    );
    entry.value.fields.insert(
        "generation".to_owned(),
        Value {
            kind: Some(Kind::StringValue(generation.to_owned())),
        },
    );
    entry.value.fields.insert(
        "part_index".to_owned(),
        Value {
            kind: Some(Kind::NumberValue(
                u32::try_from(index)
                    .expect("observed part is bounded")
                    .into(),
            )),
        },
    );
    entry.value.fields.insert(
        "part_count".to_owned(),
        Value {
            kind: Some(Kind::NumberValue(
                u32::try_from(PART_COUNT)
                    .expect("observed part count is bounded")
                    .into(),
            )),
        },
    );
    if entry.value.encoded_len() > PROFILE_DOCUMENT_BYTES_MAX {
        let compact = compact_rows(cameras)?;
        entry.value.fields.insert(
            "cameras".to_owned(),
            Value {
                kind: Some(Kind::StructValue(compact)),
            },
        );
        entry.schema = COMPACT_PART_SCHEMA.to_owned();
    }
    bounded(entry)
}

fn compact_rows(
    cameras: BTreeMap<String, crate::profiles::CameraRuntimeApplication>,
) -> Result<Struct, Error> {
    use crate::profiles::FieldSource::Profile;
    // At most 48 maximum-length IDs must fit a 16 KiB protobuf document.
    let reason_bytes = if cameras.len() > 40 {
        24
    } else if cameras.len() > 32 {
        64
    } else {
        96
    };
    let mut rows = serde_json::Map::new();
    for (id, mut camera) in cameras {
        if let Some(reason) = &mut camera.reason {
            reason.truncate(reason.floor_char_boundary(reason_bytes));
        }
        let policy = &camera.policy;
        let flags = u8::from(policy.available)
            | u8::from(policy.privacy_enforced) << 1
            | u8::from(policy.notifications_enabled) << 2
            | u8::from(policy.available_source == Profile) << 3
            | u8::from(policy.recording_mode_source == Profile) << 4
            | u8::from(policy.privacy_source == Profile) << 5
            | u8::from(policy.notifications_source == Profile) << 6;
        rows.insert(
            id,
            serde_json::json!([camera.state, policy.recording_mode, flags, camera.reason]),
        );
    }
    super::super::super::peek_layouts::json_to_struct(rows.into())
        .map_err(|error| Error::Storage(error.to_string()))
}

pub(super) fn bounded(entry: StoredEntry) -> Result<StoredEntry, Error> {
    if entry.value.encoded_len() > PROFILE_DOCUMENT_BYTES_MAX {
        return Err(Error::Invalid(Invalid::ValueTooLarge));
    }
    Ok(entry)
}

pub(super) fn snapshot_entry(
    state: &ServerState,
    principal: &ApiPrincipal,
    identities: Option<&crate::keeppeek::CameraProfileIds>,
    status: &RuntimeProfileStatus,
    key: &str,
) -> Result<StoredEntry, Error> {
    let displayed = application::observed_status(state, status);
    let status = displayed.as_ref();
    let visible;
    let projected = if principal.role == AccessRole::Administrator {
        status
    } else {
        let ids = visible_profile_camera_ids(state, principal, identities)
            .ok_or_else(|| Error::Storage("camera visibility is unavailable".to_owned()))?;
        visible = scoped_status(status, &ids);
        &visible
    };
    if let Some(index) = part_index(key) {
        let generation = state.profile_generation_cache.for_status(projected)?;
        part_entry(projected, index, &generation)
    } else {
        entry_for_key(projected, key)
    }
}

pub(in crate::server) fn scoped_status(
    status: &RuntimeProfileStatus,
    ids: &BTreeSet<String>,
) -> RuntimeProfileStatus {
    RuntimeProfileStatus {
        revision: status.revision,
        active_profile_id: status.active_profile_id.clone(),
        definition_revision: status.definition_revision,
        cameras: ids
            .iter()
            .filter_map(|id| {
                status
                    .cameras
                    .get(id)
                    .map(|camera| (id.clone(), camera.clone()))
            })
            .collect(),
    }
}

pub(in crate::server) fn entry_for_key(
    status: &RuntimeProfileStatus,
    key: &str,
) -> Result<StoredEntry, Error> {
    if let Some(index) = part_index(key) {
        part_entry(status, index, &generation(status)?)
    } else {
        bounded(status_entry(status.clone()).map_err(|error| Error::Storage(error.to_string()))?)
    }
}

pub(in crate::server) fn filter_status_rows(
    visible: Option<&BTreeSet<String>>,
    entry: &mut StoredEntry,
) {
    if let Some(value) = entry.value.fields.get_mut("cameras") {
        if let Some(Kind::StructValue(cameras)) = &mut value.kind {
            cameras
                .fields
                .retain(|id, _| visible.is_some_and(|allowed| allowed.contains(id)));
        } else {
            value.kind = Some(Kind::StructValue(Struct::default()));
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(in crate::server) struct ObservedCameraStates {
    degraded: BTreeSet<String>,
    pending: BTreeSet<String>,
}

impl ObservedCameraStates {
    pub(in crate::server) fn count(&self, visible: Option<&BTreeSet<String>>) -> Option<usize> {
        match visible {
            Some(ids) if self.pending.is_disjoint(ids) => {
                Some(self.degraded.intersection(ids).count())
            }
            None if self.pending.is_empty() => Some(self.degraded.len()),
            _ => None,
        }
    }
}

pub(super) fn observed_camera_states(
    registry: &crate::profiles::ProfileRegistry,
    status: &RuntimeProfileStatus,
) -> Option<ObservedCameraStates> {
    if registry.revision != status.revision
        || registry.active_profile_id != status.active_profile_id
        || registry.active_definition_revision() != status.definition_revision
    {
        return None;
    }
    let mut states = ObservedCameraStates::default();
    for (id, camera) in &status.cameras {
        use crate::profiles::RuntimeApplicationState;
        match camera.state {
            RuntimeApplicationState::Pending => {
                states.pending.insert(id.clone());
            }
            RuntimeApplicationState::Degraded => {
                states.degraded.insert(id.clone());
            }
            RuntimeApplicationState::Applied => {}
        }
    }
    Some(states)
}

pub(super) fn header_snapshot(
    state: &ServerState,
    principal: &ApiPrincipal,
    identities: Option<&crate::keeppeek::CameraProfileIds>,
    registry: &crate::profiles::ProfileRegistry,
    status: &RuntimeProfileStatus,
) -> StoredEntry {
    let status = application::observed_status(state, status);
    let mut entry = active_entry(registry);
    set_header_count(
        state,
        Some(principal),
        identities,
        observed_camera_states(registry, &status).as_ref(),
        &mut entry,
    );
    entry
}

pub(in crate::server) fn set_header_count(
    state: &ServerState,
    principal: Option<&ApiPrincipal>,
    identities: Option<&crate::keeppeek::CameraProfileIds>,
    observations: Option<&ObservedCameraStates>,
    entry: &mut StoredEntry,
) {
    let count = observations.and_then(|states| {
        let principal = principal?;
        if principal.role == AccessRole::Administrator {
            states.count(None)
        } else {
            visible_profile_camera_ids(state, principal, identities)
                .and_then(|visible| states.count(Some(&visible)))
        }
    });
    write_header_count(count, entry);
}

pub(in crate::server) fn write_header_count(count: Option<usize>, entry: &mut StoredEntry) {
    entry.value.fields.insert(
        "degraded_cameras".to_owned(),
        Value {
            kind: Some(
                count
                    .and_then(|count| u32::try_from(count).ok())
                    .map_or(Kind::NullValue(0), |count| Kind::NumberValue(count.into())),
            ),
        },
    );
}

#[cfg(test)]
mod key_tests {
    use super::{PART_COUNT, part_index};

    #[test]
    fn only_canonical_decimal_part_keys_address_the_published_parts() {
        for index in 0..PART_COUNT {
            assert_eq!(part_index(&format!("observed/{index}")), Some(index));
        }
        assert_eq!(part_index(&format!("observed/{PART_COUNT}")), None);
        for key in [
            "observed/",
            "observed/+1",
            "observed/01",
            "observed/-1",
            "observed/1/0",
            "other/0",
        ] {
            assert_eq!(part_index(key), None, "{key}");
        }
    }
}

#[cfg(test)]
#[test]
fn generation_cache_reuses_only_the_same_observed_status() {
    let cache = GenerationCache::default();
    let mut status = RuntimeProfileStatus::default();
    let first = cache.for_status(&status).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &first,
        &cache.for_status(&status).unwrap()
    ));
    status.definition_revision = Some(2);
    let edited = cache.for_status(&status).unwrap();
    assert_ne!(first, edited);
    status.definition_revision = None;
    assert_eq!(first, cache.for_status(&status).unwrap());
}
