use super::*;
use crate::storage::{
    catalog::locations::{Kind, Object, Reply, Request},
    volumes::{
        PlacementRule, PlacementStrategy, Volume, VolumeConfiguration, VolumeId, VolumeRole,
        VolumeState, runtime,
    },
};

mod failure_isolation;

fn named_fixture() -> anyhow::Result<(PathBuf, RecordingCatalog, StorageConfig)> {
    let (root, catalog, manager) = runtime::tests::fixture(8 * MEBIBYTE_BYTES)?;
    let mut config = storage_config("named-worker-unused");
    config.medium_term_path = root.join("legacy-medium");
    config.long_term_path = root.join("legacy-archive");
    config.recording_catalog_path = root.join("catalog.db");
    config.short_term_duration = Duration::ZERO;
    config.flush_interval = Duration::ZERO;
    config.volume_runtime = Some(Arc::new(manager));
    Ok((root, catalog, config))
}

fn assert_published(handle: &RecordingCatalogHandle, id: &str, path: &Path) {
    let Reply::Location(Some(location)) = handle
        .volume_location(Request::Lookup(Object {
            kind: Kind::Recording,
            id: id.to_owned(),
        }))
        .unwrap()
    else {
        panic!("recording was not published");
    };
    assert_eq!(location.volume, "primary");
    assert_eq!(
        path.file_name().unwrap().to_str().unwrap(),
        location.relative_key
    );
    assert_eq!(std::fs::metadata(path).unwrap().len(), location.bytes);
}

fn samples(path: &Path) -> u32 {
    let reader = mp4::read_mp4(File::open(path).unwrap()).unwrap();
    reader
        .tracks()
        .values()
        .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
        .unwrap()
        .sample_count()
}

#[test]
fn named_worker_rotation_publishes_without_moving_to_legacy_archive() -> anyhow::Result<()> {
    let (root, catalog, config) = named_fixture()?;
    let mut worker = WriterWorker::new(
        config,
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
    );
    let identity = RecordingStreamIdentity::legacy("camera");
    let now = Instant::now();
    worker.ingest(identity.clone(), key_frame(now));
    worker.ingest(identity.clone(), key_frame(now + Duration::from_millis(40)));
    let first = worker.pipelines["camera"].medium_term.as_ref().unwrap();
    let first_id = first.recording_id().to_owned();
    let first_path = first.active_path().to_path_buf();
    assert!(first_path.starts_with(root.join("primary")));
    worker.config.medium_term_duration = Duration::ZERO;
    worker.ingest(identity.clone(), key_frame(now + Duration::from_millis(80)));
    worker.config.medium_term_duration = Duration::from_secs(1_800);
    worker.ingest(identity, key_frame(now + Duration::from_millis(120)));
    let second = worker.pipelines["camera"].medium_term.as_ref().unwrap();
    let second_id = second.recording_id().to_owned();
    let second_path = second.active_path().to_path_buf();
    assert_ne!(first_id, second_id);
    assert_published(&catalog.handle(), &first_id, &first_path);
    assert_eq!(
        worker.move_to_long_term("camera", &first_path, &first_id)?,
        first_path
    );
    worker.finalize_all();
    assert_published(&catalog.handle(), &second_id, &second_path);
    assert_eq!(samples(&first_path), 2);
    assert_eq!(samples(&second_path), 2);
    assert!(!root.join("legacy-medium").exists());
    assert!(!root.join("legacy-archive").exists());
    catalog.shutdown();
    Ok(())
}

#[test]
fn all_disabled_draft_uses_legacy_writer_without_binding_or_opening_roots() -> anyhow::Result<()> {
    let mut config = storage_config(&format!("disabled-named-worker-{}", uuid::Uuid::new_v4()));
    let root = config.long_term_path.clone();
    std::fs::create_dir_all(&root)?;
    config.medium_term_path = root.join("medium");
    config.long_term_path = root.join("archive");
    config.short_term_duration = Duration::ZERO;
    config.flush_interval = Duration::ZERO;
    let missing = root.join("disabled-missing");
    let id = VolumeId::parse("draft")?;
    config.named_volumes = Some(VolumeConfiguration {
        volumes: vec![Volume {
            id: id.clone(),
            root: missing.clone(),
            roles: vec![VolumeRole::Active],
            state: VolumeState::Disabled,
            priority: 0,
            capacity_bytes: Some(MEBIBYTE_BYTES),
            minimum_free_bytes: 0,
            warning_free_bytes: 0,
            critical_free_bytes: 0,
            sources: vec![],
            groups: vec![],
        }],
        placement: vec![PlacementRule {
            role: VolumeRole::Active,
            source: None,
            group: None,
            candidates: vec![id],
            strategy: PlacementStrategy::Priority,
            allow_fallback: false,
        }],
    });
    let catalog = RecordingCatalog::open(&config.recording_catalog_path)?;
    config.initialize_named_volumes(catalog.handle())?;
    assert!(config.volume_runtime.is_none());
    let archive = config.long_term_path.clone();
    let mut worker = WriterWorker::new(
        config,
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
    );
    let now = Instant::now();
    for offset in [0, 40] {
        worker.ingest(
            RecordingStreamIdentity::legacy("camera"),
            key_frame(now + Duration::from_millis(offset)),
        );
    }
    worker.finalize_all();
    let paths = LongTermStore::new(archive.clone()).finalized_segments("camera")?;
    assert_eq!(paths.len(), 1);
    assert!(paths[0].starts_with(archive));
    assert_eq!(samples(&paths[0]), 2);
    assert_eq!(
        catalog.handle().volume_location(Request::Usage)?,
        Reply::Usage(vec![])
    );
    assert!(!missing.exists());
    catalog.shutdown();
    Ok(())
}

fn active_privacy() -> crate::privacy::PrivacySchedule {
    crate::privacy::PrivacySchedule {
        enabled: true,
        timezone: "UTC".into(),
        temporary_override: None,
        keep_camera_connected: true,
        windows: [("00:00", "12:00"), ("12:00", "00:00")]
            .into_iter()
            .map(|(start, end)| crate::privacy::PrivacyWindow {
                weekdays: (1..=7).collect(),
                start: start.into(),
                end: end.into(),
            })
            .collect(),
    }
}

#[test]
fn named_worker_privacy_boundary_finalizes_and_excludes_private_frame() -> anyhow::Result<()> {
    let (root, catalog, config) = named_fixture()?;
    let privacy = Arc::new(PrivacyRegistry::default());
    let mut worker = WriterWorker::new_with_health(
        config,
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
        RecordingHealthRegistry::default(),
        Arc::new(RwLock::new(Some(privacy.clone()))),
    );
    let identity = RecordingStreamIdentity::legacy("camera");
    let now = Instant::now();
    for offset in [0, 40] {
        worker.handle_command(Command::Ingest {
            identity: identity.clone(),
            frame: key_frame(now + Duration::from_millis(offset)),
            discontinuity: false,
        });
    }
    privacy.replace_schedules(std::collections::BTreeMap::from([(
        "camera".into(),
        active_privacy(),
    )]))?;
    worker.handle_command(Command::Ingest {
        identity: identity.clone(),
        frame: key_frame(now + Duration::from_millis(80)),
        discontinuity: false,
    });
    assert!(worker.pipelines.is_empty());
    assert_eq!(catalog.handle().stats()?.recording_files, 1);
    privacy.replace_schedules(std::collections::BTreeMap::new())?;
    for offset in [120, 160] {
        worker.handle_command(Command::Ingest {
            identity: identity.clone(),
            frame: key_frame(now + Duration::from_millis(offset)),
            discontinuity: false,
        });
    }
    worker.finalize_all();
    let paths = std::fs::read_dir(root.join("primary"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(paths.len(), 2);
    assert_eq!(paths.iter().map(|path| samples(path)).sum::<u32>(), 4);
    for path in paths {
        assert_published(
            &catalog.handle(),
            path.file_stem().unwrap().to_str().unwrap(),
            &path,
        );
    }
    catalog.shutdown();
    Ok(())
}

#[test]
fn named_event_boost_records_sub_and_main_gops_in_one_owned_file() -> anyhow::Result<()> {
    let (root, catalog, config) = named_fixture()?;
    let engine = StorageEngine::start_with_catalog(config, catalog.handle());
    let storage = engine.handle();
    storage.configure_camera_recording(
        "camera",
        CameraRecordingMode::EventBoost,
        Duration::from_secs(60),
    );
    let now = Instant::now();
    for offset in [0, 40] {
        storage.ingest_stream(
            RecordingStreamIdentity::new("camera", "sub", "camera"),
            key_frame(now + Duration::from_millis(offset)),
        );
    }
    storage.note_camera_event("camera");
    for offset in [80, 120] {
        storage.ingest_stream(
            RecordingStreamIdentity::new("camera", "main", "camera"),
            key_frame(now + Duration::from_millis(offset)),
        );
    }
    engine.shutdown();
    let fragments =
        catalog
            .handle()
            .media_fragments_in_range("camera/sub", i64::MIN + 1, i64::MAX)?;
    assert!(!fragments.is_empty());
    let path = PathBuf::from(&fragments[0].path);
    assert!(path.starts_with(root.join("primary")));
    assert!(
        fragments
            .iter()
            .all(|fragment| fragment.path == fragments[0].path)
    );
    assert_eq!(samples(&path), 4);
    assert_published(&catalog.handle(), &fragments[0].recording_id, &path);
    assert!(
        catalog
            .handle()
            .media_fragments_in_range("camera/main", i64::MIN + 1, i64::MAX)?
            .is_empty()
    );
    catalog.shutdown();
    Ok(())
}

fn legacy_archiver_fixture() -> anyhow::Result<(PathBuf, RecordingCatalog, StorageConfig, PathBuf)>
{
    let (root, catalog, mut config) = named_fixture()?;
    let (legacy, pinned) = crate::storage::volumes::root::test_root()?;
    drop(pinned);
    config.medium_term_path = legacy;
    config.event_thumbnail_path = root.join("legacy-thumbnails");
    let path = config
        .medium_term_path
        .join("camera/non-uuid-recording.mp4");
    std::fs::create_dir(path.parent().unwrap())?;
    std::fs::write(&path, [7_u8; 128])?;
    catalog
        .handle()
        .upsert_recording(crate::storage::catalog::CatalogRecording {
            id: "legacy-archiver".into(),
            stream_id: "camera/main".into(),
            source_id: Some("camera".into()),
            logical_stream_id: Some("main".into()),
            started_at_ms: 1000,
            ended_at_ms: Some(2000),
            path: path.to_string_lossy().into_owned(),
            init_offset: 0,
            init_len: 8,
            finalized: true,
        })?;
    Ok((root, catalog, config, path))
}

#[test]
fn adoption_claim_prevents_legacy_archiver_from_renaming_source() -> anyhow::Result<()> {
    let (_root, catalog, config, path) = legacy_archiver_fixture()?;
    let archive = config.long_term_path.clone();
    let handle = catalog.handle();
    let claim = handle.claim_volume_move("legacy-archiver")?;
    let worker = WriterWorker::new(
        config,
        RecordingDemand::new(Duration::ZERO),
        Some(handle.clone()),
    );
    assert!(
        worker
            .move_to_long_term("camera", &path, "legacy-archiver")
            .is_err()
    );
    assert_eq!(std::fs::read(&path)?, [7_u8; 128]);
    assert!(!archive.exists());
    drop(claim);
    let destination = worker.move_to_long_term("camera", &path, "legacy-archiver")?;
    assert_eq!(destination, archive.join("camera/non-uuid-recording.mp4"));
    assert!(!path.exists());
    assert_eq!(std::fs::read(destination)?, [7_u8; 128]);
    drop(worker);
    drop(handle);
    catalog.shutdown();
    Ok(())
}

fn adopt_archiver_source(
    root: &Path,
    catalog: &RecordingCatalog,
    config: &StorageConfig,
) -> anyhow::Result<()> {
    use crate::storage::catalog::locations::{
        Allocation,
        legacy::{LegacyPaths, adoption, inventory, roots},
        moves,
    };
    let handle = catalog.handle();
    let paths = LegacyPaths::effective(config)?;
    crate::storage::volumes::legacy::capture_roots(&handle, &paths)?;
    let Reply::LegacyReferences(mut references) =
        handle.volume_location(Request::LegacyInventory(inventory::Action::Recordings {
            after: None,
            limit: 1,
        }))?
    else {
        anyhow::bail!("legacy reference missing")
    };
    assert_eq!(references.len(), 1);
    let reference =
        crate::storage::volumes::legacy::verify_recording(&handle, &references.remove(0))?;
    let id = uuid::Uuid::new_v4().to_string();
    let primary = crate::storage::volumes::root::Root::open(&root.join("primary"))?;
    let destination = moves::Intent {
        id: id.clone(),
        object: reference.object.clone(),
        expected_revision: 1,
        destination: Allocation {
            operation: id.clone(),
            object: Object {
                kind: Kind::Recording,
                id: id.clone(),
            },
            volume: "primary".into(),
            generation: 1,
            relative_key: format!("{id}.mp4"),
            bytes: 128,
            capacity: primary.capacity(handle.volume_ledger_revision()?)?,
        },
    };
    handle.volume_location(Request::AdoptLegacyMedia(Box::new(adoption::Intent {
        reference,
        role: roots::Role::Active,
        operation: uuid::Uuid::new_v4().to_string(),
        destination,
    })))?;
    Ok(())
}

#[test]
fn already_adopted_source_never_uses_raw_archive_rename_without_runtime_manager()
-> anyhow::Result<()> {
    let (root, catalog, mut config, path) = legacy_archiver_fixture()?;
    config.volume_runtime = None;
    assert!(config.volume_runtime.is_none());
    adopt_archiver_source(&root, &catalog, &config)?;
    let object = Object {
        kind: Kind::Recording,
        id: "legacy-archiver".into(),
    };
    let before = catalog
        .handle()
        .volume_location(Request::Lookup(object.clone()))?;
    assert!(matches!(&before, Reply::Location(Some(_))));
    let archive = config.long_term_path.clone();
    let worker = WriterWorker::new(
        config,
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
    );
    assert_eq!(
        worker.move_to_long_term("camera", &path, "legacy-archiver")?,
        path
    );
    assert_eq!(std::fs::read(&path)?, [7_u8; 128]);
    assert!(!archive.exists());
    assert_eq!(
        catalog.handle().volume_location(Request::Lookup(object))?,
        before
    );
    drop(worker);
    catalog.shutdown();
    Ok(())
}

// Set volume_runtime=None explicitly in the existing test that covers a missing manager.

fn cancelled_adoption_pressure_fixture() -> anyhow::Result<(RecordingCatalog, WriterWorker, PathBuf)>
{
    let (root, catalog, mut config, path) = legacy_archiver_fixture()?;
    catalog
        .handle()
        .insert_fragment(crate::storage::catalog::CatalogFragment {
            recording_id: "legacy-archiver".into(),
            sequence: 1,
            start_ms: 1000,
            duration_ms: 1000,
            byte_offset: 8,
            byte_len: 120,
            random_access: true,
        })?;
    adopt_archiver_source(&root, &catalog, &config)?;
    let Reply::Moves(jobs) = catalog.handle().volume_location(Request::Moves(
        crate::storage::catalog::locations::moves::Page {
            after: None,
            limit: 2,
            include_terminal: false,
        },
    ))?
    else {
        anyhow::bail!("adoption move missing")
    };
    assert_eq!(jobs.len(), 1);
    let manager = config
        .volume_runtime
        .as_ref()
        .expect("fixture retains manager");
    manager.cancel_move(&jobs[0].id)?;
    let Reply::Move(cancelled) = catalog
        .handle()
        .volume_location(Request::Move(jobs[0].id.clone()))?
    else {
        anyhow::bail!("cancelled move missing")
    };
    assert_eq!(cancelled.phase, "cancelled");
    assert!(cancelled.receipt_acknowledged);
    assert_eq!(catalog.handle().legacy_recording_bytes()?, 128);
    config.long_term_max_bytes = 64;
    config.cleanup_hysteresis_bytes = 0;
    config.minimum_free_bytes = 0;
    config.warning_free_bytes = 0;
    config.critical_free_bytes = 0;
    let worker = WriterWorker::new(
        config,
        RecordingDemand::new(Duration::ZERO),
        Some(catalog.handle()),
    );
    Ok((catalog, worker, path))
}

fn adopted_pressure_capacity() -> FilesystemCapacity {
    FilesystemCapacity {
        total_bytes: 4096,
        available_bytes: 2048,
        keeppeek_bytes: 0,
    }
}

#[test]
fn legacy_cap_pressure_retires_cancelled_adopted_source_through_owned_journal() -> anyhow::Result<()>
{
    let (catalog, worker, path) = cancelled_adoption_pressure_fixture()?;
    worker.try_enforce_storage_limit(
        StorageCleanupTrigger::Periodic,
        Some(adopted_pressure_capacity()),
    )?;
    assert!(!path.exists());
    assert_eq!(catalog.handle().legacy_recording_bytes()?, 0);
    assert_eq!(catalog.handle().stats()?.recording_files, 0);
    let safety = worker.safety.snapshot();
    assert_eq!(safety.last_cleanup_files_removed, 1);
    assert_eq!(safety.last_cleanup_bytes_removed, 128);
    assert_eq!(safety.pressure, StoragePressure::Normal);
    assert_eq!(
        catalog.handle().volume_location(Request::Lookup(Object {
            kind: Kind::Recording,
            id: "legacy-archiver".into(),
        }))?,
        Reply::Location(None)
    );
    drop(worker);
    catalog.shutdown();
    Ok(())
}

#[test]
fn legacy_cap_pressure_retains_adopted_reader_then_retries_after_release() -> anyhow::Result<()> {
    let (catalog, worker, path) = cancelled_adoption_pressure_fixture()?;
    let handle = catalog.handle();
    let (fragments, lease) = handle.leased_media_fragments_in_range("camera/main", 1000, 2000)?;
    assert_eq!(fragments.len(), 1);
    assert!(
        worker
            .try_enforce_storage_limit(
                StorageCleanupTrigger::Periodic,
                Some(adopted_pressure_capacity()),
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&path)?, [7_u8; 128]);
    assert_eq!(handle.legacy_recording_bytes()?, 128);
    assert_eq!(handle.stats()?.recording_files, 1);
    drop(lease);
    worker.try_enforce_storage_limit(
        StorageCleanupTrigger::Periodic,
        Some(adopted_pressure_capacity()),
    )?;
    assert!(!path.exists());
    assert_eq!(handle.legacy_recording_bytes()?, 0);
    assert_eq!(handle.stats()?.recording_files, 0);
    assert_eq!(worker.safety.snapshot().last_cleanup_bytes_removed, 128);
    drop(handle);
    drop(worker);
    catalog.shutdown();
    Ok(())
}

#[test]
fn pressure_selection_refreshes_after_background_retirement_instead_of_deleting_next_file()
-> anyhow::Result<()> {
    let (catalog, worker, adopted_path) = cancelled_adoption_pressure_fixture()?;
    let handle = catalog.handle();
    std::fs::create_dir_all(&worker.config.long_term_path)?;
    let ordinary = worker.config.long_term_path.join("newer-ordinary.mp4");
    insert_cleanup_recording(&handle, "newer-ordinary", &ordinary, 3000, 32);
    let sampled = worker.cleanup_capacity(&handle, Some(adopted_pressure_capacity()))?;
    assert_eq!(sampled.keeppeek_bytes, 160);
    let manager = worker.config.volume_runtime.as_ref().unwrap();
    assert_eq!(
        manager.cleanup_legacy_adoption(
            crate::storage::catalog::locations::recordings::Reason::Capacity,
        )?,
        Some(128)
    );
    assert!(!adopted_path.exists());
    assert_eq!(handle.legacy_recording_bytes()?, 32);
    assert_eq!(
        worker.remove_pressure_candidate(&handle, StorageCleanupReason::ArchiveCap, 64,)?,
        (0, 0)
    );
    assert_eq!(std::fs::read(&ordinary)?, [0_u8; 32]);
    assert!(handle.pending_cleanup_candidate()?.is_none());
    assert_eq!(handle.stats()?.recording_files, 1);
    assert_eq!(
        worker
            .cleanup_capacity(&handle, Some(sampled))?
            .keeppeek_bytes,
        32
    );
    worker.try_enforce_storage_limit(StorageCleanupTrigger::Periodic, Some(sampled))?;
    assert!(ordinary.exists());
    assert_eq!(handle.legacy_recording_bytes()?, 32);
    drop(handle);
    drop(worker);
    catalog.shutdown();
    Ok(())
}

#[test]
fn cancelled_adoption_remains_cleanup_capable_after_last_named_definition_is_removed()
-> anyhow::Result<()> {
    for named in [
        None,
        Some(VolumeConfiguration {
            volumes: vec![],
            placement: vec![],
        }),
    ] {
        let (catalog, worker, path) = cancelled_adoption_pressure_fixture()?;
        let mut config = worker.config.clone();
        catalog.handle().volume_location(Request::SetDraining {
            volume: "primary".into(),
            generation: 1,
            draining: true,
        })?;
        assert_eq!(
            catalog
                .handle()
                .volume_location(Request::EnsureRemovable("primary".into()))?,
            Reply::Bound
        );
        config.volume_runtime = None;
        config.named_volumes = named;
        drop(worker);
        catalog.shutdown();
        let catalog = RecordingCatalog::open(&config.recording_catalog_path)?;
        config.initialize_named_volumes(catalog.handle())?;
        assert!(
            config.volume_runtime.is_some(),
            "adopted ownership needs recovery without named definitions"
        );
        assert!(
            config
                .volume_runtime
                .as_ref()
                .unwrap()
                .configuration()
                .volumes
                .is_empty()
        );
        assert_eq!(catalog.handle().legacy_recording_bytes()?, 128);
        let worker = WriterWorker::new(
            config,
            RecordingDemand::new(Duration::ZERO),
            Some(catalog.handle()),
        );
        worker.try_enforce_storage_limit(
            StorageCleanupTrigger::Periodic,
            Some(adopted_pressure_capacity()),
        )?;
        assert!(!path.exists());
        assert_eq!(catalog.handle().legacy_recording_bytes()?, 0);
        assert_eq!(catalog.handle().stats()?.recording_files, 0);
        drop(worker);
        catalog.shutdown();
    }
    Ok(())
}
