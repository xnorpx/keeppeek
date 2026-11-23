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

fn retire_fixture_copy(context: &Context, job: &Job) -> anyhow::Result<bool> {
    context.health(true, None);
    let pending = context.catalog.archive_job(&job.id)?;
    let result = context.process(pending, Instant::now() + Duration::from_secs(15));
    if cfg!(windows) {
        result?;
        // The initial verification only claims the job. Restart the turn after each
        // durable filesystem checkpoint and observe at most two full hashes per turn.
        for expected_complete in [false, false, false, true] {
            let pending = context.catalog.archive_job(&job.id)?;
            assert_eq!(pending.deletion.as_ref().unwrap().phase, "claimed");
            context.process(pending, Instant::now() + Duration::from_secs(15))?;
            let current = context.catalog.archive_job(&job.id)?;
            assert_eq!(
                current.deletion.unwrap().phase == "acknowledged",
                expected_complete
            );
        }
        return Ok(true);
    }
    // These freshly written test files have no filesystem-enforced Unix write protection.
    let error = result.expect_err("mutable fixture primary must retain its archive");
    assert!(
        error.is::<crate::storage::volumes::root::ArchiveSurvivorUnprotected>(),
        "{error:#}"
    );
    let health = context.shared.health.lock().unwrap();
    assert!(health.available && health.message.is_none());
    drop(health);
    let retained = context.catalog.archive_job(&job.id)?;
    assert_eq!(retained.state, "ready");
    assert_eq!(retained.deletion.unwrap().phase, "blocked");
    assert_eq!(
        retained.error.as_deref(),
        Some("archive_primary_write_protection_required")
    );
    assert_eq!(
        std::fs::read(&job.source_path)?,
        std::fs::read(job.binding.root.join(format!("{}.mp4", job.object_id)))?
    );
    Ok(false)
}

#[test]
fn archive_retention_worker_reverifies_legacy_media_before_assigning_an_age() -> anyhow::Result<()>
{
    let fixture = Fixture::new()?;
    let mut jobs = Vec::new();
    for camera in ["camera-a", "camera-b"] {
        let (id, primary) = fixture.recording(camera)?;
        jobs.push((copy(&fixture, &id)?, primary));
    }
    jobs.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    let [(damaged, primary), (healthy, _)] = jobs.as_slice() else {
        unreachable!()
    };
    let target = damaged
        .binding
        .root
        .join(format!("{}.mp4", damaged.object_id));
    let original = std::fs::read(primary)?;
    let mut corrupt = original.clone();
    corrupt[0] ^= 1;
    std::fs::write(&target, &corrupt)?;
    let mut fixture = fixture.reopen_with(|path| {
        pollster::block_on(async {
            let database = turso::Builder::new_local(path.to_str().unwrap())
                .experimental_generated_columns(true)
                .build()
                .await?;
            let changed = database
                .connect()?
                .execute("DELETE FROM archive_verified_at", ())
                .await?;
            assert_eq!(changed, 2);
            Ok(())
        })
    })?;
    fixture.settings.retention_days = Some(1);
    let selection = || Selection::Retention {
        after: String::new(),
        limit: 16,
    };
    let before = preview(&fixture, selection())?;
    assert_eq!(before.items.len(), 2);
    assert!(
        before
            .items
            .iter()
            .all(|item| item.state == "verification_date_unknown")
    );

    let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
    let handle = worker.handle();
    let failed = super::lifecycle::wait_job(&handle, &damaged.id, Job::reverification_failed)?;
    assert!(failed.verified_ms.is_none());
    // An unread, higher-ID copy must finish before the ordinary periodic scan, even
    // though the lower-ID copy failed its digest on exactly this bound target.
    let ready = super::lifecycle::wait_job(&handle, &healthy.id, |job| job.verified_ms.is_some())?;
    worker.shutdown()?;
    assert!(ready.verified_ms.is_some_and(|ms| ms > 0));
    let after = preview(&fixture, selection())?;
    assert_eq!(after.items[0].state, "verification_date_unknown");
    assert_eq!(after.items[1].state, "retained");
    assert_eq!(std::fs::read(&target)?, corrupt);

    // Make the failed lower-ID job due again alongside an unread copy. An absent
    // retry row identifies new work and must sort ahead of the due failed job.
    let mut fixture = fixture.reopen_with(|path| {
        pollster::block_on(async {
            let database = turso::Builder::new_local(path.to_str().unwrap())
                .experimental_generated_columns(true)
                .build()
                .await?;
            let connection = database.connect()?;
            connection
                .execute(
                    "UPDATE archive_reverification_retries SET retry_after_ms=0 WHERE job_id=?1",
                    [damaged.id.as_str()],
                )
                .await?;
            connection
                .execute(
                    "DELETE FROM archive_verified_at WHERE job_id=?1",
                    [healthy.id.as_str()],
                )
                .await?;
            Ok(())
        })
    })?;
    let Reply::Job(Some(next)) =
        fixture
            .catalog
            .handle()
            .archive(Request::Deletion(Deletion::NextLegacy {
                target: fixture.settings.target_id.clone(),
                maximum_attempts: 2,
                include_unrequested: true,
            }))?
    else {
        anyhow::bail!("missing legacy verification candidate")
    };
    assert_eq!(next.id, healthy.id);

    // A durable explicit retry survives restart after switching to indefinite retention.
    // Corruption remains visible, and unrequested copies on that policy stay untouched.
    fixture
        .catalog
        .handle()
        .archive(Request::Retry(damaged.id.clone()))?;
    fixture.settings.retention_days = None;
    let fixture = fixture.reopen()?;
    let worker = Worker::start(fixture.settings.clone(), fixture.catalog.handle())?;
    let failed =
        super::lifecycle::wait_job(&worker.handle(), &damaged.id, Job::reverification_failed)?;
    assert!(failed.verified_ms.is_none());
    assert!(worker.handle().get(&healthy.id)?.verified_ms.is_none());
    std::fs::write(&target, &original)?;
    worker.handle().retry(&damaged.id)?;
    let repaired = super::lifecycle::wait_job(&worker.handle(), &damaged.id, |job| {
        job.verified_ms.is_some()
    })?;
    worker.shutdown()?;
    assert!(repaired.error.is_none() && repaired.deletion.is_none());
    assert_eq!(std::fs::read(target)?, original);
    assert_eq!(std::fs::read(primary)?, original);
    fixture.close()
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
    if retire_fixture_copy(&context, &job)? {
        let deleted = context.catalog.archive_job(&job.id)?;
        assert_eq!(deleted.state, "deleted");
        assert_eq!(deleted.deletion.unwrap().phase, "acknowledged");
        assert_eq!(
            confirm(&fixture, &plan, "admin")?.items[0].state,
            "acknowledged"
        );
        assert!(!target.exists());
    }
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
fn archive_deletion_declines_a_damaged_target_before_claiming_its_recording() -> anyhow::Result<()>
{
    let fixture = Fixture::new()?;
    let (id, path) = fixture.recording("camera")?;
    let original = std::fs::read(&path)?;
    let job = copy(&fixture, &id)?;
    let target = job.binding.root.join(format!("{}.mp4", job.object_id));
    let plan = preview(&fixture, Selection::Explicit(job.id.clone()))?;
    confirm(&fixture, &plan, "admin")?;
    let mut damaged = original.clone();
    damaged[0] ^= 1;
    std::fs::write(&target, &damaged)?;
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
    assert_eq!(std::fs::read(target)?, damaged);
    assert_eq!(std::fs::read(path)?, original);
    context
        .catalog
        .archive(Request::Deletion(Deletion::EnsureNoActiveClaim(
            job.binding.id,
        )))?;
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
            assert!(blocked.id.is_empty());
            assert!(blocked.revision.is_empty());
            assert!(blocked.nonce.is_none());
            assert_eq!(blocked.expires_ms, 0);
        }
    }
    assert_eq!(std::fs::read(&path)?, original);
    let restored = preview(&fixture, selection)?;
    assert_eq!(restored.items[0].state, "eligible");
    confirm(&fixture, &restored, "admin")?;
    if retire_fixture_copy(&context, &job)? {
        assert_eq!(context.catalog.archive_job(&job.id)?.state, "deleted");
    }
    assert_eq!(std::fs::read(path)?, original);
    drop(context);
    fixture.close()
}

#[cfg(unix)]
#[test]
fn archive_deletion_retains_bytes_when_an_existing_writer_can_mutate_the_primary()
-> anyhow::Result<()> {
    use std::io::{Seek, Write};
    let fixture = Fixture::new()?;
    let (id, path) = fixture.recording("camera")?;
    let job = copy(&fixture, &id)?;
    let original = std::fs::read(&path)?;
    let mut writer = std::fs::OpenOptions::new().write(true).open(&path)?;
    let plan = preview(&fixture, Selection::Explicit(job.id.clone()))?;
    confirm(&fixture, &plan, "admin")?;
    let context = fixture.context();
    assert!(!retire_fixture_copy(&context, &job)?);
    writer.rewind()?;
    writer.write_all(&[original[0] ^ 1])?;
    writer.sync_all()?;
    assert_eq!(std::fs::metadata(&path)?.len(), original.len() as u64);
    assert_eq!(
        std::fs::read(job.binding.root.join(format!("{}.mp4", job.object_id)))?,
        original
    );
    drop(writer);
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
    // The acknowledgement committed even if its caller observed a timeout.
    fixture
        .catalog
        .handle()
        .archive(Request::Deletion(Deletion::Failed(job.id.clone())))?;
    let replay = handle.get(&job.id)?;
    assert!(replay.error.is_none());
    assert_eq!(replay.deletion.unwrap().phase, "acknowledged");
    assert_eq!(std::fs::read(path)?, original);
    drop(handle);
    worker.shutdown()?;
    fixture.close()
}

#[test]
fn archive_deletion_claim_observes_saved_retention_changed_after_confirmation() -> anyhow::Result<()>
{
    let mut fixture = Fixture::new()?;
    fixture.settings.retention_days = Some(1);
    let (id, primary) = fixture.recording("camera")?;
    let job = copy(&fixture, &id)?;
    let plan = preview(&fixture, Selection::Explicit(job.id.clone()))?;
    confirm(&fixture, &plan, "admin")?;
    let config_path = fixture.path.join("updated-config.toml");
    let mut saved = fixture.settings.clone();
    saved.retention_days = None;
    std::fs::write(
        &config_path,
        format!("[storage.archive]\n{}", toml::to_string(&saved)?),
    )?;
    let mut ctx = fixture.context();
    ctx.saved_configuration = Some((config_path, Arc::new(Mutex::new(()))));
    let queued = ctx.catalog.archive_job(&job.id)?;
    let result = ctx.claim_deletion(&queued);
    assert!(result.unwrap_err().to_string().contains("policy changed"));
    assert_eq!(
        ctx.catalog.archive_job(&job.id)?.deletion.unwrap().phase,
        "queued"
    );
    assert!(
        ctx.process(queued, Instant::now() + Duration::from_secs(15))
            .is_err()
    );
    assert_eq!(
        ctx.catalog.archive_job(&job.id)?.deletion.unwrap().phase,
        "blocked"
    );
    assert_eq!(
        std::fs::read(job.binding.root.join(format!("{}.mp4", job.object_id)))?,
        std::fs::read(primary)?
    );
    drop(ctx);
    fixture.close()
}
