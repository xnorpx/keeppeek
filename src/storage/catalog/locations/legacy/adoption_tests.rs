use super::{
    LegacyPaths,
    adoption::Intent,
    inventory::{self, Evidence},
    roots::{Capture, Role},
};
use crate::storage::catalog::{
    self,
    locations::{self, Allocation, Binding, Capacity, Kind, Object, Reply, Request, moves},
};
use std::time::{Duration, Instant};

async fn request(c: &turso::Connection, r: Request) -> anyhow::Result<Reply> {
    locations::execute(c, r, Instant::now() + Duration::from_secs(10)).await
}
fn binding(root: &std::path::Path, id: &str, writable: bool) -> Binding {
    Binding {
        id: id.into(),
        generation: 1,
        root: root.into(),
        filesystem: "disk".into(),
        root_identity: id.into(),
        writable,
        draining: false,
        limit_bytes: None,
        minimum_free_bytes: 0,
    }
}
async fn fixture() -> anyhow::Result<(turso::Connection, Intent)> {
    let db = turso::Builder::new_local(":memory:")
        .experimental_generated_columns(true)
        .build()
        .await?;
    let c = db.connect()?;
    catalog::initialize_schema(&c).await?;
    let base = std::env::temp_dir().join(format!("adopt-{}", uuid::Uuid::new_v4()));
    let paths = LegacyPaths {
        active_root: base.join("source"),
        archive_root: base.join("source"),
        export_root: base.join("exports"),
        thumbnail_root: base.join("thumbs"),
        catalog_path: base.join("catalog.db"),
        export_history_path: base.join("exports/history.json"),
    };
    let source = binding(&paths.active_root, "legacy-active", false);
    let file = paths.active_root.join("camera/old-file.mp4");
    request(
        &c,
        Request::CaptureLegacyRoots(Box::new(Capture {
            paths,
            roots: vec![
                (Role::Active, Some(source.clone())),
                (Role::Archive, Some(source)),
                (Role::Export, None),
                (Role::Thumbnail, None),
            ],
        })),
    )
    .await?;
    request(
        &c,
        Request::Bind(binding(&base.join("target"), "target", true)),
    )
    .await?;
    c.execute("INSERT INTO recording_files(id,stream_id,started_at_ms,ended_at_ms,path,init_offset,init_len,finalized,file_bytes,file_identity) VALUES('legacy-id','camera/main',1,2,?1,0,8,1,64,'catalog-file')", [file.to_string_lossy().into_owned()]).await?;
    let reference = inventory::register_recordings(&c, None, 1).await?.remove(0);
    let reference = inventory::verify(
        &c,
        &reference,
        &Evidence {
            file_identity: "pinned-file".into(),
            catalog_identity: "catalog-file".into(),
            bytes: 64,
            digest: [7; 32],
        },
    )
    .await?;
    let destination = destination(&c, reference.object.clone()).await?;
    Ok((
        c,
        Intent {
            reference,
            role: Role::Active,
            operation: uuid::Uuid::new_v4().to_string(),
            destination,
        },
    ))
}
async fn destination(c: &turso::Connection, object: Object) -> anyhow::Result<moves::Intent> {
    let id = uuid::Uuid::new_v4().to_string();
    let Reply::Revision(revision) = request(c, Request::Revision).await? else {
        anyhow::bail!("revision missing")
    };
    Ok(moves::Intent {
        id: id.clone(),
        object,
        expected_revision: 1,
        destination: Allocation {
            operation: id.clone(),
            object: Object {
                kind: Kind::Recording,
                id: id.clone(),
            },
            volume: "target".into(),
            generation: 1,
            relative_key: format!("{id}.mp4"),
            bytes: 64,
            capacity: Capacity {
                ledger_revision: revision,
                observed_at: Instant::now(),
                available_bytes: 4096,
                filesystem: "disk".into(),
                root_identity: "target".into(),
            },
        },
    })
}
async fn unchanged(
    c: &turso::Connection,
    intent: &Intent,
    revision: &Reply,
    usage: &Reply,
) -> anyhow::Result<()> {
    assert_eq!(&request(c, Request::Revision).await?, revision);
    assert_eq!(&request(c, Request::Usage).await?, usage);
    assert_eq!(
        inventory::lookup(c, &intent.reference.object).await?,
        Some(intent.reference.clone())
    );
    assert_eq!(
        request(c, Request::Lookup(intent.reference.object.clone())).await?,
        Reply::Location(None)
    );
    Ok(())
}
#[test]
fn adoption_capacity_failure_rolls_back_source_and_destination() -> anyhow::Result<()> {
    pollster::block_on(async {
        let (c, mut intent) = fixture().await?;
        let revision = request(&c, Request::Revision).await?;
        let usage = request(&c, Request::Usage).await?;
        intent.destination.destination.capacity.available_bytes = 0;
        assert!(
            request(&c, Request::AdoptLegacyRecording(Box::new(intent.clone())))
                .await
                .is_err()
        );
        unchanged(&c, &intent, &revision, &usage).await
    })
}
#[test]
fn adoption_preserves_recording_and_exact_retry_without_double_charge() -> anyhow::Result<()> {
    pollster::block_on(async {
        let (c, intent) = fixture().await?;
        let result = request(&c, Request::AdoptLegacyRecording(Box::new(intent.clone()))).await?;
        let Reply::Move(job) = &result else {
            anyhow::bail!("move missing")
        };
        assert_eq!(job.source.object, intent.reference.object);
        assert_eq!(job.source.relative_key, "camera/old-file.mp4");
        assert_eq!(job.source.bytes, 64);
        assert_eq!(job.source.file_identity, "pinned-file");
        let revision = request(&c, Request::Revision).await?;
        let usage = request(&c, Request::Usage).await?;
        let Reply::Usage(rows) = &usage else {
            anyhow::bail!("usage missing")
        };
        let source = rows.iter().find(|r| r.volume == "legacy-active").unwrap();
        let target = rows.iter().find(|r| r.volume == "target").unwrap();
        assert_eq!((source.allocated_bytes, source.reserved_bytes), (64, 0));
        assert_eq!((target.allocated_bytes, target.reserved_bytes), (64, 64));
        assert_eq!(
            request(&c, Request::AdoptLegacyRecording(Box::new(intent.clone()))).await?,
            result
        );
        assert_eq!(request(&c, Request::Revision).await?, revision);
        assert_eq!(request(&c, Request::Usage).await?, usage);
        let mut rows = c
            .query(
                "SELECT id,path,file_identity,file_bytes FROM recording_files",
                (),
            )
            .await?;
        let row = rows.next().await?.unwrap();
        assert_eq!(row.get::<String>(0)?, "legacy-id");
        assert_eq!(
            row.get::<String>(1)?,
            intent.reference.path.to_string_lossy()
        );
        assert_eq!(row.get::<String>(2)?, "catalog-file");
        assert_eq!(row.get::<i64>(3)?, 64);
        Ok(())
    })
}
#[test]
fn adoption_rejects_stale_reference_and_wrong_captured_role_without_mutation() -> anyhow::Result<()>
{
    pollster::block_on(async {
        let (c, intent) = fixture().await?;
        let revision = request(&c, Request::Revision).await?;
        let usage = request(&c, Request::Usage).await?;
        let mut stale = intent.clone();
        stale.reference.revision += 1;
        let mut wrong_role = intent.clone();
        wrong_role.role = Role::Export;
        let mut changed = intent.clone();
        changed.reference.evidence.as_mut().unwrap().digest = [9; 32];
        for invalid in [stale, wrong_role, changed] {
            assert!(
                request(&c, Request::AdoptLegacyRecording(Box::new(invalid)))
                    .await
                    .is_err()
            );
            unchanged(&c, &intent, &revision, &usage).await?;
        }
        Ok(())
    })
}

#[test]
fn adoption_cleanup_conflict_preserves_counters_and_cleanup_owner() -> anyhow::Result<()> {
    pollster::block_on(async {
        let (c, intent) = fixture().await?;
        c.execute(
            "UPDATE recording_files SET cleanup_pending=1 WHERE id='legacy-id'",
            (),
        )
        .await?;
        let revision = request(&c, Request::Revision).await?;
        let usage = request(&c, Request::Usage).await?;
        assert!(
            request(&c, Request::AdoptLegacyRecording(Box::new(intent.clone())))
                .await
                .is_err()
        );
        assert_eq!(request(&c, Request::Revision).await?, revision);
        assert_eq!(request(&c, Request::Usage).await?, usage);
        assert_eq!(
            request(&c, Request::Lookup(intent.reference.object)).await?,
            Reply::Location(None)
        );
        let mut rows = c
            .query(
                "SELECT cleanup_pending FROM recording_files WHERE id='legacy-id'",
                (),
            )
            .await?;
        assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 1);
        Ok(())
    })
}

async fn retry_unchanged(c: &turso::Connection, intent: &Intent) -> anyhow::Result<()> {
    let job = request(c, Request::Move(intent.destination.id.clone())).await?;
    let revision = request(c, Request::Revision).await?;
    let usage = request(c, Request::Usage).await?;
    assert_eq!(
        request(c, Request::AdoptLegacyRecording(Box::new(intent.clone()))).await?,
        job
    );
    assert_eq!(request(c, Request::Revision).await?, revision);
    assert_eq!(request(c, Request::Usage).await?, usage);
    Ok(())
}
async fn publish_adopted(c: &turso::Connection, intent: &Intent) -> anyhow::Result<()> {
    use crate::storage::catalog::locations::Publication;
    for step in [
        moves::Step::Verified(Publication {
            operation: intent.destination.id.clone(),
            bytes: 64,
            file_identity: "target-file".into(),
            digest: [7; 32],
        }),
        moves::Step::FilePublished(intent.destination.id.clone()),
        moves::Step::Publish(intent.destination.id.clone()),
    ] {
        request(c, Request::AdvanceMove(step)).await?;
    }
    Ok(())
}
#[test]
fn adopted_move_retry_after_publication_and_source_retirement_is_read_only() -> anyhow::Result<()> {
    pollster::block_on(async {
        let (c, intent) = fixture().await?;
        request(&c, Request::AdoptLegacyRecording(Box::new(intent.clone()))).await?;
        publish_adopted(&c, &intent).await?;
        retry_unchanged(&c, &intent).await?;
        request(
            &c,
            Request::AdvanceMove(moves::Step::Retiring(intent.destination.id.clone())),
        )
        .await?;
        request(
            &c,
            Request::AdvanceMove(moves::Step::Retired(locations::Publication {
                operation: intent.destination.id.clone(),
                bytes: 64,
                file_identity: "pinned-file".into(),
                digest: [7; 32],
            })),
        )
        .await?;
        retry_unchanged(&c, &intent).await?;
        let Reply::Usage(rows) = request(&c, Request::Usage).await? else {
            anyhow::bail!("usage missing")
        };
        assert_eq!(
            rows.iter()
                .find(|r| r.volume == "legacy-active")
                .unwrap()
                .allocated_bytes,
            0
        );
        assert_eq!(
            rows.iter()
                .find(|r| r.volume == "target")
                .unwrap()
                .allocated_bytes,
            64
        );
        Ok(())
    })
}
#[test]
fn cancelled_adopted_move_retry_preserves_published_source_ownership() -> anyhow::Result<()> {
    pollster::block_on(async {
        let (c, intent) = fixture().await?;
        request(&c, Request::AdoptLegacyRecording(Box::new(intent.clone()))).await?;
        for step in [
            moves::Step::Cancel(intent.destination.id.clone()),
            moves::Step::CancellationVerified {
                id: intent.destination.id.clone(),
                evidence: moves::Cancellation::Empty,
            },
            moves::Step::Cancelled(intent.destination.id.clone()),
            moves::Step::Acknowledged(intent.destination.id.clone()),
        ] {
            request(&c, Request::AdvanceMove(step)).await?;
        }
        retry_unchanged(&c, &intent).await?;
        let Reply::Location(Some(source)) =
            request(&c, Request::Lookup(intent.reference.object.clone())).await?
        else {
            anyhow::bail!("adopted source missing")
        };
        assert_eq!(source.volume, "legacy-active");
        assert_eq!(source.bytes, 64);
        assert!(
            inventory::lookup(&c, &intent.reference.object)
                .await?
                .is_none()
        );
        let Reply::Usage(rows) = request(&c, Request::Usage).await? else {
            anyhow::bail!("usage missing")
        };
        assert_eq!(
            rows.iter()
                .find(|r| r.volume == "legacy-active")
                .unwrap()
                .allocated_bytes,
            64
        );
        let target = rows.iter().find(|r| r.volume == "target").unwrap();
        assert_eq!((target.allocated_bytes, target.reserved_bytes), (0, 0));
        Ok(())
    })
}
#[test]
fn adopted_retry_rejects_changed_provenance_and_destination_without_mutation() -> anyhow::Result<()>
{
    pollster::block_on(async {
        let (c, intent) = fixture().await?;
        request(&c, Request::AdoptLegacyRecording(Box::new(intent.clone()))).await?;
        let revision = request(&c, Request::Revision).await?;
        let usage = request(&c, Request::Usage).await?;
        let mut changed_reference = intent.clone();
        changed_reference.reference.revision += 1;
        let mut changed_operation = intent.clone();
        changed_operation.operation = uuid::Uuid::new_v4().to_string();
        let mut changed_evidence = intent.clone();
        changed_evidence
            .reference
            .evidence
            .as_mut()
            .unwrap()
            .file_identity = "other-file".into();
        let mut changed_destination = intent.clone();
        changed_destination.destination.destination.relative_key = "changed.mp4".into();
        let mut changed_bytes = intent.clone();
        changed_bytes.destination.destination.bytes += 1;
        for invalid in [
            changed_reference,
            changed_operation,
            changed_evidence,
            changed_destination,
            changed_bytes,
        ] {
            assert!(
                request(&c, Request::AdoptLegacyRecording(Box::new(invalid)))
                    .await
                    .is_err()
            );
            assert_eq!(request(&c, Request::Revision).await?, revision);
            assert_eq!(request(&c, Request::Usage).await?, usage);
            retry_unchanged(&c, &intent).await?;
        }
        Ok(())
    })
}
#[test]
fn adoption_invalid_source_operation_uuid_preserves_legacy_owner() -> anyhow::Result<()> {
    pollster::block_on(async {
        let (c, mut intent) = fixture().await?;
        let revision = request(&c, Request::Revision).await?;
        let usage = request(&c, Request::Usage).await?;
        intent.operation = "not-a-uuid".into();
        assert!(
            request(&c, Request::AdoptLegacyRecording(Box::new(intent.clone())))
                .await
                .is_err()
        );
        unchanged(&c, &intent, &revision, &usage).await
    })
}

#[test]
fn adoption_rejects_a_distinct_catalog_owner_at_the_normalized_source_path() -> anyhow::Result<()> {
    pollster::block_on(async {
        let (c, intent) = fixture().await?;
        let alias_path = intent
            .reference
            .path
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_uppercase();
        c.execute("INSERT INTO recording_files
            (id,stream_id,started_at_ms,ended_at_ms,path,init_offset,init_len,finalized,file_bytes,file_identity)
            VALUES('alias-id','other/main',3,4,?1,0,8,1,64,'catalog-file')", [alias_path.clone()]).await?;
        let revision = request(&c, Request::Revision).await?;
        let usage = request(&c, Request::Usage).await?;
        assert!(
            request(&c, Request::AdoptLegacyRecording(Box::new(intent.clone())))
                .await
                .is_err()
        );
        unchanged(&c, &intent, &revision, &usage).await?;
        let mut rows = c
            .query(
                "SELECT id,path,file_bytes,cleanup_pending FROM recording_files ORDER BY id",
                (),
            )
            .await?;
        let alias = rows.next().await?.expect("alias preserved");
        assert_eq!(alias.get::<String>(0)?, "alias-id");
        assert_eq!(alias.get::<String>(1)?, alias_path);
        assert_eq!(alias.get::<i64>(2)?, 64);
        assert_eq!(alias.get::<i64>(3)?, 0);
        let source = rows.next().await?.expect("source preserved");
        assert_eq!(source.get::<String>(0)?, "legacy-id");
        assert_eq!(
            source.get::<String>(1)?,
            intent.reference.path.to_string_lossy()
        );
        assert_eq!(source.get::<i64>(2)?, 64);
        assert_eq!(source.get::<i64>(3)?, 0);
        assert!(rows.next().await?.is_none());
        drop(rows);
        assert_eq!(
            request(
                &c,
                Request::Lookup(Object {
                    kind: Kind::Recording,
                    id: "alias-id".into()
                })
            )
            .await?,
            Reply::Location(None)
        );
        Ok(())
    })
}
