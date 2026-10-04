use super::*;

fn fixture() -> PathBuf {
    let root = std::env::temp_dir().join(format!("keeppeek-authority-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    root
}

fn database(path: &Path) -> (Lease, turso::Connection, Authority) {
    let mut lease = Lease::acquire(path).unwrap();
    let connection = lease.connect().unwrap();
    let authority = lease.initialize(&connection).unwrap();
    (lease, connection, authority)
}

fn historical_builder_opens(path: &Path) -> anyhow::Result<()> {
    // A closed snapshot avoids both Turso's cache and its live writer lock.
    let snapshot = path.with_file_name(format!("historical-{}.db", uuid::Uuid::new_v4()));
    crate::backup::database::snapshot_turso_database_path(path, &snapshot, 64 * 1024 * 1024)
        .expect("compatibility fixture snapshot must succeed");
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "storage::catalog::authority::tests::historical_catalog_opener_process",
        ])
        .env("KEEPPEEK_TEST_HISTORICAL_CATALOG", &snapshot)
        .output()?;
    if !output.status.success() {
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            diagnostics.contains("generated_columns feature is not enabled"),
            "unexpected historical opener failure: {diagnostics}"
        );
    }
    anyhow::ensure!(
        output.status.success(),
        "historical catalog opener rejected the schema"
    );
    Ok(())
}

#[test]
fn historical_catalog_opener_process() -> anyhow::Result<()> {
    let Some(path) = std::env::var_os("KEEPPEEK_TEST_HISTORICAL_CATALOG") else {
        return Ok(());
    };
    pollster::block_on(async {
        let database = turso::Builder::new_local(Path::new(&path).to_str().unwrap())
            .build()
            .await?;
        let connection = database.connect()?;
        connection
            .query("SELECT name FROM sqlite_schema", ())
            .await?
            .next()
            .await?;
        anyhow::Ok(())
    })
}

#[test]
fn authority_format_gate_is_atomic_with_fence_and_rejects_historical_builders() {
    let root = fixture();
    let source_path = root.join("source.db");
    let target_path = root.join("target.db");
    let (source, connection, owner) = database(&source_path);
    pollster::block_on(connection.execute_batch(
        "CREATE TABLE payload(value TEXT); INSERT INTO payload VALUES ('retained');",
    ))
    .unwrap();
    assert!(historical_builder_opens(&source_path).is_ok());
    let mut target = Lease::acquire(&target_path).unwrap();
    let handoff = uuid::Uuid::new_v4().to_string();
    source
        .fence(&connection, owner.generation, &handoff, &target)
        .unwrap();
    assert!(historical_builder_opens(&source_path).is_err());
    assert_payload(&connection);
    source
        .snapshot_into(&connection, &mut target, 8 * 1024 * 1024)
        .unwrap();
    let copied = target.connect().unwrap();
    target
        .activate(&copied, &source, &connection, owner.generation, &handoff)
        .unwrap();
    assert!(historical_builder_opens(&target_path).is_err());
    assert_payload(&copied);
    assert!(target.verify(&copied).is_ok());
    assert!(source.verify(&connection).is_err());
}

#[test]
fn authority_format_gate_rolls_back_when_fencing_fails() {
    let root = fixture();
    let path = root.join("catalog.db");
    let (source, connection, owner) = database(&path);
    pollster::block_on(connection.execute_batch("CREATE TRIGGER refuse_fence BEFORE UPDATE ON recording_catalog_authority BEGIN SELECT RAISE(ABORT, 'injected fence failure'); END;")).unwrap();
    let target = Lease::acquire(&root.join("target.db")).unwrap();
    assert!(
        source
            .fence(
                &connection,
                owner.generation,
                &uuid::Uuid::new_v4().to_string(),
                &target
            )
            .is_err()
    );
    assert_eq!(source.verify(&connection).unwrap(), owner);
    assert!(historical_builder_opens(&path).is_ok());
}

#[test]
fn authority_format_gate_named_binding_reopens_and_legacy_remains_compatible() {
    use super::super::locations::{Binding, Request};
    let root = fixture();
    let path = root.join("catalog.db");
    let catalog = super::super::RecordingCatalog::open(&path).unwrap();
    assert!(historical_builder_opens(&path).is_ok());
    catalog
        .handle()
        .volume_location(Request::Bind(Binding {
            id: "named-root".into(),
            generation: 1,
            root: root.join("media"),
            filesystem: "fixture-disk".into(),
            root_identity: "fixture-root".into(),
            writable: true,
            limit_bytes: None,
            minimum_free_bytes: 0,
        }))
        .unwrap();
    catalog.shutdown();
    assert!(historical_builder_opens(&path).is_err());
    let reopened = super::super::RecordingCatalog::open(&path).unwrap();
    assert_eq!(reopened.handle().stats().unwrap().recording_files, 0);
    reopened.shutdown();
}

#[test]
fn authority_catalog_open_excludes_concurrent_writers_and_reopens() {
    let root = fixture();
    let path = root.join("catalog.db");
    let catalog = super::super::RecordingCatalog::open(&path).unwrap();
    assert!(super::super::RecordingCatalog::open(&path).is_err());
    assert!(
        super::super::rewrite_recording_paths(&path, &[(root.join("old"), root.join("new"))])
            .is_err()
    );
    assert_eq!(catalog.handle().stats().unwrap().recording_files, 0);
    catalog.shutdown();
    let reopened = super::super::RecordingCatalog::open(&path).unwrap();
    assert_eq!(reopened.handle().stats().unwrap().recording_files, 0);
    reopened.shutdown();
}

#[test]
fn authority_catalog_open_refuses_fenced_source_and_unactivated_snapshot() {
    let root = fixture();
    let source_path = root.join("source.db");
    super::super::RecordingCatalog::open(&source_path)
        .unwrap()
        .shutdown();
    let (source, connection, before) = database(&source_path);
    let target = Lease::acquire(&root.join("target.db")).unwrap();
    source
        .fence(
            &connection,
            before.generation,
            &uuid::Uuid::new_v4().to_string(),
            &target,
        )
        .unwrap();
    crate::backup::database::snapshot_turso_database(&connection, target.path(), 8 * 1024 * 1024)
        .unwrap();
    drop(connection);
    drop(source);
    drop(target);
    assert!(super::super::RecordingCatalog::open(&source_path).is_err());
    assert!(super::super::RecordingCatalog::open(&root.join("target.db")).is_err());
    assert!(
        super::super::rewrite_recording_paths(
            &source_path,
            &[(root.join("old"), root.join("new"))]
        )
        .is_err()
    );
}

#[test]
fn authority_lease_excludes_aliases_and_can_be_reacquired() {
    let root = fixture();
    let path = root.join("catalog.db");
    let lease = Lease::acquire(&path).unwrap();
    assert!(Lease::acquire(&root.join(".").join("catalog.db")).is_err());
    assert!(Lease::acquire(&path).is_err());
    drop(lease);
    assert!(Lease::acquire(&path).is_ok());
}

#[test]
fn authority_fenced_source_refuses_normal_open_and_changed_handoff() {
    let root = fixture();
    let (source, connection, before) = database(&root.join("source.db"));
    let target = Lease::acquire(&root.join("target.db")).unwrap();
    let handoff = uuid::Uuid::new_v4().to_string();
    assert!(
        source
            .fence(&connection, before.generation + 1, &handoff, &target)
            .is_err()
    );
    source
        .fence(&connection, before.generation, &handoff, &target)
        .unwrap();
    source
        .fence(&connection, before.generation, &handoff, &target)
        .unwrap();
    assert!(source.verify(&connection).is_err());
    assert!(
        source
            .fence(
                &connection,
                before.generation,
                &uuid::Uuid::new_v4().to_string(),
                &target
            )
            .is_err()
    );
    drop(connection);
    drop(source);
    let mut reopened = Lease::acquire(&root.join("source.db")).unwrap();
    let connection = reopened.connect().unwrap();
    assert!(reopened.initialize(&connection).is_err());
}

#[test]
fn authority_rejects_copied_catalog_and_wrong_connection() {
    let root = fixture();
    let (source, connection, _) = database(&root.join("source.db"));
    crate::backup::database::snapshot_turso_database(
        &connection,
        &root.join("copy.db"),
        8 * 1024 * 1024,
    )
    .unwrap();
    let mut copy = Lease::acquire(&root.join("copy.db")).unwrap();
    let copied_connection = copy.connect().unwrap();
    assert!(copy.initialize(&copied_connection).is_err());
    assert!(source.verify(&copied_connection).is_err());
    assert!(source.verify(&connection).is_ok());
}

#[test]
fn authority_validated_import_is_explicit_and_idempotent() {
    let root = fixture();
    let (_source, connection, before) = database(&root.join("source.db"));
    crate::backup::database::snapshot_turso_database(
        &connection,
        &root.join("copy.db"),
        8 * 1024 * 1024,
    )
    .unwrap();
    let mut lease = Lease::acquire(&root.join("copy.db")).unwrap();
    let copied = lease.connect().unwrap();
    assert!(lease.initialize(&copied).is_err());
    let imported = lease
        .import_validated(&copied, "restore-operation:apply")
        .unwrap();
    assert_eq!(imported.generation, before.generation + 1);
    assert_eq!(
        lease
            .import_validated(&copied, "restore-operation:apply")
            .unwrap(),
        imported
    );
    assert!(lease.imported(&copied, "restore-operation:apply").unwrap());
    assert!(!lease.imported(&copied, "another-operation:apply").unwrap());
}

#[test]
fn authority_compaction_preserves_normal_open_and_advances_generation() {
    let root = fixture();
    let path = root.join("source.db");
    super::super::RecordingCatalog::open(&path)
        .unwrap()
        .shutdown();
    crate::backup::database::compact_turso_database(
        &path,
        &root.join("compact.db"),
        8 * 1024 * 1024,
    )
    .unwrap();
    let (lease, connection, authority) = database(&path);
    assert_eq!(authority.generation, 2);
    drop(connection);
    drop(lease);
    super::super::RecordingCatalog::open(&path)
        .unwrap()
        .shutdown();
}

#[test]
fn authority_import_rejects_named_ownership_without_rebinding_identity() {
    let root = fixture();
    let (lease, connection, before) = database(&root.join("source.db"));
    pollster::block_on(connection.execute("CREATE TABLE storage_volume_bindings(id TEXT)", ()))
        .unwrap();
    pollster::block_on(connection.execute(
        "INSERT INTO storage_volume_bindings VALUES ('archive-one')",
        (),
    ))
    .unwrap();
    assert!(
        lease
            .import_validated(&connection, "restore-operation")
            .is_err()
    );
    assert_eq!(lease.verify(&connection).unwrap(), before);
}

#[test]
fn authority_compaction_retries_the_fence_after_snapshot_size_failure() {
    let root = fixture();
    let path = root.join("source.db");
    let temporary = root.join("compact.db");
    super::super::RecordingCatalog::open(&path)
        .unwrap()
        .shutdown();
    assert!(crate::backup::database::compact_turso_database(&path, &temporary, 1).is_err());
    assert!(super::super::RecordingCatalog::open(&path).is_err());
    assert!(temporary.is_file());
    crate::backup::database::compact_turso_database(&path, &temporary, 8 * 1024 * 1024).unwrap();
    let (_lease, _connection, authority) = database(&path);
    assert_eq!(authority.generation, 2);
}

#[test]
fn authority_compaction_resumes_an_already_activated_snapshot() {
    let root = fixture();
    let path = root.join("source.db");
    let temporary = root.join("compact.db");
    super::super::RecordingCatalog::open(&path)
        .unwrap()
        .shutdown();
    let (source, connection, before) = database(&path);
    let mut target = Lease::acquire(&temporary).unwrap();
    let handoff = uuid::Uuid::new_v4().to_string();
    source
        .fence(&connection, before.generation, &handoff, &target)
        .unwrap();
    crate::backup::database::snapshot_turso_database(&connection, &temporary, 8 * 1024 * 1024)
        .unwrap();
    let copied = target.connect().unwrap();
    target
        .activate(&copied, &source, &connection, before.generation, &handoff)
        .unwrap();
    drop(copied);
    drop(connection);
    drop(target);
    drop(source);
    crate::backup::database::compact_turso_database(&path, &temporary, 8 * 1024 * 1024).unwrap();
    let (_lease, _connection, authority) = database(&path);
    assert_eq!(authority.generation, before.generation + 1);
    assert!(!temporary.exists());
}

#[test]
fn authority_activation_matches_source_fence_and_retains_it() {
    let root = fixture();
    let (source, connection, before) = database(&root.join("source.db"));
    pollster::block_on(connection.execute("CREATE TABLE payload(value TEXT)", ())).unwrap();
    pollster::block_on(connection.execute("INSERT INTO payload VALUES ('retained')", ())).unwrap();
    let mut target = Lease::acquire(&root.join("target.db")).unwrap();
    let handoff = uuid::Uuid::new_v4().to_string();
    source
        .fence(&connection, before.generation, &handoff, &target)
        .unwrap();
    crate::backup::database::snapshot_turso_database(&connection, target.path(), 8 * 1024 * 1024)
        .unwrap();
    let target_connection = target.connect().unwrap();
    for (generation, id) in [
        (before.generation + 1, handoff.clone()),
        (before.generation, uuid::Uuid::new_v4().to_string()),
    ] {
        assert!(
            target
                .activate(&target_connection, &source, &connection, generation, &id)
                .is_err()
        );
    }
    let active = target
        .activate(
            &target_connection,
            &source,
            &connection,
            before.generation,
            &handoff,
        )
        .unwrap();
    assert_eq!(active.catalog_id, before.catalog_id);
    assert_eq!(active.generation, before.generation + 1);
    assert_eq!(target.verify(&target_connection).unwrap(), active);
    assert_eq!(
        target
            .activate(
                &target_connection,
                &source,
                &connection,
                before.generation,
                &handoff
            )
            .unwrap(),
        active
    );
    assert!(source.verify(&connection).is_err());
    assert_payload(&target_connection);
    drop(target_connection);
    drop(connection);
    drop(target);
    drop(source);
    let mut target = Lease::acquire(&root.join("target.db")).unwrap();
    let connection = target.connect().unwrap();
    assert_eq!(target.initialize(&connection).unwrap(), active);
    let mut source = Lease::acquire(&root.join("source.db")).unwrap();
    let connection = source.connect().unwrap();
    assert!(source.initialize(&connection).is_err());
}

fn assert_payload(connection: &turso::Connection) {
    let mut rows = pollster::block_on(connection.query("SELECT value FROM payload", ())).unwrap();
    assert_eq!(
        pollster::block_on(rows.next())
            .unwrap()
            .unwrap()
            .get::<String>(0)
            .unwrap(),
        "retained"
    );
}

#[test]
fn authority_handoff_rejects_existing_destination_without_fencing_source() {
    let root = fixture();
    let (source, connection, before) = database(&root.join("source.db"));
    std::fs::write(root.join("target.db"), b"unrelated").unwrap();
    let target = Lease::acquire(&root.join("target.db")).unwrap();
    assert!(
        source
            .fence(
                &connection,
                before.generation,
                &uuid::Uuid::new_v4().to_string(),
                &target
            )
            .is_err()
    );
    assert_eq!(source.verify(&connection).unwrap(), before);
    assert_eq!(std::fs::read(root.join("target.db")).unwrap(), b"unrelated");
}

#[test]
fn authority_rejects_database_hard_links_and_special_leaves() {
    let root = fixture();
    std::fs::write(root.join("source.db"), []).unwrap();
    std::fs::hard_link(root.join("source.db"), root.join("alias.db")).unwrap();
    assert!(Lease::acquire(&root.join("source.db")).is_err());
    std::fs::create_dir(root.join("directory.db")).unwrap();
    assert!(Lease::acquire(&root.join("directory.db")).is_err());
}

#[test]
fn authority_does_not_accept_uncommitted_state_as_durable() {
    let root = fixture();
    let (lease, connection, _) = database(&root.join("catalog.db"));
    pollster::block_on(connection.execute("BEGIN IMMEDIATE", ())).unwrap();
    assert!(lease.verify(&connection).is_err());
    assert!(lease.initialize(&connection).is_err());
    pollster::block_on(connection.execute("ROLLBACK", ())).unwrap();
    assert!(lease.verify(&connection).is_ok());
}

#[cfg(unix)]
#[test]
fn authority_parent_symlink_alias_shares_lease_but_leaf_symlink_is_rejected() {
    use std::os::unix::fs::symlink;
    let root = fixture();
    let original = root.join("original");
    std::fs::create_dir(&original).unwrap();
    symlink(&original, root.join("alias")).unwrap();
    let lease = Lease::acquire(&original.join("catalog.db")).unwrap();
    assert!(Lease::acquire(&root.join("alias/catalog.db")).is_err());
    std::fs::write(root.join("target.db"), []).unwrap();
    symlink(root.join("target.db"), original.join("linked.db")).unwrap();
    assert!(Lease::acquire(&original.join("linked.db")).is_err());
    drop(lease);
}
