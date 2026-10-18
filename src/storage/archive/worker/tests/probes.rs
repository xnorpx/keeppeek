use super::*;

mod health;

fn queued_probe(context: &Context) -> anyhow::Result<Job> {
    context.bind()?;
    admit(
        &context.settings,
        &context.catalog,
        "probe:local-archive",
        "probe",
        &ObjectId::random().to_string(),
    )
}

fn published_probe(context: &Context, job: &Job) -> anyhow::Result<Job> {
    context.catalog.archive(Request::Start {
        id: job.id.clone(),
        maximum_attempts: 3,
    })?;
    let root = Root::open(&job.binding.root)?;
    context.catalog.archive(Request::ReserveTransfer {
        id: job.id.clone(),
        capacity: root.capacity(context.catalog.volume_ledger_revision()?)?,
    })?;
    let target = FilesystemArchiveTarget::open(&job.binding.root, &job.binding.identity)?;
    target.publish(
        ObjectId::parse(&job.object_id)?,
        &mut Cursor::new(super::super::super::PROBE_PAYLOAD),
        job.bytes,
        None,
        &mut |version| {
            context.catalog.archive(Request::Stage {
                id: job.id.clone(),
                version: version.clone(),
            })?;
            Ok(())
        },
        &lifecycle::work(),
    )?;
    context.catalog.archive_job(&job.id)
}

fn ready_probe(context: &Context, job: &Job) -> anyhow::Result<Job> {
    let published = published_probe(context, job)?;
    let target = FilesystemArchiveTarget::open(&job.binding.root, &job.binding.identity)?;
    let mut sink = transfer::VerificationSink::new(job.bytes)?;
    let version = published.stage.expect("published probe evidence");
    target.read(&version, &mut sink, &lifecycle::work())?;
    let verified = sink.finish()?;
    assert_eq!(verified.digest, job.digest);
    context.catalog.archive(Request::Ready {
        id: job.id.clone(),
        version,
        digest: verified.digest,
        chunk_hashes: verified.chunk_hashes,
    })?;
    context.catalog.archive_job(&job.id)
}

fn only_probe(context: &Context) -> anyhow::Result<Job> {
    let Reply::Jobs(mut jobs) = context.catalog.archive(Request::List {
        after: String::new(),
        limit: PAGE_SIZE,
    })?
    else {
        panic!("probe list");
    };
    assert_eq!(jobs.len(), 1);
    let job = jobs.remove(0);
    assert_eq!(job.kind, "probe");
    Ok(job)
}

#[test]
fn archive_probe_replays_stale_queued_and_ready_snapshots_after_acknowledgement()
-> anyhow::Result<()> {
    for ready in [false, true] {
        let fixture = Fixture::new()?;
        let context = fixture.context();
        let queued = queued_probe(&context)?;
        let stale = if ready {
            ready_probe(&context, &queued)?
        } else {
            queued
        };
        let mut scan = Scan::new();
        scan.turn(&context)?;
        assert_eq!(scan.pending.front().expect("queued work").id, stale.id);
        context.probe(Instant::now() + Duration::from_secs(2))?;
        assert!(context.catalog.archive_job(&stale.id)?.probe_acknowledged);
        scan.turn(&context)?;
        assert!(context.shared.health.lock().expect("health").available);
        context.probe(Instant::now() + Duration::from_secs(2))?;
        let next = only_probe(&context)?;
        assert_ne!(next.id, stale.id);
        assert_eq!(next.state, "deleted");
        assert!(next.probe_acknowledged);
        assert!(matches!(
            context.catalog.archive(Request::Get(stale.id.clone()))?,
            Reply::Job(None)
        ));
        context.process(stale, lifecycle::work().deadline)?;
        assert!(context.shared.health.lock().expect("health").available);
        drop(context);
        fixture.close()?;
    }
    Ok(())
}

#[test]
fn archive_probe_replaces_cleaned_cancelled_probe_with_a_new_job() -> anyhow::Result<()> {
    for published in [false, true] {
        let fixture = Fixture::new()?;
        let context = fixture.context();
        context.probe(Instant::now() + Duration::from_secs(2))?;
        let queued = queued_probe(&context)?;
        if published {
            published_probe(&context, &queued)?;
        }
        let mut scan = Scan::new();
        scan.turn(&context)?;
        assert_eq!(scan.pending.len(), 1);
        context
            .catalog
            .archive(Request::Cancel(queued.id.clone()))?;
        scan.turn(&context)?;
        let healthy_after_stale_cancel = context.shared.health.lock().expect("health").available;
        assert!(
            !context
                .catalog
                .archive_job(&queued.id)?
                .cleanup_acknowledged
        );
        context.process(
            context.catalog.archive_job(&queued.id)?,
            lifecycle::work().deadline,
        )?;
        assert!(
            context
                .catalog
                .archive_job(&queued.id)?
                .cleanup_acknowledged
        );
        context.probe(Instant::now() + Duration::from_secs(2))?;
        let next = only_probe(&context)?;
        assert_ne!(next.id, queued.id);
        assert!(healthy_after_stale_cancel);
        assert_eq!(next.state, "deleted");
        assert!(next.probe_acknowledged);
        assert!(matches!(
            context.catalog.archive(Request::Get(queued.id))?,
            Reply::Job(None)
        ));
        drop(context);
        fixture.close()?;
    }
    Ok(())
}

#[test]
fn archive_probe_acknowledgement_does_not_bypass_target_identity_validation() -> anyhow::Result<()>
{
    let fixture = Fixture::new()?;
    let context = fixture.context();
    let stale = queued_probe(&context)?;
    context.probe(Instant::now() + Duration::from_secs(2))?;
    assert!(context.catalog.archive_job(&stale.id)?.probe_acknowledged);
    std::fs::rename(&fixture.settings.root, fixture.path.join("offline-archive"))?;
    let root = Root::open(&fixture.path)?;
    drop(root.create_private_child("archive")?);
    assert!(context.process(stale, lifecycle::work().deadline).is_err());
    assert!(!context.shared.health.lock().expect("health").available);
    assert_eq!(std::fs::read_dir(&fixture.settings.root)?.count(), 0);
    drop(root);
    drop(context);
    fixture.close()
}
