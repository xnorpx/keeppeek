use super::*;
use crate::storage::{
    CatalogMediaFragment,
    archive::{VerifiedReader, read_range},
};

mod health;
mod performance;

type Index = (String, i64, u64, i64, u64, u64, u64, u64, u64);

impl Fixture {
    fn reopen(self) -> anyhow::Result<Self> {
        drop(self.manager);
        self.catalog.shutdown();
        let catalog = RecordingCatalog::open(&self.path.join("catalog.db"))?;
        let manager = Manager::new(configuration(self.path.join("primary"))?, catalog.handle())?;
        Ok(Self {
            path: self.path,
            settings: self.settings,
            catalog,
            manager,
        })
    }
}

pub(super) fn work() -> Work<'static> {
    fn never() -> bool {
        false
    }
    Work {
        deadline: Instant::now() + Duration::from_secs(30),
        cancelled: &never,
    }
}

fn fragments(catalog: &RecordingCatalogHandle) -> anyhow::Result<Vec<CatalogMediaFragment>> {
    catalog.media_fragments_in_range("camera/main", 0, i64::MAX)
}

fn indexes(fragments: &[CatalogMediaFragment]) -> Vec<Index> {
    fragments
        .iter()
        .map(|fragment| {
            (
                fragment.recording_id.clone(),
                fragment.recording_started_at_ms,
                fragment.sequence,
                fragment.start_ms,
                fragment.duration_ms,
                fragment.byte_offset,
                fragment.byte_len,
                fragment.init_offset,
                fragment.init_len,
            )
        })
        .collect()
}

fn wait_job(handle: &Handle, id: &str, accepted: impl Fn(&Job) -> bool) -> anyhow::Result<Job> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let job = handle.get(id)?;
        if accepted(&job) {
            return Ok(job);
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "archive lifecycle transition timed out: {} {}",
            job.state,
            job.action
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_local(handle: &Handle, id: &str, local_evicted: u8) -> anyhow::Result<Job> {
    wait_job(handle, id, |job| {
        job.local_evicted == local_evicted && job.action.is_empty()
    })
}

fn check_archive_playback(
    catalog: &RecordingCatalogHandle,
    expected: &[u8],
    before: &[Index],
) -> anyhow::Result<()> {
    let (fragments, _lease) =
        catalog.leased_media_fragments_in_range("camera/main", 0, i64::MAX)?;
    assert_eq!(indexes(&fragments), before);
    assert!(!fragments.is_empty());
    for fragment in &fragments {
        let archive = fragment.archive.as_ref().expect("archive-backed timeline");
        let observed = read_range(archive, fragment.byte_offset, fragment.byte_len, &work())?;
        let from = usize::try_from(fragment.byte_offset)?;
        let to = usize::try_from(fragment.byte_offset + fragment.byte_len)?;
        assert_eq!(observed, expected[from..to]);
        let initialization = read_range(archive, fragment.init_offset, fragment.init_len, &work())?;
        let from = usize::try_from(fragment.init_offset)?;
        let to = usize::try_from(fragment.init_offset + fragment.init_len)?;
        assert_eq!(initialization, expected[from..to]);
    }
    check_seek(
        fragments[0].archive.clone().expect("archive descriptor"),
        expected,
    )
}

fn check_seek(
    archive: crate::storage::catalog::archive_playback::ArchiveRead,
    expected: &[u8],
) -> anyhow::Result<()> {
    let mut original = mp4::Mp4Reader::read_header(Cursor::new(expected), expected.len() as u64)?;
    let bytes = archive.bytes;
    let mut archived = mp4::Mp4Reader::read_header(VerifiedReader::new(archive, work())?, bytes)?;
    let track = original
        .tracks()
        .iter()
        .find_map(|(id, track)| {
            (track.track_type().ok() == Some(mp4::TrackType::Video)).then_some(*id)
        })
        .expect("video track");
    assert_eq!(archived.tracks()[&track].sample_count(), 2);
    for sample in [2, 1, 2] {
        let expected = original
            .read_sample(track, sample)?
            .expect("recorded sample");
        let observed = archived
            .read_sample(track, sample)?
            .expect("archived sample");
        assert_eq!(observed.bytes, expected.bytes);
        assert_eq!(observed.start_time, expected.start_time);
    }
    Ok(())
}

#[test]
fn archive_worker_lifecycle_evicts_plays_seeks_restarts_and_restores_same_recording()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (recording, path) = fixture.recording("camera")?;
    let original = std::fs::read(&path)?;
    let before = indexes(&fragments(&fixture.catalog.handle())?);
    assert_eq!(before.len(), 2);
    let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
    let handle = worker.handle();
    wait_available(&handle)?;
    let job = handle.enqueue(&recording, "recording", &ObjectId::random().to_string())?;
    wait_state(&handle, &job.id, "ready")?;
    handle.evict(&job.id)?;
    wait_local(&handle, &job.id, 1)?;
    assert!(!path.exists());
    check_archive_playback(&fixture.catalog.handle(), &original, &before)?;
    worker.shutdown()?;
    drop(handle);
    let fixture = fixture.reopen()?;
    check_archive_playback(&fixture.catalog.handle(), &original, &before)?;
    let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
    let handle = worker.handle();
    handle.restore(&job.id)?;
    let restored = wait_local(&handle, &job.id, 0)?;
    assert_eq!(restored.recording_id, recording);
    assert_eq!(std::fs::read(&path)?, original);
    let after = fragments(&fixture.catalog.handle())?;
    assert_eq!(indexes(&after), before);
    assert!(after.iter().all(|fragment| fragment.archive.is_none()));
    assert!(
        after
            .iter()
            .all(|fragment| fragment.path == path.to_string_lossy())
    );
    let jobs = handle.list("", PAGE_SIZE)?;
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().any(|job| job.id == restored.id));
    assert!(
        jobs.iter()
            .any(|job| { job.kind == "probe" && job.state == "deleted" && job.probe_acknowledged })
    );
    worker.shutdown()?;
    drop(handle);
    fixture.close()
}

#[test]
fn archive_worker_reader_lease_and_protection_block_eviction_until_explicit_retry()
-> anyhow::Result<()> {
    for protected in [false, true] {
        let fixture = Fixture::new()?;
        let (recording, path) = fixture.recording("camera")?;
        let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
        let handle = worker.handle();
        wait_available(&handle)?;
        let job = handle.enqueue(&recording, "recording", &ObjectId::random().to_string())?;
        wait_state(&handle, &job.id, "ready")?;
        let lease = if protected {
            fixture
                .catalog
                .handle()
                .set_recording_protected(&recording, true)?;
            None
        } else {
            Some(
                fixture
                    .catalog
                    .handle()
                    .lease_archive_source(&recording, &path.to_string_lossy())?,
            )
        };
        handle.evict(&job.id)?;
        let blocked = wait_job(&handle, &job.id, |job| job.error.is_some())?;
        assert_eq!(blocked.state, "ready");
        assert_eq!(blocked.local_evicted, 0);
        assert!(path.exists());
        assert!(
            blocked
                .binding
                .root
                .join(format!("{}.mp4", blocked.object_id))
                .exists()
        );
        drop(lease);
        if protected {
            fixture
                .catalog
                .handle()
                .set_recording_protected(&recording, false)?;
        }
        handle.retry(&job.id)?;
        wait_local(&handle, &job.id, 1)?;
        assert!(!path.exists());
        worker.shutdown()?;
        drop(handle);
        fixture.close()?;
    }
    Ok(())
}

#[test]
fn archive_worker_recovers_primary_retirement_after_crash_before_catalog_commit()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (recording, path) = fixture.recording("camera")?;
    let original = std::fs::read(&path)?;
    let before = indexes(&fragments(&fixture.catalog.handle())?);
    let context = fixture.context();
    context.bind()?;
    let job = admit(
        &fixture.settings,
        &context.catalog,
        &recording,
        "recording",
        &ObjectId::random().to_string(),
    )?;
    context.process(job.clone(), Instant::now() + Duration::from_secs(30))?;
    context.catalog.archive(Request::Action {
        id: job.id.clone(),
        action: "evict".into(),
    })?;
    context.catalog.archive(Request::StartAction {
        id: job.id.clone(),
        maximum_attempts: 3,
    })?;
    let operation = ObjectId::random().to_string();
    let Reply::Eviction(intent) = context.catalog.archive(Request::BeginEviction {
        recording_id: recording.clone(),
        operation: operation.clone(),
    })?
    else {
        panic!("eviction intent");
    };
    let ready = context.catalog.archive_job(&job.id)?;
    let root = Root::open(&intent.source_root)?;
    root.retire_owned(
        &intent.source_relative_key,
        &intent.source_identity,
        intent.source_bytes,
        ready.digest,
        &operation,
    )?;
    assert!(!path.exists());
    assert_eq!(context.catalog.archive_job(&job.id)?.local_evicted, 2);
    drop(root);
    drop(context);
    let fixture = fixture.reopen()?;
    let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
    let handle = worker.handle();
    wait_local(&handle, &job.id, 1)?;
    check_archive_playback(&fixture.catalog.handle(), &original, &before)?;
    assert_eq!(handle.get(&job.id)?.recording_id, recording);
    worker.shutdown()?;
    drop(handle);
    fixture.close()
}

#[test]
fn archive_worker_completed_restore_cannot_be_cancelled_as_pending_cleanup() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (recording, path) = fixture.recording("camera")?;
    let original = std::fs::read(&path)?;
    let context = fixture.context();
    context.bind()?;
    let job = admit(
        &fixture.settings,
        &context.catalog,
        &recording,
        "recording",
        &ObjectId::random().to_string(),
    )?;
    context.process(job.clone(), work().deadline)?;
    context.catalog.archive(Request::Action {
        id: job.id.clone(),
        action: "evict".into(),
    })?;
    context.process(context.catalog.archive_job(&job.id)?, work().deadline)?;
    assert!(!path.exists());
    context.catalog.archive(Request::Action {
        id: job.id.clone(),
        action: "restore".into(),
    })?;
    let Reply::Job(Some(started)) = context.catalog.archive(Request::StartAction {
        id: job.id.clone(),
        maximum_attempts: 3,
    })?
    else {
        panic!("restore action");
    };
    crate::storage::archive::operations::execute(&context.catalog, &started, &work())?;
    assert_eq!(context.catalog.archive_job(&job.id)?.local_evicted, 0);
    assert!(
        context
            .catalog
            .archive(Request::Cancel(job.id.clone()))
            .is_err()
    );
    let completed = context.catalog.archive_job(&job.id)?;
    assert!(!completed.cancelled);
    assert_eq!(std::fs::read(path)?, original);
    assert!(
        completed
            .binding
            .root
            .join(format!("{}.mp4", completed.object_id))
            .is_file()
    );
    context
        .catalog
        .archive(Request::ActionComplete { id: job.id })?;
    drop(context);
    fixture.close()
}

fn interrupt_restore(context: &Context, job: &Job) -> anyhow::Result<PathBuf> {
    context.catalog.archive(Request::StartAction {
        id: job.id.clone(),
        maximum_attempts: 3,
    })?;
    let root = Root::open(&job.source_root)?;
    let capacity = root.capacity(context.catalog.volume_ledger_revision()?)?;
    let Reply::Job(Some(intent)) = context.catalog.archive(Request::BeginRestore {
        id: job.id.clone(),
        capacity,
    })?
    else {
        panic!("restore intent");
    };
    let archive = fragments(&context.catalog)?[0]
        .archive
        .clone()
        .expect("archive-backed source");
    let _lease = context
        .catalog
        .lease_archive_source(&job.recording_id, &archive.path().to_string_lossy())?;
    let mut reader = VerifiedReader::new(archive, work())?;
    let target = FilesystemArchiveTarget::open(&job.source_root, &job.source_root_identity)?;
    let object = ObjectId::parse(job.source_relative_key.strip_suffix(".mp4").expect("MP4"))?;
    let result = target.publish(
        object,
        &mut reader,
        job.bytes,
        intent.restore_stage.as_ref(),
        &mut |version| {
            context.catalog.archive(Request::RestoreStage {
                id: job.id.clone(),
                version: version.clone(),
            })?;
            anyhow::ensure!(version.bytes == 0, "stop before restore publication");
            Ok(())
        },
        &work(),
    );
    assert!(result.is_err());
    let staged = job.source_root.join(format!("{object}.tmp"));
    assert_eq!(std::fs::metadata(&staged)?.len(), job.bytes);
    assert!(!job.source_root.join(&job.source_relative_key).exists());
    Ok(staged)
}

#[test]
fn archive_worker_cancelled_restore_cleans_stage_retains_archive_and_can_restore_again()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (recording, path) = fixture.recording("camera")?;
    let original = std::fs::read(&path)?;
    let before = indexes(&fragments(&fixture.catalog.handle())?);
    let context = fixture.context();
    context.bind()?;
    let job = admit(
        &fixture.settings,
        &context.catalog,
        &recording,
        "recording",
        &ObjectId::random().to_string(),
    )?;
    context.process(job.clone(), work().deadline)?;
    context.catalog.archive(Request::Action {
        id: job.id.clone(),
        action: "evict".into(),
    })?;
    context.process(context.catalog.archive_job(&job.id)?, work().deadline)?;
    context.catalog.archive(Request::Action {
        id: job.id.clone(),
        action: "restore".into(),
    })?;
    let staged = interrupt_restore(&context, &context.catalog.archive_job(&job.id)?)?;
    context.catalog.archive(Request::Cancel(job.id.clone()))?;
    context.process(context.catalog.archive_job(&job.id)?, work().deadline)?;
    let cleaned = context.catalog.archive_job(&job.id)?;
    assert_eq!(cleaned.state, "ready");
    assert_eq!(cleaned.local_evicted, 1);
    assert!(!cleaned.cancelled);
    assert!(cleaned.action.is_empty());
    assert!(cleaned.restore_stage.is_none());
    assert!(!staged.exists());
    assert!(!path.exists());
    check_archive_playback(&context.catalog, &original, &before)?;
    context.catalog.archive(Request::Action {
        id: job.id.clone(),
        action: "restore".into(),
    })?;
    context.process(context.catalog.archive_job(&job.id)?, work().deadline)?;
    let restored = context.catalog.archive_job(&job.id)?;
    assert_eq!(restored.local_evicted, 0);
    assert_eq!(restored.recording_id, recording);
    assert_eq!(std::fs::read(&path)?, original);
    assert_eq!(indexes(&fragments(&context.catalog)?), before);
    drop(context);
    fixture.close()
}
