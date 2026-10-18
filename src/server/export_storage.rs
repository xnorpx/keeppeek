//! Connects export jobs to durable volume ownership.

use super::*;
pub(super) mod archive;
mod audio_context;
#[cfg(test)]
mod audio_timing_tests;
pub(super) mod creation;
pub(super) mod download;
pub(super) mod history;
use crate::storage::{
    catalog::{
        locations::{Kind, Object, Reply, Request, export_cleanup::Action},
        readers::MoveLease,
    },
    playback::ExportArtifact,
    volumes::{VolumeRole, runtime::Reservation},
};

struct Output {
    path: PathBuf,
    reservation: Option<Reservation>,
    _lease: MoveLease,
}

pub(super) fn owned(catalog: Option<&RecordingCatalogHandle>, id: &str) -> anyhow::Result<bool> {
    let Some(catalog) = catalog else {
        return Ok(false);
    };
    let Reply::ExportOwned(owned) =
        catalog.volume_location(Request::ExportCleanup(Action::Owned(id.into())))?
    else {
        anyhow::bail!("invalid export ownership reply");
    };
    Ok(owned)
}

pub(super) fn retire(catalog: Option<&RecordingCatalogHandle>, id: &str) -> anyhow::Result<()> {
    if let Some(catalog) = catalog {
        // Legacy test/history IDs need no tombstone when no volume has ever owned them.
        if uuid::Uuid::parse_str(id).is_ok() || owned(Some(catalog), id)? {
            catalog.volume_location(Request::RetireExport(id.into()))?;
        }
    }
    Ok(())
}

fn prepare(
    state: &ServerState,
    request: &proto::CreateExportJob,
    id: &str,
    file_name: &str,
) -> anyhow::Result<Output> {
    let catalog = state
        .catalog
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("recording catalog is unavailable"))?;
    let lease = catalog.claim_volume_move(id)?;
    let groups = state
        .camera(&request.source_id)
        .map_or_else(Vec::new, |camera| camera.groups);
    let groups = groups.iter().map(String::as_str).collect::<Vec<_>>();
    let reservation = state
        .storage_config
        .volume_runtime
        .as_ref()
        .map(|manager| {
            manager.reserve(
                VolumeRole::Export,
                &request.source_id,
                &groups,
                Object {
                    kind: Kind::Export,
                    id: id.into(),
                },
                1,
            )
        })
        .transpose()?
        .flatten();
    let path = reservation.as_ref().map_or_else(
        || export_attempt_directory(state, &request.job_id, id).join(file_name),
        |reserved| reserved.path().to_path_buf(),
    );
    Ok(Output {
        path,
        reservation,
        _lease: lease,
    })
}

#[derive(Clone)]
struct Attempt {
    state: ServerState,
    job: String,
    artifact: String,
    cancel: Arc<AtomicBool>,
    file_name: String,
    audio_offset_ms: i32,
    recording_stream_id: Option<String>,
}

struct RecordedPolicy {
    offset_ms: i32,
    stream_id: Option<String>,
}

pub(super) fn spawn(
    state: ServerState,
    request: proto::CreateExportJob,
    fragments: Vec<CatalogMediaFragment>,
    cancel: Arc<AtomicBool>,
    artifact_id: String,
    end_ms: i64,
    file_name: String,
) {
    let timing = match captured_policy(&state, &request, &cancel, &artifact_id) {
        Ok(timing) => timing,
        Err(error) => {
            fail_export_job(&state, &request.job_id, &cancel, error.to_string());
            return;
        }
    };
    let output = match prepare(&state, &request, &artifact_id, &file_name) {
        Ok(output) => output,
        Err(error) => {
            fail_export_job(&state, &request.job_id, &cancel, error.to_string());
            let _ = cleanup_export_attempt_artifacts(&state, &request.job_id, &artifact_id);
            return;
        }
    };
    let attempt = Attempt {
        state,
        job: request.job_id,
        artifact: artifact_id,
        cancel,
        file_name,
        audio_offset_ms: timing.offset_ms,
        recording_stream_id: timing.stream_id,
    };
    let monitor = attempt.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(format!("export-monitor-{}", attempt.job))
        .spawn(move || run_monitor(monitor, fragments, end_ms, output))
    {
        fail_export_job(
            &attempt.state,
            &attempt.job,
            &attempt.cancel,
            format!("unable to start export monitor: {error}"),
        );
        let _ = cleanup_export_attempt_artifacts(&attempt.state, &attempt.job, &attempt.artifact);
    }
}

fn captured_policy(
    state: &ServerState,
    request: &proto::CreateExportJob,
    cancel: &Arc<AtomicBool>,
    artifact: &str,
) -> anyhow::Result<RecordedPolicy> {
    let jobs = state
        .export_jobs
        .lock()
        .map_err(|_| anyhow::anyhow!("export history is unavailable"))?;
    let record = jobs
        .get(&request.job_id)
        .ok_or_else(|| anyhow::anyhow!("export attempt is missing"))?;
    anyhow::ensure!(
        record.artifact_id == artifact
            && Arc::ptr_eq(&record.cancel, cancel)
            && !cancel.load(Ordering::Acquire)
            && record.request == *request
            && record.job.job_id == request.job_id
            && record.job.status == proto::ExportJobStatus::Running as i32,
        "export attempt is no longer current"
    );
    let offset_ms = recorded_audio::validate_snapshot(record.job.recorded_audio.as_ref())
        .map_err(|error| anyhow::anyhow!(error.message))?;
    Ok(RecordedPolicy {
        offset_ms,
        stream_id: record.recording_stream_id.clone(),
    })
}

fn run_monitor(
    attempt: Attempt,
    fragments: Vec<CatalogMediaFragment>,
    end_ms: i64,
    output: Output,
) {
    let path = output.path.clone();
    let target = ExportArtifactTarget {
        path: &path,
        file_name: &attempt.file_name,
        artifact_id: &attempt.artifact,
    };
    let (events, receiver) = mpsc::sync_channel(64);
    let worker = attempt.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(format!("export-worker-{}", attempt.job))
        .spawn(move || run_worker(worker, fragments, end_ms, output, events))
    {
        finish_export_worker(
            &attempt.state,
            &attempt.job,
            &attempt.cancel,
            target,
            Err(error.into()),
        );
        return;
    }
    monitor_export_worker(
        &attempt.state,
        &attempt.job,
        &attempt.cancel,
        target,
        receiver,
        ExportDeadlines {
            no_progress: EXPORT_NO_PROGRESS_TIMEOUT,
            total_runtime: EXPORT_TOTAL_RUNTIME_TIMEOUT,
        },
    );
}

fn run_worker(
    attempt: Attempt,
    fragments: Vec<CatalogMediaFragment>,
    end_ms: i64,
    output: Output,
    events: mpsc::SyncSender<ExportWorkerEvent>,
) {
    let result = write(
        &attempt,
        &fragments,
        end_ms,
        output.reservation,
        &output.path,
        &events,
    );
    let cleanup = result.is_err() || attempt.cancel.load(Ordering::Acquire);
    let delivered = events.send(ExportWorkerEvent::Finished(result)).is_ok();
    if cleanup || !delivered {
        let _ = cleanup_export_attempt_artifacts(&attempt.state, &attempt.job, &attempt.artifact);
    }
    drop(output._lease);
}

fn write(
    attempt: &Attempt,
    fragments: &[CatalogMediaFragment],
    end_ms: i64,
    reservation: Option<Reservation>,
    path: &Path,
    events: &mpsc::SyncSender<ExportWorkerEvent>,
) -> anyhow::Result<(ExportArtifact, String)> {
    let state = &attempt.state;
    let cancel = &attempt.cancel;
    let catalog = state
        .catalog
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("recording catalog unavailable"))?;
    let _readers = catalog.lease_media_fragments(fragments)?;
    let audio = audio_context::load(attempt, fragments, end_ms, catalog)?;
    let sources = crate::storage::playback::ExportSources {
        video_fragments: fragments,
        audio_context: &audio.fragments,
        requested_end_ms: end_ms,
    };
    let estimated = export_estimated_bytes(fragments).max(1);
    let cancelled = || {
        let _ = events.try_send(ExportWorkerEvent::Heartbeat);
        cancel.load(Ordering::Acquire)
    };
    let progress = |bytes: u64| {
        let per_mille = 200
            + u32::try_from(bytes.saturating_mul(650) / estimated)
                .unwrap_or(650)
                .min(650);
        let _ = events.try_send(ExportWorkerEvent::Progress { per_mille, bytes });
    };
    anyhow::ensure!(!cancelled(), "export cancelled");
    if let Some(reservation) = reservation {
        publish_owned(
            sources,
            reservation,
            attempt.audio_offset_ms,
            cancelled,
            progress,
        )
    } else {
        let artifact = crate::storage::playback::export_sources_to_file(
            sources,
            path,
            attempt.audio_offset_ms,
            cancelled,
            progress,
        )?;
        let checksum = sha256_file_with_progress(path, cancel, artifact.bytes, |_| {
            let _ = events.try_send(ExportWorkerEvent::Heartbeat);
        })?;
        Ok((artifact, checksum))
    }
}

fn publish_owned(
    sources: crate::storage::playback::ExportSources<'_>,
    reservation: Reservation,
    offset_ms: i32,
    cancelled: impl Fn() -> bool,
    progress: impl Fn(u64),
) -> anyhow::Result<(ExportArtifact, String)> {
    let mut file = reservation.open()?;
    let artifact = crate::storage::playback::export_sources_to_writer(
        sources, &mut file, offset_ms, &cancelled, progress,
    )?;
    let evidence = file.evidence_with_progress(|_| {
        anyhow::ensure!(!cancelled(), "export cancelled");
        Ok(())
    })?;
    anyhow::ensure!(!cancelled(), "export cancelled");
    let checksum = encode_lower_hex(evidence.digest);
    file.publish(evidence)?;
    Ok((artifact, checksum))
}
