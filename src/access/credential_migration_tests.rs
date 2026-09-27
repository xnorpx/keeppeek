use super::*;

struct TestDirectory(PathBuf);

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn fixture() -> (TestDirectory, PathBuf, AccessManager, IssuedCredential) {
    let directory = TestDirectory(
        std::env::temp_dir().join(format!("keeppeek-credential-migration-{}", Uuid::new_v4())),
    );
    std::fs::create_dir(&directory.0).unwrap();
    let path = directory.0.join("config.toml");
    std::fs::write(&path, "").unwrap();
    let manager = AccessManager::open(&path, AccessKey::default()).unwrap();
    let administrator = manager
        .create_credential("Administrator", None, AccessRole::Administrator, None, 1)
        .unwrap();
    (directory, path, manager, administrator)
}

fn remove(manager: &AccessManager, id: Uuid, revoke: bool) -> anyhow::Result<CredentialMetadata> {
    if revoke {
        manager.revoke_credential(id, now_ms())
    } else {
        manager.set_credential_enabled(id, false)
    }
}

#[test]
fn last_administrator_rejection_preserves_disk_catalog_and_session_revision() {
    for revoke in [false, true] {
        let (_directory, path, manager, administrator) = fixture();
        let authorization = format!("Bearer {}", administrator.access_key.canonical());
        manager
            .authenticate(
                "203.0.113.7".parse().unwrap(),
                &[&authorization],
                now_ms(),
                Instant::now(),
            )
            .unwrap();
        let original = std::fs::read(&path).unwrap();
        let metadata = manager.list_credentials();
        let error = remove(&manager, administrator.metadata.id, revoke).unwrap_err();
        assert!(error.to_string().contains("last remote Administrator"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(manager.list_credentials(), metadata);
        assert!(manager.credential_is_active(
            administrator.metadata.id,
            administrator.metadata.revision,
            now_ms(),
        ));
    }
}

#[test]
fn retained_permanent_administrator_allows_removal_and_invalidates_old_revision() {
    for revoke in [false, true] {
        let (_directory, path, manager, administrator) = fixture();
        let replacement = manager
            .create_credential("Replacement", None, AccessRole::Administrator, None, 1)
            .unwrap();
        remove(&manager, administrator.metadata.id, revoke).unwrap();
        remove(&manager, administrator.metadata.id, revoke).unwrap();
        assert!(!manager.credential_is_active(
            administrator.metadata.id,
            administrator.metadata.revision,
            now_ms(),
        ));
        let reopened = AccessManager::open(&path, AccessKey::default()).unwrap();
        assert!(reopened.credential_is_active(
            replacement.metadata.id,
            replacement.metadata.revision,
            now_ms(),
        ));
        assert!(remove(&manager, replacement.metadata.id, revoke).is_err());
    }
}

#[test]
fn user_and_inactive_credentials_do_not_require_a_replacement() {
    for revoke in [false, true] {
        let (_directory, _path, manager, administrator) = fixture();
        let user = manager
            .create_credential("Viewer", None, AccessRole::User, None, 1)
            .unwrap();
        remove(&manager, user.metadata.id, revoke).unwrap();
        let repeated = remove(&manager, user.metadata.id, revoke).unwrap();
        if revoke {
            assert_eq!(repeated.revision, user.metadata.revision + 1);
            assert!(
                manager
                    .set_credential_enabled(user.metadata.id, true)
                    .is_err()
            );
        } else {
            manager
                .set_credential_enabled(user.metadata.id, true)
                .unwrap();
        }
        assert!(manager.credential_is_active(
            administrator.metadata.id,
            administrator.metadata.revision,
            now_ms(),
        ));
    }
}

#[test]
fn persistence_failure_restores_revision_and_catalog() {
    for revoke in [false, true] {
        let (_directory, path, manager, _administrator) = fixture();
        let user = manager
            .create_credential("Viewer", None, AccessRole::User, None, 1)
            .unwrap();
        let metadata = manager.list_credentials();
        let original = std::fs::read(&path).unwrap();
        // A directory at the destination forces persistence to fail on every platform.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(remove(&manager, user.metadata.id, revoke).is_err());
        assert_eq!(manager.list_credentials(), metadata);
        assert!(manager.credential_is_active(user.metadata.id, user.metadata.revision, now_ms(),));
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, original).unwrap();
    }
}

#[test]
fn administrator_validation_failure_restores_catalog_and_revision() {
    for revoke in [false, true] {
        let (_directory, path, manager, administrator) = fixture();
        let mut root = crate::config::load_configuration_table(&path).unwrap();
        root.insert("port".into(), "invalid-port".into());
        std::fs::write(&path, toml::to_string(&root).unwrap()).unwrap();
        let original = std::fs::read(&path).unwrap();
        let metadata = manager.list_credentials();
        assert!(remove(&manager, administrator.metadata.id, revoke).is_err());
        assert_eq!(manager.list_credentials(), metadata);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(manager.credential_is_active(
            administrator.metadata.id,
            administrator.metadata.revision,
            now_ms(),
        ));
    }
}

#[test]
fn pending_initial_secret_and_local_recovery_do_not_count_as_remote_replacements() {
    for revoke in [false, true] {
        let initial = AccessKey::generate();
        let manager = AccessManager::ephemeral(initial);
        let id = manager.legacy_credential_id().unwrap();
        remove(&manager, id, revoke).unwrap();

        let manager = AccessManager::ephemeral(initial);
        let claimed = manager.claim_initial_access_key(initial).unwrap();
        assert!(remove(&manager, claimed.metadata.id, revoke).is_err());
        assert!(
            NetworkAccessPolicy::new(default_local_networks(), Vec::new())
                .classify("127.0.0.1".parse().unwrap(), [])
                .local
        );
    }
}

#[test]
fn expiring_administrator_is_protected_but_cannot_preserve_another_administrator() {
    for revoke in [false, true] {
        let (_directory, _path, manager, administrator) = fixture();
        let temporary = manager
            .create_credential(
                "Temporary",
                None,
                AccessRole::Administrator,
                Some(i64::MAX),
                1,
            )
            .unwrap();
        assert!(remove(&manager, administrator.metadata.id, revoke).is_err());
        remove(&manager, temporary.metadata.id, revoke).unwrap();
        let manager = AccessManager::ephemeral(AccessKey::default());
        let temporary = manager
            .create_credential(
                "Temporary",
                None,
                AccessRole::Administrator,
                Some(i64::MAX),
                1,
            )
            .unwrap();
        assert!(remove(&manager, temporary.metadata.id, revoke).is_err());
    }
}

fn external_policy(path: &Path, provision: bool) {
    let mut root = crate::config::load_configuration_table(path).unwrap();
    let policy: toml::Table = toml::from_str(
        r#"
allowed_origins = ["https://keeppeek.example"]
bearer_enabled = true
bearer_transition_until_ms = 9223372036854775807
[[providers]]
id = "company"
name = "Company"
mappings = [{claim = "role", value = "admins", role = "administrator"}]
[providers.method]
kind = "proxy"
trusted_peers = ["203.0.113.1/32"]
subject_header = "X-Identity-Subject"
role_header = "X-Identity-Role"
secret_header = "X-Identity-Secret"
shared_secret = "{secret:PROXY_SECRET}"
"#,
    )
    .unwrap();
    root.insert("external_auth".into(), policy.into());
    std::fs::write(
        path.with_file_name("secrets.toml"),
        "PROXY_SECRET = 'synthetic-test-proxy-secret'\n",
    )
    .unwrap();
    if provision {
        let config = crate::config::validated_configuration_table(path, &root).unwrap();
        let mut directory = identities::Directory::default();
        directory
            .provision_authenticated(
                identities::IdentityInput {
                    provider_id: "company",
                    namespace: "proxy:company",
                    subject: "alice",
                    display_name: "Alice",
                    grant: external::Grant {
                        role: AccessRole::Administrator,
                        camera_access: CameraAccess::unrestricted(),
                    },
                    now_ms: 1,
                },
                &config.external_auth.as_ref().unwrap().providers[0],
            )
            .unwrap();
        root.insert(
            "external_identities".into(),
            toml::Value::try_from(directory).unwrap(),
        );
    }
    crate::config::write_configuration_table(path, &root).unwrap();
}

#[test]
fn retained_external_administrator_allows_removal_and_preserves_secret_references() {
    for revoke in [false, true] {
        let (_directory, path, manager, administrator) = fixture();
        external_policy(&path, false);
        assert!(remove(&manager, administrator.metadata.id, revoke).is_err());
        let user = manager
            .create_credential("Viewer", None, AccessRole::User, None, 1)
            .unwrap();
        remove(&manager, user.metadata.id, revoke).unwrap();
        external_policy(&path, true);
        remove(&manager, administrator.metadata.id, revoke).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("{secret:PROXY_SECRET}"));
        assert!(!saved.contains("synthetic-test-proxy-secret"));
    }
}
