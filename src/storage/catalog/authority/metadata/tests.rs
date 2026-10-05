use super::*;

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "keeppeek-metadata-recovery-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&root).unwrap();
    crate::storage::RecordingCatalog::open(&root.join("source.db"))
        .unwrap()
        .shutdown();
    std::fs::write(root.join("history.json"), b"{\"version\":1,\"jobs\":[]}\n").unwrap();
    root
}

fn resume(root: &Path) -> anyhow::Result<()> {
    transfer(
        &root.join("source.db"),
        &root.join("destination.db"),
        &root.join("history.json"),
        &root.join("copied-history.json"),
    )
}

fn stage_partial_history(root: &Path) {
    let mut source = Lease::acquire(&root.join("source.db")).unwrap();
    let connection = source.connect().unwrap();
    let destination = Lease::acquire(&root.join("destination.db")).unwrap();
    let history = Lease::acquire(&root.join("history.json")).unwrap();
    let mut target = Lease::acquire(&root.join("copied-history.json")).unwrap();
    let owner = required(&connection).unwrap();
    let receipt = prepare(
        &connection,
        &[&source, &destination, &history, &target],
        &owner,
        None,
    )
    .unwrap();
    source
        .fence(
            &connection,
            owner.authority.generation,
            &receipt.intent.handoff,
            &destination,
        )
        .unwrap();
    register_history(&connection, &mut target).unwrap();
    let bytes = source_bytes(&history, &receipt.intent).unwrap();
    let file = target.file.as_mut().unwrap();
    file.write_all(&bytes[..7]).unwrap();
    file.sync_all().unwrap();
    crate::backup::database::checkpoint(&connection).unwrap();
    drop(connection);
}

#[test]
fn metadata_transfer_resumes_registered_partial_history_after_restart() {
    let root = fixture();
    let expected = std::fs::read(root.join("history.json")).unwrap();
    stage_partial_history(&root);
    assert_eq!(
        std::fs::read(root.join("copied-history.json")).unwrap(),
        expected[..7]
    );
    assert!(crate::storage::RecordingCatalog::open(&root.join("source.db")).is_err());
    resume(&root).unwrap();
    assert_eq!(
        std::fs::read(root.join("copied-history.json")).unwrap(),
        expected
    );
    assert_eq!(std::fs::read(root.join("history.json")).unwrap(), expected);
    assert!(root.join("source.db").is_file());
    assert!(crate::storage::RecordingCatalog::open(&root.join("source.db")).is_err());
    crate::storage::RecordingCatalog::open(&root.join("destination.db"))
        .unwrap()
        .shutdown();
    resume(&root).unwrap();
    assert_eq!(
        std::fs::read(root.join("copied-history.json")).unwrap(),
        expected
    );
}

#[test]
fn metadata_transfer_activates_completed_snapshot_after_restart_and_retries() {
    let root = fixture();
    let expected = std::fs::read(root.join("history.json")).unwrap();
    {
        let mut source = Lease::acquire(&root.join("source.db")).unwrap();
        let connection = source.connect().unwrap();
        let mut destination = Lease::acquire(&root.join("destination.db")).unwrap();
        let history = Lease::acquire(&root.join("history.json")).unwrap();
        let mut target = Lease::acquire(&root.join("copied-history.json")).unwrap();
        let owner = required(&connection).unwrap();
        let receipt = prepare(
            &connection,
            &[&source, &destination, &history, &target],
            &owner,
            None,
        )
        .unwrap();
        source
            .fence(
                &connection,
                owner.authority.generation,
                &receipt.intent.handoff,
                &destination,
            )
            .unwrap();
        copy_history(&connection, &mut target, &receipt, &expected).unwrap();
        let limit = crate::backup::database::snapshot_size_limit(&connection).unwrap();
        source
            .snapshot_into(&connection, &mut destination, limit)
            .unwrap();
        drop(connection);
    }
    assert!(crate::storage::RecordingCatalog::open(&root.join("source.db")).is_err());
    assert!(crate::storage::RecordingCatalog::open(&root.join("destination.db")).is_err());
    resume(&root).unwrap();
    let mut lease = Lease::acquire(&root.join("destination.db")).unwrap();
    let connection = lease.connect().unwrap();
    let activated = lease.verify(&connection).unwrap();
    drop(connection);
    drop(lease);
    resume(&root).unwrap();
    let mut lease = Lease::acquire(&root.join("destination.db")).unwrap();
    let connection = lease.connect().unwrap();
    assert_eq!(lease.verify(&connection).unwrap(), activated);
    assert_eq!(
        std::fs::read(root.join("copied-history.json")).unwrap(),
        expected
    );
    assert_eq!(std::fs::read(root.join("history.json")).unwrap(), expected);
    assert!(crate::storage::RecordingCatalog::open(&root.join("source.db")).is_err());
    drop(connection);
}

#[test]
fn metadata_transfer_rejects_replaced_registered_partial_history_without_modifying_it() {
    let root = fixture();
    let expected = std::fs::read(root.join("history.json")).unwrap();
    stage_partial_history(&root);
    let target = root.join("copied-history.json");
    let retained = root.join("retained-partial.json");
    std::fs::rename(&target, &retained).unwrap();
    let unrelated = b"unrelated replacement must survive";
    std::fs::write(&target, unrelated).unwrap();
    assert!(resume(&root).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), unrelated);
    assert_eq!(std::fs::read(&retained).unwrap(), expected[..7]);
    assert_eq!(std::fs::read(root.join("history.json")).unwrap(), expected);
    assert!(!root.join("destination.db").exists());
    assert!(root.join("source.db").is_file());
    assert!(crate::storage::RecordingCatalog::open(&root.join("source.db")).is_err());
}
