//! Bounded pages of authoritative objects for operator migration and drain previews.

use super::{Kind, Location, Object, Reply, identifier, ownership};

pub(super) async fn source(
    connection: &turso::Connection,
    object: &Object,
) -> anyhow::Result<Option<String>> {
    let sql = match object.kind {
        Kind::Recording => "SELECT source_id FROM recording_files WHERE id = ?1",
        Kind::Thumbnail => {
            "SELECT e.camera_id FROM storage_event_images i JOIN recording_events e ON e.id = i.event_id WHERE i.object_id = ?1 AND i.active = 1"
        }
        Kind::Export => return Ok(None),
    };
    let mut rows = connection.query(sql, [object.id.as_str()]).await?;
    rows.next()
        .await?
        .map(|row| row.get::<Option<String>>(0))
        .transpose()
        .map(Option::flatten)
        .map_err(Into::into)
}

/// A stable cursor ordered by kind and object ID within one volume.
#[derive(Debug, Clone)]
pub struct Page {
    pub volume: String,
    pub after: Option<Object>,
    pub limit: u16,
}

impl Page {
    pub(super) fn validate(&self) -> anyhow::Result<()> {
        identifier(&self.volume)?;
        anyhow::ensure!((1..=64).contains(&self.limit), "invalid object page size");
        if let Some(after) = &self.after {
            identifier(&after.id)?;
        }
        Ok(())
    }
}

pub(super) async fn page(
    connection: &turso::Connection,
    page: &Page,
) -> anyhow::Result<Vec<Location>> {
    let (kind, id) = page.after.as_ref().map_or(("", ""), |object| {
        (object.kind.as_str(), object.id.as_str())
    });
    let mut rows = connection
        .query(
            "SELECT kind, object_id FROM storage_volume_allocations a
         WHERE volume_id = ?1 AND state = 'published' AND (kind, object_id) > (?2, ?3)
         AND NOT EXISTS (SELECT 1 FROM storage_volume_moves m WHERE m.source_operation = a.operation
             AND m.phase IN ('published','retiring','complete'))
         ORDER BY kind, object_id LIMIT ?4",
            turso::params![page.volume.clone(), kind, id, i64::from(page.limit)],
        )
        .await?;
    let mut objects = Vec::with_capacity(usize::from(page.limit));
    while let Some(row) = rows.next().await? {
        objects.push(Object {
            kind: ownership::parse_kind(&row.get::<String>(0)?)?,
            id: row.get::<String>(1)?,
        });
    }
    drop(rows);
    let mut locations = Vec::with_capacity(objects.len());
    for object in objects {
        let Reply::Location(Some(location)) = ownership::lookup(connection, &object).await? else {
            anyhow::bail!("listed object has no authoritative location");
        };
        anyhow::ensure!(
            location.volume == page.volume,
            "listed object moved outside the catalog transaction"
        );
        locations.push(location);
    }
    Ok(locations)
}
