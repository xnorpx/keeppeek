use super::super::tests::{Fixture, SESSION, replacement_archive};
use super::*;

#[cfg(windows)]
#[test]
fn verification_upload_is_private_before_receiving_archive_secrets() {
    let fixture = Fixture::new();
    let upload = Upload::new(&fixture.directory, Uuid::new_v4()).unwrap();
    crate::backup::assert_private(upload.file.as_ref().unwrap());
    let path = upload.path.clone();
    drop(upload);
    assert!(!path.exists());
}

fn fixture() -> (Fixture, Vec<u8>) {
    let mut fixture = Fixture::new();
    let archive = replacement_archive(&fixture);
    fixture.state.backup_manager = Some(Arc::new(
        crate::backup::BackupManager::open_with_config_update(
            fixture.directory.join("config.toml"),
            fixture.state.config_update.clone(),
        )
        .unwrap(),
    ));
    (fixture, archive)
}

fn begin_archive(fixture: &Fixture, archive: &[u8]) -> proto::ConfigurationRestoreVerification {
    begin(
        &fixture.state,
        SESSION,
        &fixture.owner,
        proto::BeginConfigurationRestoreVerification {
            archive_bytes: archive.len() as u64,
            archive_sha256: Sha256::digest(archive)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        },
    )
    .unwrap()
}

fn upload(fixture: &Fixture, archive: &[u8]) -> proto::ConfigurationRestoreVerification {
    let mut response = begin_archive(fixture, archive);
    for chunk in archive.chunks(MAX_CHUNK_BYTES) {
        response = append(
            &fixture.state,
            SESSION,
            &fixture.owner,
            proto::AppendConfigurationRestoreVerification {
                preparation_id: response.preparation_id,
                offset: response.received_bytes,
                data: chunk.to_vec(),
            },
        )
        .unwrap();
    }
    assert!(response.ready);
    assert_eq!(response.received_bytes, archive.len() as u64);
    response
}

fn confirm_archive(fixture: &Fixture, archive: &[u8]) -> proto::ConfigurationRestoreVerification {
    let preparation = upload(fixture, archive);
    assert!(preparation.requires_administrator_confirmation);
    let proof = super::super::verify_bearer(
        &fixture.state,
        SESSION,
        &fixture.owner,
        proto::VerifyAdministratorBearer {
            configuration_plan_id: preparation.preparation_id.clone(),
            access_key: fixture.key.canonical(),
        },
    )
    .unwrap();
    confirm(
        &fixture.state,
        SESSION,
        &fixture.owner,
        proto::ConfirmConfigurationRestoreVerification {
            preparation_id: preparation.preparation_id,
            confirm: true,
            administrator_confirmation: Some(proto::AdministratorConfirmation {
                verification_id: proof.verification_id,
                confirm: true,
            }),
        },
    )
    .unwrap()
}

fn apply_archive(fixture: &Fixture, archive: &[u8]) -> rouille::Response {
    let classification = fixture
        .state
        .api_session_owners
        .lock()
        .unwrap()
        .get(&SESSION)
        .map(|owner| owner.classification)
        .unwrap_or_else(|| crate::access::ClientClassification {
            peer_address: "203.0.113.1".parse().unwrap(),
            effective_address: "203.0.113.1".parse().unwrap(),
            local: false,
            reason: crate::access::ClientClassificationReason::DirectRemote,
        });
    crate::server::config_apply(
        &rouille::Request::fake_https_from(
            "203.0.113.1:443".parse().unwrap(),
            "POST",
            "/config/apply",
            vec![
                ("Content-Type".into(), "application/zip".into()),
                ("Content-Length".into(), archive.len().to_string()),
            ],
            archive.to_vec(),
        ),
        &fixture.state,
        &crate::server::AuthenticatedApiRequest {
            principal: fixture.owner.clone(),
            classification,
        },
    )
}

#[test]
fn preserving_restore_keeps_the_existing_http_contract_but_rechecks_browser_revocation() {
    for revoked in [false, true] {
        let (fixture, _) = fixture();
        let path = fixture.directory.join("config.toml");
        crate::config::write_private_file(&crate::config::secrets_path(&path), b"").unwrap();
        let (archive, _) = crate::backup::create_bundle(
            std::io::Cursor::new(Vec::new()),
            crate::backup::CreateBundleOptions {
                config_path: &path,
                sections: &[],
                created_at_unix_ms: crate::server::unix_time_ms(),
            },
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        // An ordinary preserving HTTP restore does not require a control connection.
        crate::server::close_api_session(&fixture.state, SESSION);
        if revoked {
            let ApiPrincipalIdentity::External { browser, .. } = fixture.owner.identity else {
                panic!("expected external fixture principal");
            };
            fixture
                .state
                .authentication
                .lock()
                .unwrap()
                .sessions
                .revoke(browser);
        }
        assert_eq!(
            apply_archive(&fixture, &archive.into_inner()).status_code,
            if revoked { 409 } else { 202 },
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            crate::backup::active_restore(&path, crate::server::unix_time_ms())
                .unwrap()
                .is_some(),
            !revoked,
        );
    }
}

#[test]
fn exact_restore_confirmation_is_consumed_before_staging_and_does_not_activate_early() {
    let (fixture, archive) = fixture();
    let path = fixture.directory.join("config.toml");
    let original = std::fs::read(&path).unwrap();
    let preparation = confirm_archive(&fixture, &archive);
    assert!(preparation.confirmed);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert_eq!(apply_archive(&fixture, &archive).status_code, 202);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(
        fixture
            .state
            .configuration_plans
            .restore_proofs
            .lock()
            .unwrap()
            .entries
            .is_empty()
    );
    assert_ne!(apply_archive(&fixture, &archive).status_code, 202);
    crate::backup::recover_pending_restore(&path, crate::server::unix_time_ms()).unwrap();
    assert!(config::load_config(&path).unwrap().external_auth.is_none());
}

#[test]
fn restore_confirmation_requires_a_replacement_and_explicit_consent() {
    let (fixture, archive) = fixture();
    let preparation = upload(&fixture, &archive);
    for consent in [false, true] {
        assert!(
            confirm(
                &fixture.state,
                SESSION,
                &fixture.owner,
                proto::ConfirmConfigurationRestoreVerification {
                    preparation_id: preparation.preparation_id.clone(),
                    confirm: consent,
                    administrator_confirmation: None,
                }
            )
            .is_err()
        );
    }
    assert_eq!(apply_archive(&fixture, &archive).status_code, 409);
    assert!(
        crate::backup::active_restore(
            &fixture.directory.join("config.toml"),
            crate::server::unix_time_ms()
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn restore_confirmation_is_bound_to_live_parent_target_and_exact_archive() {
    for mutation in ["parent", "secret", "archive", "expiry"] {
        let (fixture, mut archive) = fixture();
        let receipt = confirm_archive(&fixture, &archive);
        match mutation {
            "parent" => {
                crate::server::close_api_session(&fixture.state, SESSION);
            }
            "secret" => {
                config::write_private_file(
                    &fixture.directory.join("secrets.toml"),
                    b"UNRELATED = 'changed'\n",
                )
                .unwrap();
            }
            "archive" => {
                archive.push(0);
            }
            "expiry" => {
                fixture
                    .state
                    .configuration_plans
                    .restore_proofs
                    .lock()
                    .unwrap()
                    .entries
                    .get_mut(&Uuid::parse_str(&receipt.preparation_id).unwrap())
                    .unwrap()
                    .expires = Instant::now();
            }
            _ => unreachable!(),
        }
        let original = std::fs::read(fixture.directory.join("config.toml")).unwrap();
        assert_ne!(
            apply_archive(&fixture, &archive).status_code,
            202,
            "{mutation}"
        );
        assert_eq!(
            std::fs::read(fixture.directory.join("config.toml")).unwrap(),
            original
        );
        assert!(
            crate::backup::active_restore(
                &fixture.directory.join("config.toml"),
                crate::server::unix_time_ms()
            )
            .unwrap()
            .is_none()
        );
    }
}

#[test]
fn restore_upload_rejects_wrong_offsets_and_digest_and_removes_private_temporary_files() {
    for wrong_offset in [false, true] {
        let (fixture, archive) = fixture();
        let receipt = begin_archive(&fixture, &archive);
        let mut bytes = archive.clone();
        if !wrong_offset {
            bytes[0] ^= 1;
        }
        let request = proto::AppendConfigurationRestoreVerification {
            preparation_id: receipt.preparation_id,
            offset: u64::from(wrong_offset),
            data: bytes,
        };
        assert!(!format!("{request:?}").contains("PK"));
        assert!(append(&fixture.state, SESSION, &fixture.owner, request).is_err());
        assert!(
            fixture
                .state
                .configuration_plans
                .restore_proofs
                .lock()
                .unwrap()
                .entries
                .is_empty()
        );
        assert_eq!(
            std::fs::read_dir(fixture.directory.join(".config.toml.restore-verification"))
                .unwrap()
                .count(),
            0
        );
    }
}

#[test]
fn restore_upload_bounds_capacity_and_chunks() {
    let (fixture, archive) = fixture();
    let receipt = begin_archive(&fixture, &archive);
    assert!(
        begin(
            &fixture.state,
            SESSION,
            &fixture.owner,
            proto::BeginConfigurationRestoreVerification {
                archive_bytes: MAX_ARCHIVE_BYTES + 1,
                archive_sha256: "0".repeat(64),
            }
        )
        .is_err()
    );
    assert_eq!(
        begin(
            &fixture.state,
            SESSION,
            &fixture.owner,
            proto::BeginConfigurationRestoreVerification {
                archive_bytes: 1,
                archive_sha256: "0".repeat(64),
            }
        )
        .unwrap_err()
        ._http_status,
        429
    );
    let request = proto::AppendConfigurationRestoreVerification {
        preparation_id: receipt.preparation_id,
        offset: 0,
        data: vec![0; MAX_CHUNK_BYTES + 1],
    };
    assert!(append(&fixture.state, SESSION, &fixture.owner, request).is_err());
}

#[test]
fn restore_upload_rejects_cross_session_access_and_expires_owner_upload() {
    let (fixture, archive) = fixture();
    let receipt = begin_archive(&fixture, &archive);
    assert!(
        get(
            &fixture.state,
            SessionId::from_u64(999),
            &fixture.owner,
            proto::GetConfigurationRestoreVerification {
                preparation_id: receipt.preparation_id.clone(),
            }
        )
        .is_err()
    );
    assert!(
        get(
            &fixture.state,
            SESSION,
            &fixture.owner,
            proto::GetConfigurationRestoreVerification {
                preparation_id: receipt.preparation_id,
            }
        )
        .is_ok()
    );
    fixture
        .state
        .configuration_plans
        .restore_proofs
        .lock()
        .unwrap()
        .expire(Instant::now() + PROOF_LIFETIME, i64::MAX);
    assert!(
        fixture
            .state
            .configuration_plans
            .restore_proofs
            .lock()
            .unwrap()
            .entries
            .is_empty()
    );
}
