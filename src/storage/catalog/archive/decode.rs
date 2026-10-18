//! Decodes persisted archive jobs and their optional ownership evidence.

use super::*;

pub(super) fn job(row: &turso::Row) -> anyhow::Result<Job> {
    let object_id: String = row.get(4)?;
    Ok(Job {
        id: row.get(0)?,
        recording_id: row.get(1)?,
        kind: row.get(2)?,
        binding: Binding {
            id: row.get(3)?,
            root: PathBuf::from(row.get::<String>(22)?),
            identity: Identity {
                filesystem: row.get(23)?,
                directory: row.get(24)?,
            },
        },
        stage: stage(row, &object_id)?,
        restore_stage: restore_stage(row)?,
        cleanup_version: cleanup_version(row, &object_id)?,
        object_id,
        source_path: row.get(5)?,
        source_identity: row.get(6)?,
        bytes: to_u64(row.get(7)?, "archive bytes")?,
        digest: row
            .get::<Vec<u8>>(8)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid archive digest"))?,
        source_root: PathBuf::from(row.get::<String>(9)?),
        source_root_identity: Identity {
            filesystem: row.get(10)?,
            directory: row.get(11)?,
        },
        source_relative_key: row.get(12)?,
        source_operation: row.get(13)?,
        version: row.get(16)?,
        state: row.get(17)?,
        attempts: u32::try_from(row.get::<i64>(18)?)?,
        cancelled: row.get::<i64>(19)? != 0,
        local_evicted: u8::try_from(row.get::<i64>(20)?)?,
        error: row.get(21)?,
        chunk_hashes: row.get(25)?,
        action: row.get(26)?,
        action_attempts: u32::try_from(row.get::<i64>(27)?)?,
        source_retirement_operation: row.get(28)?,
        restore_operation: row.get(29)?,
        probe_acknowledged: row.get::<i64>(32)? != 0,
        cleanup_digest: cleanup_digest(row)?,
        cleanup_key: row.get(36)?,
        cleanup_complete: row.get::<i64>(37)? != 0,
        cleanup_acknowledged: row.get::<i64>(38)? != 0,
        export_job_id: row.get(39)?,
        export_metadata: row.get(40)?,
    })
}

fn stage(row: &turso::Row, object_id: &str) -> anyhow::Result<Option<Version>> {
    row.get::<Option<String>>(14)?
        .map(|token| {
            Version::new(
                crate::storage::archive::target::ObjectId::parse(object_id)?,
                token,
                to_u64(row.get(15)?, "staged bytes")?,
            )
        })
        .transpose()
}

fn restore_stage(row: &turso::Row) -> anyhow::Result<Option<Version>> {
    row.get::<Option<String>>(30)?
        .map(|token| {
            Version::new(
                restore_object(&row.get::<String>(12)?)?,
                token,
                to_u64(row.get(31)?, "restore staged bytes")?,
            )
        })
        .transpose()
}

fn cleanup_version(row: &turso::Row, object_id: &str) -> anyhow::Result<Option<Version>> {
    row.get::<Option<String>>(33)?
        .map(|token| {
            Version::new(
                if row.get::<String>(26)? == "restore" {
                    restore_object(&row.get::<String>(12)?)?
                } else {
                    crate::storage::archive::target::ObjectId::parse(object_id)?
                },
                token,
                to_u64(row.get(34)?, "cleanup bytes")?,
            )
        })
        .transpose()
}

fn cleanup_digest(row: &turso::Row) -> anyhow::Result<Option<[u8; 32]>> {
    row.get::<Option<Vec<u8>>>(35)?
        .map(|digest| {
            digest
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid cleanup digest"))
        })
        .transpose()
}
