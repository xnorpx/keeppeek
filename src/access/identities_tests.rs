use super::{Directory, IdentityInput};
use crate::access::{AccessRole, CameraAccess, external::Grant};

fn login<'a>(namespace: &'a str, subject: &'a str, name: &'a str) -> IdentityInput<'a> {
    IdentityInput {
        provider_id: "office",
        namespace,
        subject,
        display_name: name,
        grant: Grant {
            role: AccessRole::User,
            camera_access: CameraAccess::default(),
        },
        now_ms: 1_000,
    }
}

#[test]
fn external_identities_preserve_subject_binding_when_names_change() {
    let mut directory = Directory::default();
    let alice = directory
        .provision(login("oidc:https://id.example", "alice", "Alice"))
        .unwrap();
    let renamed = directory
        .provision(login("oidc:https://id.example", "alice", "New name"))
        .unwrap();
    assert_eq!(alice.id, renamed.id);
    assert_eq!(alice.revision, renamed.revision);
    assert_eq!(renamed.display_name, "New name");
    let other = directory
        .provision(login("oidc:https://other.example", "alice", "Alice"))
        .unwrap();
    assert_ne!(alice.id, other.id);
    let proxy = directory
        .provision(login("proxy:office", "alice", "Alice"))
        .unwrap();
    assert_ne!(alice.id, proxy.id);
    let serialized = toml::to_string(&directory).unwrap();
    assert!(!serialized.contains("oidc:https://id.example"));
    let loaded: Directory = toml::from_str(&serialized).unwrap();
    loaded.validate().unwrap();
    assert!(loaded.active(alice.id, alice.revision).is_some());
}

#[test]
fn external_identity_policy_changes_and_revocation_invalidate_old_revisions() {
    let mut directory = Directory::default();
    let alice = directory
        .provision(login("issuer", "alice", "Alice"))
        .unwrap();
    let mut changed = login("issuer", "alice", "Alice");
    changed.grant.camera_access = CameraAccess::unrestricted();
    let new = directory.provision(changed).unwrap();
    assert!(directory.active(alice.id, alice.revision).is_none());
    assert!(directory.active(new.id, new.revision).is_some());
    directory.revoke(new.id).unwrap();
    assert!(directory.active(new.id, new.revision).is_none());
    assert!(
        directory
            .provision(login("issuer", "alice", "Alice"))
            .is_err()
    );
}

#[test]
fn external_identity_failure_does_not_change_directory() {
    let mut directory = Directory::default();
    let before = toml::to_string(&directory).unwrap();
    assert!(directory.provision(login("issuer", "", "Alice")).is_err());
    assert_eq!(toml::to_string(&directory).unwrap(), before);
    let subject = "x".repeat(257);
    assert!(
        directory
            .provision(login("issuer", &subject, "Alice"))
            .is_err()
    );
    assert_eq!(toml::to_string(&directory).unwrap(), before);
}

#[test]
fn prepared_identity_preserves_the_provisional_id_and_increments_revision_once() {
    let mut directory = Directory::default();
    let prepared = directory
        .prepare_admission(login("issuer", "alice", "Alice"))
        .unwrap();
    assert!(directory.records.is_empty());
    let first = directory.admit_prepared(&prepared).unwrap();
    assert_eq!(first.id, prepared.identity().id);
    let mut administrator = login("issuer", "alice", "Alice");
    administrator.grant = Grant {
        role: AccessRole::Administrator,
        camera_access: CameraAccess::unrestricted(),
    };
    let elevated = directory.prepare_admission(administrator).unwrap();
    assert_eq!(directory.records[0].revision, 1);
    let admitted = directory.admit_prepared(&elevated).unwrap();
    assert_eq!(admitted.id, first.id);
    assert_eq!(admitted.created_at_ms, first.created_at_ms);
    assert_eq!(admitted.revision, 2);
    assert_eq!(directory.admit_prepared(&elevated).unwrap().revision, 2);
}

#[test]
fn prepared_identity_cannot_bypass_a_revoked_fingerprint_with_a_new_uuid() {
    let mut directory = Directory::default();
    let existing = directory
        .provision(login("issuer", "alice", "Alice"))
        .unwrap();
    let mut prepared = directory
        .prepare_admission(login("issuer", "alice", "Alice"))
        .unwrap();
    directory.revoke(existing.id).unwrap();
    let before = directory.clone();
    assert!(directory.admit_prepared(&prepared).is_err());
    prepared.identity.id = uuid::Uuid::new_v4();
    assert!(directory.admit_prepared(&prepared).is_err());
    assert_eq!(directory, before);
}

#[test]
fn prepared_identity_rechecks_capacity_and_revision_overflow_without_mutation() {
    let mut directory = Directory::default();
    let prepared = directory
        .prepare_admission(login("issuer", "new-subject", "New"))
        .unwrap();
    for index in 0..super::IDENTITY_LIMIT {
        directory
            .provision(login("issuer", &index.to_string(), "Existing"))
            .unwrap();
    }
    let before = directory.clone();
    assert!(directory.admit_prepared(&prepared).is_err());
    assert_eq!(directory, before);
    let mut changed = login("issuer", "0", "Existing");
    changed.grant = Grant {
        role: AccessRole::Administrator,
        camera_access: CameraAccess::unrestricted(),
    };
    let prepared = directory.prepare_admission(changed).unwrap();
    directory.records[0].revision = u64::MAX;
    let before = directory.clone();
    assert!(directory.admit_prepared(&prepared).is_err());
    assert_eq!(directory, before);
}
