//! Publishes event attachments and their owned files in one catalog transaction.

use super::{Kind, Location, Object, Publication, Reply, ownership};
use crate::storage::{catalog::EventPublicationIdentity, metadata::TimelineEvent};
use sha2::{Digest, Sha256};
pub(super) mod pressure;
pub(super) mod recovery;
pub mod retirement;

#[derive(Debug, Clone)]
pub struct Image {
    pub attachment_id: String,
    pub object_id: String,
    pub evidence: Publication,
}

#[derive(Debug, Clone)]
pub struct Commit {
    pub event: TimelineEvent,
    pub(crate) publication: Option<EventPublicationIdentity>,
    pub images: Vec<Image>,
}

impl Commit {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        super::identifier(&self.event.id)?;
        anyhow::ensure!(
            (1..=16).contains(&self.images.len()),
            "invalid image publication count"
        );
        let mut attachments = std::collections::HashSet::new();
        for image in &self.images {
            super::identifier(&image.attachment_id)?;
            uuid::Uuid::parse_str(&image.object_id)?;
            super::validate_publication(&image.evidence)?;
            anyhow::ensure!(
                attachments.insert(&image.attachment_id),
                "duplicate image attachment"
            );
            anyhow::ensure!(
                self.event.attachments.iter().any(|descriptor| {
                    descriptor.id == image.attachment_id
                        && descriptor.content_type == "image/jpeg"
                        && descriptor.byte_len == Some(image.evidence.bytes)
                }),
                "image publication does not match its descriptor"
            );
        }
        let canonical = self
            .images
            .iter()
            .find(|image| {
                self.event.canonical_attachment_id.as_deref() == Some(&image.attachment_id)
            })
            .ok_or_else(|| anyhow::anyhow!("canonical image publication is missing"))?;
        anyhow::ensure!(
            self.event.thumbnail_filename.as_deref()
                == Some(&format!("{}.jpg", canonical.object_id)),
            "canonical image filename differs from its allocation"
        );
        Ok(())
    }
}

pub(super) async fn initialize(connection: &turso::Connection) -> anyhow::Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS storage_event_images (
        event_id TEXT NOT NULL,
        attachment_id TEXT NOT NULL,
        event_revision INTEGER NOT NULL CHECK(event_revision > 0),
        object_id TEXT NOT NULL UNIQUE,
        operation TEXT NOT NULL UNIQUE REFERENCES storage_volume_allocations(operation),
        intent BLOB NOT NULL CHECK(length(intent) = 32),
        active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
        PRIMARY KEY(event_id, attachment_id, event_revision)
    );
    CREATE UNIQUE INDEX IF NOT EXISTS storage_event_image_current ON storage_event_images(event_id, attachment_id) WHERE active = 1;").await?;
    retirement::initialize(connection).await?;
    pressure::initialize(connection).await?;
    Ok(())
}

pub(super) async fn commit(
    connection: &turso::Connection,
    mut commit: Commit,
) -> anyhow::Result<Reply> {
    super::super::event_write::prepare(&mut commit.event)?;
    commit.validate()?;
    let intent = intent_digest(&commit)?;
    if committed(connection, &commit, &intent).await? {
        return Ok(Reply::Bound);
    }
    super::super::event_write::write(connection, &commit.event, commit.publication.as_ref())
        .await?;
    for image in &commit.images {
        let Reply::Location(Some(location)) =
            ownership::publish(connection, &image.evidence).await?
        else {
            anyhow::bail!("image publication has no location");
        };
        anyhow::ensure!(
            location.object.kind == Kind::Thumbnail && location.object.id == image.object_id,
            "image allocation identity differs"
        );
        connection
            .execute(
                "UPDATE storage_event_images SET active=0 WHERE event_id=?1 AND attachment_id=?2",
                (commit.event.id.as_str(), image.attachment_id.as_str()),
            )
            .await?;
        connection.execute("INSERT INTO storage_event_images(event_id,attachment_id,event_revision,object_id,operation,intent) VALUES (?1,?2,?3,?4,?5,?6)",
            turso::params![commit.event.id.clone(),image.attachment_id.clone(),
                super::to_i64(commit.event.revision,"event revision")?, image.object_id.clone(),
                image.evidence.operation.clone(), intent.to_vec()]).await?;
    }
    Ok(Reply::Bound)
}

fn intent_digest(commit: &Commit) -> anyhow::Result<[u8; 32]> {
    let evidence = commit
        .images
        .iter()
        .map(|image| {
            (
                &image.attachment_id,
                &image.object_id,
                &image.evidence.operation,
                image.evidence.bytes,
                &image.evidence.file_identity,
                image.evidence.digest,
            )
        })
        .collect::<Vec<_>>();
    let publication = commit
        .publication
        .as_ref()
        .map(|identity| (&identity.publication_id, &identity.fingerprint));
    Ok(Sha256::digest(serde_json::to_vec(&(&commit.event, publication, evidence))?).into())
}

async fn committed(
    connection: &turso::Connection,
    commit: &Commit,
    intent: &[u8; 32],
) -> anyhow::Result<bool> {
    let mut rows = connection.query("SELECT intent FROM storage_event_images WHERE event_id=?1 AND event_revision=?2 LIMIT 17",
        turso::params![commit.event.id.clone(),super::to_i64(commit.event.revision,"event revision")?]).await?;
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        anyhow::ensure!(
            row.get::<Vec<u8>>(0)? == intent,
            "image publication intent changed"
        );
        count += 1;
    }
    anyhow::ensure!(
        count == 0 || count == commit.images.len(),
        "image publication set changed"
    );
    Ok(count != 0)
}

pub(in crate::storage::catalog) async fn lookup(
    connection: &turso::Connection,
    event: &str,
    attachment: &str,
) -> anyhow::Result<Option<Location>> {
    let mut rows = connection
        .query(
            "SELECT i.object_id, a.volume_id, a.generation, a.relative_key,
                    a.location_revision, a.bytes, a.file_identity, a.digest
             FROM storage_event_images AS i
             LEFT JOIN storage_volume_allocations AS a
               ON a.kind = 'thumbnail'
              AND a.object_id = i.object_id
              AND a.state = 'published'
              AND NOT EXISTS (
                  SELECT 1 FROM storage_volume_moves AS m
                  WHERE m.source_operation = a.operation
                    AND m.phase IN ('published', 'retiring', 'complete')
              )
             WHERE i.event_id = ?1 AND i.attachment_id = ?2 AND i.active = 1",
            (event, attachment),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let object_id: String = row.get(0)?;
    if let Some(location) = location_from_joined_row(&row, &object_id, 1)? {
        return Ok(Some(location));
    }
    drop(rows);
    pressure::retired_location(connection, &object_id).await
}

pub(in crate::storage::catalog) async fn lookup_batch(
    connection: &turso::Connection,
    pairs: &[(String, String)],
) -> anyhow::Result<Vec<Option<Location>>> {
    if pairs.is_empty() {
        return Ok(Vec::new());
    }
    let mut values = String::new();
    let mut params = Vec::<turso::Value>::with_capacity(pairs.len() * 3);
    for (index, (event_id, attachment_id)) in pairs.iter().enumerate() {
        if index > 0 {
            values.push(',');
        }
        values.push_str("(?, ?, ?)");
        params.push(turso::Value::Integer(i64::try_from(index)?));
        params.push(turso::Value::Text(event_id.clone()));
        params.push(turso::Value::Text(attachment_id.clone()));
    }
    let sql = format!(
        "WITH requested(request_index, event_id, attachment_id) AS (VALUES {values})
         SELECT r.request_index, i.object_id, a.volume_id, a.generation, a.relative_key,
                a.location_revision, a.bytes, a.file_identity, a.digest
         FROM requested AS r
         JOIN storage_event_images AS i
           ON i.event_id = r.event_id
          AND i.attachment_id = r.attachment_id
          AND i.active = 1
         LEFT JOIN storage_volume_allocations AS a
           ON a.kind = 'thumbnail'
          AND a.object_id = i.object_id
          AND a.state = 'published'
          AND NOT EXISTS (
              SELECT 1 FROM storage_volume_moves AS m
              WHERE m.source_operation = a.operation
                AND m.phase IN ('published', 'retiring', 'complete')
          )"
    );
    let mut results = vec![None; pairs.len()];
    let mut retired_candidates = Vec::new();
    let mut rows = connection
        .query(sql, turso::params_from_iter(params))
        .await?;
    while let Some(row) = rows.next().await? {
        let index = usize::try_from(row.get::<i64>(0)?)?;
        let object_id: String = row.get(1)?;
        if let Some(location) = location_from_joined_row(&row, &object_id, 2)? {
            if let Some(slot) = results.get_mut(index) {
                *slot = Some(location);
            }
        } else {
            retired_candidates.push((index, object_id));
        }
    }
    drop(rows);
    for (index, object_id) in retired_candidates {
        if let Some(slot) = results.get_mut(index) {
            *slot = pressure::retired_location(connection, &object_id).await?;
        }
    }
    Ok(results)
}

fn location_from_joined_row(
    row: &turso::Row,
    object_id: &str,
    offset: usize,
) -> anyhow::Result<Option<Location>> {
    let Some(volume) = row.get::<Option<String>>(offset)? else {
        return Ok(None);
    };
    let relative_key = row.get::<String>(offset + 2)?;
    super::validate_key(&relative_key)?;
    let digest: [u8; 32] = row
        .get::<Vec<u8>>(offset + 6)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid location digest"))?;
    Ok(Some(Location {
        object: Object {
            kind: Kind::Thumbnail,
            id: object_id.to_owned(),
        },
        volume,
        generation: super::to_u64(row.get::<i64>(offset + 1)?, "volume generation")?,
        relative_key,
        revision: super::to_u64(row.get::<i64>(offset + 3)?, "location revision")?,
        bytes: super::to_u64(row.get::<i64>(offset + 4)?, "location bytes")?,
        file_identity: row.get::<String>(offset + 5)?,
        digest,
    }))
}

pub(in crate::storage::catalog) async fn reconcile(
    connection: &turso::Connection,
    previous: &TimelineEvent,
    next: &TimelineEvent,
) -> anyhow::Result<()> {
    for descriptor in previous.attachments.iter().take(16) {
        let unchanged = next.attachments.contains(descriptor)
            && (previous.canonical_attachment_id.as_deref() != Some(&descriptor.id)
                || previous.thumbnail_filename == next.thumbnail_filename);
        if !unchanged {
            connection.execute("UPDATE storage_event_images SET active=0 WHERE event_id=?1 AND attachment_id=?2",
                (previous.id.as_str(),descriptor.id.as_str())).await?;
        }
    }
    Ok(())
}

pub(in crate::storage::catalog) async fn detach(
    connection: &turso::Connection,
    id: &str,
) -> anyhow::Result<()> {
    connection
        .execute(
            "UPDATE storage_event_images SET active=0 WHERE event_id=?1",
            [id],
        )
        .await?;
    Ok(())
}
