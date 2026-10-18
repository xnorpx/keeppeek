use super::*;

fn block_probe(job: &Job) -> anyhow::Result<PathBuf> {
    let root = Root::open(&job.binding.root)?;
    let key = format!("{}.tmp", job.object_id);
    let mut unknown = root.create_file(&key)?;
    unknown.file_mut().write_all(b"unknown fixture object")?;
    unknown.checkpoint()?;
    Ok(job.binding.root.join(key))
}

fn available(context: &Context) -> bool {
    context.shared.health.lock().expect("health").available
}

#[test]
fn archive_health_starts_unverified_until_a_probe_performs_real_io() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let context = fixture.context();
    assert!(!available(&context));
    context.bind()?;
    assert!(!available(&context));
    context.probe(Instant::now() + Duration::from_secs(2))?;
    assert!(available(&context));
    assert!(only_probe(&context)?.probe_acknowledged);
    drop(context);
    fixture.close()
}

#[test]
fn archive_health_failure_survives_binding_and_noop_work_until_real_probe_succeeds()
-> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let context = fixture.context();
    let completed = queued_probe(&context)?;
    context.probe(Instant::now() + Duration::from_secs(2))?;
    assert!(available(&context));
    let blocked = queued_probe(&context)?;
    let unknown = block_probe(&blocked)?;
    assert!(
        context
            .probe(Instant::now() + Duration::from_secs(2))
            .is_err()
    );
    assert!(!available(&context));
    assert_eq!(context.catalog.archive_job(&blocked.id)?.state, "failed");
    let mut scan = Scan::new();
    scan.turn(&context)?;
    assert!(!available(&context));
    context.process(completed, lifecycle::work().deadline)?;
    assert!(!available(&context));
    context
        .catalog
        .archive(Request::Cancel(blocked.id.clone()))?;
    context.process(blocked.clone(), lifecycle::work().deadline)?;
    assert!(!available(&context));
    assert!(
        !context
            .catalog
            .archive_job(&blocked.id)?
            .cleanup_acknowledged
    );
    assert_eq!(std::fs::read(&unknown)?, b"unknown fixture object");
    std::fs::remove_file(unknown)?;
    context.process(
        context.catalog.archive_job(&blocked.id)?,
        lifecycle::work().deadline,
    )?;
    assert!(
        context
            .catalog
            .archive_job(&blocked.id)?
            .cleanup_acknowledged
    );
    assert!(!available(&context));
    context.probe(Instant::now() + Duration::from_secs(2))?;
    let verified = only_probe(&context)?;
    assert_ne!(verified.id, blocked.id);
    assert!(verified.probe_acknowledged);
    assert!(available(&context));
    drop(context);
    fixture.close()
}
