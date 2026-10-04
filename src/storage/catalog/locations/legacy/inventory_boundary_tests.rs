use super::inventory::{self, Action, Evidence};
use crate::storage::catalog::{
    self, CatalogRecording, RecordingCatalog,
    locations::{Kind, Object, Reply, Request},
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

fn missing_path() -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "keeppeek-inventory-boundary-{}",
            uuid::Uuid::new_v4()
        ))
        .join("recording.mp4")
}

async fn owner(connection: &turso::Connection, path: &Path) -> anyhow::Result<()> {
    connection
        .execute(
            "INSERT INTO recording_files
        (id,stream_id,started_at_ms,ended_at_ms,path,init_offset,init_len,finalized,file_bytes)
        VALUES ('legacy','camera/main',1000,2000,?1,0,8,1,64)",
            [path.to_string_lossy().into_owned()],
        )
        .await?;
    Ok(())
}

fn evidence() -> Evidence {
    Evidence {
        file_identity: "captured-legacy-file".into(),
        catalog_identity: "captured-legacy-file".into(),
        bytes: 64,
        digest: [7; 32],
    }
}

#[test]
fn recreated_owner_cannot_reuse_an_updated_deleted_owners_snapshot() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        let path = missing_path();
        owner(&connection, &path).await?;
        let initial = inventory::register_recordings(&connection, None, 1)
            .await?
            .remove(0);
        for _ in 0..4 {
            connection
                .execute(
                    "UPDATE recording_files SET ended_at_ms=ended_at_ms+1 WHERE id='legacy'",
                    (),
                )
                .await?;
        }
        let previous = inventory::lookup(&connection, &initial.object)
            .await?
            .unwrap();
        assert!(previous.revision > initial.revision);
        let verified = inventory::verify(&connection, &previous, &evidence()).await?;
        connection
            .execute("DELETE FROM recording_files WHERE id='legacy'", ())
            .await?;
        owner(&connection, &path).await?;
        let replacement = inventory::register_recordings(&connection, None, 1)
            .await?
            .remove(0);
        assert_eq!(replacement.object, previous.object);
        assert_eq!(replacement.path, previous.path);
        assert!(replacement.revision > previous.revision);
        assert_eq!(replacement.evidence, None);
        for stale in [initial, previous, verified] {
            assert!(
                inventory::verify(&connection, &stale, &evidence())
                    .await
                    .is_err()
            );
        }
        assert_eq!(
            inventory::lookup(&connection, &replacement.object).await?,
            Some(replacement)
        );
        Ok(())
    })
}

#[test]
fn a_different_named_object_owning_the_same_path_blocks_legacy_inventory() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        let path = missing_path();
        owner(&connection, &path).await?;
        let reference = inventory::register_recordings(&connection, None, 1)
            .await?
            .remove(0);
        inventory::verify(&connection, &reference, &evidence()).await?;
        connection
            .execute_batch(
                "INSERT INTO storage_volume_bindings
            (id,generation,root,filesystem,root_identity,writable,minimum_free_bytes)
            VALUES ('named',1,'/named','disk','root',1,0)",
            )
            .await?;
        connection.execute("INSERT INTO storage_volume_allocations
            (operation,kind,object_id,volume_id,generation,relative_key,destination_path,bytes,intent_bytes,state)
            VALUES ('allocation','recording','different-owner','named',1,'recording.mp4',replace(?1,char(92),'/'),64,64,'reserved')",
            [path.to_string_lossy().into_owned()]).await?;
        assert!(
            inventory::register_recordings(&connection, None, 64)
                .await?
                .is_empty()
        );
        assert_eq!(
            inventory::lookup(&connection, &reference.object).await?,
            None
        );
        assert!(
            inventory::verify(&connection, &reference, &evidence())
                .await
                .is_err()
        );
        assert!(!path.parent().unwrap().exists());
        Ok(())
    })
}

#[test]
fn cleanup_admission_after_registration_refuses_stale_file_verification() -> anyhow::Result<()> {
    pollster::block_on(async {
        let connection = fixture().await?;
        owner(&connection, &missing_path()).await?;
        let reference = inventory::register_recordings(&connection, None, 1)
            .await?
            .remove(0);
        let candidate = catalog::claim_cleanup_candidate(&connection)
            .await?
            .unwrap();
        assert_eq!(candidate.recording_id, reference.object.id);
        assert!(
            inventory::verify(&connection, &reference, &evidence())
                .await
                .is_err()
        );
        assert_eq!(
            inventory::lookup(&connection, &reference.object).await?,
            None
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
fn maintenance_claim_for_an_alias_path_fences_previously_registered_evidence() -> anyhow::Result<()>
{
    pollster::block_on(async {
        let connection = fixture().await?;
        let path = missing_path();
        owner(&connection, &path).await?;
        let reference = inventory::register_recordings(&connection, None, 1)
            .await?
            .remove(0);
        let original = path.to_string_lossy();
        let alias = if original.contains('\\') {
            original.replace('\\', "/")
        } else {
            original.replace('/', "\\")
        };
        assert_ne!(alias, original);
        connection
            .execute(
                "INSERT INTO recording_maintenance_claims
            (job_id,ordinal,recording_id,token,path,file_identity,file_bytes,active)
            VALUES ('job',1,'alias-owner','claim-token',?1,zeroblob(32),64,1)",
                [alias],
            )
            .await?;
        assert!(
            inventory::verify(&connection, &reference, &evidence())
                .await
                .is_err()
        );
        assert_eq!(
            inventory::lookup(&connection, &reference.object).await?,
            None
        );
        assert!(
            inventory::register_recordings(&connection, None, 64)
                .await?
                .is_empty()
        );
        Ok(())
    })
}

fn actor_recording(path: &Path) -> CatalogRecording {
    CatalogRecording {
        id: "legacy".into(),
        stream_id: "camera/main".into(),
        source_id: Some("camera".into()),
        logical_stream_id: Some("main".into()),
        started_at_ms: 1000,
        ended_at_ms: Some(2000),
        path: path.to_string_lossy().into_owned(),
        init_offset: 0,
        init_len: 8,
        finalized: true,
    }
}

#[test]
fn actor_inventory_roundtrip_survives_reopen_without_creating_volume_ownership()
-> anyhow::Result<()> {
    let root = catalog::tests::test_dir("legacy-inventory-actor");
    let catalog_path = root.join("catalog.db");
    let path = root.join("offline/recording.mp4");
    let catalog = RecordingCatalog::open_for_adoption(&catalog_path)?;
    catalog.handle().upsert_recording(actor_recording(&path))?;
    let Reply::LegacyReferences(mut page) =
        catalog
            .handle()
            .volume_location(Request::LegacyInventory(Action::Recordings {
                after: None,
                limit: 1,
            }))?
    else {
        anyhow::bail!("legacy inventory page missing")
    };
    assert_eq!(page.len(), 1);
    let reference = page.remove(0);
    let verified = catalog
        .handle()
        .volume_location(Request::LegacyInventory(Action::Verify(
            Box::new(reference.clone()),
            evidence(),
        )))?;
    let Reply::LegacyReference(Some(ref captured)) = verified else {
        anyhow::bail!("verified reference missing")
    };
    assert_eq!(captured.evidence, Some(evidence()));
    catalog.shutdown();
    let catalog = RecordingCatalog::open_for_adoption(&catalog_path)?;
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::LegacyInventory(Action::Lookup(
                reference.object.clone()
            )))?,
        verified
    );
    assert_eq!(
        catalog
            .handle()
            .volume_location(Request::LegacyInventory(Action::Verify(
                Box::new(reference),
                evidence()
            )))?,
        verified
    );
    assert_eq!(
        catalog.handle().volume_location(Request::Lookup(Object {
            kind: Kind::Recording,
            id: "legacy".into()
        }))?,
        Reply::Location(None)
    );
    assert_eq!(
        catalog.handle().volume_location(Request::Usage)?,
        Reply::Usage(vec![])
    );
    assert_eq!(catalog.handle().stats()?.recording_files, 1);
    assert!(!path.parent().unwrap().exists());
    catalog.shutdown();
    std::fs::remove_dir_all(root)?;
    Ok(())
}
