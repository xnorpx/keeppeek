use super::*;
use crate::profiles::RuntimeProfileStatus;

pub(super) const PART_COUNT: usize =
    crate::profiles::MAX_PROFILE_TARGETS.div_ceil(PROFILE_PART_TARGETS);
const PART_SCHEMA: &str = "keeppeek.runtime-profiles.observed.v1";

pub(in crate::server) fn part_index(key: &str) -> Option<usize> {
    let [digit] = key.strip_prefix("observed/")?.as_bytes() else {
        return None;
    };
    let index = usize::from(digit.checked_sub(b'0')?);
    (index < PART_COUNT).then_some(index)
}

pub(super) fn generation(status: &RuntimeProfileStatus) -> Result<String, Error> {
    if status.cameras.len() > crate::profiles::MAX_PROFILE_TARGETS {
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

pub(super) fn part_entry(
    status: &RuntimeProfileStatus,
    index: usize,
    generation: &str,
) -> Result<StoredEntry, Error> {
    let cameras = status
        .cameras
        .iter()
        .skip(index * PROFILE_PART_TARGETS)
        .take(PROFILE_PART_TARGETS)
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
        cameras,
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
    let mut entry = if let Some(index) = part_index(key) {
        part_entry(status, index, &generation(status)?)?
    } else {
        status_entry(status.clone()).map_err(|error| Error::Storage(error.to_string()))?
    };
    project(state, Some(principal), identities, &mut entry);
    bounded(entry)
}

pub(in crate::server) fn project(
    state: &ServerState,
    principal: Option<&ApiPrincipal>,
    identities: Option<&crate::keeppeek::CameraProfileIds>,
    entry: &mut StoredEntry,
) {
    if principal.is_some_and(|principal| principal.role == AccessRole::Administrator) {
        return;
    }
    let visible = principal
        .map(|principal| visible_profile_camera_ids(state, principal, identities))
        .unwrap_or_default();
    if let Some(value) = entry.value.fields.get_mut("cameras") {
        if let Some(Kind::StructValue(cameras)) = &mut value.kind {
            cameras.fields.retain(|id, _| visible.contains(id));
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
    let count = degraded.map(|ids| match principal {
        Some(principal) if principal.role == AccessRole::Administrator => ids.len(),
        Some(principal) => ids
            .intersection(&visible_profile_camera_ids(state, principal, identities))
            .count(),
        None => 0,
    });
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
