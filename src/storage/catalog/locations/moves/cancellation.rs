use super::{Job, Object, Reply, bump_revision, ownership, transition};

/// Immutable evidence captured before removing a cancelled destination.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Cancellation {
    Empty,
    File {
        relative_key: String,
        bytes: u64,
        file_identity: String,
        digest: [u8; 32],
    },
}

impl std::fmt::Debug for Cancellation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::File { bytes, .. } => f
                .debug_struct("File")
                .field("bytes", bytes)
                .finish_non_exhaustive(),
        }
    }
}

pub(super) async fn verify(
    connection: &turso::Connection,
    job: &Job,
    evidence: &Cancellation,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        job.cancellation_requested,
        "move cancellation was not requested"
    );
    if let Some(existing) = &job.cancellation {
        anyhow::ensure!(existing == evidence, "cancellation evidence changed");
        return Ok(());
    }
    anyhow::ensure!(
        matches!(
            job.phase.as_str(),
            "reserved" | "verified" | "file_published"
        ),
        "move cannot accept cancellation evidence"
    );
    ensure_source(connection, job).await?;
    validate_evidence(job, evidence)?;
    let value = serde_json::to_string(evidence)?;
    anyhow::ensure!(value.len() <= 2048, "cancellation evidence is too large");
    connection
        .execute(
            "UPDATE storage_volume_moves SET cancellation = ?2 WHERE id = ?1",
            turso::params![job.id.clone(), value],
        )
        .await?;
    bump_revision(connection).await
}

fn validate_evidence(job: &Job, evidence: &Cancellation) -> anyhow::Result<()> {
    match evidence {
        Cancellation::Empty => anyhow::ensure!(
            job.destination.file_identity.is_none()
                && job.destination.materialized_bytes == 0
                && job.phase == "reserved",
            "move already owns a destination file"
        ),
        Cancellation::File {
            relative_key,
            bytes,
            file_identity,
            digest,
        } => {
            super::super::validate_key(relative_key)?;
            super::super::identifier(file_identity)?;
            anyhow::ensure!(
                relative_key == &job.destination.relative_key
                    || relative_key == &format!("{}.tmp", job.id),
                "cancellation key changed"
            );
            anyhow::ensure!(
                Some(file_identity) == job.destination.file_identity.as_ref()
                    && *bytes >= job.destination.materialized_bytes
                    && *bytes <= job.destination.bytes,
                "cancellation file evidence changed"
            );
            if job.phase != "reserved" {
                anyhow::ensure!(
                    *bytes == job.source.bytes && digest == &job.source.digest,
                    "verified cancellation bytes changed"
                );
            }
            if job.phase == "file_published" {
                anyhow::ensure!(
                    relative_key == &job.destination.relative_key,
                    "published cancellation key changed"
                );
            }
        }
    }
    Ok(())
}

async fn ensure_source(connection: &turso::Connection, job: &Job) -> anyhow::Result<()> {
    let object: &Object = &job.object;
    anyhow::ensure!(
        ownership::lookup(connection, object).await? == Reply::Location(Some(job.source.clone())),
        "cancelled move source authority changed"
    );
    Ok(())
}

pub(super) async fn complete(connection: &turso::Connection, job: &Job) -> anyhow::Result<()> {
    anyhow::ensure!(
        job.cancellation_requested && job.cancellation.is_some(),
        "cancellation removal is not verified"
    );
    if job.phase == "cancelled" {
        return Ok(());
    }
    anyhow::ensure!(
        matches!(
            job.phase.as_str(),
            "reserved" | "verified" | "file_published"
        ),
        "published move cannot be cancelled"
    );
    ensure_source(connection, job).await?;
    let changed = connection.execute("UPDATE storage_volume_allocations SET state = 'cancelled' WHERE operation = ?1 AND state = 'reserved'", [job.destination_operation.as_str()]).await?;
    anyhow::ensure!(changed == 1, "cancelled destination ownership changed");
    transition(connection, job, &job.phase, "cancelled").await
}
