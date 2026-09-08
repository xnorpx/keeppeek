use super::{Report, Row, validate_current};
use crate::storage::{
    catalog::{
        self, BUSY_TIMEOUT, Command, RecordingCatalogHandle,
        maintenance::{FileIdentity, check_deadline, jobs::Failure},
    },
    long_term::inspection::{
        Archive, Observation,
        container::{Fragment, Index},
    },
};
use std::{sync::mpsc, time::Instant};

pub(in crate::storage::catalog) struct Evidence {
    archive: Archive,
    observation: Observation,
}

pub(super) fn submit(
    catalog: &RecordingCatalogHandle,
    report: &Report,
    expected: &Row,
    archive: &Archive,
) -> anyhow::Result<()> {
    let deadline = (Instant::now() + BUSY_TIMEOUT).min(report.deadline);
    let observation = archive.inspect_until(
        expected.path.as_ref().ok_or(Failure::Invalid)?,
        expected.bytes,
        deadline,
    )?;
    anyhow::ensure!(
        Some(FileIdentity::from_observed(observation.identity())) == expected.identity,
        Failure::Conflict
    );
    let index = archive
        .container_index(&observation, deadline)
        .map_err(|_| Failure::Invalid)?;
    let (reply, response) = mpsc::sync_channel(1);
    catalog
        .tx
        .try_send(Command::ReindexRecording {
            expected: expected.clone(),
            revision: report.revision,
            index,
            evidence: Evidence {
                archive: archive.try_clone()?,
                observation,
            },
            deadline,
            reply,
        })
        .map_err(|_| Failure::Unavailable)?;
    response
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| Failure::Unavailable)?
}

pub(in crate::storage::catalog) async fn apply(
    connection: &turso::Connection,
    expected: &Row,
    revision: u64,
    index: Index,
    evidence: Evidence,
    deadline: Instant,
) -> anyhow::Result<()> {
    check_deadline(deadline)?;
    evidence
        .archive
        .revalidate_until(&evidence.observation, deadline)
        .map_err(|_| Failure::Conflict)?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        validate_current(connection, expected, revision).await?;
        anyhow::ensure!(
            super::fingerprint::read(connection, &expected.id, deadline).await?
                == expected.index_fingerprint,
            Failure::Conflict
        );
        connection
            .execute(
                "DELETE FROM recording_fragments WHERE recording_id = ?1",
                turso::params![expected.id.as_str()],
            )
            .await?;
        for fragment in index.fragments {
            check_deadline(deadline)?;
            insert(connection, expected, fragment).await?;
        }
        connection
            .execute(
                "UPDATE recording_files SET init_offset = ?2, init_len = ?3 WHERE id = ?1",
                turso::params![
                    expected.id.as_str(),
                    i64::try_from(index.initialization.offset)?,
                    i64::try_from(index.initialization.size)?
                ],
            )
            .await?;
        catalog::rebuild_recording_coverage(connection, &expected.id).await?;
        catalog::bump_catalog_revision(connection).await?;
        check_deadline(deadline)?;
        evidence
            .archive
            .revalidate_until(&evidence.observation, deadline)
            .map_err(|_| Failure::Conflict)?;
        connection.execute_batch("COMMIT").await?;
        anyhow::Ok(())
    }
    .await;
    if result.is_err() && !connection.is_autocommit()? {
        connection.execute_batch("ROLLBACK").await?;
    }
    result
}

async fn insert(
    connection: &turso::Connection,
    expected: &Row,
    fragment: Fragment,
) -> anyhow::Result<()> {
    let start_ms = expected
        .started_ms
        .checked_add(i64::try_from(fragment.start_ms)?)
        .ok_or(Failure::Invalid)?;
    let end_ms = start_ms
        .checked_add(i64::try_from(fragment.duration_ms)?)
        .ok_or(Failure::Invalid)?;
    anyhow::ensure!(
        expected.ended_ms.is_some_and(|end| end_ms <= end),
        Failure::Conflict
    );
    let sequence = u64::from(fragment.first_sample.sequence_number);
    catalog::insert_fragment(
        connection,
        catalog::CatalogFragment {
            recording_id: expected.id.clone(),
            sequence,
            start_ms,
            duration_ms: fragment.duration_ms,
            byte_offset: fragment.range.offset,
            byte_len: fragment.range.size,
            random_access: true,
        },
    )
    .await?;
    connection.execute("INSERT INTO recording_keyframes (recording_id, fragment_sequence, byte_offset, byte_len) VALUES (?1, ?2, ?3, ?4)",
        turso::params![expected.id.as_str(), i64::try_from(sequence)?, i64::try_from(fragment.first_sample.location.offset)?, i64::from(fragment.first_sample.location.size)]).await?;
    catalog::reconcile_events_for_fragment(connection, &expected.id, sequence).await
}

#[cfg(test)]
mod tests;
