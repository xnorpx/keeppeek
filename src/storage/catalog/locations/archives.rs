//! Captures automatic archive intent before a recording writer creates its file.

use super::{Allocation, Kind, Location, Object, Reply, identifier, moves, ownership};
use crate::storage::volumes::{CANDIDATES_MAX, RULES_MAX, VolumeConfiguration, VolumeRole};
use serde::{Deserialize, Serialize};

const POLICY_BYTES_MAX: usize = 131_072;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub configuration: VolumeConfiguration,
    pub source: String,
    pub groups: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Intent {
    pub id: String,
    pub policy: Policy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: String,
    pub source: Location,
    pub policy: Policy,
}

impl Intent {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            uuid::Uuid::parse_str(&self.id).is_ok(),
            "invalid archive job ID"
        );
        self.policy.validate()
    }
}

impl Policy {
    fn validate(&self) -> anyhow::Result<()> {
        identifier(&self.source)?;
        anyhow::ensure!(
            self.groups.len() <= RULES_MAX,
            "too many archive source groups"
        );
        for group in &self.groups {
            identifier(group)?;
        }
        anyhow::ensure!(
            self.configuration.volumes.len() <= CANDIDATES_MAX,
            "too many archive candidates"
        );
        anyhow::ensure!(
            self.configuration.placement.len() == 1
                && self.configuration.placement[0].role == VolumeRole::Archive,
            "archive requires one captured archive rule"
        );
        self.configuration.validate()?;
        anyhow::ensure!(
            serde_json::to_vec(self)?.len() <= POLICY_BYTES_MAX,
            "archive policy is too large"
        );
        Ok(())
    }
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS storage_volume_archives (
        id TEXT PRIMARY KEY,
        operation TEXT NOT NULL UNIQUE REFERENCES storage_volume_allocations(operation),
        policy TEXT NOT NULL CHECK(length(policy) <= 131072),
        done INTEGER NOT NULL DEFAULT 0 CHECK(done IN (0,1))
    );
    CREATE INDEX IF NOT EXISTS storage_volume_archive_pending ON storage_volume_archives(id) WHERE done = 0;").await?;
    Ok(())
}

pub(super) async fn reserve(
    connection: &turso::Connection,
    allocation: &Allocation,
    intent: &Intent,
) -> anyhow::Result<Reply> {
    let policy = serde_json::to_string(&intent.policy)?;
    let mut rows = connection
        .query(
            "SELECT q.id, q.policy FROM storage_volume_allocations a
        LEFT JOIN storage_volume_archives q ON q.operation = a.operation WHERE a.operation = ?1",
            [allocation.operation.as_str()],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        anyhow::ensure!(
            row.get::<Option<String>>(0)?.as_deref() == Some(&intent.id)
                && row.get::<Option<String>>(1)?.as_deref() == Some(&policy),
            "archive reservation intent changed"
        );
        drop(rows);
        return super::reserve(connection, allocation).await;
    }
    drop(rows);
    let reply = super::reserve(connection, allocation).await?;
    connection
        .execute(
            "INSERT INTO storage_volume_archives(id,operation,policy) VALUES (?1,?2,?3)",
            turso::params![intent.id.clone(), allocation.operation.clone(), policy],
        )
        .await?;
    Ok(reply)
}

pub(super) async fn pending(
    connection: &turso::Connection,
    page: &moves::Page,
) -> anyhow::Result<Vec<String>> {
    // ponytail: Both kinds of work share one bounded scan and one local worker.
    let mut rows = connection.query("SELECT id FROM (
        SELECT id FROM storage_volume_moves WHERE phase NOT IN ('complete','cancelled') OR receipt_acknowledged = 0
        UNION ALL
        SELECT q.id FROM storage_volume_archives q JOIN storage_volume_allocations a ON a.operation = q.operation
            WHERE q.done = 0 AND a.state = 'published'
        ) WHERE (?1 IS NULL OR id > ?1) ORDER BY id LIMIT ?2",
        turso::params![page.after.clone(), i64::from(page.limit)]).await?;
    let mut ids = Vec::with_capacity(usize::from(page.limit));
    while let Some(row) = rows.next().await? {
        ids.push(row.get::<String>(0)?);
    }
    Ok(ids)
}

pub(super) async fn load(connection: &turso::Connection, id: &str) -> anyhow::Result<Option<Job>> {
    let mut rows = connection
        .query(
            "SELECT a.object_id, q.policy FROM storage_volume_archives q
        JOIN storage_volume_allocations a ON a.operation = q.operation
        WHERE q.id = ?1 AND q.done = 0 AND a.state = 'published'",
            [id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let object = Object {
        kind: Kind::Recording,
        id: row.get::<String>(0)?,
    };
    let encoded = row.get::<String>(1)?;
    anyhow::ensure!(
        encoded.len() <= POLICY_BYTES_MAX,
        "archive policy is too large"
    );
    let policy: Policy = serde_json::from_str(&encoded)?;
    policy.validate()?;
    drop(rows);
    let Reply::Location(Some(source)) = ownership::lookup(connection, &object).await? else {
        anyhow::bail!("archive source is not published");
    };
    Ok(Some(Job {
        id: id.to_owned(),
        source,
        policy,
    }))
}

pub(super) async fn begin_move(
    connection: &turso::Connection,
    intent: &moves::Intent,
) -> anyhow::Result<moves::Job> {
    if let Some(job) = load(connection, &intent.id).await? {
        anyhow::ensure!(
            job.source.object == intent.object && job.source.revision == intent.expected_revision,
            "archive source changed before admission"
        );
        connection
            .execute(
                "UPDATE storage_volume_archives SET done = 1 WHERE id = ?1",
                [intent.id.as_str()],
            )
            .await?;
    }
    let mut pending = connection.query("SELECT 1 FROM storage_volume_archives q JOIN storage_volume_allocations a ON a.operation = q.operation WHERE q.done = 0 AND a.kind = ?1 AND a.object_id = ?2", turso::params![intent.object.kind.as_str(), intent.object.id.clone()]).await?;
    anyhow::ensure!(
        pending.next().await?.is_none(),
        "pending archive owns this recording"
    );
    drop(pending);
    // A refused destination rolls back the archive acknowledgement in the same transaction.
    moves::begin(connection, intent).await
}

pub(super) async fn complete(
    connection: &turso::Connection,
    id: &str,
    source: &Location,
) -> anyhow::Result<Reply> {
    if let Some(job) = load(connection, id).await? {
        anyhow::ensure!(
            job.source == *source,
            "archive source changed before completion"
        );
        connection
            .execute(
                "UPDATE storage_volume_archives SET done = 1 WHERE id = ?1",
                [id],
            )
            .await?;
        super::bump_revision(connection).await?;
    }
    Ok(Reply::Bound)
}
