use super::*;

fn worker() -> anyhow::Result<(PathBuf, RecordingCatalog, WriterWorker)> {
    let (root, catalog, mut config) = named_fixture()?;
    let mut volumes = config
        .volume_runtime
        .as_ref()
        .unwrap()
        .configuration()
        .clone();
    volumes
        .placement
        .iter_mut()
        .find(|rule| rule.role == VolumeRole::Active)
        .unwrap()
        .group = Some("named".into());
    config.volume_runtime = Some(Arc::new(runtime::Manager::new(volumes, catalog.handle())?));
    let worker = WriterWorker::new(
        config,
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
    );
    Ok((root, catalog, worker))
}

fn groups(worker: &WriterWorker, source: &str, named: bool) {
    worker.config.volume_groups.write().unwrap().insert(
        source.into(),
        if named { vec!["named".into()] } else { vec![] },
    );
}

#[test]
fn paused_ingest_routes_by_active_policy_and_keeps_named_admission_checks() -> anyhow::Result<()> {
    let (root, catalog, mut worker) = worker()?;
    let manager = worker.config.volume_runtime.as_ref().unwrap();
    assert!(!manager.uses_named_policy(VolumeRole::Active, "camera", &[])?);
    assert!(manager.uses_named_policy(VolumeRole::Active, "camera", &["named"])?);
    assert!(!manager.uses_named_policy(VolumeRole::Active, "camera", &["other"])?);
    assert!(manager.uses_named_policy(VolumeRole::Export, "camera", &[])?);
    let mut configuration = manager.configuration().clone();
    configuration.volumes[0].capacity_bytes = Some(1);
    worker.config.volume_runtime = Some(Arc::new(runtime::Manager::new(
        configuration,
        catalog.handle(),
    )?));
    groups(&worker, "camera", true);
    worker
        .safety
        .cleanup_failed("legacy archive is unavailable");
    worker.ingest(
        RecordingStreamIdentity::legacy("camera"),
        key_frame(Instant::now()),
    );
    assert!(worker.pipelines["camera"].medium_term.is_none());
    assert!(!root.join("legacy-medium").exists());
    assert_eq!(std::fs::read_dir(root.join("primary"))?.count(), 0);
    drop(worker);
    catalog.shutdown();
    Ok(())
}

#[test]
fn legacy_pause_preserves_named_ingest_and_pauses_unmatched_camera() -> anyhow::Result<()> {
    let (root, catalog, mut worker) = worker()?;
    groups(&worker, "camera", true);
    worker
        .safety
        .cleanup_failed("legacy archive is unavailable");
    let now = Instant::now();
    for offset in [0, 40] {
        worker.ingest(
            RecordingStreamIdentity::legacy("camera"),
            key_frame(now + Duration::from_millis(offset)),
        );
        worker.ingest(
            RecordingStreamIdentity::legacy("legacy"),
            key_frame(now + Duration::from_millis(offset)),
        );
    }
    assert!(!worker.pipelines.contains_key("legacy"));
    let writer = worker.pipelines["camera"].medium_term.as_ref().unwrap();
    let path = writer.active_path().to_owned();
    let id = writer.recording_id().to_owned();
    assert!(path.starts_with(root.join("primary")));
    worker.finalize_all();
    assert_published(&catalog.handle(), &id, &path);
    assert_eq!(samples(&path), 2);
    assert!(!root.join("legacy-medium").exists());
    drop(worker);
    catalog.shutdown();
    Ok(())
}

#[test]
fn named_writer_ignores_changed_groups_but_rotation_cannot_enter_paused_legacy()
-> anyhow::Result<()> {
    let (root, catalog, mut worker) = worker()?;
    groups(&worker, "camera", true);
    let now = Instant::now();
    worker.ingest(RecordingStreamIdentity::legacy("camera"), key_frame(now));
    worker.ingest(
        RecordingStreamIdentity::legacy("camera"),
        key_frame(now + Duration::from_millis(40)),
    );
    let path = worker.pipelines["camera"]
        .medium_term
        .as_ref()
        .unwrap()
        .active_path()
        .to_owned();
    groups(&worker, "camera", false);
    worker
        .safety
        .cleanup_failed("legacy archive is unavailable");
    worker.ingest(
        RecordingStreamIdentity::legacy("camera"),
        key_frame(now + Duration::from_millis(80)),
    );
    assert_eq!(
        worker.pipelines["camera"]
            .medium_term
            .as_ref()
            .unwrap()
            .frames_written(),
        3
    );
    worker.config.medium_term_duration = Duration::ZERO;
    worker.ingest(
        RecordingStreamIdentity::legacy("camera"),
        key_frame(now + Duration::from_millis(120)),
    );
    assert!(worker.pipelines["camera"].medium_term.is_none());
    assert!(!root.join("legacy-medium").exists());
    assert_eq!(samples(&path), 3);
    drop(worker);
    catalog.shutdown();
    Ok(())
}

#[test]
fn legacy_writer_stays_paused_when_groups_begin_matching_named_policy() -> anyhow::Result<()> {
    let (_root, catalog, mut worker) = worker()?;
    let identity = RecordingStreamIdentity::legacy("camera");
    let now = Instant::now();
    worker.ingest(identity.clone(), key_frame(now));
    worker.ingest(identity.clone(), key_frame(now + Duration::from_millis(40)));
    groups(&worker, "camera", true);
    worker
        .safety
        .cleanup_failed("legacy archive is unavailable");
    worker.ingest(identity, key_frame(now + Duration::from_millis(80)));
    assert_eq!(
        worker.pipelines["camera"]
            .medium_term
            .as_ref()
            .unwrap()
            .frames_written(),
        2
    );
    worker.finalize_all();
    drop(worker);
    catalog.shutdown();
    Ok(())
}
