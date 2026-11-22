use super::*;
use crate::profiles::RuntimeProfileStatus;

pub(super) const PART_COUNT: usize =
    crate::profiles::MAX_PROFILE_TARGETS.div_ceil(PROFILE_PART_TARGETS);
const MAX_OBSERVED_CAMERAS: usize = 2 * crate::profiles::MAX_PROFILE_TARGETS;
const COMPACT_PART_SCHEMA: &str = "keeppeek.runtime-profiles.observed.v2";
const PART_SCHEMA: &str = "keeppeek.runtime-profiles.observed.v1";

pub(in crate::server) fn part_index(key: &str) -> Option<usize> {
    let [digit] = key.strip_prefix("observed/")?.as_bytes() else {
        return None;
    };
    let index = usize::from(digit.checked_sub(b'0')?);
    (index < PART_COUNT).then_some(index)
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
        use crate::profiles::FieldSource::Profile;
        let mut rows = serde_json::Map::new();
        for (id, camera) in cameras {
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
        let compact = super::super::super::peek_layouts::json_to_struct(rows.into())
            .map_err(|error| Error::Storage(error.to_string()))?;
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
    let visible;
    let projected = if principal.role == AccessRole::Administrator {
        status
    } else {
        let ids =
            visible_profile_camera_ids(state, principal, identities).ok_or(Error::NotAuthorized)?;
        visible = scoped_status(status, &ids);
        &visible
    };
    entry_for_key(projected, key)
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

pub(super) fn degraded_camera_ids(
    registry: &crate::profiles::ProfileRegistry,
    status: &RuntimeProfileStatus,
) -> Option<BTreeSet<String>> {
    (registry.revision == status.revision && registry.active_profile_id == status.active_profile_id)
        .then(|| {
            status
                .cameras
                .iter()
                .filter(|(_, camera)| {
                    camera.state == crate::profiles::RuntimeApplicationState::Degraded
                })
                .map(|(id, _)| id.clone())
                .collect()
        })
}

pub(super) fn header_snapshot(
    state: &ServerState,
    principal: &ApiPrincipal,
    identities: Option<&crate::keeppeek::CameraProfileIds>,
    registry: &crate::profiles::ProfileRegistry,
    status: &RuntimeProfileStatus,
) -> StoredEntry {
    let mut entry = active_entry(registry);
    set_header_count(
        state,
        Some(principal),
        identities,
        degraded_camera_ids(registry, status).as_ref(),
        &mut entry,
    );
    entry
}

pub(in crate::server) fn set_header_count(
    state: &ServerState,
    principal: Option<&ApiPrincipal>,
    identities: Option<&crate::keeppeek::CameraProfileIds>,
    degraded: Option<&BTreeSet<String>>,
    entry: &mut StoredEntry,
) {
    let count = degraded.and_then(|ids| {
        let principal = principal?;
        if principal.role == AccessRole::Administrator {
            Some(ids.len())
        } else {
            visible_profile_camera_ids(state, principal, identities)
                .map(|visible| ids.intersection(&visible).count())
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
