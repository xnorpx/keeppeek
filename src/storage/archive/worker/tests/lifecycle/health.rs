use super::*;

fn finish_eviction_before_acknowledgement(fixture: &Fixture) -> anyhow::Result<Job> {
    let (recording, path) = fixture.recording("camera")?;
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
    root.retire_owned_until(
        &intent.source_relative_key,
        &intent.source_identity,
        intent.source_bytes,
        ready.digest,
        &operation,
        work().deadline,
    )?;
    context.catalog.archive(Request::FinishEviction {
        recording_id: recording,
        operation,
    })?;
    assert!(!path.exists());
    context.catalog.archive_job(&job.id)
}

#[test]
fn archive_eviction_receipt_replay_preserves_unverified_or_failed_health() -> anyhow::Result<()> {
    for offline in [false, true] {
        let fixture = Fixture::new()?;
        let finished = finish_eviction_before_acknowledgement(&fixture)?;
        let operation = finished
            .source_retirement_operation
            .as_ref()
            .expect("retirement");
        let receipt = finished.source_root.join(".retired").join(operation);
        assert!(receipt.join("staged.json").is_file());
        assert_eq!(finished.local_evicted, 1);
        assert_eq!(finished.action, "evict");
        let fixture = fixture.reopen()?;
        let retained = if offline {
            let retained = fixture.path.join("offline-archive");
            std::fs::rename(&fixture.settings.root, &retained)?;
            retained
        } else {
            fixture.settings.root.clone()
        };
        let context = fixture.context();
        let mut scan = Scan::new();
        assert!(scan.turn(&context)?);
        assert_eq!(
            scan.pending.front().expect("pending eviction").id,
            finished.id
        );
        let before = context.shared.health.lock().expect("health").clone();
        assert!(!before.available);
        assert_eq!(before.message.is_some(), offline);
        assert!(scan.turn(&context)?);
        let after = context.shared.health.lock().expect("health").clone();
        assert!(!after.available);
        assert_eq!(after.message, before.message);
        let completed = context.catalog.archive_job(&finished.id)?;
        assert_eq!(completed.local_evicted, 1);
        assert!(completed.action.is_empty());
        assert_eq!(completed.action_attempts, 0);
        assert!(completed.error.is_none());
        assert!(!receipt.exists());
        assert!(!receipt.with_file_name(format!("{operation}.ack")).exists());
        assert!(
            !finished
                .source_root
                .join(&finished.source_relative_key)
                .exists()
        );
        let archived = retained.join(format!("{}.mp4", finished.object_id));
        assert_eq!(std::fs::metadata(archived)?.len(), finished.bytes);
        assert_eq!(fixture.settings.root.exists(), !offline);
        drop(context);
        fixture.close()?;
    }
    Ok(())
}
