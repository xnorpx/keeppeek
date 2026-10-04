use crate::storage::volumes::{VolumeConfiguration, VolumeId, VolumeRole, VolumeState};
use crate::{
    api::proto,
    server::{ControlCommandError, ServerState},
    storage::{
        StorageConfig,
        catalog::locations::{Reply, Request, legacy::LegacyPaths},
    },
};
use std::path::Path;

/// Configuration coordination also excludes operator undrain until the save finishes.
pub(in crate::server) fn validate_removals(
    state: &ServerState,
    path: &Path,
    draft: Option<&VolumeConfiguration<String>>,
) -> Result<(), ControlCommandError> {
    let Some(draft) = draft else {
        return Ok(());
    };
    let current = crate::config::load_config(path).map_err(|_| unavailable())?;
    let retained = draft
        .volumes
        .iter()
        .map(|volume| crate::config::resolve_secret_references(path, &volume.id))
        .collect::<anyhow::Result<Vec<_>>>()
        .map_err(|_| unavailable())?;
    let removed = current
        .storage
        .named_volumes
        .iter()
        .flat_map(|value| &value.volumes)
        .filter(|volume| !retained.iter().any(|id| id == volume.id.as_str()))
        .collect::<Vec<_>>();
    if removed.is_empty() {
        return Ok(());
    }
    let catalog = state.catalog.as_ref().ok_or_else(unavailable)?;
    let Reply::Usage(usage) = catalog
        .volume_location(Request::Usage)
        .map_err(|_| unavailable())?
    else {
        return Err(unavailable());
    };
    for volume in removed {
        validate_running_archive(state, &volume.id)?;
        let may_recover =
            state
                .storage_config
                .volume_runtime
                .as_ref()
                .is_some_and(|manager| {
                    manager.configuration().volumes.iter().any(|running| {
                        running.id == volume.id && running.state == VolumeState::Enabled
                    })
                });
        if may_recover
            && !usage
                .iter()
                .any(|item| item.volume == volume.id.as_str() && item.operator_draining)
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                409,
                "Stop new writes before removal; disable and restart an unbound offline volume first",
            ));
        }
        // An operator drain survives root recovery, including an old configured drain being cleared.
        catalog.volume_location(Request::EnsureRemovable(volume.id.to_string())).map_err(|_| {
            ControlCommandError::new(proto::ErrorCode::Rejected, 409,
                "Stop new writes and finish moving or removing media and pending cleanup before removal")
        })?;
    }
    Ok(())
}

fn validate_running_archive(
    state: &ServerState,
    volume: &VolumeId,
) -> Result<(), ControlCommandError> {
    let referenced =
        state
            .storage_config
            .volume_runtime
            .as_ref()
            .is_some_and(|manager| {
                manager.configuration().placement.iter().any(|rule| {
                    rule.role == VolumeRole::Archive && rule.candidates.contains(volume)
                })
            });
    if referenced {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "Remove archive policy references and restart before removing this volume",
        ));
    }
    Ok(())
}

/// The caller holds configuration coordination through validation and persistence.
pub(in crate::server) fn validate_captured_paths(
    state: &ServerState,
    next: &StorageConfig,
) -> Result<(), ControlCommandError> {
    let Some(catalog) = &state.catalog else {
        return Ok(());
    };
    let reply = catalog
        .volume_location(Request::LegacyPaths)
        .map_err(|_| unavailable())?;
    let Reply::LegacyPaths(captured) = reply else {
        return Err(unavailable());
    };
    let Some(captured) = captured else {
        return Ok(());
    };
    let next = LegacyPaths::effective(next).map_err(|_| unavailable())?;
    let current = LegacyPaths::effective(&state.storage_config).map_err(|_| unavailable())?;
    // ponytail: ordinary settings cannot perform the separate confirmed catalog handoff.
    if captured.ensure_same_media_roots(&next).is_err() || current.catalog_path != next.catalog_path
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "Captured storage paths require confirmed migration; keep the current paths",
        ));
    }
    Ok(())
}

fn unavailable() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        503,
        "Storage ownership could not be verified; retry before changing settings",
    )
}
