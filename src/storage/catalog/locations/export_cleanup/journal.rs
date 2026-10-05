use super::{Cancellation, Job, Owned};
use crate::storage::catalog::locations::{bump_revision, to_u64};

pub(super) async fn load(connection: &turso::Connection, id: &str) -> anyhow::Result<Option<Job>> {
    let mut rows = connection
        .query(
            "SELECT evidence,complete,acknowledged FROM storage_export_cleanup WHERE object_id=?1",
            [id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let encoded = row.get::<Option<String>>(0)?;
    let evidence = encoded
        .map(|value| {
            anyhow::ensure!(value.len() <= 2048, "export evidence is too large");
            Ok::<_, anyhow::Error>(serde_json::from_str(&value)?)
        })
        .transpose()?;
    let mut job = Job {
        id: id.into(),
        allocation: None,
        evidence,
        complete: row.get::<i64>(1)? != 0,
        acknowledged: row.get::<i64>(2)? != 0,
    };
    drop(rows);
    let mut moves = connection.query("SELECT 1 FROM storage_volume_moves WHERE kind='export' AND object_id=?1 AND (phase NOT IN ('complete','cancelled') OR receipt_acknowledged=0)", [id]).await?;
    anyhow::ensure!(moves.next().await?.is_none(), "move still owns this export");
    drop(moves);
    job.allocation = allocation(connection, id).await?;
    Ok(Some(job))
}

async fn allocation(connection: &turso::Connection, id: &str) -> anyhow::Result<Option<Owned>> {
    let mut rows = connection.query("SELECT operation,volume_id,generation,relative_key,bytes,materialized_bytes,file_identity,digest FROM storage_volume_allocations WHERE kind='export' AND object_id=?1", [id]).await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(Owned {
        operation: row.get(0)?,
        volume: row.get(1)?,
        generation: to_u64(row.get(2)?, "export generation")?,
        relative_key: row.get(3)?,
        bytes: to_u64(row.get(4)?, "export bytes")?,
        materialized_bytes: to_u64(row.get(5)?, "export materialized bytes")?,
        file_identity: row.get(6)?,
        digest: row
            .get::<Option<Vec<u8>>>(7)?
            .map(|bytes| {
                bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("invalid export digest"))
            })
            .transpose()?,
    }))
}

pub(super) async fn verify(
    connection: &turso::Connection,
    id: &str,
    evidence: &Cancellation,
) -> anyhow::Result<()> {
    let job = load(connection, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("export cleanup missing"))?;
    if let Some(existing) = job.evidence {
        anyhow::ensure!(existing == *evidence, "export cleanup evidence changed");
        return Ok(());
    }
    validate(job.allocation.as_ref(), evidence)?;
    let encoded = serde_json::to_string(evidence)?;
    anyhow::ensure!(encoded.len() <= 2048, "export evidence is too large");
    connection
        .execute(
            "UPDATE storage_export_cleanup SET evidence=?2 WHERE object_id=?1",
            turso::params![id, encoded],
        )
        .await?;
    bump_revision(connection).await
}

fn validate(allocation: Option<&Owned>, evidence: &Cancellation) -> anyhow::Result<()> {
    match evidence {
        Cancellation::Empty => anyhow::ensure!(
            allocation
                .is_none_or(|owned| owned.file_identity.is_none() && owned.materialized_bytes == 0),
            "export owns a file"
        ),
        Cancellation::File {
            relative_key,
            bytes,
            file_identity,
            digest,
        } => {
            let owned = allocation.ok_or_else(|| anyhow::anyhow!("export allocation missing"))?;
            anyhow::ensure!(
                relative_key == &owned.relative_key
                    && Some(file_identity) == owned.file_identity.as_ref()
                    && *bytes >= owned.materialized_bytes
                    && *bytes <= owned.bytes,
                "export cleanup evidence changed"
            );
            if let Some(expected) = owned.digest {
                anyhow::ensure!(
                    *bytes == owned.bytes && *digest == expected,
                    "published export evidence changed"
                );
            }
        }
    }
    Ok(())
}
