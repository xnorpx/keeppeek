use super::*;

async fn seed_archive_backlog(connection: &turso::Connection, policy: &str) -> anyhow::Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    for index in 0..4096 {
        let id = format!("old-{index}");
        connection.execute("INSERT INTO storage_volume_allocations
            (operation,kind,object_id,volume_id,generation,relative_key,destination_path,bytes,intent_bytes,state)
            VALUES (?1,'recording',?1,'primary',1,?2,?3,1,1,'published')",
            turso::params![id.clone(), format!("{id}.mp4"), format!("/{id}.mp4")]).await?;
    }
    connection
        .execute(
            "INSERT INTO storage_volume_archives(id,operation,policy)
        SELECT 'job-' || operation,operation,?1 FROM storage_volume_allocations",
            [policy],
        )
        .await?;
    connection.execute_batch("COMMIT").await?;
    Ok(())
}

fn archive_intent(root: &std::path::Path) -> anyhow::Result<archives::Intent> {
    Ok(archives::Intent {
        id: uuid::Uuid::new_v4().to_string(),
        policy: archives::Policy {
            source: "camera".into(),
            groups: vec![],
            configuration: serde_json::from_value(serde_json::json!({
                "volumes": [{ "id": "archive", "root": root.join("archive"), "roles": ["archive"] }],
                "placement": [{ "role": "archive", "candidates": ["archive"] }]
            }))?,
        },
    })
}

async fn new_allocation(connection: &turso::Connection) -> anyhow::Result<Allocation> {
    let id = uuid::Uuid::new_v4().to_string();
    Ok(Allocation {
        operation: uuid::Uuid::new_v4().to_string(),
        object: Object {
            kind: Kind::Recording,
            id: id.clone(),
        },
        volume: "primary".into(),
        generation: 1,
        relative_key: format!("{id}.mp4"),
        bytes: 1,
        capacity: Capacity {
            ledger_revision: revision(connection).await?,
            observed_at: Instant::now(),
            available_bytes: 100_000,
            filesystem: "disk".into(),
            root_identity: "primary".into(),
        },
    })
}

#[test]
fn archive_backlog_does_not_block_new_recording_reservations() -> anyhow::Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:")
            .experimental_generated_columns(true)
            .build()
            .await?;
        let connection = database.connect()?;
        super::super::initialize_schema(&connection).await?;
        let root = std::env::current_dir()?
            .join("target")
            .join("archive-backlog");
        execute(
            &connection,
            Request::Bind(Binding {
                id: "primary".into(),
                generation: 1,
                root: root.join("primary"),
                filesystem: "disk".into(),
                root_identity: "primary".into(),
                writable: true,
                draining: false,
                limit_bytes: None,
                minimum_free_bytes: 0,
            }),
            Instant::now() + BUSY_TIMEOUT,
        )
        .await?;
        let intent = archive_intent(&root)?;
        seed_archive_backlog(&connection, &serde_json::to_string(&intent.policy)?).await?;
        let allocation = new_allocation(&connection).await?;
        let operation = allocation.operation.clone();
        assert_eq!(
            execute(
                &connection,
                Request::ReserveArchive(allocation, intent),
                Instant::now() + BUSY_TIMEOUT
            )
            .await?,
            Reply::Reserved {
                operation,
                bytes: 1
            }
        );
        let mut rows = connection
            .query(
                "SELECT COUNT(*) FROM storage_volume_archives WHERE done = 0",
                (),
            )
            .await?;
        assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 4097);
        Ok(())
    })
}
