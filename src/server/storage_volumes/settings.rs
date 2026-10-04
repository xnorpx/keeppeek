use crate::{
    api::proto,
    server::{ControlCommandError, ServerState},
    storage::{
        StorageConfig,
        catalog::locations::{Reply, Request, legacy::LegacyPaths},
    },
};

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
