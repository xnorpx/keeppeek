//! Shares event writes between ordinary updates and atomic image publication.

use super::*;

pub(super) async fn insert(
    connection: &turso::Connection,
    mut event: TimelineEvent,
    publication: Option<EventPublicationIdentity>,
) -> anyhow::Result<()> {
    prepare(&mut event)?;
    connection.execute_batch("BEGIN IMMEDIATE").await?;
    let result = async {
        if let Some(previous) = event_by_id(connection, &event.id).await?
            && previous.thumbnail_filename != event.thumbnail_filename
        {
            locations::images::detach(connection, &event.id).await?;
        }
        write(connection, &event, publication.as_ref()).await
    }
    .await;
    match result {
        Ok(()) => connection.execute_batch("COMMIT").await.map_err(Into::into),
        Err(error) => {
            connection.execute_batch("ROLLBACK").await?;
            Err(error)
        }
    }
}

pub(super) fn prepare(event: &mut TimelineEvent) -> anyhow::Result<()> {
    normalize_event_presentation(event)?;
    anyhow::ensure!(!event.kind.is_empty(), "event kind must not be empty");
    anyhow::ensure!(
        event.revision > 0,
        "event revision must be greater than zero"
    );
    anyhow::ensure!(
        event
            .end_time_ms
            .is_none_or(|end| end >= event.start_time_ms),
        "event end must not precede its start"
    );
    Ok(())
}

pub(super) async fn write(
    connection: &turso::Connection,
    event: &TimelineEvent,
    publication: Option<&EventPublicationIdentity>,
) -> anyhow::Result<()> {
    let existing = event_by_id(connection, &event.id).await?;
    if let Some(existing) = &existing {
        anyhow::ensure!(
            event.revision > existing.revision,
            "event revision {} does not exceed stored revision {}",
            event.revision,
            existing.revision
        );
        anyhow::ensure!(
            event.camera_id == existing.camera_id && event.source == existing.source,
            "event revision cannot change source identity"
        );
        locations::images::reconcile(connection, existing, event).await?;
    } else {
        anyhow::ensure!(event.revision == 1, "new events must start at revision one");
    }
    upsert(connection, event, publication).await?;
    replace_intrinsic_event_terms(connection, &event.id, &event.kind, event.text.as_deref())
        .await?;
    if existing.is_some() {
        record_event_search_mutation(connection, &event.id).await?;
    }
    reconcile_keyframes_for_event(
        connection,
        &event.id,
        &event.camera_id,
        event.stream.as_deref(),
        event.start_time_ms,
    )
    .await
}

async fn upsert(
    connection: &turso::Connection,
    event: &TimelineEvent,
    publication: Option<&EventPublicationIdentity>,
) -> anyhow::Result<()> {
    let bbox = event
        .bbox
        .map(|value| serde_json::to_string(&value))
        .transpose()?;
    let attachments = serde_json::to_string(&event.attachments)?;
    let payload = event
        .payload
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    connection.execute("INSERT INTO recording_events (
        id,camera_id,stream,source,kind,start_time_ms,end_time_ms,confidence,bbox_json,zone,
        thumbnail_filename,revision,bbox_attachment_id,attachments_json,canonical_attachment_id,
        icon_key,rejected_icon_key,text,payload_json,publication_id,publication_fingerprint)
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)
        ON CONFLICT(id) DO UPDATE SET camera_id=excluded.camera_id,stream=excluded.stream,
        source=excluded.source,kind=excluded.kind,start_time_ms=excluded.start_time_ms,
        end_time_ms=excluded.end_time_ms,confidence=excluded.confidence,bbox_json=excluded.bbox_json,
        zone=excluded.zone,thumbnail_filename=excluded.thumbnail_filename,revision=excluded.revision,
        bbox_attachment_id=excluded.bbox_attachment_id,attachments_json=excluded.attachments_json,
        canonical_attachment_id=excluded.canonical_attachment_id,icon_key=excluded.icon_key,
        rejected_icon_key=excluded.rejected_icon_key,text=excluded.text,payload_json=excluded.payload_json,
        publication_id=excluded.publication_id,publication_fingerprint=excluded.publication_fingerprint",
        turso::params![event.id.clone(), event.camera_id.clone(), event.stream.clone(),
            event.source.as_str(), event.kind.clone(), event.start_time_ms, event.end_time_ms,
            event.confidence, bbox, event.zone.clone(), event.thumbnail_filename.clone(),
            to_i64(event.revision, "event revision")?, event.bbox_attachment_id.clone(), attachments,
            event.canonical_attachment_id.clone(), event.icon_key.clone(), event.rejected_icon_key.clone(),
            event.text.clone(), payload, publication.map(|value| value.publication_id.clone()),
            publication.map(|value| value.fingerprint.clone())]).await?;
    Ok(())
}
