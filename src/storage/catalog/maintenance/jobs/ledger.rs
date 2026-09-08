use super::{Failure, Job, Object, ObjectState, State, check_deadline};
use crate::storage::catalog::maintenance::MAX_RECORDINGS;
use std::time::Instant;

pub(super) async fn enqueue(
    connection: &turso::Connection,
    job: &Job,
    deadline: Instant,
) -> anyhow::Result<()> {
    anyhow::ensure!(job.state == State::Prepared, Failure::InvalidState);
    anyhow::ensure!(job.objects.is_empty(), Failure::InvalidState);
    anyhow::ensure!(
        job.snapshot.recordings.len() <= MAX_RECORDINGS,
        Failure::Invalid
    );
    check_deadline(deadline)?;
    let mut statement = connection
        .prepare(
            "INSERT INTO recording_maintenance_objects (job_id, ordinal, recording_id, state)
         VALUES (?1, ?2, ?3, 'queued')",
        )
        .await?;
    for (ordinal, recording) in job.snapshot.recordings.iter().enumerate() {
        check_deadline(deadline)?;
        statement
            .execute(turso::params![
                job.id.as_str(),
                i64::try_from(ordinal)?,
                recording.recording_id.as_str()
            ])
            .await?;
    }
    Ok(())
}

pub(super) async fn read(
    connection: &turso::Connection,
    job: &Job,
    deadline: Instant,
) -> anyhow::Result<Vec<Object>> {
    check_deadline(deadline)?;
    let confirmed = job.confirmed_at_ms.is_some();
    let expected_state = match job.state {
        State::Queued => {
            anyhow::ensure!(confirmed, Failure::Invalid);
            ObjectState::Queued
        }
        State::Cancelled => ObjectState::Cancelled,
        State::Prepared | State::Expired => {
            anyhow::ensure!(!confirmed, Failure::Invalid);
            ObjectState::Queued
        }
    };
    let expected_count = if confirmed {
        job.snapshot.recordings.len()
    } else {
        0
    };
    let mut rows = connection
        .query(
            "SELECT ordinal, recording_id, state FROM recording_maintenance_objects
         WHERE job_id = ?1 ORDER BY ordinal LIMIT ?2",
            turso::params![job.id.as_str(), i64::try_from(MAX_RECORDINGS + 1)?],
        )
        .await?;
    let mut objects = Vec::with_capacity(expected_count);
    while let Some(row) = rows.next().await? {
        check_deadline(deadline)?;
        anyhow::ensure!(objects.len() < expected_count, Failure::Invalid);
        anyhow::ensure!(
            row.get::<i64>(0)? == i64::try_from(objects.len())?,
            Failure::Invalid
        );
        let recording_id: String = row.get(1)?;
        anyhow::ensure!(
            recording_id == job.snapshot.recordings[objects.len()].recording_id,
            Failure::Invalid
        );
        let state = match row.get::<String>(2)?.as_str() {
            "queued" => ObjectState::Queued,
            "cancelled" => ObjectState::Cancelled,
            _ => return Err(Failure::Invalid.into()),
        };
        anyhow::ensure!(state == expected_state, Failure::Invalid);
        objects.push(Object {
            recording_id,
            state,
        });
    }
    anyhow::ensure!(objects.len() == expected_count, Failure::Invalid);
    check_deadline(deadline)?;
    Ok(objects)
}
