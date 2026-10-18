//! Authorizes bounded local archive controls and preserves private configuration references.

use super::{ApiPrincipal, ControlCommandError, ServerState};
use crate::{
    api::proto,
    config::Config,
    storage::archive::{Handle, settings::Settings, target::ObjectId},
};
use prost::Message;
use std::path::Path;

type Result<T> = std::result::Result<T, ControlCommandError>;

#[cfg(test)]
mod tests;

pub(super) fn from_wire(path: &Path, value: proto::ArchiveSettings) -> anyhow::Result<Settings> {
    anyhow::ensure!(
        value.source_ids.len() <= 127,
        "archive source selector limit exceeded"
    );
    let settings = Settings {
        target_id: value.target_id,
        root: value.root.into(),
        enabled: value.enabled,
        automatic: value.automatic,
        source_ids: value.source_ids,
        minimum_age_secs: value.minimum_age_secs,
        evict_after_archive: value.evict_after_archive,
        maximum_object_bytes: value.maximum_object_bytes,
        maximum_pending_count: value.maximum_pending_count,
        maximum_pending_bytes: value.maximum_pending_bytes,
        operation_timeout_secs: value.operation_timeout_secs,
        maximum_attempts: value.maximum_attempts,
    };
    crate::config::validate_archive_configuration(path, &settings)
        .map_err(|_| anyhow::anyhow!("archive settings are invalid or overlap existing storage"))?;
    Ok(settings)
}

pub(super) fn to_wire(value: Settings) -> proto::ArchiveSettings {
    proto::ArchiveSettings {
        target_id: value.target_id,
        root: value.root.to_string_lossy().into_owned(),
        enabled: value.enabled,
        automatic: value.automatic,
        source_ids: value.source_ids,
        minimum_age_secs: value.minimum_age_secs,
        evict_after_archive: value.evict_after_archive,
        maximum_object_bytes: value.maximum_object_bytes,
        maximum_pending_count: value.maximum_pending_count,
        maximum_pending_bytes: value.maximum_pending_bytes,
        operation_timeout_secs: value.operation_timeout_secs,
        maximum_attempts: value.maximum_attempts,
    }
}

pub(super) fn sanitized(config: &Config) -> Option<Settings> {
    let resolved = config.storage.archive.as_ref()?;
    match config
        .source
        .get("storage")
        .and_then(|value| value.get("archive"))
    {
        Some(raw) => Some(
            raw.clone()
                .try_into()
                .expect("validated raw archive settings retain their schema"),
        ),
        None => Some(resolved.clone()),
    }
}

pub(super) fn dispatch(
    state: &ServerState,
    principal: &ApiPrincipal,
    command: proto::ArchiveCommand,
) -> Result<proto::ok::Result> {
    authorize(principal)?;
    let handle = state.storage_config.archive_runtime.as_ref();
    use proto::archive_command::Action;
    use proto::archive_result::Result as Wire;
    let result = match command.action {
        Some(Action::Probe(_)) => {
            let handle = handle.ok_or_else(unavailable)?;
            handle.request_probe().map_err(operation_failed)?;
            Wire::Status(status(Some(handle)))
        }
        Some(Action::List(request)) => Wire::Jobs(list(state, handle, request)?),
        Some(Action::Get(request)) => {
            require_job_id(&request.job_id)?;
            Wire::Job(wire_job(
                target_names(state)?.as_ref(),
                handle
                    .ok_or_else(unavailable)?
                    .get(&request.job_id)
                    .map_err(operation_failed)?,
            ))
        }
        Some(Action::Copy(request)) => {
            Wire::Job(copy(state, handle.ok_or_else(unavailable)?, request)?)
        }
        Some(Action::Cancel(request)) => {
            Wire::Job(change(state, handle, &request.job_id, "cancel")?)
        }
        Some(Action::Retry(request)) => Wire::Job(change(state, handle, &request.job_id, "retry")?),
        Some(Action::Restore(request)) => {
            Wire::Job(change(state, handle, &request.job_id, "restore")?)
        }
        Some(Action::Evict(request)) => Wire::Job(change(state, handle, &request.job_id, "evict")?),
        None => return Err(invalid("archive action is required")),
    };
    let result = proto::ArchiveResult {
        result: Some(result),
    };
    if result.encoded_len() > 48 * 1024 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            413,
            "archive response exceeds its bounded page; request fewer jobs",
        ));
    }
    Ok(proto::ok::Result::ArchiveResult(result))
}

fn authorize(principal: &ApiPrincipal) -> Result<()> {
    if principal.role != crate::access::AccessRole::Administrator {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "Administrator access is required",
        ));
    }
    Ok(())
}

fn status(handle: Option<&Handle>) -> proto::ArchiveStatus {
    let Some(handle) = handle else {
        return proto::ArchiveStatus {
            error_code: "archive_runtime_unavailable".into(),
            ..Default::default()
        };
    };
    let health = handle.health();
    proto::ArchiveStatus {
        runtime_available: true,
        online: health.available,
        error_code: if health.available || health.probe_pending {
            String::new()
        } else {
            "archive_target_unavailable".into()
        },
        probe_pending: health.probe_pending,
    }
}

fn list(
    state: &ServerState,
    handle: Option<&Handle>,
    request: proto::ListArchiveJobs,
) -> Result<proto::ArchiveJobList> {
    let limit = if request.limit == 0 {
        16
    } else {
        request.limit
    };
    if limit > 64 {
        return Err(invalid("archive job page limit must be between 1 and 64"));
    }
    if !request.after_job_id.is_empty() {
        require_job_id(&request.after_job_id)?;
    }
    let Some(handle) = handle else {
        return Ok(proto::ArchiveJobList {
            status: Some(status(None)),
            ..Default::default()
        });
    };
    let jobs = handle
        .list(
            &request.after_job_id,
            u16::try_from(limit).expect("validated archive page limit fits u16"),
        )
        .map_err(operation_failed)?;
    let next_after_job_id =
        if jobs.len() == usize::try_from(limit).expect("validated archive page limit fits usize") {
            jobs.last()
                .expect("full archive page is nonempty")
                .id
                .clone()
        } else {
            String::new()
        };
    let names = target_names(state)?;
    Ok(proto::ArchiveJobList {
        jobs: jobs
            .into_iter()
            .filter(|job| matches!(job.kind.as_str(), "recording" | "export"))
            .map(|job| wire_job(names.as_ref(), job))
            .collect(),
        next_after_job_id,
        status: Some(status(Some(handle))),
    })
}

pub(super) fn copy(
    state: &ServerState,
    handle: &Handle,
    request: proto::CopyArchiveObject,
) -> Result<proto::ArchiveJob> {
    require_job_id(&request.job_id)?;
    if request.object_id.is_empty()
        || request.object_id.len() > 256
        || request.object_id.chars().any(char::is_control)
    {
        return Err(invalid(
            "archive object ID must contain 1 to 256 bytes without control characters",
        ));
    }
    let kind = match proto::StorageObjectKind::try_from(request.kind) {
        Ok(proto::StorageObjectKind::Recording) => "recording",
        Ok(proto::StorageObjectKind::Export) => "export",
        _ => {
            return Err(invalid(
                "archive copy requires a finalized recording or completed export",
            ));
        }
    };
    use crate::storage::catalog::archive::{Reply, Request};
    let catalog = state.catalog.as_ref().ok_or_else(unavailable)?;
    let existing = match catalog
        .archive(Request::Get(request.job_id.clone()))
        .map_err(operation_failed)?
    {
        Reply::Job(existing) => existing,
        _ => {
            return Err(operation_failed(anyhow::anyhow!(
                "invalid archive job lookup reply"
            )));
        }
    };
    if let Some(job) = existing {
        if job.kind != kind
            || job.recording_id != request.object_id
            || job.binding.id != handle.target_id()
        {
            return Err(operation_failed(anyhow::anyhow!(
                "archive job ID belongs to another copy"
            )));
        }
        return Ok(wire_job(target_names(state)?.as_ref(), job));
    }
    let job = if kind == "export" {
        let metadata =
            super::export_storage::archive::metadata_for_copy(state, &request.object_id)?;
        handle.enqueue_export(
            &request.object_id,
            &request.job_id,
            &metadata.original_job_id,
            &metadata.json,
        )
    } else {
        handle.enqueue(&request.object_id, kind, &request.job_id)
    };
    Ok(wire_job(
        target_names(state)?.as_ref(),
        job.map_err(operation_failed)?,
    ))
}

fn change(
    state: &ServerState,
    handle: Option<&Handle>,
    id: &str,
    action: &str,
) -> Result<proto::ArchiveJob> {
    require_job_id(id)?;
    let handle = handle.ok_or_else(unavailable)?;
    let result = match action {
        "cancel" => handle.cancel(id),
        "retry" => handle.retry(id),
        "restore" => handle.restore(id),
        "evict" => handle.evict(id),
        _ => unreachable!("archive action is an internal fixed label"),
    };
    Ok(wire_job(
        target_names(state)?.as_ref(),
        result.map_err(operation_failed)?,
    ))
}

fn target_names(state: &ServerState) -> Result<Option<(String, String)>> {
    if let Some(path) = &state.camera_config_path {
        let config = crate::config::load_config(path).map_err(operation_failed)?;
        Ok(config
            .storage
            .archive
            .as_ref()
            .zip(sanitized(&config))
            .map(|(resolved, raw)| (resolved.target_id.clone(), raw.target_id)))
    } else {
        Ok(None)
    }
}

fn wire_job(
    names: Option<&(String, String)>,
    job: crate::storage::catalog::archive::Job,
) -> proto::ArchiveJob {
    let target_id = names
        .filter(|(private, _)| *private == job.binding.id)
        .map_or_else(|| "unavailable-target".into(), |(_, public)| public.clone());
    proto::ArchiveJob {
        job_id: job.id,
        object_id: if job.kind == "probe" {
            String::new()
        } else {
            job.recording_id
        },
        kind: match job.kind.as_str() {
            "recording" => proto::StorageObjectKind::Recording as i32,
            "export" => proto::StorageObjectKind::Export as i32,
            _ => proto::StorageObjectKind::Unspecified as i32,
        },
        target_id,
        state: match job.state.as_str() {
            "queued" | "copying" | "ready" | "failed" | "cancelled" | "deleted" => job.state,
            _ => "unavailable".into(),
        },
        bytes: job.bytes,
        attempts: job.attempts,
        error_code: if job.error.is_some() {
            "archive_operation_failed".into()
        } else {
            String::new()
        },
        primary_copy: match job.local_evicted {
            0 => proto::ArchivePrimaryCopyState::Present as i32,
            1 => proto::ArchivePrimaryCopyState::Evicted as i32,
            2 => proto::ArchivePrimaryCopyState::Evicting as i32,
            _ => proto::ArchivePrimaryCopyState::Unspecified as i32,
        },
        action: match job.action.as_str() {
            "restore" | "evict" => job.action,
            _ => String::new(),
        },
        action_attempts: job.action_attempts,
    }
}

fn require_job_id(id: &str) -> Result<()> {
    ObjectId::parse(id).map_err(|_| invalid("archive job ID must be a canonical UUID"))?;
    Ok(())
}

fn invalid(message: &'static str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

fn unavailable() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        503,
        "archive runtime is unavailable; save configuration and restart to apply it",
    )
}

fn operation_failed(_: anyhow::Error) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        409,
        "archive request was not accepted; refresh job state before retrying",
    )
}
