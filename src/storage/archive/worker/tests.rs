use super::*;
use crate::storage::{
    catalog::{
        RecordingCatalog,
        locations::{Kind, Object},
    },
    identity::RecordingStreamIdentity,
    medium_term::MediumTermWriter,
    volumes::{
        PlacementRule, PlacementStrategy, Volume, VolumeConfiguration, VolumeId, VolumeRole,
        VolumeState, runtime::Manager,
    },
};
use sha2::Digest;
use std::{
    io::{Cursor, Write},
    path::PathBuf,
};

mod lifecycle;
mod probes;

struct Fixture {
    path: PathBuf,
    settings: Settings,
    catalog: RecordingCatalog,
    manager: Manager,
}

impl Fixture {
    fn new() -> anyhow::Result<Self> {
        use crate::storage::long_term::inspection::removal::create_private_directory;
        let base = std::env::temp_dir();
        #[cfg(unix)]
        let base = std::fs::canonicalize(base)?;
        let parent = cap_std::fs::Dir::open_ambient_dir(&base, cap_std::ambient_authority())?;
        let name = format!("keeppeek-archive-worker-{}", ObjectId::random());
        drop(create_private_directory(
            &parent,
            std::ffi::OsStr::new(&name),
        )?);
        let path = base.join(name);
        let root = Root::open(&path)?;
        drop(root.create_private_child("primary")?);
        drop(root.create_private_child("archive")?);
        let catalog = RecordingCatalog::open(&path.join("catalog.db"))?;
        let manager = Manager::new(configuration(path.join("primary"))?, catalog.handle())?;
        let settings = Settings {
            target_id: "local-archive".into(),
            root: path.join("archive"),
            enabled: true,
            automatic: false,
            source_ids: vec![],
            minimum_age_secs: 0,
            evict_after_archive: false,
            maximum_object_bytes: 4 * 1024 * 1024 * 1024,
            maximum_pending_count: 256,
            maximum_pending_bytes: 64 * 1024 * 1024 * 1024,
            operation_timeout_secs: 30,
            maximum_attempts: 3,
        };
        Ok(Self {
            path,
            settings,
            catalog,
            manager,
        })
    }

    fn recording(&self, source: &str) -> anyhow::Result<(String, PathBuf)> {
        let object = Object {
            kind: Kind::Recording,
            id: ObjectId::random().to_string(),
        };
        let reservation = self
            .manager
            .reserve(VolumeRole::Active, source, &[], object.clone(), 1)?
            .expect("recording reservation");
        let now = Instant::now() - Duration::from_secs(2);
        let mut writer = MediumTermWriter::create_with_reservation(
            reservation,
            object.id.clone(),
            RecordingStreamIdentity::new(source, "main", source),
            now,
            8192,
            self.catalog.handle(),
        )?;
        for elapsed in [0, 100] {
            writer.append_one(frame(now, elapsed))?;
        }
        Ok((object.id, writer.finalize()?))
    }

    fn context(&self) -> Context {
        Context {
            settings: Arc::new(self.settings.clone()),
            catalog: self.catalog.handle(),
            shared: Arc::new(Shared {
                stopped: AtomicBool::new(false),
                active: Mutex::new(None),
                health: Mutex::new(Health {
                    available: false,
                    message: None,
                    probe_pending: false,
                }),
            }),
        }
    }

    fn close(self) -> anyhow::Result<()> {
        drop(self.manager);
        self.catalog.shutdown();
        std::fs::remove_dir_all(self.path)?;
        Ok(())
    }
}

fn configuration(root: PathBuf) -> anyhow::Result<VolumeConfiguration> {
    let volume = Volume {
        id: VolumeId::parse("primary")?,
        root,
        roles: vec![VolumeRole::Active, VolumeRole::Export],
        state: VolumeState::Enabled,
        priority: 0,
        capacity_bytes: Some(64 * 1_048_576),
        minimum_free_bytes: 0,
        warning_free_bytes: 0,
        critical_free_bytes: 0,
        sources: vec![],
        groups: vec![],
    };
    Ok(VolumeConfiguration {
        volumes: vec![volume],
        placement: [VolumeRole::Active, VolumeRole::Export]
            .into_iter()
            .map(|role| PlacementRule {
                role,
                source: None,
                group: None,
                candidates: vec![VolumeId::parse("primary").expect("static volume ID")],
                strategy: PlacementStrategy::Priority,
                allow_fallback: false,
            })
            .collect(),
    })
}

fn frame(now: Instant, milliseconds: u64) -> crate::storage::RecordingFrame {
    use crate::storage::{MediaFrame, RecordingFrame, VideoCodec, VideoFrame};
    let elapsed = Duration::from_millis(milliseconds);
    RecordingFrame {
        received_at: now + elapsed,
        timestamp: Some(elapsed),
        frame: MediaFrame::Video(VideoFrame {
            codec: VideoCodec::H264,
            is_keyframe: true,
            width: 320,
            height: 240,
            data: bytes::Bytes::from_static(&[
                0, 0, 0, 8, 0x67, 0x42, 0, 0x1f, 0xe5, 0x88, 0x68, 0x40, 0, 0, 0, 4, 0x68, 0xce,
                0x3c, 0x80, 0, 0, 0, 1, 0x65,
            ]),
        }),
    }
}

fn wait_available(handle: &Handle) -> anyhow::Result<()> {
    handle.request_probe()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let health = handle.health();
        if !health.probe_pending {
            anyhow::ensure!(health.available, "archive probe failed");
            return Ok(());
        }
        anyhow::ensure!(Instant::now() < deadline, "archive probe did not complete");
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_state(handle: &Handle, id: &str, state: &str) -> anyhow::Result<Job> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let job = handle.get(id)?;
        if job.state == state {
            return Ok(job);
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "archive job did not reach {state}; actual {}",
            job.state
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn prepared(fixture: &Fixture, recording: &str, publish: bool) -> anyhow::Result<Job> {
    let context = fixture.context();
    context.bind()?;
    let job = admit(
        &fixture.settings,
        &context.catalog,
        recording,
        "recording",
        &ObjectId::random().to_string(),
    )?;
    context.catalog.archive(Request::Start {
        id: job.id.clone(),
        maximum_attempts: 3,
    })?;
    let job = context.catalog.archive_job(&job.id)?;
    let target_root = Root::open(&job.binding.root)?;
    let capacity = target_root.capacity(context.catalog.volume_ledger_revision()?)?;
    context.catalog.archive(Request::ReserveTransfer {
        id: job.id.clone(),
        capacity,
    })?;
    let target = FilesystemArchiveTarget::open(&job.binding.root, &job.binding.identity)?;
    let result = target.publish(
        ObjectId::parse(&job.object_id)?,
        &mut Cursor::new(std::fs::read(&job.source_path)?),
        job.bytes,
        None,
        &mut |version| {
            context.catalog.archive(Request::Stage {
                id: job.id.clone(),
                version: version.clone(),
            })?;
            anyhow::ensure!(
                publish || version.bytes == 0,
                "injected crash before rename"
            );
            Ok(())
        },
        &Work {
            deadline: Instant::now() + Duration::from_secs(30),
            cancelled: &|| false,
        },
    );
    assert_eq!(result.is_ok(), publish);
    context.catalog.archive_job(&job.id)
}

#[test]
fn archive_worker_copies_finalized_recording_and_persists_verified_manifest() -> anyhow::Result<()>
{
    let fixture = Fixture::new()?;
    let (recording, path) = fixture.recording("camera")?;
    let original = std::fs::read(&path)?;
    let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
    let handle = worker.handle();
    wait_available(&handle)?;
    let job = handle.enqueue(&recording, "recording", &ObjectId::random().to_string())?;
    let archived = wait_state(&handle, &job.id, "ready")?;
    assert_eq!(archived.recording_id, recording);
    assert_eq!(archived.chunk_hashes.len(), 32);
    assert_eq!(
        archived.digest,
        <[u8; 32]>::from(sha2::Sha256::digest(&original))
    );
    assert_eq!(std::fs::read(&path)?, original);
    let copy = archived
        .binding
        .root
        .join(format!("{}.mp4", archived.object_id));
    assert_eq!(std::fs::read(&copy)?, original);
    assert_eq!(
        mp4::read_mp4(std::fs::File::open(copy)?)?
            .tracks()
            .values()
            .find(|track| track.track_type().ok() == Some(mp4::TrackType::Video))
            .expect("video track")
            .sample_count(),
        2
    );
    worker.shutdown()?;
    assert_eq!(
        fixture.catalog.handle().archive_job(&job.id)?.state,
        "ready"
    );
    drop(handle);
    fixture.close()
}

#[test]
fn archive_worker_recovers_full_stage_before_publication() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (recording, path) = fixture.recording("camera")?;
    let job = prepared(&fixture, &recording, false)?;
    assert_eq!(job.state, "copying");
    assert_eq!(
        job.stage.as_ref().expect("durable staging evidence").bytes,
        job.bytes
    );
    let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
    let handle = worker.handle();
    let recovered = wait_state(&handle, &job.id, "ready")?;
    assert_eq!(recovered.attempts, 2);
    assert_eq!(
        std::fs::read(
            recovered
                .binding
                .root
                .join(format!("{}.mp4", recovered.object_id))
        )?,
        std::fs::read(path)?
    );
    worker.shutdown()?;
    drop(handle);
    fixture.close()
}

#[test]
fn archive_worker_checks_source_after_recovered_publication_and_retries_same_identity()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (recording, path) = fixture.recording("camera")?;
    let original = std::fs::read(&path)?;
    let prepared = prepared(&fixture, &recording, true)?;
    let mut changed = original.clone();
    changed[0] ^= 1;
    std::fs::write(&path, changed)?;
    let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
    let handle = worker.handle();
    let failed = wait_state(&handle, &prepared.id, "failed")?;
    assert!(failed.version.is_none());
    assert_eq!(failed.attempts, 2);
    std::fs::write(&path, &original)?;
    handle.retry(&prepared.id)?;
    let retried = wait_state(&handle, &prepared.id, "ready")?;
    assert_eq!(retried.object_id, prepared.object_id);
    assert_eq!(
        retried.stage.expect("same file identity").token,
        prepared.stage.expect("prior evidence").token
    );
    assert_eq!(handle.list("", PAGE_SIZE)?.len(), 1);
    worker.shutdown()?;
    drop(handle);
    fixture.close()
}

#[test]
fn archive_worker_auto_selection_is_bounded_and_respects_source_filter() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let (selected, _) = fixture.recording("selected")?;
    fixture.recording("excluded")?;
    fixture.settings.automatic = true;
    fixture.settings.source_ids = vec!["selected".into()];
    let context = fixture.context();
    let mut scan = Scan::new();
    for _ in 0..32 {
        scan.turn(&context)?;
        if !scan.active() {
            break;
        }
    }
    assert!(!scan.active());
    let Reply::Jobs(jobs) = context.catalog.archive(Request::List {
        after: String::new(),
        limit: PAGE_SIZE,
    })?
    else {
        panic!("job page");
    };
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].recording_id, selected);
    assert_eq!(jobs[0].state, "ready");
    drop(context);
    fixture.close()
}

#[test]
fn archive_worker_queue_limit_preserves_source_and_ready_copy_releases_admission()
-> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let (first, path) = fixture.recording("first")?;
    let (second, _) = fixture.recording("second")?;
    fixture.settings.maximum_pending_count = 1;
    let context = fixture.context();
    context.bind()?;
    let job = admit(
        &fixture.settings,
        &context.catalog,
        &first,
        "recording",
        &ObjectId::random().to_string(),
    )?;
    assert!(
        admit(
            &fixture.settings,
            &context.catalog,
            &second,
            "recording",
            &ObjectId::random().to_string()
        )
        .is_err()
    );
    assert!(path.is_file());
    context.process(job, Instant::now() + Duration::from_secs(30))?;
    let next = admit(
        &fixture.settings,
        &context.catalog,
        &second,
        "recording",
        &ObjectId::random().to_string(),
    )?;
    assert_eq!(next.state, "queued");
    drop(context);
    fixture.close()
}

#[test]
fn archive_worker_destination_outage_does_not_prevent_local_recording() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let retained = fixture.path.join("offline-archive");
    std::fs::rename(&fixture.settings.root, &retained)?;
    let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
    let (recording, path) = fixture.recording("camera")?;
    let reader = mp4::read_mp4(std::fs::File::open(&path)?)?;
    assert!(
        reader
            .tracks()
            .values()
            .any(|track| track.sample_count() == 2)
    );
    assert!(!worker.handle().health().available);
    assert!(!fixture.settings.root.exists());
    std::fs::rename(retained, &fixture.settings.root)?;
    let handle = worker.handle();
    wait_available(&handle)?;
    let job = handle.enqueue(&recording, "recording", &ObjectId::random().to_string())?;
    wait_state(&handle, &job.id, "ready")?;
    drop(reader);
    worker.shutdown()?;
    drop(handle);
    fixture.close()
}

fn finish_scan(scan: &mut Scan, context: &Context) -> anyhow::Result<()> {
    for _ in 0..32 {
        scan.turn(context)?;
        if !scan.active() {
            return Ok(());
        }
    }
    anyhow::bail!("archive fixture scan did not finish its bounded pass")
}

#[test]
fn archive_worker_offline_target_preserves_copy_and_probe_attempt_budgets() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (recording, path) = fixture.recording("camera")?;
    let original = std::fs::read(&path)?;
    let context = fixture.context();
    context.bind()?;
    let copy = admit(
        &fixture.settings,
        &context.catalog,
        &recording,
        "recording",
        &ObjectId::random().to_string(),
    )?;
    let probe = admit(
        &fixture.settings,
        &context.catalog,
        "probe:local-archive",
        "probe",
        &ObjectId::random().to_string(),
    )?;
    let offline = fixture.path.join("offline-archive");
    std::fs::rename(&fixture.settings.root, &offline)?;
    for _ in 0..4 {
        let mut scan = Scan::new();
        scan.begin(true);
        finish_scan(&mut scan, &context)?;
        assert_eq!(context.catalog.archive_job(&copy.id)?.attempts, 0);
        assert_eq!(context.catalog.archive_job(&probe.id)?.attempts, 0);
    }
    assert_eq!(std::fs::read(&path)?, original);
    std::fs::rename(offline, &fixture.settings.root)?;
    context.probe(Instant::now() + Duration::from_secs(2))?;
    let mut scan = Scan::new();
    scan.begin(true);
    finish_scan(&mut scan, &context)?;
    let ready = context.catalog.archive_job(&copy.id)?;
    assert_eq!(ready.state, "ready");
    assert_eq!(ready.attempts, 1);
    assert_eq!(context.catalog.archive_job(&probe.id)?.state, "deleted");
    assert_eq!(
        std::fs::read(ready.binding.root.join(format!("{}.mp4", ready.object_id)))?,
        original
    );
    drop(context);
    fixture.close()
}

#[test]
fn archive_worker_verification_hashes_fixed_chunks_without_buffering_objects() -> anyhow::Result<()>
{
    use sha2::Sha256;
    let size = usize::try_from(super::super::CHUNK_BYTES)?;
    let bytes = vec![23_u8; size + 3];
    let mut sink = transfer::VerificationSink::new(bytes.len() as u64)?;
    for part in bytes.chunks(65_537) {
        sink.write_all(part)?;
    }
    let verified = sink.finish()?;
    assert_eq!(verified.digest, <[u8; 32]>::from(Sha256::digest(&bytes)));
    assert_eq!(verified.chunk_hashes.len(), 64);
    let first: [u8; 32] = Sha256::digest(&bytes[..size]).into();
    let last: [u8; 32] = Sha256::digest(&bytes[size..]).into();
    assert_eq!(&verified.chunk_hashes[..32], &first);
    assert_eq!(&verified.chunk_hashes[32..], &last);
    let mut too_long = transfer::VerificationSink::new(1)?;
    assert!(too_long.write_all(b"ab").is_err());
    assert!(transfer::VerificationSink::new(2)?.finish().is_err());
    Ok(())
}

#[test]
fn archive_worker_cancels_and_cleans_staged_or_published_copy_with_receipts() -> anyhow::Result<()>
{
    for published in [false, true] {
        let fixture = Fixture::new()?;
        let (recording, path) = fixture.recording("camera")?;
        let original = std::fs::read(&path)?;
        let job = prepared(&fixture, &recording, published)?;
        let context = fixture.context();
        context.catalog.archive(Request::Cancel(job.id.clone()))?;
        let cancelled = context.catalog.archive_job(&job.id)?;
        assert!(cancelled.cancelled);
        context.process(cancelled, Instant::now() + Duration::from_secs(30))?;
        let cleaned = context.catalog.archive_job(&job.id)?;
        assert!(cleaned.cleanup_complete);
        assert!(cleaned.cleanup_acknowledged);
        assert!(
            !fixture
                .settings
                .root
                .join(format!("{}.tmp", job.object_id))
                .exists()
        );
        assert!(
            !fixture
                .settings
                .root
                .join(format!("{}.mp4", job.object_id))
                .exists()
        );
        assert_eq!(std::fs::read(path)?, original);
        retry_cleaned_copy(&context, &cleaned, &original)?;
        drop(context);
        fixture.close()?;
    }
    Ok(())
}

fn retry_cleaned_copy(context: &Context, job: &Job, expected: &[u8]) -> anyhow::Result<()> {
    context.catalog.archive(Request::Retry(job.id.clone()))?;
    let queued = context.catalog.archive_job(&job.id)?;
    assert_eq!(queued.id, job.id);
    assert_eq!(queued.object_id, job.object_id);
    assert_eq!(queued.state, "queued");
    assert!(!queued.cancelled);
    assert!(queued.stage.is_none());
    context.process(queued, Instant::now() + Duration::from_secs(30))?;
    let ready = context.catalog.archive_job(&job.id)?;
    assert_eq!(ready.state, "ready");
    assert_eq!(ready.recording_id, job.recording_id);
    assert_eq!(std::fs::read(&ready.source_path)?, expected);
    assert_eq!(
        std::fs::read(ready.binding.root.join(format!("{}.mp4", ready.object_id)))?,
        expected
    );
    Ok(())
}

#[test]
fn archive_worker_keeps_unknown_cancelled_artifact_bounded_and_never_adopts_it()
-> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let (recording, _) = fixture.recording("camera")?;
    fixture.settings.maximum_pending_count = 1;
    let context = fixture.context();
    context.bind()?;
    let job = admit(
        &fixture.settings,
        &context.catalog,
        &recording,
        "recording",
        &ObjectId::random().to_string(),
    )?;
    context.catalog.archive(Request::Start {
        id: job.id.clone(),
        maximum_attempts: 3,
    })?;
    let root = Root::open(&fixture.settings.root)?;
    let key = format!("{}.tmp", job.object_id);
    drop(root.create_file(&key)?);
    context.catalog.archive(Request::Cancel(job.id.clone()))?;
    let cancelled = context.catalog.archive_job(&job.id)?;
    assert!(cancelled.stage.is_none());
    assert!(
        context
            .process(cancelled, Instant::now() + Duration::from_secs(30))
            .is_err()
    );
    assert!(!context.catalog.archive_job(&job.id)?.cleanup_acknowledged);
    assert!(fixture.settings.root.join(key).is_file());
    assert!(
        admit(
            &fixture.settings,
            &context.catalog,
            &recording,
            "recording",
            &ObjectId::random().to_string()
        )
        .is_err()
    );
    drop(root);
    drop(context);
    fixture.close()
}

#[test]
fn archive_worker_probe_request_is_queued_and_durably_cleans_its_object() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let context = fixture.context();
    let (sender, receiver) = mpsc::sync_channel(1);
    let handle = Handle {
        settings: context.settings.clone(),
        catalog: context.catalog.clone(),
        sender,
        shared: context.shared.clone(),
    };
    handle.request_probe()?;
    assert!(handle.health().probe_pending);
    assert_eq!(std::fs::read_dir(&fixture.settings.root)?.count(), 0);
    let command = receiver.try_recv()?;
    dispatch(&context, &mut Scan::new(), command);
    assert!(!handle.health().probe_pending);
    assert!(handle.health().available);
    let jobs = handle.list("", PAGE_SIZE)?;
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].kind, "probe");
    assert_eq!(jobs[0].state, "deleted");
    assert!(jobs[0].probe_acknowledged);
    assert!(
        !fixture
            .settings
            .root
            .join(format!("{}.mp4", jobs[0].object_id))
            .exists()
    );
    drop(handle);
    drop(context);
    fixture.close()
}

fn claim_maintenance(fixture: &Fixture, recording: &str) -> anyhow::Result<()> {
    use crate::storage::catalog::maintenance::{Scope, jobs};
    use anyhow::Context as _;
    let catalog = fixture.catalog.handle();
    let scope = Scope::Recording {
        source_id: "camera".into(),
        stream_id: "main".into(),
        recording_id: recording.into(),
    };
    let snapshot = catalog.recording_maintenance_snapshot(scope.clone())?;
    let mut prepared = catalog
        .recording_deletion_intent(
            "admin",
            jobs::Action::Prepare(jobs::Intent {
                scope,
                expected_revision: snapshot.revision,
                reason: jobs::Reason::Operator,
            }),
        )
        .context("prepare fixture maintenance intent")?;
    catalog
        .recording_deletion_intent(
            "admin",
            jobs::Action::Confirm {
                id: prepared.id.clone(),
                nonce: prepared.confirmation.take().expect("confirmation secret"),
                expected_revision: prepared.revision,
            },
        )
        .context("confirm fixture maintenance intent")?;
    let authorizations = std::cell::Cell::new(0_u8);
    // Stop after the owner commits named claims and before it changes recording bytes.
    let result = catalog.execute_named_recording_deletion_authorized(
        "admin",
        &prepared.id,
        &fixture.manager,
        |_| {
            authorizations.set(authorizations.get() + 1);
            anyhow::ensure!(authorizations.get() == 1, "fixture authorization withdrawn");
            Ok(())
        },
    );
    assert!(result.is_err());
    assert_eq!(authorizations.get(), 2);
    Ok(())
}

#[test]
fn archive_worker_bad_candidate_does_not_skip_later_eligible_recording() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    let (first, _) = fixture.recording("camera")?;
    let (second, _) = fixture.recording("camera")?;
    let (blocked, eligible) = if first < second {
        (&first, &second)
    } else {
        (&second, &first)
    };
    claim_maintenance(&fixture, blocked)?;
    fixture.settings.automatic = true;
    let context = fixture.context();
    let mut scan = Scan::new();
    for _ in 0..32 {
        scan.turn(&context)?;
        if !scan.active() {
            break;
        }
    }
    assert!(!scan.active());
    let Reply::Jobs(jobs) = context.catalog.archive(Request::List {
        after: String::new(),
        limit: PAGE_SIZE,
    })?
    else {
        panic!("job page");
    };
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].recording_id, *eligible);
    assert_eq!(jobs[0].state, "ready");
    drop(context);
    fixture.close()
}
