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
            request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone())))
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
        let result = request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone()))).await?;
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
            request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone()))).await?,
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
                request(&c, Request::AdoptLegacyMedia(Box::new(invalid)))
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
            request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone())))
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
        request(c, Request::AdoptLegacyMedia(Box::new(intent.clone()))).await?,
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
        request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone()))).await?;
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
        request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone()))).await?;
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
        request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone()))).await?;
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
                request(&c, Request::AdoptLegacyMedia(Box::new(invalid)))
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
            request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone())))
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
            request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone())))
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

async fn cancelled_adoption(c: &turso::Connection, intent: &Intent) -> anyhow::Result<()> {
    request(c, Request::AdoptLegacyMedia(Box::new(intent.clone()))).await?;
    for step in [
        moves::Step::Cancel(intent.destination.id.clone()),
        moves::Step::CancellationVerified {
            id: intent.destination.id.clone(),
            evidence: moves::Cancellation::Empty,
        },
        moves::Step::Cancelled(intent.destination.id.clone()),
        moves::Step::Acknowledged(intent.destination.id.clone()),
    ] {
        request(c, Request::AdvanceMove(step)).await?;
    }
    Ok(())
}
async fn legacy_pressure(c: &turso::Connection, filesystem: Option<&str>) -> anyhow::Result<Reply> {
    use crate::storage::catalog::locations::recordings::{Action, Reason};
    request(
        c,
        Request::RecordingRetention(Action::BeginLegacy {
            reason: if filesystem.is_some() {
                Reason::DiskPressure
            } else {
                Reason::Capacity
            },
            filesystem: filesystem.map(str::to_owned),
        }),
    )
    .await
}
#[test]
fn legacy_pressure_isolates_filesystems_and_reuses_pending_retirement() -> anyhow::Result<()> {
    pollster::block_on(async {
        let (c, intent) = fixture().await?;
        cancelled_adoption(&c, &intent).await?;
        let revision = request(&c, Request::Revision).await?;
        let usage = request(&c, Request::Usage).await?;
        assert_eq!(
            legacy_pressure(&c, Some("unrelated-disk")).await?,
            Reply::RecordingRetirement(None)
        );
        assert_eq!(request(&c, Request::Revision).await?, revision);
        assert_eq!(request(&c, Request::Usage).await?, usage);
        let admitted = legacy_pressure(&c, Some("disk")).await?;
        let Reply::RecordingRetirement(Some(job)) = &admitted else {
            anyhow::bail!("retirement missing")
        };
        assert_eq!(job.operation, intent.operation);
        assert_eq!(job.location.object, intent.reference.object);
        assert!(!job.complete);
        let revision = request(&c, Request::Revision).await?;
        assert_eq!(legacy_pressure(&c, Some("disk")).await?, admitted);
        assert_eq!(request(&c, Request::Revision).await?, revision);
        assert_eq!(request(&c, Request::Usage).await?, usage);
        Ok(())
    })
}
#[test]
fn legacy_pressure_waits_for_older_ordinary_candidate_then_selects_adopted_source()
-> anyhow::Result<()> {
    pollster::block_on(async {
        let (c, intent) = fixture().await?;
        cancelled_adoption(&c, &intent).await?;
        let path = intent.reference.path.parent().unwrap().join("ordinary.mp4");
        c.execute("INSERT INTO recording_files(id,stream_id,started_at_ms,ended_at_ms,path,init_offset,init_len,finalized,file_bytes) VALUES('ordinary','camera/main',0,1,?1,0,8,1,32)", [path.to_string_lossy().into_owned()]).await?;
        let revision = request(&c, Request::Revision).await?;
        assert_eq!(
            legacy_pressure(&c, None).await?,
            Reply::RecordingRetirement(None)
        );
        assert_eq!(request(&c, Request::Revision).await?, revision);
        c.execute(
            "UPDATE recording_files SET protected=1 WHERE id='ordinary'",
            (),
        )
        .await?;
        let Reply::RecordingRetirement(Some(job)) = legacy_pressure(&c, None).await? else {
            anyhow::bail!("retirement missing")
        };
        assert_eq!(job.operation, intent.operation);
        let mut rows = c
            .query(
                "SELECT protected FROM recording_files WHERE id='ordinary'",
                (),
            )
            .await?;
        assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 1);
        Ok(())
    })
}
#[test]
fn legacy_pressure_preserves_active_move_and_protected_adopted_source() -> anyhow::Result<()> {
    pollster::block_on(async {
        let (c, intent) = fixture().await?;
        request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone()))).await?;
        assert_eq!(
            legacy_pressure(&c, None).await?,
            Reply::RecordingRetirement(None)
        );
        for step in [
            moves::Step::Cancel(intent.destination.id.clone()),
            moves::Step::CancellationVerified {
                id: intent.destination.id.clone(),
                evidence: moves::Cancellation::Empty,
            },
            moves::Step::Cancelled(intent.destination.id.clone()),
        ] {
            request(&c, Request::AdvanceMove(step)).await?;
        }
        assert_eq!(
            legacy_pressure(&c, None).await?,
            Reply::RecordingRetirement(None)
        );
        request(
            &c,
            Request::AdvanceMove(moves::Step::Acknowledged(intent.destination.id.clone())),
        )
        .await?;
        c.execute(
            "UPDATE recording_files SET protected=1 WHERE id='legacy-id'",
            (),
        )
        .await?;
        let revision = request(&c, Request::Revision).await?;
        let usage = request(&c, Request::Usage).await?;
        assert_eq!(
            legacy_pressure(&c, None).await?,
            Reply::RecordingRetirement(None)
        );
        assert_eq!(request(&c, Request::Revision).await?, revision);
        assert_eq!(request(&c, Request::Usage).await?, usage);
        Ok(())
    })
}
mod export_adoption {
    use super::*;

    async fn export_fixture() -> anyhow::Result<(turso::Connection, Intent)> {
        let (c, _) = fixture().await?;
        let Reply::LegacyPaths(Some(paths)) = request(&c, Request::LegacyPaths).await? else {
            anyhow::bail!("legacy paths missing")
        };
        let source = binding(&paths.export_root, "legacy-export", false);
        let id = uuid::Uuid::new_v4().to_string();
        let path = paths.export_root.join(format!("{id}.mp4"));
        request(
            &c,
            Request::CaptureLegacyRoots(Box::new(Capture {
                paths: *paths,
                roots: vec![
                    (Role::Active, None),
                    (Role::Archive, None),
                    (Role::Export, Some(source)),
                    (Role::Thumbnail, None),
                ],
            })),
        )
        .await?;
        let reference = inventory::Reference {
            object: Object {
                kind: Kind::Export,
                id,
            },
            path,
            revision: 1,
            evidence: Some(Evidence {
                file_identity: "export-file".into(),
                catalog_identity: "export-catalog-file".into(),
                bytes: 64,
                digest: [7; 32],
            }),
        };
        let mut destination = destination(&c, reference.object.clone()).await?;
        destination.destination.object.kind = Kind::Export;
        Ok((
            c,
            Intent {
                reference,
                role: Role::Export,
                operation: uuid::Uuid::new_v4().to_string(),
                destination,
            },
        ))
    }

    async fn rejected_without_ownership(
        c: &turso::Connection,
        intent: &Intent,
    ) -> anyhow::Result<()> {
        let revision = request(c, Request::Revision).await?;
        let usage = request(c, Request::Usage).await?;
        assert!(
            request(c, Request::AdoptLegacyMedia(Box::new(intent.clone())))
                .await
                .is_err()
        );
        assert_eq!(request(c, Request::Revision).await?, revision);
        assert_eq!(request(c, Request::Usage).await?, usage);
        assert_eq!(
            request(c, Request::Lookup(intent.reference.object.clone())).await?,
            Reply::Location(None)
        );
        assert!(
            request(c, Request::Move(intent.destination.id.clone()))
                .await
                .is_err()
        );
        let mut rows = c
            .query(
                "SELECT COUNT(*) FROM storage_legacy_adoptions WHERE operation=?1",
                [intent.operation.as_str()],
            )
            .await?;
        assert_eq!(rows.next().await?.unwrap().get::<i64>(0)?, 0);
        Ok(())
    }

    #[test]
    fn export_adoption_publishes_source_and_durable_move_with_exact_retry() -> anyhow::Result<()> {
        pollster::block_on(async {
            let (c, intent) = export_fixture().await?;
            let Reply::Move(job) =
                request(&c, Request::AdoptLegacyMedia(Box::new(intent.clone()))).await?
            else {
                anyhow::bail!("adopted export move missing")
            };
            assert_eq!(job.source.object, intent.reference.object);
            assert_eq!(job.source.volume, "legacy-export");
            assert_eq!(
                job.source.relative_key,
                format!("{}.mp4", intent.reference.object.id)
            );
            assert_eq!(job.source.file_identity, "export-file");
            assert_eq!(job.source.bytes, 64);
            assert_eq!(job.object.kind, Kind::Export);
            assert_eq!(
                request(&c, Request::Lookup(intent.reference.object.clone())).await?,
                Reply::Location(Some(job.source.clone()))
            );
            retry_unchanged(&c, &intent).await?;
            let Reply::Usage(rows) = request(&c, Request::Usage).await? else {
                anyhow::bail!("usage missing")
            };
            let source = rows
                .iter()
                .find(|row| row.volume == "legacy-export")
                .unwrap();
            let target = rows.iter().find(|row| row.volume == "target").unwrap();
            assert_eq!((source.allocated_bytes, source.reserved_bytes), (64, 0));
            assert_eq!((target.allocated_bytes, target.reserved_bytes), (64, 64));
            publish_adopted(&c, &intent).await?;
            retry_unchanged(&c, &intent).await?;
            let Reply::Location(Some(location)) =
                request(&c, Request::Lookup(intent.reference.object.clone())).await?
            else {
                anyhow::bail!("published export missing")
            };
            assert_eq!(location.object, intent.reference.object);
            assert_eq!(location.volume, "target");
            Ok(())
        })
    }

    #[test]
    fn export_adoption_cleanup_tombstone_rejects_without_mutation() -> anyhow::Result<()> {
        pollster::block_on(async {
            let (c, mut intent) = export_fixture().await?;
            request(
                &c,
                Request::RetireExport(intent.reference.object.id.clone()),
            )
            .await?;
            let Reply::Revision(revision) = request(&c, Request::Revision).await? else {
                anyhow::bail!("revision missing")
            };
            intent.destination.destination.capacity.ledger_revision = revision;
            rejected_without_ownership(&c, &intent).await
        })
    }

    #[test]
    fn export_adoption_capacity_and_role_errors_roll_back_source_ownership() -> anyhow::Result<()> {
        pollster::block_on(async {
            let (c, intent) = export_fixture().await?;
            let mut full = intent.clone();
            full.destination.destination.capacity.available_bytes = 0;
            rejected_without_ownership(&c, &full).await?;
            for role in [Role::Active, Role::Archive, Role::Thumbnail] {
                let mut invalid = intent.clone();
                invalid.role = role;
                rejected_without_ownership(&c, &invalid).await?;
            }
            Ok(())
        })
    }

    #[test]
    fn export_adoption_cannot_take_an_existing_recording_location() -> anyhow::Result<()> {
        pollster::block_on(async {
            let (c, mut intent) = export_fixture().await?;
            let path = intent.reference.path.to_string_lossy().replace('\\', "/");
            c.execute(
                "UPDATE recording_files SET path=?1 WHERE id='legacy-id'",
                [path.as_str()],
            )
            .await?;
            let Reply::Revision(revision) = request(&c, Request::Revision).await? else {
                anyhow::bail!("revision missing")
            };
            intent.destination.destination.capacity.ledger_revision = revision;
            rejected_without_ownership(&c, &intent).await?;
            c.execute("INSERT INTO storage_volume_allocations(operation,kind,object_id,volume_id,generation,relative_key,destination_path,bytes,intent_bytes,materialized_bytes,state,file_identity,digest,location_revision)
                VALUES('existing-recording','recording','legacy-id','legacy-export',1,?1,?2,64,64,64,'published','recording-file',?3,1)",
                turso::params![format!("{}.mp4", intent.reference.object.id), path.clone(), vec![9_u8; 32]]).await?;
            let Reply::Revision(revision) = request(&c, Request::Revision).await? else {
                anyhow::bail!("revision missing")
            };
            intent.destination.destination.capacity.ledger_revision = revision;
            let before = request(
                &c,
                Request::Lookup(Object {
                    kind: Kind::Recording,
                    id: "legacy-id".into(),
                }),
            )
            .await?;
            assert!(matches!(&before, Reply::Location(Some(_))));
            rejected_without_ownership(&c, &intent).await?;
            assert_eq!(
                request(
                    &c,
                    Request::Lookup(Object {
                        kind: Kind::Recording,
                        id: "legacy-id".into()
                    })
                )
                .await?,
                before
            );
            Ok(())
        })
    }
}
