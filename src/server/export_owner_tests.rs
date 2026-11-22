use super::export_provenance_tests::{configured_source, current_record, restored_record};
use super::export_storage_tests::setup;
use super::*;

fn principal(identity: ApiPrincipalIdentity) -> ApiPrincipal {
    ApiPrincipal {
        identity,
        display_name: "export owner".into(),
        role: AccessRole::Administrator,
        credential_expires_at_ms: None,
    }
}

fn credential(id: Uuid, revision: u64) -> ApiPrincipal {
    principal(ApiPrincipalIdentity::Credential { id, revision })
}

fn external(id: Uuid, revision: u64) -> ApiPrincipal {
    principal(ApiPrincipalIdentity::External {
        id,
        revision,
        browser: Uuid::new_v4(),
    })
}

#[test]
fn export_owner_namespace_separates_equal_uuids_and_ignores_revision_and_browser() {
    let id = Uuid::new_v4();
    let credential_owner = credential(id, 1);
    let external_owner = external(id, 1);
    assert_eq!(credential_owner.id(), external_owner.id());
    assert_ne!(
        credential_owner.sharing_issuer().creator_id(),
        external_owner.sharing_issuer().creator_id()
    );
    assert_eq!(
        credential_owner.sharing_issuer().creator_id(),
        credential(id, 2).sharing_issuer().creator_id()
    );
    assert_eq!(
        external_owner.sharing_issuer().creator_id(),
        external(id, 2).sharing_issuer().creator_id()
    );
    assert_eq!(
        ApiPrincipal::local("127.0.0.1".parse().unwrap())
            .sharing_issuer()
            .creator_id(),
        crate::shares::IssuerBinding::LocalAdministrator.creator_id()
    );
}

#[test]
fn export_owner_capture_survives_durable_history_without_changing_normal_requester() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    let source = Uuid::new_v4();
    configured_source(&mut state, &root, source);
    state.export_history_path = Some(Arc::new(root.join("export-history.json")));
    let principal = credential(Uuid::new_v4(), 3);
    let owner = principal.sharing_issuer().creator_id();
    export_storage::provenance::create_for_principal(&state, &principal, request).unwrap();
    assert_eq!(
        completed(&state, &principal).status,
        proto::ExportJobStatus::Ready as i32
    );
    let current = current_record(&state);
    assert_eq!(current.requester_id, principal.id());
    assert_eq!(current.owner_key.as_deref(), Some(owner.as_str()));
    assert_eq!(current.source_identity, Some(source));
    let disk = std::fs::read(state.export_history_path.as_deref().unwrap()).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&disk).unwrap();
    assert_eq!(json["jobs"][0]["owner_key"].as_str(), Some(owner.as_str()));
    assert_eq!(
        restored_record(&state, &catalog.handle()).owner_key,
        Some(owner)
    );
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn deduplicated_running_export_never_adopts_another_or_legacy_owner_proof() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    configured_source(&mut state, &root, Uuid::new_v4());
    let id = Uuid::new_v4();
    let first = credential(id, 1);
    let next = external(id, 1);
    export_storage::provenance::create_for_principal(&state, &first, request.clone()).unwrap();
    assert_eq!(
        completed(&state, &first).status,
        proto::ExportJobStatus::Ready as i32
    );
    let artifact = current_record(&state).artifact_id;
    for owner in [None, Some(first.sharing_issuer().creator_id())] {
        {
            let mut jobs = state.export_jobs.lock().unwrap();
            let record = jobs.get_mut("named-export").unwrap();
            record.job.status = proto::ExportJobStatus::Running as i32;
            record.owner_key.clone_from(&owner);
        }
        let mut duplicate = request.clone();
        duplicate.job_id = Uuid::new_v4().simple().to_string();
        let reused =
            export_storage::provenance::create_for_principal(&state, &next, duplicate).unwrap();
        assert_eq!(reused.job_id, "named-export");
        let current = current_record(&state);
        assert_eq!(current.artifact_id, artifact);
        assert_eq!(current.owner_key, owner);
    }
    drop(state);
    drop(manager);
    catalog.shutdown();
}

#[test]
fn export_owner_retry_captures_new_principal_only_for_new_artifact_generation() {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    configured_source(&mut state, &root, Uuid::new_v4());
    let id = Uuid::new_v4();
    let first = credential(id, 1);
    let next = external(id, 1);
    export_storage::provenance::create_for_principal(&state, &first, request).unwrap();
    assert_eq!(
        completed(&state, &first).status,
        proto::ExportJobStatus::Ready as i32
    );
    let obsolete = current_record(&state);
    export_storage::download::checksum_failure(&state, &obsolete);
    export_storage::provenance::retry_for_principal(&state, &next, "named-export").unwrap();
    assert_eq!(
        completed(&state, &next).status,
        proto::ExportJobStatus::Ready as i32
    );
    let current = current_record(&state);
    assert_ne!(current.artifact_id, obsolete.artifact_id);
    assert_ne!(current.path, obsolete.path);
    assert_eq!(
        obsolete.owner_key,
        Some(first.sharing_issuer().creator_id())
    );
    assert_eq!(current.owner_key, Some(next.sharing_issuer().creator_id()));
    assert_eq!(
        restored_record(&state, &catalog.handle()).owner_key,
        current.owner_key
    );
    drop(state);
    drop(manager);
    catalog.shutdown();
}

fn completed(state: &ServerState, principal: &ApiPrincipal) -> proto::ExportJob {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let job = export_job(state, &principal.id(), "named-export").unwrap();
        if job.status != proto::ExportJobStatus::Running as i32 {
            return job;
        }
        assert!(Instant::now() < deadline, "owned export did not finish");
        std::thread::sleep(Duration::from_millis(10));
    }
}
