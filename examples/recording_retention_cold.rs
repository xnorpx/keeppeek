//! Verifies cold migration of an actual pre-runtime archive without logging catalog contents.

use anyhow::{Context, Result, ensure};
use keeppeek::storage::RecordingCatalog;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::{path::Path, time::Instant};

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
struct Snapshot {
    table: &'static str,
    rows: u64,
    sha256: String,
}

fn main() -> Result<()> {
    let path = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .context("provide a closed pre-runtime generated archive")?,
    )
    .canonicalize()?;
    ensure!(
        path.is_file()
            && path.starts_with(std::env::temp_dir().canonicalize()?)
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("retention-scale-")),
        "cold migration is restricted to a closed generated retention-scale archive"
    );
    std::thread::Builder::new()
        .name("retention-cold".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(move || qualify(&path))?
        .join()
        .map_err(|_| anyhow::anyhow!("cold archive migration panicked"))?
}

fn snapshots(path: &Path, require_old: bool) -> Result<Vec<Snapshot>> {
    let database = pollster::block_on(
        turso::Builder::new_local(path.to_str().context("non-UTF8 fixture path")?).build(),
    )?;
    let connection = database.connect()?;
    if require_old {
        let mut rows = pollster::block_on(connection.query(
            "SELECT 1 FROM sqlite_schema WHERE name='recording_retention_runtime'",
            (),
        ))?;
        ensure!(
            pollster::block_on(rows.next())?.is_none(),
            "fixture already has runtime retention schema"
        );
    }
    let mut rows = pollster::block_on(connection.query(
        "SELECT deadline_ms FROM recording_retention_decisions WHERE recording_id='cam-000-00006-main'", ()))?;
    let prior =
        pollster::block_on(rows.next())?.context("known pre-migration commitment is missing")?;
    ensure!(
        prior.get::<Option<i64>>(0)? == Some(3_000_000_000_000 + 7 * 1_800_000 + 31 * 86_400_000),
        "known 31-day committed floor changed or is NULL"
    );
    drop(rows);
    let mut snapshots = Vec::with_capacity(6);
    for (table, sql) in [
        (
            "recording_files",
            "SELECT id,stream_id,source_id,logical_stream_id,started_at_ms,ended_at_ms,path,init_offset,init_len,finalized,finalized_at_ms,file_identity,file_bytes,protected,cleanup_pending FROM recording_files ORDER BY id",
        ),
        (
            "recording_fragments",
            "SELECT recording_id,sequence,start_ms,duration_ms,byte_offset,byte_len,random_access FROM recording_fragments ORDER BY recording_id,sequence",
        ),
        (
            "recording_keyframes",
            "SELECT recording_id,fragment_sequence,byte_offset,byte_len FROM recording_keyframes ORDER BY recording_id,fragment_sequence",
        ),
        (
            "recording_events",
            "SELECT id,revision,publication_id,publication_fingerprint,camera_id,stream,source,kind,start_time_ms,end_time_ms,confidence,bbox_json,bbox_attachment_id,zone,text,payload_json,attachments_json,canonical_attachment_id,icon_key,rejected_icon_key,thumbnail_filename,search_revision FROM recording_events ORDER BY id",
        ),
        (
            "recording_retention_decisions",
            "SELECT recording_id,policy_revision,hex(policy_fingerprint),event_revision,deadline_ms,matching_rules_json,reason_json FROM recording_retention_decisions ORDER BY recording_id",
        ),
    ] {
        snapshots.push(digest_rows(&connection, table, sql)?);
    }
    snapshots.push(encoded_media(
        &connection,
        path.parent().context("fixture parent missing")?,
    )?);
    Ok(snapshots)
}

fn encoded_media(connection: &turso::Connection, root: &Path) -> Result<Snapshot> {
    let mut rows = pollster::block_on(connection.query(
        "SELECT id,path,file_bytes FROM recording_files WHERE file_bytes>0 ORDER BY id",
        (),
    ))?;
    let mut digest = Sha256::new();
    let began = Instant::now();
    for count in 0..=4096 {
        ensure!(
            began.elapsed() < std::time::Duration::from_secs(600),
            "media snapshot elapsed limit exceeded"
        );
        let Some(row) = pollster::block_on(rows.next())? else {
            return Ok(Snapshot {
                table: "encoded_media",
                rows: count,
                sha256: digest
                    .finalize()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            });
        };
        ensure!(count < 4096, "media file snapshot count exceeded");
        let id: String = row.get(0)?;
        ensure!(id.len() <= 256, "media identity exceeds fixture limit");
        let media = std::path::PathBuf::from(row.get::<String>(1)?).canonicalize()?;
        ensure!(
            media.starts_with(root),
            "encoded media escaped the generated fixture"
        );
        let bytes = u64::try_from(row.get::<i64>(2)?)?;
        digest.update(u64::try_from(id.len())?.to_le_bytes());
        digest.update(id.as_bytes());
        digest.update(bytes.to_le_bytes());
        hash_media(&mut digest, &media, bytes)?;
    }
    anyhow::bail!("media file snapshot exceeds fixture limit")
}

fn hash_media(digest: &mut Sha256, path: &Path, expected: u64) -> Result<()> {
    ensure!(
        expected <= 16_777_216,
        "encoded fixture file exceeds size limit"
    );
    let mut file = std::fs::File::open(path)?;
    ensure!(
        file.metadata()?.len() == expected,
        "encoded media length differs from catalog"
    );
    let mut buffer = [0_u8; 65_536];
    let mut observed = 0_u64;
    for _ in 0..=256 {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            ensure!(observed == expected, "encoded media changed while hashing");
            return Ok(());
        }
        observed += u64::try_from(count)?;
        ensure!(observed <= expected, "encoded media grew while hashing");
        digest.update(&buffer[..count]);
    }
    anyhow::bail!("encoded fixture exceeded read work limit")
}

fn digest_rows(connection: &turso::Connection, table: &'static str, sql: &str) -> Result<Snapshot> {
    let mut rows = pollster::block_on(connection.query(sql, ()))?;
    let mut digest = Sha256::new();
    let began = Instant::now();
    for count in 0_u64..=1_000_000 {
        ensure!(
            began.elapsed() < std::time::Duration::from_secs(600),
            "snapshot elapsed limit exceeded"
        );
        let Some(row) = pollster::block_on(rows.next())? else {
            return Ok(Snapshot {
                table,
                rows: count,
                sha256: digest
                    .finalize()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            });
        };
        ensure!(
            count < 1_000_000 && row.column_count() <= 32,
            "snapshot work limit exceeded"
        );
        digest.update(u64::try_from(row.column_count())?.to_le_bytes());
        for column in 0..row.column_count() {
            let value = format!("{:?}", row.get_value(column)?);
            ensure!(
                value.len() <= 65_536,
                "snapshot value exceeds fixture limit"
            );
            digest.update(u64::try_from(value.len())?.to_le_bytes());
            digest.update(value.as_bytes());
        }
    }
    anyhow::bail!("snapshot exceeds one million rows")
}

fn qualify(path: &Path) -> Result<()> {
    let before = snapshots(path, true)?;
    ensure!(
        before[0].rows >= 365_760 && before[3].rows == 91_475,
        "cold fixture does not contain the matching 127-camera/30-day archive"
    );
    ensure!(
        before[4].rows > 0,
        "cold fixture has no pre-existing committed floor"
    );
    ensure!(
        before[5].rows == 560,
        "cold fixture has incorrect real-media count"
    );
    let started = Instant::now();
    let mut catalog = RecordingCatalog::open(path)?;
    let open_us = u64::try_from(started.elapsed().as_micros())?;
    catalog.wait_for_maintenance();
    let open_and_maintenance_us = u64::try_from(started.elapsed().as_micros())?;
    let index_current_before_restart = catalog.handle().reconcile_retention_events(256)?;
    catalog.shutdown();
    let after = snapshots(path, false)?;
    ensure!(
        before == after,
        "cold schema migration changed legacy media metadata, holds, evidence or obligations"
    );
    let mut reopened = RecordingCatalog::open(path)?;
    reopened.wait_for_maintenance();
    let index_started = Instant::now();
    let mut index_complete = false;
    let mut index_batches = 0;
    for _ in 0..before[3].rows.div_ceil(256) + 2 {
        index_batches += 1;
        if reopened.handle().reconcile_retention_events(256)? {
            index_complete = true;
            break;
        }
    }
    ensure!(
        index_complete,
        "cold event index did not resume and finish within the work limit"
    );
    let index_resume_us = u64::try_from(index_started.elapsed().as_micros())?;
    reopened.shutdown();
    let restarted = snapshots(path, false)?;
    ensure!(
        before == restarted,
        "restart changed migrated legacy archive state"
    );
    let report = serde_json::json!({"complete":true,"before":before,"after":after,"restarted":restarted,
        "open_us":open_us,"open_and_maintenance_us":open_and_maintenance_us,
        "index_current_before_restart":index_current_before_restart,"index_resumed_batches":index_batches,
        "index_resume_us":index_resume_us,"index_complete":index_complete,
        "scope":"actual pre-runtime generated b877 catalog; exact ordered digests of all legacy fields, fragments, keyframes, canonical evidence, protections, committed decisions and 560 encoded H264 files before/after cold runtime schema migration and maintenance; index readiness recorded before restart and verified afterward; new derived columns excluded; historical media is synthetic"});
    std::fs::write(
        path.parent()
            .context("fixture parent missing")?
            .join("cold-migration-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
