use super::*;

pub(in crate::server) fn persist(
    state: &ServerState,
    jobs: &HashMap<String, ExportJobRecord>,
) -> anyhow::Result<()> {
    if let Some(binding) = &state.storage_config.metadata {
        return state
            .storage_config
            .metadata_root()?
            .replace_history(&binding.history_file, &export_history_bytes(jobs)?);
    }
    if let Some(path) = &state.export_history_path {
        persist_export_jobs(path, jobs)?;
    }
    Ok(())
}

pub(in crate::server) fn recover(
    record: &mut ExportJobRecord,
    root: &Path,
    catalog: Option<&RecordingCatalogHandle>,
    now: i64,
) -> anyhow::Result<()> {
    let named = owned(catalog, &record.artifact_id)?;
    match proto::ExportJobStatus::try_from(record.job.status)? {
        proto::ExportJobStatus::Running => {
            if named {
                retire(catalog, &record.artifact_id)?;
            }
            fail(record, now, "Server restarted before the export completed");
        }
        proto::ExportJobStatus::Ready => {
            let name = record
                .job
                .file_name
                .as_deref()
                .filter(|name| safe_export_path_component(name))
                .ok_or_else(|| anyhow::anyhow!("ready export has an invalid file name"))?;
            if named {
                // The catalog remains authoritative while the volume is unavailable.
                record.path = None;
            } else {
                let path = root
                    .join(&record.job.job_id)
                    .join(&record.artifact_id)
                    .join(name);
                if path.is_file() {
                    record.path = Some(path);
                } else {
                    fail(record, now, "Export artifact is missing; retry the export");
                }
            }
        }
        proto::ExportJobStatus::Unspecified => {
            anyhow::bail!("persisted export job status is invalid")
        }
        _ => {
            if named {
                retire(catalog, &record.artifact_id)?;
            }
        }
    }
    if record.job.status != proto::ExportJobStatus::Ready as i32 {
        if !named {
            cleanup_export_attempt_directory(root, &record.job.job_id, &record.artifact_id)?;
        }
    }
    Ok(())
}

fn fail(record: &mut ExportJobRecord, now: i64, error: &str) {
    record.job.status = proto::ExportJobStatus::Failed as i32;
    record.job.error = Some(error.into());
    record.job.retryable = true;
    record.updated_at_ms = now;
    record.completed_at_ms = Some(now);
    record.path = None;
}

pub(in crate::server) fn restore(state: &mut ServerState) {
    let Some(path) = &state.export_history_path else {
        return;
    };
    match load(state, path).and_then(|jobs| {
        persist(state, &jobs)?;
        Ok(jobs)
    }) {
        Ok(jobs) => {
            state.export_jobs = Arc::new(Mutex::new(jobs));
            state.export_history_error = None;
        }
        Err(error) => {
            tracing::error!(%error, "export history recovery failed; history preserved and export creation disabled");
            state.export_history_error = Some(Arc::from(error.to_string()));
        }
    }
}

fn load(state: &ServerState, path: &Path) -> anyhow::Result<HashMap<String, ExportJobRecord>> {
    let export_root = state.storage_config.long_term_path.join(".exports");
    if let Some(binding) = &state.storage_config.metadata {
        let bytes = state
            .storage_config
            .metadata_root()?
            .read_history(&binding.history_file)?;
        return restore_export_history(&bytes, &export_root, state.catalog.as_ref());
    }
    load_export_jobs(path, &export_root, state.catalog.as_ref())
}

pub(in crate::server) fn expire(state: &ServerState) {
    if state.export_history_error.is_some() {
        return;
    }
    let now = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut changed = false;
    for record in jobs.values_mut() {
        if record.job.status != proto::ExportJobStatus::Ready as i32 {
            continue;
        }
        let named = owned(state.catalog.as_ref(), &record.artifact_id).unwrap_or(true);
        let missing = !named && record.path.as_ref().is_none_or(|path| !path.is_file());
        let expired = record
            .job
            .expires_at
            .as_ref()
            .and_then(timestamp_ms)
            .is_some_and(|expires| expires <= now);
        if !missing && !expired {
            continue;
        }
        if let Err(error) =
            cleanup_export_attempt_artifacts(state, &record.job.job_id, &record.artifact_id)
        {
            tracing::warn!(%error, "export expiry deferred until cleanup is admitted");
            continue;
        }
        record.job.status = if missing {
            proto::ExportJobStatus::Failed
        } else {
            proto::ExportJobStatus::Expired
        } as i32;
        record.job.error = missing.then(|| "Export artifact is missing; retry the export".into());
        record.job.retryable = true;
        record.updated_at_ms = now;
        record.completed_at_ms = Some(now);
        record.path = None;
        changed = true;
    }
    changed |= prune(state, &mut jobs, now);
    if changed {
        persist_export_jobs_logged(state, &jobs, "expiry");
    }
}

fn prune(state: &ServerState, jobs: &mut HashMap<String, ExportJobRecord>, now: i64) -> bool {
    let cutoff = now
        .saturating_sub(i64::try_from(EXPORT_METADATA_RETENTION.as_millis()).unwrap_or(i64::MAX));
    let mut terminal = jobs
        .iter()
        .filter(|(_, record)| record.job.status != proto::ExportJobStatus::Running as i32)
        .map(|(id, record)| (id.clone(), record.updated_at_ms))
        .collect::<Vec<_>>();
    terminal.sort_unstable_by_key(|(_, updated)| *updated);
    let mut changed = false;
    for (id, updated) in terminal {
        if updated >= cutoff && jobs.len() <= MAX_EXPORT_HISTORY_JOBS {
            continue;
        }
        let record = &jobs[&id];
        if let Err(error) = cleanup_export_attempt_artifacts(state, &id, &record.artifact_id) {
            tracing::warn!(%error, "export history retained until cleanup is admitted");
            continue;
        }
        jobs.remove(&id);
        changed = true;
    }
    changed
}

/// Reads the persisted source without running recovery or changing export ownership.
pub(in crate::server) fn snapshot(state: &ServerState) -> anyhow::Result<Vec<u8>> {
    use std::io::Read as _;
    anyhow::ensure!(
        state.export_history_error.is_none(),
        "export history is unavailable"
    );
    let _jobs = state
        .export_jobs
        .lock()
        .map_err(|_| anyhow::anyhow!("export history is unavailable"))?;
    let bytes = if let Some(binding) = &state.storage_config.metadata {
        state
            .storage_config
            .metadata_root()?
            .read_history(&binding.history_file)?
    } else {
        let path = state
            .export_history_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("export history path is unavailable"))?;
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_EXPORT_HISTORY_BYTES + 1)
            .read_to_end(&mut bytes)?;
        bytes
    };
    validate_export_history_snapshot(&bytes)?;
    Ok(bytes)
}
