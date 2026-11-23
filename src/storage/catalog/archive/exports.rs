//! Metadata needed to discover a verified export without opening its archived media.

use super::*;

#[derive(Debug)]
pub struct Description {
    pub original_id: String,
    pub artifact_id: String,
    pub digest: [u8; 32],
    pub metadata: String,
}

pub(super) async fn list(
    connection: &turso::Connection,
    requester: &str,
    retained: &[String],
) -> anyhow::Result<Reply> {
    let retained = serde_json::to_string(retained)?;
    let mut rows = connection
        .query(
            "SELECT export_job_id,recording_id,digest,export_metadata
             FROM archive_replicas INDEXED BY archive_ready_export_owner
             WHERE kind='export' AND state='ready' AND json_valid(export_metadata)
               AND version IS NOT NULL AND json_extract(export_metadata,'$.requester_id')=?1
               AND length(CAST(export_metadata AS BLOB)) BETWEEN 1 AND 65536
               AND export_job_id NOT IN (SELECT value FROM json_each(?2))
             ORDER BY json_extract(export_metadata,'$.created_at_ms') DESC,export_job_id
             LIMIT 64",
            turso::params![requester, retained],
        )
        .await?;
    let mut descriptions = Vec::new();
    while let Some(row) = rows.next().await? {
        descriptions.push(Description {
            original_id: row.get(0)?,
            artifact_id: row.get(1)?,
            digest: row
                .get::<Vec<u8>>(2)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid archived export digest"))?,
            metadata: row.get(3)?,
        });
    }
    Ok(Reply::ExportDescriptions(descriptions))
}
