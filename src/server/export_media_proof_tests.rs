use super::export_provenance_tests::{current_record, restored_record};
use super::export_storage_tests::{completed, setup};
use super::*;

fn prepare_history(state: &mut ServerState, root: &Path) -> PathBuf {
    let path = root.join("history.json");
    state.export_history_path = Some(Arc::new(path.clone()));
    path
}

fn download_still_works(state: &ServerState) {
    let (_, chunks) = download_export(
        state,
        "owner",
        proto::DownloadExport {
            job_id: "named-export".into(),
            channel: proto::DataChannelKind::ReliableData as i32,
        },
    )
    .unwrap();
    assert!(!chunks.is_empty());
}

#[test]
fn export_media_proof_survives_history_and_binds_exact_artifact() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    let history_path = prepare_history(&mut state, &root);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let record = current_record(&state);
    let bounds = record.verified_media_bounds().unwrap();
    assert!(bounds.start_ms < bounds.end_ms);
    let history = std::fs::read(history_path).unwrap();
    let persisted: serde_json::Value = serde_json::from_slice(&history).unwrap();
    assert_eq!(
        persisted["jobs"][0]["media_proof"]["artifact_id"],
        record.artifact_id
    );
    assert_eq!(
        persisted["jobs"][0]["media_proof"]["bounds"]["end_ms"].as_i64(),
        Some(bounds.end_ms)
    );
    let restored = restore_export_history(
        &history,
        &state.storage_config.long_term_path.join(".exports"),
        Some(&catalog.handle()),
    )
    .unwrap();
    assert_eq!(
        restored["named-export"].verified_media_bounds(),
        Some(bounds)
    );
    for mutation in 0..4 {
        let mut changed = record.clone();
        match mutation {
            0 => changed.artifact_id = Uuid::new_v4().simple().to_string(),
            1 => changed.job.sha256 = Some("0".repeat(64)),
            2 => changed.job.bytes_written += 1,
            _ => changed.media_proof.as_mut().unwrap().version += 1,
        }
        assert!(
            changed.verified_media_bounds().is_none(),
            "mutation {mutation}"
        );
    }
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn legacy_export_media_proof_stays_absent_and_downloadable() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    prepare_history(&mut state, &root);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    state
        .export_jobs
        .lock()
        .unwrap()
        .get_mut("named-export")
        .unwrap()
        .media_proof = None;
    let recovered = restored_record(&state, &catalog.handle());
    assert!(recovered.verified_media_bounds().is_none());
    state
        .export_jobs
        .lock()
        .unwrap()
        .insert("named-export".into(), recovered);
    download_still_works(&state);
    assert!(current_record(&state).media_proof.is_none());
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn export_media_proof_retry_never_reuses_prior_generation() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    prepare_history(&mut state, &root);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let old = current_record(&state);
    assert!(old.verified_media_bounds().is_some());
    export_storage::download::checksum_failure(&state, &old);
    retry_export_job(&state, "owner", "named-export").unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let mut next = current_record(&state);
    assert_ne!(old.artifact_id, next.artifact_id);
    assert!(next.verified_media_bounds().is_some());
    next.media_proof = old.media_proof;
    assert!(next.verified_media_bounds().is_none());
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn export_media_proof_failed_history_write_preserves_ordinary_download() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    prepare_history(&mut state, &root);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let old = current_record(&state);
    let proof = old.media_proof;
    assert!(proof.is_some());
    let blocked = root.join("not-a-directory");
    std::fs::write(&blocked, b"block history parent").unwrap();
    state.export_history_path = Some(Arc::new(blocked.join("history.json")));
    let mut jobs = state.export_jobs.lock().unwrap();
    persist_export_completion(&state, &mut jobs, "named-export", proof);
    assert!(jobs["named-export"].media_proof.is_none());
    assert_eq!(
        jobs["named-export"].job.status,
        proto::ExportJobStatus::Ready as i32
    );
    drop(jobs);
    state.export_history_path = None;
    download_still_works(&state);
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn export_media_proof_enforces_actual_envelope_ceiling() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    prepare_history(&mut state, &root);
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let mut record = current_record(&state);
    let proof = record.media_proof.as_mut().unwrap();
    proof.bounds.end_ms = proof.bounds.start_ms + 120_001;
    assert!(record.verified_media_bounds().is_none());
    record.media_proof.as_mut().unwrap().bounds.end_ms -= 1;
    assert!(record.verified_media_bounds().is_some());
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn export_media_proof_requires_persistent_history_without_blocking_normal_export() {
    let (_root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    state.storage_config.metadata = None;
    state.export_history_path = None;
    create_export_job(&state, "owner", request).unwrap();
    assert_eq!(
        completed(&state).status,
        proto::ExportJobStatus::Ready as i32
    );
    let record = current_record(&state);
    assert!(record.media_proof.is_none());
    assert!(record.verified_media_bounds().is_none());
    download_still_works(&state);
    assert!(current_record(&state).media_proof.is_none());
    drop(state);
    drop(manager);
    catalog.shutdown();
}
