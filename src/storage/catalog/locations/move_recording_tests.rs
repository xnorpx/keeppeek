use super::*;

async fn request(connection: &turso::Connection, request: Request) -> anyhow::Result<Reply> {
    execute(connection, request, Instant::now() + BUSY_TIMEOUT).await
}

async fn allocation(
    connection: &turso::Connection,
    operation: &str,
    volume: &str,
) -> anyhow::Result<Allocation> {
    Ok(Allocation {
        operation: operation.into(),
        object: Object {
            kind: Kind::Recording,
            id: operation.into(),
        },
        volume: volume.into(),
        generation: 1,
        relative_key: format!("{operation}.mp4"),
        bytes: 100,
        capacity: Capacity {
            ledger_revision: revision(connection).await?,
            observed_at: Instant::now(),
            available_bytes: 1000,
            filesystem: "disk".into(),
            root_identity: volume.into(),
        },
    })
}

async fn fixture(connection: &turso::Connection) -> anyhow::Result<(moves::Intent, Location)> {
    super::super::initialize_schema(connection).await?;
    let root = std::env::current_dir()?
        .join("target")
        .join("synthetic-recording-moves");
    for volume in ["source", "destination"] {
        request(
            connection,
            Request::Bind(Binding {
                id: volume.into(),
                generation: 1,
                root: root.join(volume),
                filesystem: "disk".into(),
                root_identity: volume.into(),
                writable: true,
                draining: false,
                limit_bytes: Some(200),
                minimum_free_bytes: 0,
            }),
        )
        .await?;
    }
    let source = allocation(connection, "recording", "source").await?;
    request(connection, Request::Reserve(source)).await?;
    let path = root
        .join("source")
        .join("recording.mp4")
        .to_string_lossy()
        .into_owned();
    connection.execute("INSERT INTO recording_files (id, stream_id, source_id, logical_stream_id, started_at_ms, path, init_offset, init_len, finalized) VALUES ('recording', 'camera/main', 'camera', 'main', 100, ?1, 0, 10, 0)", [path]).await?;
    connection.execute("INSERT INTO recording_fragments (recording_id, sequence, start_ms, duration_ms, byte_offset, byte_len, random_access) VALUES ('recording', 1, 100, 50, 10, 60, 1)", ()).await?;
    connection.execute("INSERT INTO recording_keyframes (recording_id, fragment_sequence, byte_offset, byte_len) VALUES ('recording', 1, 20, 10)", ()).await?;
    let Reply::Location(Some(source)) = request(
        connection,
        Request::Finalize(Publication {
            operation: "recording".into(),
            bytes: 70,
            file_identity: "source-file".into(),
            digest: [9; 32],
        }),
    )
    .await?
    else {
        anyhow::bail!("source finalization did not publish a location");
    };
    let intent = moves::Intent {
        id: "move".into(),
        object: source.object.clone(),
        expected_revision: source.revision,
        destination: allocation(connection, "move", "destination").await?,
    };
    Ok((intent, source))
}

async fn ready(
    connection: &turso::Connection,
    intent: &moves::Intent,
    source: &Location,
) -> anyhow::Result<()> {
    request(connection, Request::BeginMove(intent.clone())).await?;
    request(
        connection,
        Request::AdvanceMove(moves::Step::Verified(Publication {
            operation: intent.id.clone(),
            bytes: source.bytes,
            file_identity: "destination-file".into(),
            digest: source.digest,
        })),
    )
    .await?;
    request(
        connection,
        Request::AdvanceMove(moves::Step::FilePublished(intent.id.clone())),
    )
    .await?;
    Ok(())
}

async fn stable_metadata(connection: &turso::Connection) -> anyhow::Result<Vec<Option<String>>> {
    let mut rows = connection
        .query(
            "SELECT id, stream_id, source_id, logical_stream_id,
        CAST(started_at_ms AS TEXT), CAST(ended_at_ms AS TEXT), CAST(init_offset AS TEXT),
        CAST(init_len AS TEXT), CAST(finalized AS TEXT), CAST(finalized_at_ms AS TEXT),
        CAST(file_bytes AS TEXT), CAST(cleanup_pending AS TEXT), CAST(protected AS TEXT)
        FROM recording_files WHERE id = 'recording'",
            (),
        )
        .await?;
    let row = rows.next().await?.unwrap();
    (0..13)
        .map(|index| row.get(index).map_err(Into::into))
        .collect()
}

async fn location_metadata(connection: &turso::Connection) -> anyhow::Result<(String, String)> {
    let mut rows = connection
        .query(
            "SELECT path, file_identity FROM recording_files WHERE id = 'recording'",
            (),
        )
        .await?;
    let row = rows.next().await?.unwrap();
    Ok((row.get(0)?, row.get(1)?))
}

async fn index_metadata(connection: &turso::Connection) -> anyhow::Result<Vec<i64>> {
    let mut rows = connection.query("SELECT f.sequence,f.start_ms,f.duration_ms,f.byte_offset,f.byte_len,f.random_access,
        k.byte_offset,k.byte_len,c.coverage_ms,c.fragment_count,c.fragment_bytes
        FROM recording_fragments f JOIN recording_keyframes k ON k.recording_id = f.recording_id AND k.fragment_sequence = f.sequence
        JOIN recording_coverage_files c ON c.recording_id = f.recording_id WHERE f.recording_id = 'recording'", ()).await?;
    let row = rows.next().await?.unwrap();
    let values = (0..11)
        .map(|index| row.get(index).map_err(Into::into))
        .collect();
    assert!(rows.next().await?.is_none());
    values
}

#[test]
fn recording_move_rejects_admitted_deletion_and_path_aliases_without_reserving_capacity()
-> anyhow::Result<()> {
    pollster::block_on(async {
        for recording_id in ["recording", "path-alias"] {
            let database = turso::Builder::new_local(":memory:")
                .experimental_generated_columns(true)
                .build()
                .await?;
            let connection = database.connect()?;
            let (intent, _) = fixture(&connection).await?;
            connection
                .execute(
                    "INSERT INTO recording_maintenance_claims
                (job_id,ordinal,recording_id,token,path,file_identity,file_bytes,volume_operation)
                SELECT 'deletion',0,id,'claim-token',path,zeroblob(32),file_bytes,'recording'
                FROM recording_files WHERE id='recording'",
                    (),
                )
                .await?;
            connection
                .execute(
                    "UPDATE recording_maintenance_claims SET recording_id=?1",
                    [recording_id],
                )
                .await?;
            assert!(
                request(&connection, Request::BeginMove(intent.clone()))
                    .await
                    .is_err()
            );
            let mut rows = connection
                .query(
                    "SELECT
                (SELECT COUNT(*) FROM storage_volume_moves),
                (SELECT COUNT(*) FROM storage_volume_allocations WHERE operation='move'),
                (SELECT allocated_bytes FROM storage_volume_bindings WHERE id='destination')",
                    (),
                )
                .await?;
            let row = rows.next().await?.unwrap();
            assert_eq!(row.get::<i64>(0)?, 0);
            assert_eq!(row.get::<i64>(1)?, 0);
            assert_eq!(row.get::<i64>(2)?, 0);
        }
        Ok(())
    })
}

#[test]
fn recording_move_preserves_identity_fragments_and_finalization_across_exact_retry()
-> anyhow::Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await?;
        let connection = database.connect()?;
        let (intent, source) = fixture(&connection).await?;
        let metadata_before = stable_metadata(&connection).await?;
        let index_before = index_metadata(&connection).await?;
        let path_before = location_metadata(&connection).await?;
        ready(&connection, &intent, &source).await?;
        assert_eq!(location_metadata(&connection).await?, path_before);
        assert_eq!(
            request(&connection, Request::Lookup(intent.object.clone())).await?,
            Reply::Location(Some(source.clone()))
        );
        let publish = Request::AdvanceMove(moves::Step::Publish(intent.id.clone()));
        let published = request(&connection, publish.clone()).await?;
        let after_revision = revision(&connection).await?;
        let usage = request(&connection, Request::Usage).await?;
        assert_eq!(request(&connection, publish).await?, published);
        assert_eq!(
            request(&connection, Request::BeginMove(intent.clone())).await?,
            published
        );
        assert_eq!(revision(&connection).await?, after_revision);
        assert_eq!(request(&connection, Request::Usage).await?, usage);
        assert_eq!(stable_metadata(&connection).await?, metadata_before);
        assert_eq!(index_metadata(&connection).await?, index_before);
        let (path, identity) = location_metadata(&connection).await?;
        assert_ne!(path, path_before.0);
        assert_eq!(identity, "destination-file");
        let mut rows = connection
            .query(
                "SELECT destination_path FROM storage_volume_allocations WHERE operation = 'move'",
                (),
            )
            .await?;
        assert_eq!(path, rows.next().await?.unwrap().get::<String>(0)?);
        drop(rows);
        let Reply::Location(Some(destination)) =
            request(&connection, Request::Lookup(intent.object)).await?
        else {
            anyhow::bail!("destination not authoritative");
        };
        assert_eq!(destination.object.id, "recording");
        assert_eq!(destination.volume, "destination");
        assert_eq!(destination.revision, source.revision + 1);
        assert_eq!(destination.bytes, source.bytes);
        assert_eq!(destination.digest, source.digest);
        let Reply::Usage(usage) = usage else {
            anyhow::bail!("usage missing");
        };
        assert_eq!(
            usage
                .iter()
                .map(|volume| volume.allocated_bytes)
                .sum::<u64>(),
            140
        );
        assert!(usage.iter().all(|volume| volume.reserved_bytes == 0));
        Ok(())
    })
}

#[test]
fn recording_move_sql_failure_rolls_back_source_destination_and_journal() -> anyhow::Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await?;
        let connection = database.connect()?;
        let (intent, source) = fixture(&connection).await?;
        ready(&connection, &intent, &source).await?;
        let metadata_before = stable_metadata(&connection).await?;
        let index_before = index_metadata(&connection).await?;
        let path_before = location_metadata(&connection).await?;
        let usage_before = request(&connection, Request::Usage).await?;
        let revision_before = revision(&connection).await?;
        connection
            .execute_batch(
                "CREATE TRIGGER refuse_move_publication BEFORE UPDATE ON storage_volume_allocations
            WHEN OLD.operation = 'move' AND NEW.state = 'published'
            BEGIN SELECT RAISE(ABORT, 'injected destination publication failure'); END;",
            )
            .await?;
        let publish = Request::AdvanceMove(moves::Step::Publish(intent.id.clone()));
        assert!(request(&connection, publish.clone()).await.is_err());
        assert_eq!(revision(&connection).await?, revision_before);
        assert_eq!(request(&connection, Request::Usage).await?, usage_before);
        assert_eq!(location_metadata(&connection).await?, path_before);
        assert_eq!(stable_metadata(&connection).await?, metadata_before);
        assert_eq!(index_metadata(&connection).await?, index_before);
        assert_eq!(
            request(&connection, Request::Lookup(intent.object.clone())).await?,
            Reply::Location(Some(source))
        );
        let Reply::Move(job) = request(&connection, Request::Move(intent.id.clone())).await? else {
            anyhow::bail!("move missing");
        };
        assert_eq!(job.phase, "file_published");
        let mut rows = connection.query("SELECT object_id,state,bytes,materialized_bytes,location_revision,file_identity FROM storage_volume_allocations WHERE operation = 'recording'", ()).await?;
        let row = rows.next().await?.unwrap();
        assert_eq!(row.get::<String>(0)?, "recording");
        assert_eq!(row.get::<String>(1)?, "published");
        assert_eq!(row.get::<i64>(2)?, 70);
        assert_eq!(row.get::<i64>(4)?, 1);
        assert_eq!(row.get::<String>(5)?, "source-file");
        drop(rows);
        let mut rows = connection.query("SELECT object_id,state,bytes,materialized_bytes,location_revision,file_identity FROM storage_volume_allocations WHERE operation = 'move'", ()).await?;
        let row = rows.next().await?.unwrap();
        assert_eq!(row.get::<String>(0)?, "move");
        assert_eq!(row.get::<String>(1)?, "reserved");
        assert_eq!(row.get::<i64>(2)?, 100);
        assert_eq!(row.get::<i64>(3)?, 70);
        assert_eq!(row.get::<i64>(4)?, 0);
        assert_eq!(row.get::<String>(5)?, "destination-file");
        drop(rows);
        connection
            .execute_batch("DROP TRIGGER refuse_move_publication")
            .await?;
        request(&connection, publish).await?;
        assert_eq!(stable_metadata(&connection).await?, metadata_before);
        assert_eq!(index_metadata(&connection).await?, index_before);
        Ok(())
    })
}
