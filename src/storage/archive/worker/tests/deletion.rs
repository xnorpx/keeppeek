use super::*;
use crate::storage::catalog::archive::deletion::{Plan, Policy, Request as Deletion, Selection};

fn command(fixture: &Fixture, request: Deletion) -> anyhow::Result<Plan> {
    match fixture
        .catalog
        .handle()
        .archive(Request::Deletion(request))?
    {
        Reply::Deletion(plan) => Ok(*plan),
        _ => anyhow::bail!("unexpected archive deletion reply"),
    }
}

fn preview(fixture: &Fixture, selection: Selection) -> anyhow::Result<Plan> {
    command(
        fixture,
        Deletion::Preview {
            actor: "admin".into(),
            policy: Policy::from_settings(&fixture.settings)?,
            selection,
        },
    )
}

fn confirm(fixture: &Fixture, preview: &Plan, actor: &str) -> anyhow::Result<Plan> {
    command(
        fixture,
        Deletion::Confirm {
            actor: actor.into(),
            policy: Policy::from_settings(&fixture.settings)?,
            plan: preview.id.clone(),
            revision: preview.revision.clone(),
            nonce: preview.nonce.clone().unwrap(),
        },
    )
}

fn copy(fixture: &Fixture, recording: &str) -> anyhow::Result<Job> {
    let context = fixture.context();
    context.bind()?;
    let job = admit(
        &fixture.settings,
        &context.catalog,
        recording,
        "recording",
        &ObjectId::random().to_string(),
    )?;
    context.process(job.clone(), Instant::now() + Duration::from_secs(15))?;
    context.catalog.archive_job(&job.id)
}

#[test]
fn archive_retention_confirmation_covers_only_the_displayed_page() -> anyhow::Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.settings.retention_days = Some(1);
    let mut jobs = Vec::new();
    for camera in ["camera-a", "camera-b"] {
        let (id, _) = fixture.recording(camera)?;
        jobs.push(copy(&fixture, &id)?.id);
    }
    jobs.sort();
    let fixture = fixture.reopen_with(|path| {
        pollster::block_on(async {
            let database = turso::Builder::new_local(path.to_str().unwrap())
                .experimental_generated_columns(true)
                .build()
                .await?;
            database
                .connect()?
                .execute("UPDATE archive_verified_at SET verified_ms=1", ())
                .await?;
            Ok(())
        })
    })?;
    let first = preview(
        &fixture,
        Selection::Retention {
            after: String::new(),
            limit: 1,
        },
    )?;
    assert_eq!(first.items.len(), 1);
    assert_eq!(first.items[0].job_id, jobs[0]);
    assert_eq!(first.items[0].state, "eligible");
    assert_eq!(first.next, jobs[0]);
    assert_eq!(confirm(&fixture, &first, "admin")?.items[0].state, "queued");
    let second = preview(
        &fixture,
        Selection::Retention {
            after: first.next,
            limit: 1,
        },
    )?;
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].job_id, jobs[1]);
    assert_eq!(second.items[0].state, "eligible");
    assert!(second.next.is_empty());
    assert!(
        fixture
            .catalog
            .handle()
            .archive_job(&jobs[1])?
            .deletion
            .is_none()
    );
    assert_eq!(
        confirm(&fixture, &second, "admin")?.items[0].state,
        "queued"
    );
    fixture.close()
}

#[test]
fn archive_deletion_requires_same_administrator_rechecks_readers_and_preserves_primary()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (id, path) = fixture.recording("camera")?;
    let data = std::fs::read(&path)?;
    let job = copy(&fixture, &id)?;
    let target = job.binding.root.join(format!("{}.mp4", job.object_id));
    let unrelated = job.binding.root.join("unrelated.mp4");
    std::fs::write(&unrelated, b"unrelated")?;
    let selection = Selection::Explicit(job.id.clone());
    let plan = preview(&fixture, selection.clone())?;
    assert_eq!(plan.items[0].state, "eligible");
    assert!(plan.items[0].verified_ms.is_some());
    assert!(confirm(&fixture, &plan, "other-admin").is_err());
    let lease = fixture
        .catalog
        .handle()
        .lease_archive_source(&id, &path.to_string_lossy())?;
    assert_eq!(
        preview(&fixture, selection.clone())?.items[0].state,
        "active_reader"
    );
    assert!(confirm(&fixture, &plan, "admin").is_err());
    assert_eq!(std::fs::read(&target)?, data);
    drop(lease);
    let plan = preview(&fixture, selection)?;
    assert_eq!(confirm(&fixture, &plan, "admin")?.items[0].state, "queued");
    assert_eq!(confirm(&fixture, &plan, "admin")?.items[0].state, "queued");
    let context = fixture.context();
    let job = context.catalog.archive_job(&job.id)?;
    context.process(job.clone(), Instant::now() + Duration::from_secs(15))?;
    let deleted = context.catalog.archive_job(&job.id)?;
    assert_eq!(deleted.state, "deleted");
    assert_eq!(deleted.deletion.unwrap().phase, "acknowledged");
    assert_eq!(
        confirm(&fixture, &plan, "admin")?.items[0].state,
        "acknowledged"
    );
    assert!(!target.exists());
    assert_eq!(std::fs::read(&unrelated)?, b"unrelated");
    assert_eq!(std::fs::read(&path)?, data);
    drop(context);
    fixture.close()
}

#[test]
fn archive_deletion_retains_verified_replica_when_primary_bytes_change_after_confirmation()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (id, path) = fixture.recording("camera")?;
    let job = copy(&fixture, &id)?;
    let original = std::fs::read(&path)?;
    let target = job.binding.root.join(format!("{}.mp4", job.object_id));
    let selection = Selection::Explicit(job.id.clone());
    let plan = preview(&fixture, selection)?;
    confirm(&fixture, &plan, "admin")?;
    let mut changed = original.clone();
    changed[0] ^= 1;
    std::fs::write(&path, &changed)?;
    let context = fixture.context();
    let pending = context.catalog.archive_job(&job.id)?;
    assert!(
        context
            .process(pending, Instant::now() + Duration::from_secs(15))
            .is_err()
    );
    let retained = context.catalog.archive_job(&job.id)?;
    assert_eq!(retained.state, "ready");
    assert_eq!(retained.deletion.unwrap().phase, "blocked");
    assert_eq!(std::fs::read(target)?, original);
    assert_eq!(std::fs::read(path)?, changed);
    drop(context);
    fixture.close()
}

#[test]
fn archive_deletion_blocks_the_only_copy_until_successful_restore() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (id, path) = fixture.recording("camera")?;
    let job = copy(&fixture, &id)?;
    let original = std::fs::read(&path)?;
    let context = fixture.context();
    let selection = Selection::Explicit(job.id.clone());
    let plan = preview(&fixture, selection.clone())?;
    for action in ["evict", "restore"] {
        context.catalog.archive(Request::Action {
            id: job.id.clone(),
            action: action.into(),
        })?;
        let pending = context.catalog.archive_job(&job.id)?;
        context.process(pending, Instant::now() + Duration::from_secs(15))?;
        if action == "evict" {
            assert!(!path.exists());
            assert!(confirm(&fixture, &plan, "admin").is_err());
            let blocked = preview(&fixture, selection.clone())?;
            assert_eq!(blocked.items[0].state, "restore_required");
            assert!(confirm(&fixture, &blocked, "admin").is_err());
        }
    }
    assert_eq!(std::fs::read(&path)?, original);
    let restored = preview(&fixture, selection)?;
    assert_eq!(restored.items[0].state, "eligible");
    confirm(&fixture, &restored, "admin")?;
    context.process(
        context.catalog.archive_job(&job.id)?,
        Instant::now() + Duration::from_secs(15),
    )?;
    assert_eq!(context.catalog.archive_job(&job.id)?.state, "deleted");
    assert_eq!(std::fs::read(path)?, original);
    drop(context);
    fixture.close()
}

#[test]
fn archive_deletion_reconciles_a_lost_retirement_acknowledgement_after_restart()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (id, path) = fixture.recording("camera")?;
    let original = std::fs::read(&path)?;
    let job = copy(&fixture, &id)?;
    let plan = preview(&fixture, Selection::Explicit(job.id.clone()))?;
    confirm(&fixture, &plan, "admin")?;
    let catalog = fixture.catalog.handle();
    catalog.archive(Request::Deletion(Deletion::Claim {
        job: job.id.clone(),
        policy: Policy::from_settings(&fixture.settings)?,
    }))?;
    let pending = catalog.archive_job(&job.id)?;
    let operation = ObjectId::parse(&pending.deletion.unwrap().operation)?;
    let version = crate::storage::archive::target::Version::new(
        ObjectId::parse(&job.object_id)?,
        job.version.clone().unwrap(),
        job.bytes,
    )?;
    let target = FilesystemArchiveTarget::open(&job.binding.root, &job.binding.identity)?;
    target.delete(&version, job.digest, operation, &lifecycle::work())?;
    drop(target);
    drop(catalog);
    let fixture = fixture.reopen()?;
    let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
    let handle = worker.handle();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let archived = handle.get(&job.id)?;
        if archived
            .deletion
            .as_ref()
            .is_some_and(|d| d.phase == "acknowledged")
        {
            assert_eq!(archived.state, "deleted");
            break;
        }
        anyhow::ensure!(
            Instant::now() < deadline,
            "archive deletion receipt was not reconciled"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        confirm(&fixture, &plan, "admin")?.items[0].state,
        "acknowledged"
    );
    assert_eq!(std::fs::read(path)?, original);
    drop(handle);
    worker.shutdown()?;
    fixture.close()
}
