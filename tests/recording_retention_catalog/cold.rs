use super::*;
use sha2::{Digest, Sha256};

const START: i64 = 3_000_000_000_000;
const END: i64 = START + 10_000;
const FLOOR: i64 = END + 86_400_000;

fn open_after_maintenance(fixture: &Fixture) -> RecordingCatalog {
    let mut catalog = fixture.open();
    catalog.wait_for_maintenance();
    catalog
}

fn seed_legacy(fixture: &Fixture) {
    fixture.execute_sql(include_str!("../fixtures/recording_retention_b877.sql"));
    let database = pollster::block_on(
        turso::Builder::new_local(fixture.0.join("catalog.db").to_str().unwrap()).build(),
    )
    .unwrap();
    let connection = database.connect().unwrap();
    pollster::block_on(async {
        for (id, protected, bytes) in [("recording", 0, 123), ("held", 1, 456)] {
            connection
                .execute(
                    "INSERT INTO recording_files
                (id,stream_id,source_id,logical_stream_id,started_at_ms,ended_at_ms,path,
                 init_offset,init_len,finalized,finalized_at_ms,file_bytes,protected)
                VALUES(?1,'front/main','front','main',?2,?3,?4,0,0,1,?3,?5,?6)",
                    turso::params![
                        id,
                        START,
                        END,
                        fixture
                            .0
                            .join(format!("offline-{id}.mp4"))
                            .to_str()
                            .unwrap(),
                        bytes,
                        protected
                    ],
                )
                .await
                .unwrap();
        }
        connection
            .execute(
                "INSERT INTO recording_events
            (id,camera_id,stream,source,kind,start_time_ms,end_time_ms)
            VALUES('legacy-motion','front','main','keeppeek','motion',?1,?2)",
                turso::params![START, END],
            )
            .await
            .unwrap();
        let fingerprint = Sha256::digest(serde_json::to_vec(&policy(86_400_000)).unwrap());
        connection.execute("INSERT INTO recording_retention_decisions
            (recording_id,policy_revision,policy_fingerprint,event_revision,deadline_ms,matching_rules_json,reason_json)
            VALUES('recording',1,?1,0,?2,'[\"continuous\"]','\"matching-rules\"')",
            turso::params![fingerprint.as_slice(), FLOOR]).await.unwrap();
    });
}

fn verify_migrated_columns_and_accounting(fixture: &Fixture) {
    let database = pollster::block_on(
        turso::Builder::new_local(fixture.0.join("catalog.db").to_str().unwrap()).build(),
    )
    .unwrap();
    let connection = database.connect().unwrap();
    pollster::block_on(async {
        let mut rows = connection
            .query(
                "SELECT count(*),sum(file_bytes),sum(protected),
            sum(retention_pending),sum(cleanup_retention_expiry) FROM recording_files",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 2);
        assert_eq!(row.get::<i64>(1).unwrap(), 579);
        assert_eq!(row.get::<i64>(2).unwrap(), 1);
        assert_eq!(row.get::<i64>(3).unwrap(), 0);
        assert_eq!(row.get::<i64>(4).unwrap(), 0);
        drop(rows);
        let mut rows = connection
            .query(
                "SELECT total_bytes FROM recording_legacy_byte_total WHERE singleton=1",
                (),
            )
            .await
            .unwrap();
        assert_eq!(
            rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(),
            579
        );
    });
}

#[test]
fn cold_pre_runtime_schema_preserves_offline_references_holds_and_committed_floor_across_restart() {
    // Preserve the database on failure so migration loss can be inspected.
    let fixture = std::mem::ManuallyDrop::new(Fixture::new());
    eprintln!("cold_fixture={}", fixture.0.display());
    seed_legacy(&fixture);
    let catalog = open_after_maintenance(&fixture);
    let handle = catalog.handle();
    let original = handle.retention_decision("recording").unwrap().unwrap();
    assert_eq!(original.deadline_ms, Some(FLOOR));
    assert_eq!(original.policy_revision, 1);
    assert_eq!(original.matching_rules, ["continuous"]);
    let legacy_event = handle.event_by_id("legacy-motion").unwrap().unwrap();
    assert_eq!(legacy_event.start_time_ms, START);
    assert!(!handle.reconcile_retention_events(1).unwrap());
    catalog.shutdown();
    let catalog = open_after_maintenance(&fixture);
    let handle = catalog.handle();
    assert_eq!(
        handle.retention_decision("recording").unwrap(),
        Some(original)
    );
    assert!(handle.reconcile_retention_events(1).unwrap());
    assert_eq!(
        handle.event_by_id("legacy-motion").unwrap(),
        Some(legacy_event)
    );
    let shorter = handle.commit_retention("recording", 2, &policy(1)).unwrap();
    assert_eq!(shorter.deadline_ms, Some(FLOOR));
    assert_eq!(shorter.reason, Reason::CommittedDeadline);
    let settings: keeppeek::storage::retention::settings::Settings =
        toml::from_str("[default]\ncontinuous_days=0.0").unwrap();
    assert!(handle.request_retention_settings(Some(&settings)).unwrap());
    let mut complete = false;
    for _ in 0..16 {
        if !handle.reconcile_retention_runtime(8).unwrap().pending {
            complete = true;
            break;
        }
    }
    assert!(
        complete,
        "cold activation must finish within the fixture work limit"
    );
    let activated = handle.retention_decision("recording").unwrap().unwrap();
    assert_eq!(activated.deadline_ms, Some(FLOOR));
    assert_eq!(activated.reason, Reason::CommittedDeadline);
    catalog.shutdown();
    verify_migrated_columns_and_accounting(&fixture);
    let catalog = open_after_maintenance(&fixture);
    assert_eq!(
        catalog.handle().retention_decision("recording").unwrap(),
        Some(activated)
    );
    catalog.shutdown();
    drop(std::mem::ManuallyDrop::into_inner(fixture));
}
