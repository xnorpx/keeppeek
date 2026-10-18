use super::super::export_provenance_tests::{configured_source, current_record};
use super::super::export_storage_tests::setup;
use super::*;
use crate::shares::{CreateGrant, IssuerBinding, Mode};
use crate::storage::RecordingCatalog;
use proto::{share_command::Action, share_result::Result as Value};

struct Fixture {
    state: ServerState,
    _catalog: RecordingCatalog,
    root: PathBuf,
    principal: ApiPrincipal,
}

impl Fixture {
    fn new() -> Self {
        let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
        state.export_history_path = Some(Arc::new(root.join("history.json")));
        let identity = Uuid::parse_str("b70a223d-3e4f-42e6-b950-c7a4e9429c77").unwrap();
        let path = configured_source(&mut state, &root, identity);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n[sharing]\nenabled = true\npublic_origin = 'https://review.example'\n");
        config::write_private_file_atomically(&path, text.as_bytes()).unwrap();
        let configuration = config::load_config(&path).unwrap();
        let clock = raw_clock().unwrap();
        let store = Store::open(&Self::store_path(&root), clock.unix_ms).unwrap();
        *state.sharing.store.lock().unwrap() = Some((store, ClockAnchor::new(clock).unwrap()));
        activate(&state, &configuration).unwrap();
        let principal = ApiPrincipal::local("127.0.0.1".parse().unwrap());
        export_storage::provenance::create_for_principal(&state, &principal, request).unwrap();
        wait_for_export(&state, &principal);
        drop(manager);
        Self {
            state,
            _catalog: catalog,
            root,
            principal,
        }
    }

    fn store_path(root: &Path) -> PathBuf {
        root.join("primary").join("guest-review").join("grants.db")
    }

    fn path(&self) -> &Path {
        self.state.camera_config_path.as_deref().unwrap()
    }

    fn action(&self, action: Action) -> Value {
        let result = dispatch(
            &self.state,
            &self.principal,
            proto::ShareCommand {
                action: Some(action),
            },
        )
        .unwrap();
        let control_ok::Result::ShareResult(result) = result else {
            panic!("sharing command must return a sharing result");
        };
        result.result.unwrap()
    }

    fn preview(&self) -> proto::ShareEvidence {
        let Value::Evidence(evidence) =
            self.action(Action::PreviewExport(proto::PreviewShareExport {
                export_job_id: "named-export".into(),
            }))
        else {
            panic!("export preview must return evidence");
        };
        evidence
    }

    fn issue(&self) -> proto::IssuedShareGrant {
        let evidence = self.preview();
        let Value::Created(issued) = self.action(Action::Create(proto::CreateShareGrant {
            request_id: Uuid::new_v4().to_string(),
            export_job_id: evidence.export_job_id,
            artifact_id: evidence.artifact_id,
            expected_sha256: evidence.sha256,
            expires_at_ms: i64::try_from(unix_time_ms()).unwrap() + 60_000,
            display_name: "Front entrance evidence".into(),
            note: "The export can become unavailable before the link expires.".into(),
            allow_stream: true,
            allow_download: true,
            max_views: Some(2),
            max_downloads: Some(2),
        })) else {
            panic!("grant creation must return one issued link");
        };
        issued
    }

    fn list(&self, page_size: u32) -> proto::ShareGrantList {
        let Value::Grants(page) = self.action(Action::List(proto::ListShareGrants {
            page_size,
            after_grant_id: None,
            all_creators: true,
        })) else {
            panic!("grant listing must return a page");
        };
        page
    }

    fn summary(&self, issued: &proto::IssuedShareGrant) -> GrantSummary {
        let id = Uuid::parse_str(&issued.grant.as_ref().unwrap().grant_id).unwrap();
        with_store(&self.state, |store, clock| store.get(id, clock))
            .unwrap()
            .unwrap()
    }

    fn session(&self, issued: &proto::IssuedShareGrant) -> (SessionContext, String) {
        let context = context(&self.state).unwrap();
        let token = issued.url.split_once('#').unwrap().1;
        let issued = with_guest_store(&self.state, &context, |store, clock| {
            store.exchange(token, &context, clock)
        })
        .unwrap();
        (context, issued.handle.into_string())
    }

    fn policy_change(&self, origin: &str) -> proto::ConfigureSharePolicy {
        proto::ConfigureSharePolicy {
            settings: Some(proto::SharePolicySettings {
                enabled: true,
                public_origin: Some(origin.into()),
            }),
            expected_revision: config::load_config(self.path())
                .unwrap()
                .sharing
                .revision
                .to_string(),
        }
    }
}

fn wait_for_export(state: &ServerState, principal: &ApiPrincipal) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let job = export_job(state, &principal.id(), "named-export").unwrap();
        if job.status != proto::ExportJobStatus::Running as i32 {
            assert_eq!(job.status, proto::ExportJobStatus::Ready as i32);
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("fixture export did not finish within ten seconds");
}

#[test]
fn oversized_multibyte_camera_labels_remain_usable_in_preview_creation_and_pages() {
    let fixture = Fixture::new();
    let name = "入口📹".repeat(10_000);
    assert!(name.len() > 48 * 1024);
    fixture.state.cameras.write().unwrap()[0].info.name = Some(name);
    let evidence = fixture.preview();
    assert!(!evidence.camera_name.is_empty());
    assert!(evidence.camera_name.len() <= 128);
    assert!(
        evidence
            .camera_name
            .chars()
            .all(|character| !character.is_control())
    );
    let first = fixture.issue();
    let second = fixture.issue();
    assert!(first.encoded_len() < 48 * 1024);
    let page = fixture.list(1);
    assert_eq!(page.grants.len(), 1);
    assert!(page.encoded_len() < 48 * 1024);
    assert_eq!(
        page.grants[0].status,
        proto::ShareGrantStatus::Active as i32
    );
    assert!(page.grants[0].evidence.as_ref().unwrap().camera_name.len() <= 128);
    let after = page.next_after_grant_id.unwrap();
    let Value::Grants(last) = fixture.action(Action::List(proto::ListShareGrants {
        page_size: 1,
        after_grant_id: Some(after),
        all_creators: true,
    })) else {
        panic!("the second listing must return the remaining grant");
    };
    assert_eq!(last.grants.len(), 1);
    assert!(last.next_after_grant_id.is_none());
    let actual: std::collections::BTreeSet<_> = [
        page.grants[0].grant_id.as_str(),
        last.grants[0].grant_id.as_str(),
    ]
    .into_iter()
    .collect();
    let expected = [
        first.grant.as_ref().unwrap().grant_id.as_str(),
        second.grant.as_ref().unwrap().grant_id.as_str(),
    ]
    .into_iter()
    .collect();
    assert_eq!(actual, expected);
}

#[test]
fn a_committed_policy_write_error_suspends_runtime_and_rejects_obsolete_sessions() {
    let fixture = Fixture::new();
    let issued = fixture.issue();
    let (old_context, handle) = fixture.session(&issued);
    let result = management::configure_with(
        &fixture.state,
        fixture.policy_change("https://replacement.example"),
        |path, revision, settings| {
            config::sharing::update_with_writer(path, revision, settings, |path, bytes| {
                config::write_private_file_atomically(path, bytes)?;
                Err(std::io::Error::other("synthetic-private-commit-payload"))
            })
        },
    );
    let error = result.expect_err("an indeterminate write must fail management");
    assert_eq!(error.code, proto::ErrorCode::Unavailable);
    assert!(!error.message.contains("synthetic-private-commit-payload"));
    let disk = config::load_config(fixture.path()).unwrap();
    assert!(disk.sharing.enabled);
    assert_eq!(
        disk.sharing.public_origin.as_deref(),
        Some("https://replacement.example")
    );
    assert_eq!(disk.sharing.revision, 2);
    assert!(context(&fixture.state).is_err());
    assert!(
        with_store(&fixture.state, |store, clock| {
            store.inspect_session(&handle, None, &old_context, clock)
        })
        .is_err()
    );
    let called = std::cell::Cell::new(false);
    assert!(
        with_guest_store(&fixture.state, &old_context, |_, _| {
            called.set(true);
            Ok(())
        })
        .is_err()
    );
    assert!(!called.get());
    let Value::Policy(policy) = fixture.action(Action::GetPolicy(proto::GetSharePolicy {})) else {
        panic!("a suspended policy must remain inspectable");
    };
    assert!(!policy.available);
    assert_eq!(policy.revision, "2");
    assert!(policy.unavailable_reason.is_some());
    fixture.action(Action::ConfigurePolicy(
        fixture.policy_change("https://replacement.example"),
    ));
    let (fresh_context, _) = fixture.session(&issued);
    assert_ne!(fresh_context, old_context);
    assert_eq!(fresh_context.origin, "https://replacement.example");
}

#[test]
fn invalid_policy_candidates_preserve_existing_runtime_sessions_and_configuration() {
    let fixture = Fixture::new();
    let issued = fixture.issue();
    let (before, handle) = fixture.session(&issued);
    let bytes = std::fs::read(fixture.path()).unwrap();
    let called = std::cell::Cell::new(false);
    let result = management::configure_with(
        &fixture.state,
        fixture.policy_change("http://invalid.example"),
        |_, _, _| {
            called.set(true);
            Err(config::sharing::UpdateError::Persistence)
        },
    );
    assert!(result.is_err());
    assert!(!called.get());
    assert_eq!(std::fs::read(fixture.path()).unwrap(), bytes);
    assert_eq!(context(&fixture.state).unwrap(), before);
    let session = with_guest_store(&fixture.state, &before, |store, clock| {
        store.inspect_session(&handle, None, &before, clock)
    })
    .unwrap();
    assert!(session.media_active);
    assert_eq!(session.grant.id, fixture.summary(&issued).id);
}

#[test]
fn equivalent_source_uuid_spellings_revoke_the_selected_live_grant() {
    let fixture = Fixture::new();
    let source = current_record(&fixture.state).source_identity.unwrap();
    for selector in [
        source.to_string().to_uppercase(),
        source.simple().to_string(),
    ] {
        let issued = fixture.issue();
        let Value::RevokedEvidence(result) =
            fixture.action(Action::RevokeEvidence(proto::RevokeShareEvidence {
                evidence: Some(proto::revoke_share_evidence::Evidence::SourceIdentity(
                    selector,
                )),
                reason: "The operator revoked this camera's guest access.".into(),
            }))
        else {
            panic!("source revocation must return a bounded count");
        };
        assert_eq!(result.revoked_count, 1);
        let grant = fixture.summary(&issued);
        assert!(grant.revoked_at_ms.is_some());
        assert_eq!(grant.revision, 2);
        let context = context(&fixture.state).unwrap();
        assert!(
            with_guest_store(&fixture.state, &context, |store, clock| {
                store.exchange(issued.url.split_once('#').unwrap().1, &context, clock)
            })
            .is_err()
        );
    }
}

#[test]
fn a_missing_owned_artifact_is_unavailable_even_while_its_history_remains_ready() {
    let fixture = Fixture::new();
    let issued = fixture.issue();
    assert_eq!(
        fixture.list(50).grants[0].status,
        proto::ShareGrantStatus::Active as i32
    );
    let record = current_record(&fixture.state);
    std::fs::remove_file(record.path.unwrap()).unwrap();
    assert_eq!(
        current_record(&fixture.state).job.status,
        proto::ExportJobStatus::Ready as i32
    );
    let page = fixture.list(50);
    assert_eq!(page.grants.len(), 1);
    assert_eq!(page.grants[0].grant_id, issued.grant.unwrap().grant_id);
    assert_eq!(
        page.grants[0].status,
        proto::ShareGrantStatus::Unavailable as i32
    );
    assert!(evidence::preview(&fixture.state, &fixture.principal, "named-export").is_err());
}

#[test]
fn activating_policy_reclaims_sessions_and_reservations_without_refunding_delivered_access() {
    let fixture = Fixture::new();
    let issued = fixture.issue();
    let binding = fixture.summary(&issued).intent.binding;
    let (old_context, first) = fixture.session(&issued);
    let (_, second) = fixture.session(&issued);
    with_guest_store(&fixture.state, &old_context, |store, clock| {
        let ticket = store.begin_delivery(&first, &old_context, &binding, Mode::Stream, clock)?;
        store.commit_delivery(&ticket, &old_context, &binding, clock)?;
        store.begin_delivery(&second, &old_context, &binding, Mode::Download, clock)?;
        store.begin_delivery(&second, &old_context, &binding, Mode::Download, clock)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(fixture.summary(&issued).views, 1);
    fixture.action(Action::ConfigurePolicy(
        fixture.policy_change("https://rotated.example"),
    ));
    let (current, fresh) = fixture.session(&issued);
    fixture.session(&issued);
    assert_ne!(current, old_context);
    assert!(with_guest_store(&fixture.state, &old_context, |_, _| Ok(())).is_err());
    assert!(
        with_store(&fixture.state, |store, clock| {
            store.inspect_session(&first, None, &old_context, clock)
        })
        .is_err()
    );
    let grant = fixture.summary(&issued);
    assert_eq!(grant.views, 1);
    assert_eq!(grant.downloads, 0);
    with_guest_store(&fixture.state, &current, |store, clock| {
        let ticket = store.begin_delivery(&fresh, &current, &binding, Mode::Stream, clock)?;
        store.commit_delivery(&ticket, &current, &binding, clock)?;
        let download = store.begin_delivery(&fresh, &current, &binding, Mode::Download, clock)?;
        store.commit_delivery(&download, &current, &binding, clock)?;
        Ok(())
    })
    .unwrap();
    assert_eq!(fixture.summary(&issued).views, 2);
    assert_eq!(fixture.summary(&issued).downloads, 1);
}

#[test]
fn overdue_audit_records_are_hidden_before_bounded_maintenance_purges_them() {
    let fixture = Fixture::new();
    let binding = evidence::preview(&fixture.state, &fixture.principal, "named-export")
        .unwrap()
        .binding;
    let sampled = raw_clock().unwrap();
    let ancient = sampled.unix_ms - 40 * 86_400_000;
    fixture.state.sharing.store.lock().unwrap().take();
    let path = fixture
        .root
        .join("primary")
        .join("audit-review")
        .join("grants.db");
    let mut store = Store::open(&path, ancient).unwrap();
    for index in 0..257 {
        let clock = Clock {
            unix_ms: ancient + i64::from(index) * 2,
            monotonic: sampled.monotonic,
        };
        store
            .create(
                CreateGrant {
                    request_id: Uuid::new_v4(),
                    issuer: IssuerBinding::LocalAdministrator,
                    binding: binding.clone(),
                    label: "Expired historical grant".into(),
                    note: String::new(),
                    expires_at_ms: clock.unix_ms + 1,
                    allow_stream: true,
                    allow_download: false,
                    max_views: Some(1),
                    max_downloads: None,
                },
                clock,
            )
            .unwrap();
    }
    let anchor = ClockAnchor::new(Clock {
        unix_ms: ancient,
        monotonic: sampled.monotonic,
    })
    .unwrap();
    *fixture.state.sharing.store.lock().unwrap() = Some((store, anchor));
    assert!(fixture.list(100).grants.is_empty());
    let count = || with_store(&fixture.state, |store, clock| Ok(store.all(clock)?.len())).unwrap();
    assert_eq!(count(), 257);
    maintain(&fixture.state);
    assert_eq!(count(), 1);
    maintain(&fixture.state);
    assert_eq!(count(), 0);
}

#[test]
fn disabled_sharing_keeps_only_the_configured_isolated_document_context() {
    let fixture = Fixture::new();
    let issued = fixture.issue();
    let previous = context(&fixture.state).unwrap();
    let mut config = config::load_config(fixture.path()).unwrap();
    config.sharing.enabled = false;
    activate(&fixture.state, &config).unwrap();
    let document = page_context(&fixture.state).unwrap();
    assert_eq!(document.origin, previous.origin);
    assert_ne!(document.policy_revision, previous.policy_revision);
    assert!(context(&fixture.state).is_err());
    let token = issued.url.split_once('#').unwrap().1;
    assert!(
        with_guest_store(&fixture.state, &document, |store, clock| {
            store.exchange(token, &document, clock)
        })
        .is_err()
    );
    assert!(
        with_guest_store(&fixture.state, &previous, |store, clock| {
            store.exchange(token, &previous, clock)
        })
        .is_err()
    );
}
