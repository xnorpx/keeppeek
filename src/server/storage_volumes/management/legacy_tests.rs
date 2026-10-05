use super::*;
use crate::storage::catalog::{CatalogRecording, RecordingCatalog};
use std::path::PathBuf;

fn legacy_fixture() -> (PathBuf, RecordingCatalog, ServerState) {
    let root = std::env::temp_dir().join(format!("legacy-api-{}", uuid::Uuid::new_v4()));
    let catalog = RecordingCatalog::open_for_adoption(&root.join("catalog.db")).unwrap();
    let state = ServerState::empty().with_recording_catalog(catalog.handle());
    (root, catalog, state)
}

fn seed_legacy(catalog: &RecordingCatalog, root: &std::path::Path, id: &str, finalized: bool) {
    // ponytail: absent media proves listing needs catalog references, not filesystem discovery.
    catalog
        .handle()
        .upsert_recording(CatalogRecording {
            id: id.into(),
            stream_id: "camera/main".into(),
            source_id: Some("camera".into()),
            logical_stream_id: Some("main".into()),
            started_at_ms: 1000,
            ended_at_ms: finalized.then_some(2000),
            path: root
                .join("offline-private-media")
                .join(format!("{id}.mp4"))
                .to_string_lossy()
                .into_owned(),
            init_offset: 0,
            init_len: 8,
            finalized,
        })
        .unwrap();
}

fn legacy_command(after: Option<proto::StorageObject>) -> proto::StorageVolumeCommand {
    proto::StorageVolumeCommand {
        action: Some(proto::storage_volume_command::Action::LegacyObjects(
            proto::ListLegacyStorageObjects { after },
        )),
    }
}

fn legacy_page(
    state: &ServerState,
    after: Option<proto::StorageObject>,
) -> proto::StorageLegacyObjectList {
    let principal = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    let proto::ok::Result::StorageVolumeResult(reply) =
        dispatch(state, &principal, legacy_command(after)).unwrap()
    else {
        panic!("missing storage result")
    };
    let Some(proto::storage_volume_result::Result::LegacyObjects(page)) = reply.result else {
        panic!("missing legacy page")
    };
    page
}

#[test]
fn legacy_inventory_requires_admin_before_catalog_access() {
    let state = ServerState::empty();
    let mut principal = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    principal.role = crate::access::AccessRole::User;
    let command = legacy_command(None);
    assert_eq!(
        proto::StorageVolumeCommand::decode(command.encode_to_vec().as_slice()).unwrap(),
        command
    );
    assert_eq!(
        dispatch(&state, &principal, command)
            .unwrap_err()
            ._http_status,
        403
    );
}

#[test]
fn managed_legacy_sources_can_be_listed_but_not_selected_as_destinations() {
    use proto::storage_volume_command::Action;
    let state = ServerState::empty();
    let names = names::Names::load(&state).unwrap();
    let action = Some(Action::Objects(proto::ListStorageObjects {
        volume_id: "legacy-active".into(),
        after: None,
    }));
    assert_eq!(names.resolve(action.clone()).unwrap(), action);
    assert!(
        names
            .resolve(Some(Action::PreviewMove(proto::PreviewStorageMove {
                destination_volume_id: "legacy-active".into(),
                ..Default::default()
            })))
            .is_err()
    );
}

#[test]
fn legacy_inventory_pages_offline_recordings_without_disclosing_paths() {
    let (root, catalog, state) = legacy_fixture();
    for index in 0..65 {
        seed_legacy(&catalog, &root, &format!("recording-{index:03}"), true);
    }
    seed_legacy(&catalog, &root, "active-not-eligible", false);
    let first = legacy_page(&state, None);
    assert_eq!(first.objects.len(), 64);
    assert_eq!(legacy_page(&state, None), first);
    assert_eq!(first.next_after, first.objects.last().unwrap().object);
    let second = legacy_page(&state, first.next_after.clone());
    assert_eq!(second.objects.len(), 1);
    assert!(second.next_after.is_none());
    for (index, entry) in first.objects.iter().chain(&second.objects).enumerate() {
        let object = entry.object.as_ref().unwrap();
        assert_eq!(object.kind, proto::StorageObjectKind::Recording as i32);
        assert_eq!(object.id, format!("recording-{index:03}"));
        assert_eq!(entry.bytes, None);
        assert!(entry.revision > 0);
    }
    for page in [&first, &second] {
        let bytes = page.encode_to_vec();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("offline-private-media"));
        assert!(!text.contains(root.to_string_lossy().as_ref()));
    }
    assert!(!root.join("offline-private-media").exists());
    drop(state);
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_inventory_rejects_non_recording_cursor_without_advancing_page() {
    let (root, catalog, state) = legacy_fixture();
    seed_legacy(&catalog, &root, "recording-000", true);
    let before = legacy_page(&state, None);
    let principal = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    let cursor = proto::StorageObject {
        kind: proto::StorageObjectKind::Export as i32,
        id: "recording-000".into(),
    };
    assert!(dispatch(&state, &principal, legacy_command(Some(cursor))).is_err());
    assert_eq!(legacy_page(&state, None), before);
    drop(state);
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();
}
