use super::{LegacyPaths, startup, startup_repair_tests};
use crate::storage::catalog::{self, authority::Lease};
use std::path::PathBuf;

struct Stopped {
    connection: turso::Connection,
    _database: turso::Database,
    _lease: Lease,
    root: PathBuf,
    paths: LegacyPaths,
}

fn stopped(name: &str, finalized: bool) -> anyhow::Result<Stopped> {
    let (root, catalog) = startup_repair_tests::fixture(name, finalized)?;
    catalog.shutdown();
    let mut lease = Lease::acquire(&root.join("catalog.db"))?;
    let database = lease.database()?;
    let connection = database.connect()?;
    lease.verify(&connection)?;
    let paths = pollster::block_on(super::load(&connection))?.unwrap();
    Ok(Stopped {
        connection,
        _database: database,
        _lease: lease,
        root,
        paths,
    })
}

async fn number(connection: &turso::Connection, sql: &str) -> anyhow::Result<i64> {
    Ok(connection
        .query(sql, ())
        .await?
        .next()
        .await?
        .unwrap()
        .get(0)?)
}

async fn state(connection: &turso::Connection) -> anyhow::Result<Vec<String>> {
    // ponytail: fixed SQL snapshots cover the touched tables without another fixture framework.
    let queries = [
        "SELECT json_group_array(json_array(id,path,ended_at_ms,finalized,finalized_at_ms,file_identity,file_bytes)) FROM recording_files",
        "SELECT json_group_array(json_array(recording_id,sequence,start_ms,duration_ms,byte_offset,byte_len,random_access)) FROM recording_fragments",
        "SELECT json_group_array(json_array(recording_id,fragment_sequence,byte_offset,byte_len)) FROM recording_keyframes",
        "SELECT json_group_array(json_array(event_id,stream_id,recording_id,fragment_sequence)) FROM recording_event_keyframes",
        "SELECT json_group_array(json_array(recording_id,fragment_count,fragment_bytes,coverage_ms,committed_at_ms)) FROM recording_coverage_files",
        "SELECT json_group_array(json_array(recording_id,start_ms,end_ms)) FROM recording_coverage_ranges",
        "SELECT json_group_array(json_array(recording_id,path,revision,file_identity,bytes,hex(digest),catalog_identity)) FROM storage_legacy_recordings",
        "SELECT json_group_array(json_array(revision,updated_at_ms)) FROM recording_catalog_state",
    ];
    let mut result = Vec::with_capacity(queries.len());
    for sql in queries {
        result.push(
            connection
                .query(sql, ())
                .await?
                .next()
                .await?
                .unwrap()
                .get(0)?,
        );
    }
    Ok(result)
}

#[test]
fn late_fragment_or_existing_keyframe_mismatch_preserves_the_entire_repair() -> anyhow::Result<()> {
    pollster::block_on(async {
        for existing_key in [false, true] {
            let fixture = stopped("captured-late-metadata-mismatch", true)?;
            let connection = &fixture.connection;
            if existing_key {
                let key = catalog::read_legacy_keyframes(
                    &fixture.root.join("recording.mp4"),
                    "recording-1",
                )?
                .pop()
                .unwrap();
                connection
                    .execute(
                        "INSERT INTO recording_keyframes VALUES(?1,?2,?3,?4)",
                        turso::params![
                            key.recording_id,
                            i64::try_from(key.fragment_sequence)?,
                            i64::try_from(key.byte_offset)? + 1,
                            i64::try_from(key.byte_len)?
                        ],
                    )
                    .await?;
            } else {
                connection.execute("UPDATE recording_fragments SET duration_ms=duration_ms+1 WHERE sequence=(SELECT MAX(sequence) FROM recording_fragments)", ()).await?;
            }
            super::inventory::register_recordings(connection, None, 1).await?;
            let before = state(connection).await?;
            let bytes = std::fs::read(fixture.root.join("recording.mp4"))?;
            startup::repair(connection, &fixture.paths).await?;
            assert_eq!(state(connection).await?, before);
            assert_eq!(std::fs::read(fixture.root.join("recording.mp4"))?, bytes);
            assert!(connection.is_autocommit()?);
        }
        Ok(())
    })
}

async fn conflict(
    connection: &turso::Connection,
    path: String,
    maintenance: bool,
) -> anyhow::Result<()> {
    if maintenance {
        connection
            .execute(
                "INSERT INTO recording_maintenance_claims
            (job_id,ordinal,recording_id,token,path,file_identity,file_bytes,active)
            VALUES ('job',1,'different-owner','token',?1,zeroblob(32),64,1)",
                [path],
            )
            .await?;
    } else {
        connection
            .execute_batch(
                "INSERT INTO storage_volume_bindings
            (id,generation,root,filesystem,root_identity,writable,minimum_free_bytes)
            VALUES ('named',1,'/named','disk','root',1,0)",
            )
            .await?;
        connection.execute("INSERT INTO storage_volume_allocations
            (operation,kind,object_id,volume_id,generation,relative_key,destination_path,bytes,intent_bytes,state)
            VALUES ('allocation','recording','different-owner','named',1,'recording.mp4',?1,64,64,'reserved')", [path]).await?;
    }
    Ok(())
}

#[test]
fn source_and_final_sibling_alias_owners_block_captured_startup_repair() -> anyhow::Result<()> {
    pollster::block_on(async {
        for (maintenance, sibling) in [(false, false), (false, true), (true, false), (true, true)] {
            let fixture = stopped("captured-alias-conflict", false)?;
            let name = if sibling {
                "recording.mp4"
            } else {
                "recording.mp4.active"
            };
            let original = fixture.root.join(name).to_string_lossy().into_owned();
            let alias = if maintenance {
                original.replace('/', "\\")
            } else {
                original.replace('\\', "/")
            };
            conflict(&fixture.connection, alias.to_ascii_uppercase(), maintenance).await?;
            let before = state(&fixture.connection).await?;
            let bytes = std::fs::read(fixture.root.join("recording.mp4"))?;
            startup::repair(&fixture.connection, &fixture.paths).await?;
            assert_eq!(state(&fixture.connection).await?, before);
            assert_eq!(std::fs::read(fixture.root.join("recording.mp4"))?, bytes);
            assert!(fixture.connection.is_autocommit()?);
        }
        Ok(())
    })
}

#[test]
fn interrupted_finalization_rolls_back_late_sql_failure_and_retries_exactly() -> anyhow::Result<()>
{
    pollster::block_on(async {
        let fixture = stopped("captured-atomic-finalization", false)?;
        let connection = &fixture.connection;
        connection
            .execute_batch(
                "CREATE TRIGGER fail_second_startup_key BEFORE INSERT ON recording_keyframes
            WHEN NEW.fragment_sequence=(SELECT MAX(sequence) FROM recording_fragments)
            BEGIN SELECT RAISE(ABORT,'injected late keyframe failure'); END;",
            )
            .await?;
        let before = state(connection).await?;
        let bytes = std::fs::read(fixture.root.join("recording.mp4"))?;
        startup::repair(connection, &fixture.paths).await?;
        assert_eq!(state(connection).await?, before);
        assert!(connection.is_autocommit()?);
        connection
            .execute_batch("DROP TRIGGER fail_second_startup_key")
            .await?;
        startup::repair(connection, &fixture.paths).await?;
        assert_eq!(
            number(connection, "SELECT finalized FROM recording_files").await?,
            1
        );
        assert_eq!(
            number(connection, "SELECT count(*) FROM recording_keyframes").await?,
            2
        );
        assert_eq!(
            number(connection, "SELECT count(*) FROM recording_event_keyframes").await?,
            1
        );
        let completed = state(connection).await?;
        startup::repair(connection, &fixture.paths).await?;
        assert_eq!(state(connection).await?, completed);
        assert_eq!(std::fs::read(fixture.root.join("recording.mp4"))?, bytes);
        Ok(())
    })
}

async fn seed_indexed_finalization(fixture: &Stopped) -> anyhow::Result<()> {
    let connection = &fixture.connection;
    let keys = catalog::read_legacy_keyframes(&fixture.root.join("recording.mp4"), "recording-1")?;
    for key in keys {
        connection
            .execute(
                "INSERT INTO recording_keyframes VALUES(?1,?2,?3,?4)",
                turso::params![
                    key.recording_id,
                    i64::try_from(key.fragment_sequence)?,
                    i64::try_from(key.byte_offset)?,
                    i64::try_from(key.byte_len)?
                ],
            )
            .await?;
    }
    connection
        .execute_batch(
            "UPDATE recording_files SET ended_at_ms=NULL,finalized_at_ms=NULL;
        DELETE FROM recording_coverage_ranges; DELETE FROM recording_coverage_files;",
        )
        .await?;
    Ok(())
}

#[test]
fn already_indexed_finalization_restores_end_time_timestamp_and_coverage() -> anyhow::Result<()> {
    pollster::block_on(async {
        let fixture = stopped("captured-indexed-finalization", false)?;
        let connection = &fixture.connection;
        seed_indexed_finalization(&fixture).await?;
        startup::repair(connection, &fixture.paths).await?;
        assert_eq!(
            number(connection, "SELECT finalized FROM recording_files").await?,
            1
        );
        assert_eq!(
            number(connection, "SELECT ended_at_ms FROM recording_files").await?,
            3000
        );
        assert!(number(connection, "SELECT finalized_at_ms FROM recording_files").await? > 0);
        assert_eq!(
            number(
                connection,
                "SELECT fragment_count FROM recording_coverage_files"
            )
            .await?,
            2
        );
        assert_eq!(
            number(
                connection,
                "SELECT coverage_ms FROM recording_coverage_files"
            )
            .await?,
            2000
        );
        assert_eq!(
            number(
                connection,
                "SELECT MIN(start_ms) FROM recording_coverage_ranges"
            )
            .await?,
            1000
        );
        assert_eq!(
            number(
                connection,
                "SELECT MAX(end_ms) FROM recording_coverage_ranges"
            )
            .await?,
            3000
        );
        let completed = state(connection).await?;
        startup::repair(connection, &fixture.paths).await?;
        assert_eq!(state(connection).await?, completed);
        Ok(())
    })
}

#[test]
fn rotating_startup_cursor_reaches_healthy_recording_after_unavailable_page() -> anyhow::Result<()>
{
    pollster::block_on(async {
        let fixture = stopped("captured-rotating-repair-cursor", true)?;
        let connection = &fixture.connection;
        connection.execute_batch("BEGIN IMMEDIATE").await?;
        for index in 0..64 {
            let id = format!("offline-{index:03}");
            let path = fixture.root.join(format!("{id}.mp4.active"));
            connection
                .execute(
                    "INSERT INTO recording_files
                (id,stream_id,started_at_ms,path,init_offset,init_len,finalized)
                VALUES(?1,'offline/main',1000,?2,0,8,0)",
                    turso::params![id, path.to_string_lossy().into_owned()],
                )
                .await?;
        }
        connection.execute_batch("COMMIT").await?;
        let before = state(connection).await?;
        let bytes = std::fs::read(fixture.root.join("recording.mp4"))?;
        startup::repair(connection, &fixture.paths).await?;
        assert_eq!(state(connection).await?, before);
        assert_eq!(
            number(
                connection,
                "SELECT after_id='offline-063' FROM storage_legacy_repair_cursor"
            )
            .await?,
            1
        );
        startup::repair(connection, &fixture.paths).await?;
        assert_eq!(
            number(
                connection,
                "SELECT count(*) FROM recording_keyframes WHERE recording_id='recording-1'"
            )
            .await?,
            2
        );
        assert_eq!(
            number(
                connection,
                "SELECT count(*) FROM recording_event_keyframes WHERE recording_id='recording-1'"
            )
            .await?,
            1
        );
        assert_eq!(number(connection, "SELECT count(*) FROM recording_files WHERE id LIKE 'offline-%' AND finalized=0 AND file_bytes=0").await?, 64);
        assert_eq!(
            number(connection, "SELECT count(*) FROM recording_files").await?,
            65
        );
        let completed = state(connection).await?;
        startup::repair(connection, &fixture.paths).await?;
        assert_eq!(state(connection).await?, completed);
        assert_eq!(std::fs::read(fixture.root.join("recording.mp4"))?, bytes);
        assert!(connection.is_autocommit()?);
        Ok(())
    })
}
