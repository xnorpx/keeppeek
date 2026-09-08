use super::{ApiPrincipal, ControlCommandError, ServerControlHandler, ServerState, error, proto};
use crate::{access::AccessRole, storage::long_term::inspection::Archive, webrtc::SessionId};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) struct Permit(Arc<AtomicBool>);

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) fn admit(state: &ServerState) -> Result<Permit, ControlCommandError> {
    let _configuration = state.config_update.try_lock().map_err(|_| {
        error(
            proto::ErrorCode::Rejected,
            409,
            "storage coordination is busy",
        )
    })?;
    if !cfg!(any(unix, windows)) {
        return Err(error(
            proto::ErrorCode::Unavailable,
            503,
            "recording removal is not qualified on this platform",
        ));
    }
    state
        .maintenance_active
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| {
            error(
                proto::ErrorCode::Rejected,
                409,
                "another recording maintenance job is running",
            )
        })?;
    let permit = Permit(Arc::clone(&state.maintenance_active));
    if !state.stored_media_cursors.lock().unwrap().is_empty() {
        return Err(error(
            proto::ErrorCode::Rejected,
            409,
            "close stored playback before recording maintenance",
        ));
    }
    if !state
        .stored_media_cursor_reservations
        .lock()
        .unwrap()
        .is_empty()
    {
        return Err(error(
            proto::ErrorCode::Rejected,
            409,
            "stored playback is opening",
        ));
    }
    if state
        .export_jobs
        .lock()
        .unwrap()
        .values()
        .any(|record| record.job.status == proto::ExportJobStatus::Running as i32)
    {
        return Err(error(
            proto::ErrorCode::Rejected,
            409,
            "wait for running exports before recording maintenance",
        ));
    }
    Ok(permit)
}

pub(super) fn start(
    handler: &ServerControlHandler,
    session_id: SessionId,
    principal: ApiPrincipal,
    id: String,
    permit: Permit,
) -> Result<(), ControlCommandError> {
    let job = super::event_search_catalog(&handler.state)?
        .recording_deletion_intent(
            &principal.id(),
            super::jobs::Action::Read { id: id.clone() },
        )
        .map_err(super::catalog_error)?;
    let controller = ServerControlHandler::new(handler.state.clone(), handler.router_tx.clone());
    std::thread::Builder::new()
        .name("recording-maintenance".to_owned())
        .spawn(move || {
            let _permit = permit;
            let state = &controller.state;
            let result = run(&controller, session_id, &principal, &job);
            if result.is_err()
                && let Some(catalog) = &state.catalog
                && catalog
                    .reject_recording_deletion(&principal.id(), &id)
                    .is_err()
            {
                tracing::warn!(job_id = id, "unable to persist maintenance worker failure");
            }
            super::audit::record(&controller, session_id, &principal, &job, &result);
            if result.is_err() {
                tracing::warn!(
                    job_id = id,
                    "recording maintenance requires retry or inspection"
                );
            }
        })
        .map_err(|_| {
            error(
                proto::ErrorCode::Unavailable,
                503,
                "recording maintenance worker is unavailable",
            )
        })?;
    Ok(())
}

fn run(
    controller: &ServerControlHandler,
    session_id: SessionId,
    principal: &ApiPrincipal,
    job: &super::jobs::Job,
) -> anyhow::Result<crate::storage::catalog::maintenance::jobs::execution::Report> {
    let state = &controller.state;
    let _configuration = state
        .config_update
        .try_lock()
        .map_err(|_| anyhow::anyhow!("configuration is changing"))?;
    if let Some(config_path) = &state.camera_config_path {
        let restore = crate::backup::active_restore(config_path, super::super::unix_time_ms())?;
        anyhow::ensure!(
            restore.is_none_or(|record| matches!(
                crate::api::backup_proto::RestoreState::try_from(record.state),
                Ok(crate::api::backup_proto::RestoreState::Complete
                    | crate::api::backup_proto::RestoreState::RolledBack)
            )),
            "restore is active"
        );
    }
    let archive = Archive::open(&state.storage_config.long_term_path)?;
    let catalog = state
        .catalog
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("catalog unavailable"))?;
    catalog.execute_recording_deletion_authorized(&principal.id(), &job.id, &archive, |unstaged| {
        let current = controller
            .authorize_api_session(session_id, AccessRole::Administrator, "recording_delete")
            .map_err(|_| anyhow::anyhow!("recording maintenance authorization is unavailable"))?;
        anyhow::ensure!(
            current.id() == principal.id(),
            "recording maintenance principal changed"
        );
        if unstaged {
            let exports = super::related_exports(state, &job.snapshot)
                .map_err(|_| anyhow::anyhow!("export relationships are unavailable"))?;
            anyhow::ensure!(
                exports == job.export_ids,
                "export relationships changed; preview again"
            );
        }
        Ok(())
    })
}
