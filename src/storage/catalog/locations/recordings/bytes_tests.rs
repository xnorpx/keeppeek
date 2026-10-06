use anyhow::{Context, Result};

async fn seed(connection: &turso::Connection) -> Result<()> {
    connection.execute_batch(r"CREATE TABLE recording_files (
        id TEXT PRIMARY KEY,path TEXT NOT NULL,file_bytes INTEGER NOT NULL,protected INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE storage_volume_allocations (
        operation TEXT PRIMARY KEY,kind TEXT,state TEXT,object_id TEXT,destination_path TEXT);
        INSERT INTO recording_files VALUES ('a','c:\store\a.mp4',10,0),('b','b.mp4',20,0);").await?;
    Ok(())
}

async fn total(connection: &turso::Connection) -> Result<Option<i64>> {
    let mut rows = connection
        .query(
            "SELECT total_bytes FROM recording_legacy_byte_total WHERE singleton=1",
            (),
        )
        .await?;
    Ok(rows
        .next()
        .await?
        .context("recording byte total missing")?
        .get(0)?)
}

#[test]
fn legacy_byte_total_bootstraps_tracks_changes_and_rolls_back() -> Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:").build().await?;
        let connection = database.connect()?;
        seed(&connection).await?;
        super::initialize(&connection).await?;
        assert_eq!(total(&connection).await?, Some(30));
        assert_eq!(super::legacy_bytes(&connection).await?, 30);
        connection
            .execute_batch(
                "INSERT INTO recording_files VALUES ('c','c.mp4',30,0);
            UPDATE recording_files SET file_bytes=25 WHERE id='b';
            DELETE FROM recording_files WHERE id='a';",
            )
            .await?;
        assert_eq!(total(&connection).await?, Some(55));
        assert_eq!(super::legacy_bytes(&connection).await?, 55);
        connection
            .execute_batch(
                "BEGIN IMMEDIATE; UPDATE recording_files SET file_bytes=1000 WHERE id='b';",
            )
            .await?;
        assert_eq!(total(&connection).await?, Some(1030));
        connection.execute_batch("ROLLBACK").await?;
        assert_eq!(total(&connection).await?, Some(55));
        connection
            .execute_batch("UPDATE recording_files SET file_bytes=9223372036854775807 WHERE id='b'")
            .await?;
        assert_eq!(total(&connection).await?, None);
        assert!(
            super::legacy_bytes(&connection).await.is_err(),
            "legacy sum overflow must fail closed"
        );
        connection
            .execute_batch("UPDATE recording_files SET file_bytes=25 WHERE id='b'")
            .await?;
        assert_eq!(super::legacy_bytes(&connection).await?, 55);
        anyhow::Ok(())
    })
}

#[test]
fn legacy_byte_total_preserves_named_id_path_and_cancellation_ownership() -> Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:").build().await?;
        let connection = database.connect()?;
        seed(&connection).await?;
        super::initialize(&connection).await?;
        connection
            .execute_batch(
                "INSERT INTO storage_volume_allocations VALUES
            ('export','export','reserved','a','c:/store/a.mp4');",
            )
            .await?;
        assert_eq!(super::legacy_bytes(&connection).await?, 30);
        connection
            .execute_batch(
                "INSERT INTO storage_volume_allocations VALUES
            ('named','recording','reserved','a','unrelated.mp4');",
            )
            .await?;
        assert_eq!(super::legacy_bytes(&connection).await?, 20);
        connection
            .execute_batch(
                "UPDATE storage_volume_allocations SET state='cancelled' WHERE operation='named'",
            )
            .await?;
        assert_eq!(super::legacy_bytes(&connection).await?, 30);
        connection
            .execute_batch(
                "UPDATE storage_volume_allocations SET state='published',object_id='other',
            destination_path='C:/STORE/A.MP4' WHERE operation='named'",
            )
            .await?;
        assert_eq!(super::legacy_bytes(&connection).await?, 20);
        connection
            .execute_batch("UPDATE recording_files SET path='renamed.mp4' WHERE id='a'")
            .await?;
        assert_eq!(super::legacy_bytes(&connection).await?, 30);
        connection
            .execute_batch(
                "UPDATE storage_volume_allocations SET object_id='b' WHERE operation='named'",
            )
            .await?;
        assert_eq!(super::legacy_bytes(&connection).await?, 10);
        assert_eq!(total(&connection).await?, Some(30));
        anyhow::Ok(())
    })
}

#[test]
fn legacy_byte_total_survives_native_catalog_reopen() -> Result<()> {
    let root = std::env::temp_dir().join(format!("legacy-byte-total-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root)?;
    let path = root.join("catalog.db");
    let path = path.to_str().context("fixture path not UTF-8")?;
    let result = pollster::block_on(async {
        let database = turso::Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        seed(&connection).await?;
        super::initialize(&connection).await?;
        connection
            .execute_batch("UPDATE recording_files SET file_bytes=45 WHERE id='b'")
            .await?;
        assert_eq!(total(&connection).await?, Some(55));
        drop(connection);
        drop(database);
        let database = turso::Builder::new_local(path).build().await?;
        let connection = database.connect()?;
        super::initialize(&connection).await?;
        assert_eq!(total(&connection).await?, Some(55));
        assert_eq!(super::legacy_bytes(&connection).await?, 55);
        anyhow::Ok(())
    });
    anyhow::ensure!(
        root.canonicalize()?
            .starts_with(std::env::temp_dir().canonicalize()?),
        "fixture left the temporary directory"
    );
    std::fs::remove_dir_all(&root)?;
    result
}

#[test]
fn legacy_byte_total_overflow_does_not_reject_representable_named_ownership() -> Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:").build().await?;
        let connection = database.connect()?;
        seed(&connection).await?;
        connection.execute_batch("UPDATE recording_files SET file_bytes=9223372036854775807 WHERE id='a';
            UPDATE recording_files SET file_bytes=1 WHERE id='b';
            INSERT INTO storage_volume_allocations VALUES ('named','recording','published','a','unused.mp4');").await?;
        super::initialize(&connection).await?;
        assert_eq!(total(&connection).await?, None);
        assert_eq!(super::legacy_bytes(&connection).await?, 1);
        connection
            .execute_batch("UPDATE recording_files SET file_bytes=2 WHERE id='b'")
            .await?;
        assert_eq!(super::legacy_bytes(&connection).await?, 2);
        anyhow::Ok(())
    })
}

#[test]
fn legacy_byte_total_replacement_avoids_intermediate_overflow() -> Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:").build().await?;
        let connection = database.connect()?;
        seed(&connection).await?;
        super::initialize(&connection).await?;
        connection
            .execute_batch("UPDATE recording_files SET file_bytes=9223372036854775787 WHERE id='a'")
            .await?;
        assert_eq!(total(&connection).await?, Some(i64::MAX));
        connection
            .execute_batch("UPDATE recording_files SET file_bytes=9223372036854775786 WHERE id='a'")
            .await?;
        assert_eq!(total(&connection).await?, Some(i64::MAX - 1));
        connection
            .execute_batch("UPDATE recording_files SET file_bytes=9223372036854775787 WHERE id='a'")
            .await?;
        assert_eq!(total(&connection).await?, Some(i64::MAX));
        assert_eq!(
            super::legacy_bytes(&connection).await?,
            u64::try_from(i64::MAX)?
        );
        anyhow::Ok(())
    })
}

#[test]
fn legacy_byte_total_initialization_rolls_back_failed_schema_installation() -> Result<()> {
    pollster::block_on(async {
        let database = turso::Builder::new_local(":memory:").build().await?;
        let connection = database.connect()?;
        seed(&connection).await?;
        connection
            .execute_batch(
                "ALTER TABLE storage_volume_allocations RENAME COLUMN kind TO broken_kind",
            )
            .await?;
        assert!(super::initialize(&connection).await.is_err());
        assert!(connection.is_autocommit()?);
        let mut rows = connection
            .query(
                "SELECT 1 FROM sqlite_master WHERE name='recording_legacy_byte_total'",
                (),
            )
            .await?;
        assert!(
            rows.next().await?.is_none(),
            "failed initialization left a stale total"
        );
        anyhow::Ok(())
    })
}
