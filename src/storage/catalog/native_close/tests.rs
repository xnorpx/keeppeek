use super::super::{
    EventMetadataQuery, close_native_events, initialize_schema, insert_event,
    maximum_search_revision, search_event_metadata, tests::test_event,
};

async fn fixture() -> anyhow::Result<turso::Connection> {
    let database = turso::Builder::new_local(":memory:").build().await?;
    let connection = database.connect()?;
    initialize_schema(&connection).await?;
    for id in ["alpha", "zulu", "closed"] {
        let mut event = test_event(id, 1000);
        if id != "closed" {
            event.end_time_ms = None;
        }
        insert_event(&connection, event, None).await?;
    }
    Ok(connection)
}

#[derive(Debug, PartialEq, Eq)]
struct StoredEvent {
    revision: i64,
    end: Option<i64>,
    search: i64,
}

async fn stored(
    connection: &turso::Connection,
) -> anyhow::Result<std::collections::BTreeMap<String, StoredEvent>> {
    let mut rows = connection
        .query(
            "SELECT id, revision, end_time_ms, search_revision FROM recording_events",
            (),
        )
        .await?;
    let mut events = std::collections::BTreeMap::new();
    while let Some(row) = rows.next().await? {
        events.insert(
            row.get(0)?,
            StoredEvent {
                revision: row.get(1)?,
                end: row.get(2)?,
                search: row.get(3)?,
            },
        );
    }
    Ok(events)
}

#[test]
fn native_close_persists_ordered_search_revisions_and_invalidates_old_pages() -> anyhow::Result<()>
{
    pollster::block_on(async {
        let connection = fixture().await?;
        let before = stored(&connection).await?;
        let mut query = EventMetadataQuery::new("main", 0, 5000);
        query.page_size = 1;
        query.page_token = search_event_metadata(&connection, query.clone())
            .await?
            .next_page_token;
        assert!(query.page_token.is_some());
        let first = maximum_search_revision(&connection).await?;
        let endings = [
            ("zulu".into(), 2000),
            ("missing".into(), 3000),
            ("zulu".into(), 3000),
            ("closed".into(), 2000),
            ("alpha".into(), 2100),
        ];
        let updated = close_native_events(&connection, &endings).await?;
        assert_eq!(
            updated
                .iter()
                .map(|entry| entry.as_ref().map(|event| event.id.as_str()))
                .collect::<Vec<_>>(),
            [Some("zulu"), None, None, None, Some("alpha")]
        );
        let after = stored(&connection).await?;
        assert_eq!(
            after["zulu"],
            StoredEvent {
                revision: before["zulu"].revision + 1,
                end: Some(2000),
                search: first + 1
            }
        );
        assert_eq!(
            after["alpha"],
            StoredEvent {
                revision: before["alpha"].revision + 1,
                end: Some(2100),
                search: first + 2
            }
        );
        assert_eq!(after["closed"], before["closed"]);
        assert_eq!(after.len(), before.len());
        assert_eq!(maximum_search_revision(&connection).await?, first + 2);
        assert!(
            search_event_metadata(&connection, query)
                .await
                .unwrap_err()
                .to_string()
                .contains("event search snapshot changed")
        );
        assert!(
            close_native_events(&connection, &endings)
                .await?
                .iter()
                .all(Option::is_none)
        );
        assert_eq!(stored(&connection).await?, after);
        assert_eq!(maximum_search_revision(&connection).await?, first + 2);
        Ok(())
    })
}

#[test]
fn native_close_rejects_invalid_times_and_search_revision_exhaustion_atomically()
-> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        let before = stored(&connection).await?;
        let first = maximum_search_revision(&connection).await?;
        assert!(
            close_native_events(&connection, &[("zulu".into(), 2000), ("alpha".into(), 999)])
                .await
                .unwrap_err()
                .to_string()
                .contains("end precedes its start")
        );
        assert_eq!(stored(&connection).await?, before);
        assert_eq!(maximum_search_revision(&connection).await?, first);
        connection
            .execute(
                "UPDATE recording_event_search_state SET revision = ?1 WHERE id = 1",
                turso::params![i64::MAX - 1],
            )
            .await?;
        assert!(
            close_native_events(
                &connection,
                &[("zulu".into(), 2000), ("alpha".into(), 2100)]
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("search revision exceeded")
        );
        assert_eq!(stored(&connection).await?, before);
        assert_eq!(maximum_search_revision(&connection).await?, i64::MAX - 1);
        Ok(())
    })
}

#[test]
fn native_close_rolls_back_updated_events_when_search_counter_write_fails() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        let before = stored(&connection).await?;
        let first = maximum_search_revision(&connection).await?;
        connection.execute_batch("CREATE TRIGGER reject_close_counter BEFORE UPDATE ON recording_event_search_state
            WHEN (SELECT COUNT(*) FROM recording_events WHERE id IN ('alpha','zulu') AND end_time_ms IS NOT NULL) = 2
            BEGIN SELECT RAISE(ABORT, 'injected close counter failure'); END;").await?;
        let endings = [("zulu".into(), 2000), ("alpha".into(), 2100)];
        assert!(
            close_native_events(&connection, &endings)
                .await
                .unwrap_err()
                .to_string()
                .contains("injected close counter failure")
        );
        assert_eq!(stored(&connection).await?, before);
        assert_eq!(maximum_search_revision(&connection).await?, first);
        connection
            .execute_batch("DROP TRIGGER reject_close_counter")
            .await?;
        assert!(
            close_native_events(&connection, &endings)
                .await?
                .iter()
                .all(Option::is_some)
        );
        let after = stored(&connection).await?;
        assert_eq!(after["zulu"].search, first + 1);
        assert_eq!(after["alpha"].search, first + 2);
        assert_eq!(maximum_search_revision(&connection).await?, first + 2);
        Ok(())
    })
}
