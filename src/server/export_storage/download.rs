use super::*;

pub(in crate::server) fn download(
    state: &ServerState,
    requester: &str,
    request: proto::DownloadExport,
) -> Result<(proto::ExportDownloadResult, Vec<OutboundDataMessage>), ControlCommandError> {
    let (target, channel) = data_channel_target(request.channel)?;
    if target != DataChannelTarget::Reliable {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "export downloads require reliable-data",
        ));
    }
    let record = snapshot(state, requester, &request.job_id)?;
    privacy(state, &record.job.source_id)?;
    let payload = payload(state, &record)?;
    if record.job.sha256.as_deref() != Some(&encode_lower_hex(Sha256::digest(&payload))) {
        checksum_failure(state, &record);
        return Err(unavailable(
            "export checksum verification failed; retry the export",
        ));
    }
    privacy(state, &record.job.source_id)?;
    let chunk_count =
        u32::try_from(payload.len().div_ceil(DATA_MESSAGE_CHUNK_BYTES)).map_err(unavailable)?;
    let messages = payload
        .chunks(DATA_MESSAGE_CHUNK_BYTES)
        .enumerate()
        .map(|(index, chunk)| OutboundDataMessage {
            target,
            group: format!("export:{}", request.job_id),
            message: proto::Message {
                message: Some(proto::message::Message::Export(proto::ExportMessage {
                    message: Some(proto::export_message::Message::FileChunk(
                        proto::ExportFileChunk {
                            job_id: request.job_id.clone(),
                            chunk_index: index as u32,
                            chunk_count,
                            payload: chunk.to_vec(),
                        },
                    )),
                })),
            },
        })
        .collect();
    downloaded(state, &record)?;
    Ok((
        proto::ExportDownloadResult {
            job: Some(record.job),
            channel: channel as i32,
            chunk_count,
        },
        messages,
    ))
}

fn snapshot(
    state: &ServerState,
    requester: &str,
    id: &str,
) -> Result<ExportJobRecord, ControlCommandError> {
    let jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let record = jobs
        .get(id)
        .filter(|record| record.requester_id == requester)
        .ok_or_else(|| {
            ControlCommandError::new(proto::ErrorCode::NotFound, 404, "export job was not found")
        })?;
    if record.job.status != proto::ExportJobStatus::Ready as i32 {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "export file is not ready",
        ));
    }
    if record.job.sha256.is_none() {
        return Err(unavailable("ready export has no checksum"));
    }
    Ok(record.clone())
}

fn payload(state: &ServerState, record: &ExportJobRecord) -> Result<Vec<u8>, ControlCommandError> {
    if owned(state.catalog.as_ref(), &record.artifact_id).map_err(unavailable)? {
        let catalog = state
            .catalog
            .as_ref()
            .ok_or_else(|| unavailable("export catalog unavailable"))?;
        let (location, _lease) = catalog
            .leased_export(&record.artifact_id)
            .map_err(unavailable)?
            .ok_or_else(|| unavailable("export location unavailable"))?;
        let manager = state
            .storage_config
            .volume_runtime
            .as_ref()
            .ok_or_else(|| unavailable("export volume unavailable"))?;
        let mut file = manager.open_owned(&location).map_err(unavailable)?;
        read(&mut file, location.bytes)
    } else {
        let path = record
            .path
            .as_ref()
            .ok_or_else(|| unavailable("ready export has no file"))?;
        let _lease = state
            .catalog
            .as_ref()
            .map(|catalog| catalog.lease_legacy_export(&record.artifact_id, path))
            .transpose()
            .map_err(unavailable)?;
        let mut file = File::open(path).map_err(unavailable)?;
        let length = file.metadata().map_err(unavailable)?.len();
        read(&mut file, length)
    }
}

fn read(file: &mut impl std::io::Read, length: u64) -> Result<Vec<u8>, ControlCommandError> {
    if length > MAX_EXPORT_DOWNLOAD_BYTES {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            413,
            "export file exceeds the browser download limit",
        ));
    }
    let mut bytes = vec![0; usize::try_from(length).map_err(unavailable)?];
    file.read_exact(&mut bytes).map_err(unavailable)?;
    Ok(bytes)
}

fn privacy(state: &ServerState, source: &str) -> Result<(), ControlCommandError> {
    if privacy_active_for_media(state, source)? {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "camera privacy is active",
        ));
    }
    Ok(())
}

fn unavailable(error: impl std::fmt::Display) -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        503,
        format!("export file is unavailable: {error}"),
    )
}

pub(in crate::server) fn checksum_failure(state: &ServerState, snapshot: &ExportJobRecord) {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(record) = jobs.get_mut(&snapshot.job.job_id)
        && record.artifact_id == snapshot.artifact_id
    {
        let now = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        record.job.status = proto::ExportJobStatus::Failed as i32;
        record.job.error = Some("Export checksum verification failed; retry the export".into());
        record.job.retryable = true;
        record.updated_at_ms = now;
        record.completed_at_ms = Some(now);
        record.path = None;
        persist_export_jobs_logged(state, &jobs, "checksum failure");
    }
    drop(jobs);
    let _ = cleanup_export_attempt_artifacts(state, &snapshot.job.job_id, &snapshot.artifact_id);
}

fn downloaded(state: &ServerState, snapshot: &ExportJobRecord) -> Result<(), ControlCommandError> {
    let mut jobs = state
        .export_jobs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(record) = jobs.get_mut(&snapshot.job.job_id)
        && record.artifact_id == snapshot.artifact_id
    {
        let now = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        record.downloaded_at_ms = Some(now);
        record.updated_at_ms = now;
    }
    history::persist(state, &jobs).map_err(unavailable)?;
    Ok(())
}
