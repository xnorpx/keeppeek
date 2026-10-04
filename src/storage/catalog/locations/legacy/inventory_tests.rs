use super::inventory::{self, Evidence, Reference};
use crate::storage::catalog::{
    self,
    locations::{Kind, Object},
};
use std::path::{Path, PathBuf};

async fn fixture() -> anyhow::Result<turso::Connection> {
    let database = turso::Builder::new_local(":memory:")
        .experimental_generated_columns(true)
        .build()
        .await?;
    let connection = database.connect()?;
    catalog::initialize_schema(&connection).await?;
    Ok(connection)
}

fn object(id: &str) -> Object {
    Object {
        kind: Kind::Recording,
        id: id.into(),
    }
}

fn missing_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "keeppeek-legacy-inventory-{}",
        uuid::Uuid::new_v4()
    ))
}

async fn recording(
    connection: &turso::Connection,
    id: &str,
    path: &Path,
    finalized: bool,
) -> anyhow::Result<()> {
    connection
        .execute(
            "INSERT INTO recording_files
        (id,stream_id,started_at_ms,ended_at_ms,path,init_offset,init_len,finalized,file_bytes)
        VALUES (?1,'camera/main',1000,2000,?2,0,8,?3,64)",
            turso::params![
                id,
                path.to_string_lossy().into_owned(),
                i64::from(finalized)
            ],
        )
        .await?;
    Ok(())
}

async fn allocation(connection: &turso::Connection, id: &str, state: &str) -> anyhow::Result<()> {
    connection
        .execute_batch(
            "INSERT OR IGNORE INTO storage_volume_bindings
        (id,generation,root,filesystem,root_identity,writable,minimum_free_bytes)
        VALUES ('named',1,'/named','filesystem','named-root',1,0)",
        )
        .await?;
    connection.execute("INSERT INTO storage_volume_allocations
        (operation,kind,object_id,volume_id,generation,relative_key,destination_path,bytes,intent_bytes,
         materialized_bytes,state,file_identity,digest,location_revision)
        VALUES (?1,'recording',?1,'named',1,?1||'.mp4','/named/'||?1||'.mp4',64,64,64,?2,'file',zeroblob(32),1)",
        (id,state)).await?;
    Ok(())
}

fn evidence() -> Evidence {
    Evidence {
        file_identity: "verified-file".into(),
        catalog_identity: "verified-file".into(),
        bytes: 64,
        digest: [7; 32],
    }
}

#[test]
fn first_verification_must_match_existing_catalog_file_identity_and_size() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        recording(
            &connection,
            "recording",
            &missing_root().join("recording.mp4"),
            true,
        )
        .await?;
        connection
            .execute(
                "UPDATE recording_files SET file_identity='catalog-original'",
                (),
            )
            .await?;
        let reference = register_one(&connection).await?;
        let mut observed = evidence();
        assert!(
            inventory::verify(&connection, &reference, &observed)
                .await
                .is_err()
        );
        observed.file_identity = "catalog-original".into();
        observed.bytes = 63;
        assert!(
            inventory::verify(&connection, &reference, &observed)
                .await
                .is_err()
        );
        observed.bytes = 64;
        assert_eq!(
            inventory::verify(&connection, &reference, &observed)
                .await?
                .evidence,
            Some(observed)
        );
        anyhow::Ok(())
    })
}

async fn register_one(connection: &turso::Connection) -> anyhow::Result<Reference> {
    let references = inventory::register_recordings(connection, None, 1).await?;
    anyhow::ensure!(references.len() == 1, "legacy reference missing");
    Ok(references.into_iter().next().unwrap())
}

#[test]
fn recording_inventory_pages_are_bounded_stable_and_do_not_probe_missing_paths()
-> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        let root = missing_root();
        for id in ["d", "b", "a", "c"] {
            recording(&connection, id, &root.join(format!("{id}.mp4")), true).await?;
        }
        for limit in [0, 65] {
            assert!(
                inventory::register_recordings(&connection, None, limit)
                    .await
                    .is_err()
            );
        }
        let first = inventory::register_recordings(&connection, None, 2).await?;
        assert_eq!(
            first
                .iter()
                .map(|item| item.object.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            inventory::register_recordings(&connection, None, 2).await?,
            first
        );
        let second = inventory::register_recordings(&connection, Some("b"), 2).await?;
        assert_eq!(
            second
                .iter()
                .map(|item| item.object.id.as_str())
                .collect::<Vec<_>>(),
            ["c", "d"]
        );
        assert!(
            inventory::register_recordings(&connection, Some("d"), 2)
                .await?
                .is_empty()
        );
        for reference in first.iter().chain(&second) {
            assert_eq!(reference.object.kind, Kind::Recording);
            assert_eq!(
                reference.path,
                root.join(format!("{}.mp4", reference.object.id))
            );
            assert!(reference.revision > 0);
            assert_eq!(reference.evidence, None);
            assert_eq!(
                inventory::lookup(&connection, &reference.object).await?,
                Some(reference.clone())
            );
        }
        assert!(!root.exists());
        Ok(())
    })
}

#[test]
fn inventory_excludes_active_and_owned_recordings_but_includes_cancelled_allocations()
-> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        let root = missing_root();
        for (id, finalized) in [
            ("active", false),
            ("legacy", true),
            ("published", true),
            ("reserved", true),
            ("cancelled", true),
        ] {
            recording(&connection, id, &root.join(format!("{id}.mp4")), finalized).await?;
        }
        for state in ["published", "reserved", "cancelled"] {
            allocation(&connection, state, state).await?;
        }
        let references = inventory::register_recordings(&connection, None, 64).await?;
        assert_eq!(
            references
                .iter()
                .map(|item| item.object.id.as_str())
                .collect::<Vec<_>>(),
            ["cancelled", "legacy"]
        );
        for id in ["active", "published", "reserved"] {
            assert_eq!(inventory::lookup(&connection, &object(id)).await?, None);
        }
        let previous = inventory::lookup(&connection, &object("legacy"))
            .await?
            .unwrap();
        allocation(&connection, "legacy", "reserved").await?;
        assert_eq!(
            inventory::lookup(&connection, &previous.object).await?,
            None
        );
        assert!(
            inventory::verify(&connection, &previous, &evidence())
                .await
                .is_err()
        );
        assert!(!root.exists());
        Ok(())
    })
}

#[test]
fn verification_is_exactly_repeatable_and_cannot_replace_evidence_for_one_revision()
-> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        let root = missing_root();
        recording(&connection, "recording", &root.join("missing.mp4"), true).await?;
        let reference = register_one(&connection).await?;
        let evidence = evidence();
        let verified = inventory::verify(&connection, &reference, &evidence).await?;
        assert_eq!(verified.evidence, Some(evidence.clone()));
        assert_eq!(
            inventory::verify(&connection, &reference, &evidence).await?,
            verified
        );
        let mut changed = evidence;
        changed.digest[0] ^= 1;
        assert!(
            inventory::verify(&connection, &verified, &changed)
                .await
                .is_err()
        );
        assert_eq!(
            inventory::lookup(&connection, &reference.object).await?,
            Some(verified)
        );
        assert!(!root.exists());
        Ok(())
    })
}

#[test]
fn changed_owner_path_invalidates_evidence_and_rejects_stale_verification() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        let root = missing_root();
        recording(&connection, "recording", &root.join("before.mp4"), true).await?;
        let reference = register_one(&connection).await?;
        let verified = inventory::verify(&connection, &reference, &evidence()).await?;
        let after = root.join("after.mp4");
        connection
            .execute(
                "UPDATE recording_files SET path=?1 WHERE id='recording'",
                [after.to_string_lossy().into_owned()],
            )
            .await?;
        let current = inventory::lookup(&connection, &reference.object)
            .await?
            .unwrap();
        assert_eq!(current.path, after);
        assert!(current.revision > verified.revision);
        assert_eq!(current.evidence, None);
        assert!(
            inventory::verify(&connection, &verified, &evidence())
                .await
                .is_err()
        );
        let refreshed = inventory::verify(&connection, &current, &evidence()).await?;
        assert_eq!(refreshed.evidence, Some(evidence()));
        assert_eq!(register_one(&connection).await?, refreshed);
        assert!(!root.exists());
        Ok(())
    })
}

#[test]
fn deleting_the_owner_removes_its_reference_and_fences_old_evidence() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        recording(
            &connection,
            "recording",
            &missing_root().join("recording.mp4"),
            true,
        )
        .await?;
        let reference = register_one(&connection).await?;
        let verified = inventory::verify(&connection, &reference, &evidence()).await?;
        connection
            .execute("DELETE FROM recording_files WHERE id='recording'", ())
            .await?;
        assert_eq!(
            inventory::lookup(&connection, &reference.object).await?,
            None
        );
        assert!(
            inventory::verify(&connection, &verified, &evidence())
                .await
                .is_err()
        );
        assert!(
            inventory::register_recordings(&connection, None, 64)
                .await?
                .is_empty()
        );
        Ok(())
    })
}

#[test]
fn verified_inventory_retains_legacy_quota_and_cleanup_eligibility() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        let root = catalog::tests::test_dir("legacy-inventory-cleanup");
        let path = root.join("recording.mp4");
        let bytes = [7_u8; 64];
        std::fs::write(&path, bytes)?;
        recording(&connection, "recording", &path, true).await?;
        let before = catalog::locations::recordings::legacy_bytes(&connection).await?;
        assert_eq!(before, 64);
        let reference = register_one(&connection).await?;
        inventory::verify(&connection, &reference, &evidence()).await?;
        assert_eq!(
            catalog::locations::recordings::legacy_bytes(&connection).await?,
            before
        );
        let candidate = catalog::claim_cleanup_candidate(&connection)
            .await?
            .unwrap();
        assert_eq!(candidate.recording_id, "recording");
        assert_eq!(candidate.path, path);
        assert_eq!(candidate.file_bytes, before);
        assert_eq!(std::fs::read(&path)?, bytes);
        let mut allocations = connection
            .query("SELECT COUNT(*) FROM storage_volume_allocations", ())
            .await?;
        assert_eq!(allocations.next().await?.unwrap().get::<i64>(0)?, 0);
        drop(allocations);
        std::fs::remove_dir_all(root)?;
        Ok(())
    })
}
