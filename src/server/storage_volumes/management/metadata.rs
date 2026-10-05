//! Confirms stopped metadata relocation through the existing configuration and authority fence.

use super::*;
use crate::{
    config::{
        MetadataBinding,
        metadata::pending::{self, control::View},
    },
    storage::{
        StorageConfig,
        catalog::authority::{Authority, MetadataInfo},
        volumes::{Volume, VolumeRole, VolumeState, root::Root},
    },
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::Path,
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

#[derive(Default)]
pub(super) struct Registry {
    plans: Mutex<HashMap<String, Plan>>,
}

#[derive(Clone)]
struct Plan {
    actor: String,
    revision: String,
    expires: Instant,
    source: Authority,
    source_digest: [u8; 32],
    target: MetadataBinding,
}

pub(super) fn preview(
    state: &ServerState,
    actor: &str,
    request: proto::PreviewStorageMetadata,
) -> Result<proto::StorageMetadataPreview> {
    let _guard = lock(state)?;
    let view = View::load(path(state)?).map_err(failure)?;
    ensure_live(state, &view)?;
    if view.pending.is_some()
        || view
            .root
            .contains_key(crate::config::STORAGE_MIGRATION_SECTION)
    {
        return Err(rejected("a storage migration is already pending"));
    }
    let info = information(state)?;
    let bytes = required_bytes(state, &info)?;
    let (volume, root) = destination(state, &view, &request.destination_volume_id, bytes)?;
    let token = uuid::Uuid::new_v4().to_string();
    let revision = revision(&view)?;
    let target = MetadataBinding {
        volume_id: volume.id.clone(),
        catalog_file: format!("catalog-{token}.db"),
        history_file: format!("exports-{token}.json"),
        catalog_id: info.authority.catalog_id.clone(),
        generation: info
            .authority
            .generation
            .checked_add(1)
            .filter(|generation| *generation <= i64::MAX as u64)
            .ok_or_else(|| rejected("metadata generation is exhausted"))?,
        filesystem: root.identity().filesystem.clone(),
        root_identity: root.identity().directory.clone(),
    };
    store(
        state,
        &token,
        Plan {
            actor: actor.to_owned(),
            revision: revision.clone(),
            expires: Instant::now() + Duration::from_secs(300),
            source: info.authority,
            source_digest: pending::fingerprint(&view.config.storage).map_err(failure)?,
            target,
        },
    )?;
    Ok(proto::StorageMetadataPreview {
        preview_token: token,
        configuration_revision: revision,
        destination_volume_id: volume.id.to_string(),
        required_bytes: bytes,
        expires_in_seconds: 300,
        restart_required: true,
        requires_downtime: true,
    })
}

pub(super) fn confirm(
    state: &ServerState,
    actor: &str,
    request: proto::ConfirmStorageMetadata,
) -> Result<proto::StorageMetadataStatus> {
    let _guard = lock(state)?;
    let plan = load(state, actor, &request.preview_token)?;
    if request.expected_configuration_revision != plan.revision {
        return Err(rejected("metadata configuration changed; preview again"));
    }
    let view = View::load(path(state)?).map_err(failure)?;
    ensure_live(state, &view)?;
    let info = information(state)?;
    if info.authority != plan.source {
        return Err(rejected("metadata authority changed; preview again"));
    }
    if let Some(pending) = &view.pending {
        if pending.target == plan.target && pending.source_digest == plan.source_digest {
            return status_from(state, &view);
        }
        return Err(rejected("another metadata handoff is pending"));
    }
    if revision(&view)? != plan.revision {
        return Err(rejected("metadata configuration changed; preview again"));
    }
    let bytes = required_bytes(state, &info)?;
    let (_, root) = destination(state, &view, plan.target.volume_id.as_str(), bytes)?;
    if *root.identity() != plan.target.root_identity() {
        return Err(rejected("metadata destination changed; preview again"));
    }
    root.revalidate().map_err(failure)?;
    view.stage(path(state)?, &plan.target).map_err(failure)?;
    status(state)
}

pub(super) fn status(state: &ServerState) -> Result<proto::StorageMetadataStatus> {
    let view = View::load(path(state)?).map_err(failure)?;
    status_from(state, &view)
}

fn status_from(state: &ServerState, view: &View) -> Result<proto::StorageMetadataStatus> {
    Ok(proto::StorageMetadataStatus {
        configuration_revision: revision(view)?,
        current_volume_id: state
            .storage_config
            .metadata
            .as_ref()
            .map(|owner| owner.volume_id.to_string()),
        pending_volume_id: view
            .pending
            .as_ref()
            .map(|pending| pending.target.volume_id.to_string()),
        restart_required: view.pending.is_some(),
    })
}

pub(super) fn cancel(
    state: &ServerState,
    request: proto::CancelStorageMetadata,
) -> Result<proto::StorageMetadataStatus> {
    let _guard = lock(state)?;
    let view = View::load(path(state)?).map_err(failure)?;
    if request.expected_configuration_revision != revision(&view)? {
        return Err(rejected(
            "metadata configuration changed; refresh and retry",
        ));
    }
    if let Some(pending) = &view.pending {
        ensure_live(state, &view)?;
        let source = information(state)?.authority;
        if pending.target.catalog_id != source.catalog_id
            || source.generation.checked_add(1) != Some(pending.target.generation)
        {
            return Err(rejected(
                "metadata handoff cannot be cancelled by this authority",
            ));
        }
        view.cancel(path(state)?).map_err(failure)?;
        // ponytail: cancellation invalidates the bounded preview cache; new plans are cheap.
        state
            .volume_previews
            .metadata
            .plans
            .lock()
            .map_err(|_| rejected("metadata previews are unavailable"))?
            .clear();
    }
    status(state)
}

fn revision(view: &View) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(b"keeppeek.metadata.v1\0");
    digest.update(crate::server::configuration_revision(&view.config).as_bytes());
    let marker = view
        .root
        .get("storage")
        .and_then(|storage| storage.get(pending::PENDING));
    digest.update(serde_json::to_vec(&marker).map_err(|cause| failure(cause.into()))?);
    Ok(crate::server::encode_lower_hex(digest.finalize()))
}

fn information(state: &ServerState) -> Result<MetadataInfo> {
    crate::server::event_search_catalog(state)?
        .metadata_info()
        .map_err(failure)
}

fn required_bytes(state: &ServerState, info: &MetadataInfo) -> Result<u64> {
    let history = crate::server::export_storage::history::snapshot(state).map_err(failure)?;
    info.snapshot_bytes
        .checked_add(u64::try_from(history.len()).map_err(|cause| failure(cause.into()))?)
        .ok_or_else(|| rejected("metadata size exceeds its limit"))
}

fn destination<'a>(
    state: &ServerState,
    view: &'a View,
    id: &str,
    bytes: u64,
) -> Result<(&'a Volume, Root)> {
    let volume = view
        .config
        .storage
        .named_volumes
        .as_ref()
        .and_then(|configuration| {
            configuration
                .volumes
                .iter()
                .find(|volume| volume.id.as_str() == id)
        })
        .ok_or_else(|| rejected("metadata volume is not configured"))?;
    if volume.state != VolumeState::Disabled
        || volume.roles != [VolumeRole::Metadata]
        || !volume.sources.is_empty()
        || !volume.groups.is_empty()
        || view
            .config
            .storage
            .metadata
            .as_ref()
            .is_some_and(|owner| owner.volume_id == volume.id)
    {
        return Err(rejected(
            "choose a disabled metadata-only volume without camera restrictions",
        ));
    }
    let root = Root::open(&volume.root).map_err(failure)?;
    root.sync().map_err(failure)?;
    catalog(
        state,
        Request::CheckBinding(locations::Binding::metadata(volume, &root)),
    )?;
    let available = root.capacity(0).map_err(failure)?.available_bytes;
    let reserve = volume.minimum_free_bytes.max(volume.critical_free_bytes);
    if volume.capacity_bytes.is_some_and(|cap| bytes > cap)
        || available.saturating_sub(reserve) < bytes
    {
        return Err(rejected("metadata destination has insufficient capacity"));
    }
    Ok((volume, root))
}

fn ensure_live(state: &ServerState, view: &View) -> Result<()> {
    let configured = StorageConfig::from_toml(&view.config.storage);
    let history = crate::server::export_history_path(&configured);
    if configured.recording_catalog_path != state.storage_config.recording_catalog_path
        || configured.metadata != state.storage_config.metadata
        || state.export_history_path.as_deref() != Some(&history)
    {
        return Err(rejected(
            "storage paths have pending changes; restart before metadata relocation",
        ));
    }
    Ok(())
}

fn store(state: &ServerState, token: &str, plan: Plan) -> Result<()> {
    let mut plans = state
        .volume_previews
        .metadata
        .plans
        .lock()
        .map_err(|_| rejected("metadata previews are unavailable"))?;
    plans.retain(|_, plan| plan.expires > Instant::now());
    if plans.len() >= 64 {
        return Err(error(
            proto::ErrorCode::Rejected,
            429,
            "too many pending metadata previews",
        ));
    }
    plans.insert(token.to_owned(), plan);
    Ok(())
}

fn load(state: &ServerState, actor: &str, token: &str) -> Result<Plan> {
    state
        .volume_previews
        .metadata
        .plans
        .lock()
        .map_err(|_| rejected("metadata previews are unavailable"))?
        .get(token)
        .filter(|plan| plan.actor == actor && plan.expires > Instant::now())
        .cloned()
        .ok_or_else(|| rejected("metadata preview expired or belongs to another administrator"))
}

fn lock(state: &ServerState) -> Result<MutexGuard<'_, ()>> {
    state
        .config_update
        .try_lock()
        .map_err(|_| rejected("configuration is changing; retry"))
}

fn path(state: &ServerState) -> Result<&Path> {
    state
        .camera_config_path
        .as_deref()
        .ok_or_else(|| rejected("configuration is unavailable"))
}

fn rejected(message: &str) -> ControlCommandError {
    error(proto::ErrorCode::Rejected, 409, message)
}
