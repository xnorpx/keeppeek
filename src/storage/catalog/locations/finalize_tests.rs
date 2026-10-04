use super::*;

async fn fixture(connection: &turso::Connection) -> anyhow::Result<Publication> {
    super::super::initialize_schema(connection).await?;
    let root = super::super::tests::test_dir("volume-atomic-finalize");
    let binding = Binding {
        id: "primary".into(),
        generation: 1,
        root: root.join("primary"),
        filesystem: "disk".into(),
        root_identity: "root".into(),
        writable: true,
        draining: false,
        limit_bytes: Some(100),
        minimum_free_bytes: 0,
    };
    execute(
        connection,
        Request::Bind(binding.clone()),
        Instant::now() + BUSY_TIMEOUT,
    )
    .await?;
    let path = binding
        .root
        .join("recording.mp4")
        .to_string_lossy()
        .into_owned();
    reserve(
        connection,
        &Allocation {
            operation: "operation".into(),
            object: Object {
                kind: Kind::Recording,
                id: "recording".into(),
            },
            volume: binding.id,
            generation: 1,
            relative_key: "recording.mp4".into(),
            bytes: 100,
            capacity: Capacity {
                ledger_revision: revision(connection).await?,
                observed_at: Instant::now(),
                available_bytes: 1_000,
                filesystem: binding.filesystem,
                root_identity: binding.root_identity,
            },
        },
    )
    .await?;
    connection.execute("INSERT INTO recording_files (id, stream_id, started_at_ms, path, init_offset, init_len, finalized) VALUES ('recording', 'camera/main', 100, ?1, 0, 10, 0)", [path]).await?;
    connection.execute("INSERT INTO recording_fragments (recording_id, sequence, start_ms, duration_ms, byte_offset, byte_len, random_access) VALUES ('recording', 1, 100, 50, 10, 60, 1)", ()).await?;
    connection.execute("INSERT INTO recording_keyframes (recording_id, fragment_sequence, byte_offset, byte_len) VALUES ('recording', 1, 20, 10)", ()).await?;
    Ok(Publication {
        operation: "operation".into(),
        bytes: 70,
        file_identity: "pinned-identity".into(),
        digest: [9; 32],
    })
}

async fn finalize(
    connection: &turso::Connection,
    publication: Publication,
) -> anyhow::Result<Reply> {
    execute(
        connection,
        Request::Finalize(publication),
        Instant::now() + BUSY_TIMEOUT,
    )
    .await
}

#[test]
fn publication_failure_rolls_back_finalized_metadata_and_coverage() -> anyhow::Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await?;
        let connection = database.connect()?;
        let publication = fixture(&connection).await?;
        connection.execute_batch("CREATE TRIGGER refuse_publication BEFORE UPDATE ON storage_volume_allocations WHEN NEW.state = 'published' BEGIN SELECT RAISE(ABORT, 'injected publication failure'); END;").await?;
        let before = revision(&connection).await?;
        assert!(finalize(&connection, publication.clone()).await.is_err());
        assert_eq!(revision(&connection).await?, before);
        let mut rows = connection.query("SELECT finalized, finalized_at_ms, ended_at_ms, file_bytes, file_identity FROM recording_files WHERE id = 'recording'", ()).await?;
        let row = rows.next().await?.unwrap();
        assert_eq!(row.get::<i64>(0)?, 0);
        assert_eq!(row.get::<Option<i64>>(1)?, None);
        assert_eq!(row.get::<Option<i64>>(2)?, None);
        assert_eq!(row.get::<i64>(3)?, 0);
        assert_eq!(row.get::<Option<String>>(4)?, None);
        drop(rows);
        let mut rows = connection.query("SELECT (SELECT COUNT(*) FROM recording_coverage_files), (SELECT COUNT(*) FROM recording_coverage_ranges), state FROM storage_volume_allocations", ()).await?;
        let row = rows.next().await?.unwrap();
        assert_eq!(row.get::<i64>(0)?, 0);
        assert_eq!(row.get::<i64>(1)?, 0);
        assert_eq!(row.get::<String>(2)?, "reserved");
        drop(rows);
        assert_eq!(
            ownership::lookup(
                &connection,
                &Object {
                    kind: Kind::Recording,
                    id: "recording".into()
                }
            )
            .await?,
            Reply::Location(None)
        );
        connection
            .execute_batch("DROP TRIGGER refuse_publication")
            .await?;
        assert!(matches!(
            finalize(&connection, publication).await?,
            Reply::Location(Some(_))
        ));
        Ok(())
    })
}

#[test]
fn finalization_uses_pinned_evidence_and_exact_retry_is_unchanged() -> anyhow::Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await?;
        let connection = database.connect()?;
        let publication = fixture(&connection).await?;
        let first = finalize(&connection, publication.clone()).await?;
        let revision_before = revision(&connection).await?;
        assert_eq!(finalize(&connection, publication.clone()).await?, first);
        assert_eq!(revision(&connection).await?, revision_before);
        let mut rows = connection.query("SELECT finalized, ended_at_ms, file_bytes, file_identity, finalized_at_ms FROM recording_files WHERE id = 'recording'", ()).await?;
        let row = rows.next().await?.unwrap();
        assert_eq!(row.get::<i64>(0)?, 1);
        assert_eq!(row.get::<i64>(1)?, 150);
        assert_eq!(row.get::<i64>(2)?, 70);
        assert_eq!(row.get::<String>(3)?, publication.file_identity);
        assert!(row.get::<i64>(4)? > 0);
        drop(rows);
        let mut rows = connection
            .query(
                "SELECT coverage_ms FROM recording_coverage_files WHERE recording_id = 'recording'",
                (),
            )
            .await?;
        assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 50);
        drop(rows);
        let mut changed = publication;
        changed.digest = [0; 32];
        assert!(finalize(&connection, changed).await.is_err());
        assert_eq!(revision(&connection).await?, revision_before);
        Ok(())
    })
}
