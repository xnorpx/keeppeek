use super::{
    LegacyPaths,
    roots::{Capture, Role, State},
};
use crate::storage::catalog::{
    RecordingCatalog, RecordingCatalogHandle,
    locations::{Binding, Reply, Request},
    tests::test_dir,
};

fn root_binding(path: &std::path::Path, id: &str) -> Binding {
    Binding {
        id: id.into(),
        generation: 1,
        root: path.into(),
        filesystem: "disk".into(),
        root_identity: format!("identity-{id}"),
        writable: false,
        draining: false,
        limit_bytes: None,
        minimum_free_bytes: 0,
    }
}

fn capture_fixture(name: &str) -> (std::path::PathBuf, RecordingCatalog, Capture) {
    let root = test_dir(name);
    let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
    let paths = LegacyPaths {
        active_root: root.join("media"),
        archive_root: root.join("media"),
        export_root: root.join("media/.exports"),
        thumbnail_root: root.join("thumbnails"),
        catalog_path: root.join("catalog.db"),
        export_history_path: root.join("media/.exports/history.json"),
    };
    let shared = root_binding(&paths.active_root, "legacy-active");
    let roots = vec![
        (Role::Active, Some(shared.clone())),
        (Role::Archive, Some(shared)),
        (Role::Export, None),
        (Role::Thumbnail, None),
    ];
    (root, catalog, Capture { paths, roots })
}

fn capture(handle: &RecordingCatalogHandle, value: Capture) {
    assert_eq!(
        handle
            .volume_location(Request::CaptureLegacyRoots(Box::new(value)))
            .unwrap(),
        Reply::Bound
    );
}

fn state(handle: &RecordingCatalogHandle, role: Role) -> State {
    let Reply::LegacyRoot(state) = handle.volume_location(Request::LegacyRoot(role)).unwrap()
    else {
        panic!("missing legacy root reply");
    };
    state
}

#[test]
fn legacy_root_capture_deduplicates_and_preserves_offline_state_across_restart() {
    let (root, catalog, value) = capture_fixture("legacy-root-capture-reopen");
    let handle = catalog.handle();
    assert!(matches!(state(&handle, Role::Active), State::Uncaptured));
    capture(&handle, value.clone());
    for role in [Role::Active, Role::Archive] {
        let State::Bound(binding) = state(&handle, role) else {
            panic!("root not bound")
        };
        assert_eq!(binding.id, "legacy-active");
        assert_eq!(binding.root, value.paths.active_root);
        assert!(!binding.writable);
    }
    assert!(matches!(state(&handle, Role::Export), State::Offline));
    let usage = handle.volume_location(Request::Usage).unwrap();
    let Reply::Usage(rows) = &usage else {
        panic!("missing usage")
    };
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].allocated_bytes, rows[0].reserved_bytes), (0, 0));
    assert!(!value.paths.active_root.exists());
    assert!(!value.paths.export_root.exists());
    drop(handle);
    catalog.shutdown();
    let catalog = RecordingCatalog::open(&root.join("catalog.db")).unwrap();
    assert!(matches!(
        state(&catalog.handle(), Role::Archive),
        State::Bound(_)
    ));
    assert!(matches!(
        state(&catalog.handle(), Role::Thumbnail),
        State::Offline
    ));
    assert_eq!(
        catalog.handle().volume_location(Request::Usage).unwrap(),
        usage
    );
    catalog.shutdown();
}

#[test]
fn legacy_root_missing_requires_explicit_recapture_and_none_does_not_erase_identity() {
    let (_, catalog, mut value) = capture_fixture("legacy-root-explicit-fill");
    let handle = catalog.handle();
    capture(&handle, value.clone());
    std::fs::create_dir_all(&value.paths.export_root).unwrap();
    assert!(matches!(state(&handle, Role::Export), State::Offline));
    value.roots[2].1 = Some(root_binding(&value.paths.export_root, "legacy-export"));
    value.paths.catalog_path = value.paths.catalog_path.with_file_name("new-catalog.db");
    capture(&handle, value.clone());
    let State::Bound(before) = state(&handle, Role::Export) else {
        panic!("root not bound")
    };
    value.roots[2].1 = None;
    capture(&handle, value);
    let State::Bound(after) = state(&handle, Role::Export) else {
        panic!("identity erased")
    };
    assert_eq!(before.root_identity, after.root_identity);
    assert_eq!(before.root, after.root);
    drop(handle);
    catalog.shutdown();
}

#[test]
fn legacy_root_replacement_rolls_back_other_role_capture() {
    let (_, catalog, value) = capture_fixture("legacy-root-replacement");
    let handle = catalog.handle();
    capture(&handle, value.clone());
    let revision = handle.volume_ledger_revision().unwrap();
    let usage = handle.volume_location(Request::Usage).unwrap();
    let mut replacement = value;
    replacement.roots[2].1 = Some(root_binding(
        &replacement.paths.export_root,
        "legacy-export",
    ));
    replacement.roots.swap(0, 2);
    replacement.roots[2].1.as_mut().unwrap().root_identity = "replacement".into();
    assert!(
        handle
            .volume_location(Request::CaptureLegacyRoots(Box::new(replacement)))
            .is_err()
    );
    assert!(matches!(state(&handle, Role::Export), State::Offline));
    assert_eq!(handle.volume_ledger_revision().unwrap(), revision);
    assert_eq!(handle.volume_location(Request::Usage).unwrap(), usage);
    drop(handle);
    catalog.shutdown();
}

#[test]
fn legacy_root_invalid_roles_paths_and_write_capabilities_preserve_state() {
    let (_, catalog, value) = capture_fixture("legacy-root-invalid-capture");
    let handle = catalog.handle();
    capture(&handle, value.clone());
    let revision = handle.volume_ledger_revision().unwrap();
    let usage = handle.volume_location(Request::Usage).unwrap();
    let mut changed_path = value.clone();
    changed_path.paths.archive_root = changed_path.paths.archive_root.join("changed");
    let mut duplicate = value.clone();
    duplicate.roots[3].0 = Role::Export;
    let mut missing = value.clone();
    missing.roots.pop();
    let mut writable = value.clone();
    writable.roots[0].1.as_mut().unwrap().writable = true;
    let mut wrong_id = value.clone();
    wrong_id.roots[0].1.as_mut().unwrap().id = "named-volume".into();
    let mut quota = value;
    quota.roots[0].1.as_mut().unwrap().limit_bytes = Some(100);
    for invalid in [changed_path, duplicate, missing, writable, wrong_id, quota] {
        assert!(
            handle
                .volume_location(Request::CaptureLegacyRoots(Box::new(invalid)))
                .is_err()
        );
        assert_eq!(handle.volume_ledger_revision().unwrap(), revision);
        assert_eq!(handle.volume_location(Request::Usage).unwrap(), usage);
    }
    drop(handle);
    catalog.shutdown();
}
