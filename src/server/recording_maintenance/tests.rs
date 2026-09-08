use super::{ApiPrincipal, ServerControlHandler, ServerState, dispatch, proto};
use crate::{
    access::AccessRole,
    storage::catalog::{CatalogRecording, RecordingCatalog},
    webrtc::SessionId,
};
use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    time::{Duration, Instant},
};

mod audit;

struct Fixture {
    root: PathBuf,
    catalog: Option<RecordingCatalog>,
    handler: ServerControlHandler,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-maintenance-server-{:032x}",
            rand::random::<u128>()
        ));
        std::fs::create_dir(&root).unwrap();
        let file = root.join("recording.mp4");
        std::fs::write(&file, [42; 64]).unwrap();
        let catalog = RecordingCatalog::open(&root.join("recordings.db")).unwrap();
        let handle = catalog.handle();
        handle
            .upsert_recording(CatalogRecording {
                id: "recording".to_owned(),
                stream_id: "front/sub".to_owned(),
                source_id: Some("front".to_owned()),
                logical_stream_id: Some("sub".to_owned()),
                started_at_ms: 1_000,
                ended_at_ms: Some(2_000),
                path: file.to_str().unwrap().to_owned(),
                init_offset: 0,
                init_len: 8,
                finalized: true,
            })
            .unwrap();
        handle
            .update_recording_path("recording", &file, true)
            .unwrap();
        let mut state = ServerState::empty();
        state.catalog = Some(handle);
        state.storage_config.long_term_path = root.clone();
        let (_router, router_tx) = crate::runtime::Router::new().unwrap();
        Self {
            root,
            catalog: Some(catalog),
            handler: ServerControlHandler::new(state, router_tx),
        }
    }

    fn request(
        &self,
        action: proto::recording_maintenance_command::Action,
    ) -> Result<proto::ok::Result, super::ControlCommandError> {
        dispatch(
            &self.handler,
            SessionId::from_u64(0),
            &ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            proto::RecordingMaintenanceCommand {
                action: Some(action),
            },
        )
    }

    fn preview(&self) -> proto::RecordingDeletionJob {
        let result = self
            .request(proto::recording_maintenance_command::Action::Preview(
                proto::PreviewRecordingDeletion {
                    scope: Some(proto::RecordingMaintenanceScope {
                        source_id: "front".to_owned(),
                        stream_id: "sub".to_owned(),
                        selection: Some(
                            proto::recording_maintenance_scope::Selection::RecordingId(
                                "recording".to_owned(),
                            ),
                        ),
                    }),
                    reason: proto::RecordingDeletionReason::Operator as i32,
                },
            ))
            .unwrap();
        let proto::ok::Result::RecordingDeletionJob(job) = result else {
            panic!("expected maintenance preview");
        };
        job
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self
            .handler
            .state
            .maintenance_active
            .load(std::sync::atomic::Ordering::Acquire)
            && Instant::now() < deadline
        {
            std::thread::park_timeout(Duration::from_millis(10));
        }
        self.catalog.take().unwrap().shutdown();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn maintenance_blocks_export_retry_without_losing_the_existing_job() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let fixture = Fixture::new();
    let state = &fixture.handler.state;
    let request = proto::CreateExportJob {
        job_id: "retryable".to_owned(),
        source_id: "front".to_owned(),
        stream_id: "sub".to_owned(),
        ..Default::default()
    };
    let job = proto::ExportJob {
        job_id: request.job_id.clone(),
        status: proto::ExportJobStatus::Failed as i32,
        retryable: true,
        ..Default::default()
    };
    let cancel = Arc::new(AtomicBool::new(false));
    state.export_jobs.lock().unwrap().insert(
        request.job_id.clone(),
        super::super::ExportJobRecord {
            requester_id: "administrator".to_owned(),
            artifact_id: "failed-attempt".to_owned(),
            request,
            job: job.clone(),
            path: None,
            cancel: Arc::clone(&cancel),
            created_at_ms: 1,
            started_at_ms: Some(1),
            updated_at_ms: 2,
            completed_at_ms: Some(2),
            downloaded_at_ms: None,
        },
    );
    let permit = super::worker::admit(state).unwrap();
    let error = super::super::retry_export_job(state, "administrator", "retryable").unwrap_err();
    let retained = state
        .export_jobs
        .lock()
        .unwrap()
        .get("retryable")
        .map(|record| record.job.clone());
    drop(permit);

    assert_eq!(error._http_status, 409);
    assert_eq!(retained, Some(job));
    assert!(!cancel.load(Ordering::Acquire));
}

#[test]
fn maintenance_admission_waits_for_storage_coordination_without_claiming_media() {
    use std::sync::atomic::Ordering;
    let fixture = Fixture::new();
    let state = &fixture.handler.state;
    let configuration = state.config_update.lock().unwrap();
    let admission = super::worker::admit(state);
    drop(configuration);

    assert!(admission.is_err());
    assert!(!state.maintenance_active.load(Ordering::Acquire));
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    drop(super::worker::admit(state).unwrap());
}

#[test]
fn revoked_execution_authorization_preserves_media_and_claims() {
    use crate::storage::catalog::maintenance::jobs::{Action, Nonce};
    let fixture = Fixture::new();
    let preview = fixture.preview();
    let principal = ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let catalog = fixture.catalog.as_ref().unwrap().handle();
    catalog
        .recording_deletion_intent(
            &principal.id(),
            Action::Confirm {
                id: preview.job_id.clone(),
                nonce: Nonce::parse(preview.confirmation_nonce.as_deref().unwrap()).unwrap(),
                expected_revision: preview.revision,
            },
        )
        .unwrap();
    let archive = crate::storage::long_term::inspection::Archive::open(&fixture.root).unwrap();
    let error = catalog
        .execute_recording_deletion_authorized(&principal.id(), &preview.job_id, &archive, |_| {
            anyhow::bail!("revoked test principal")
        })
        .unwrap_err();

    assert!(error.to_string().contains("revoked"));
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
    assert_eq!(
        catalog
            .recording_deletion_progress(&principal.id(), &preview.job_id)
            .unwrap()
            .deleted,
        0
    );
    assert!(!fixture.root.join(".maintenance").exists());
}

#[test]
fn reconciliation_protocol_requires_owned_reports_and_explicit_remedies() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.root.join("recording.mp4")).unwrap();
    let unknown = fixture.root.join("unknown.mp4");
    std::fs::write(&unknown, [24; 64]).unwrap();
    let result = fixture
        .request(
            proto::recording_maintenance_command::Action::InspectCatalog(
                proto::InspectRecordingCatalog {},
            ),
        )
        .unwrap();
    let proto::ok::Result::RecordingReconciliationReport(report) = result else {
        panic!("expected reconciliation report");
    };
    assert!(report.complete);
    let missing = report
        .items
        .iter()
        .find(|item| item.kind == proto::RecordingDriftKind::MissingFile as i32)
        .unwrap();
    let unknown_item = report
        .items
        .iter()
        .find(|item| item.kind == proto::RecordingDriftKind::UnknownFile as i32)
        .unwrap();
    let request = proto::ApplyRecordingRemedy {
        report_id: report.report_id.clone(),
        item_id: missing.item_id.clone(),
        remedy: proto::RecordingRemedy::RetainTombstone as i32,
    };
    assert_remedy_owner(&fixture, &request);
    assert_eq!(
        unknown_item.remedies,
        [proto::RecordingRemedy::Ignore as i32]
    );
    fixture
        .request(proto::recording_maintenance_command::Action::ApplyRemedy(
            proto::ApplyRecordingRemedy {
                report_id: report.report_id,
                item_id: unknown_item.item_id.clone(),
                remedy: proto::RecordingRemedy::Ignore as i32,
            },
        ))
        .unwrap();
    let completed = fixture
        .request(proto::recording_maintenance_command::Action::ApplyRemedy(
            request.clone(),
        ))
        .unwrap();
    let retried = fixture
        .request(proto::recording_maintenance_command::Action::ApplyRemedy(
            request,
        ))
        .unwrap();
    assert_eq!(completed, retried);
    assert_eq!(std::fs::read(unknown).unwrap(), [24; 64]);
    assert_eq!(
        fixture
            .catalog
            .as_ref()
            .unwrap()
            .handle()
            .stats()
            .unwrap()
            .recording_files,
        0
    );
}

#[test]
fn catalog_remedies_wait_for_storage_coordination() {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.root.join("recording.mp4")).unwrap();
    let result = fixture
        .request(
            proto::recording_maintenance_command::Action::InspectCatalog(
                proto::InspectRecordingCatalog {},
            ),
        )
        .unwrap();
    let proto::ok::Result::RecordingReconciliationReport(report) = result else {
        panic!("expected catalog report");
    };
    let item = report
        .items
        .iter()
        .find(|item| item.kind == proto::RecordingDriftKind::MissingFile as i32)
        .unwrap();
    let guard = fixture.handler.state.config_update.lock().unwrap();
    let result = fixture.request(proto::recording_maintenance_command::Action::ApplyRemedy(
        proto::ApplyRecordingRemedy {
            report_id: report.report_id.clone(),
            item_id: item.item_id.clone(),
            remedy: proto::RecordingRemedy::RetainTombstone as i32,
        },
    ));
    drop(guard);

    assert_eq!(result.unwrap_err()._http_status, 409);
    assert_eq!(
        fixture
            .catalog
            .as_ref()
            .unwrap()
            .handle()
            .stats()
            .unwrap()
            .recording_files,
        1
    );
}

fn assert_remedy_owner(fixture: &Fixture, request: &proto::ApplyRecordingRemedy) {
    let mut other = ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST));
    other.identity = super::super::ApiPrincipalIdentity::Credential {
        id: uuid::Uuid::new_v4(),
        revision: 1,
    };
    let command = proto::RecordingMaintenanceCommand {
        action: Some(proto::recording_maintenance_command::Action::ApplyRemedy(
            request.clone(),
        )),
    };
    let error = dispatch(&fixture.handler, SessionId::from_u64(0), &other, command).unwrap_err();
    assert_eq!(error._http_status, 404);
}

#[test]
fn maintenance_requires_administrator_and_preview_retains_scope_nonce_and_gaps() {
    let fixture = Fixture::new();
    let mut principal = ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST));
    principal.role = AccessRole::User;
    let command = proto::RecordingMaintenanceCommand {
        action: Some(proto::recording_maintenance_command::Action::List(
            proto::ListRecordingDeletions::default(),
        )),
    };
    assert_eq!(
        dispatch(
            &fixture.handler,
            SessionId::from_u64(0),
            &principal,
            command
        )
        .unwrap_err()
        ._http_status,
        403
    );
    let preview = fixture.preview();
    assert_eq!(preview.objects.len(), 1);
    assert_eq!(preview.objects[0].recording_id, "recording");
    assert_eq!(preview.required_confirmation_text, "DELETE 1");
    assert_eq!(preview.confirmation_nonce.as_ref().unwrap().len(), 64);
    assert_eq!(preview.gaps.len(), 1);
    assert_eq!(
        std::fs::read(fixture.root.join("recording.mp4")).unwrap(),
        [42; 64]
    );
}

#[test]
fn protocol_confirmation_rejects_wrong_text_and_reports_durable_deletion() {
    let fixture = Fixture::new();
    let preview = fixture.preview();
    let mut confirm = proto::ConfirmRecordingDeletion {
        job_id: preview.job_id.clone(),
        expected_revision: preview.revision,
        confirmation_nonce: preview.confirmation_nonce.unwrap(),
        confirmation_text: "DELETE ALL".to_owned(),
    };
    assert_eq!(
        fixture
            .request(proto::recording_maintenance_command::Action::Confirm(
                confirm.clone()
            ))
            .unwrap_err()
            ._http_status,
        400
    );
    assert!(fixture.root.join("recording.mp4").exists());
    confirm.confirmation_text = preview.required_confirmation_text;
    fixture
        .request(proto::recording_maintenance_command::Action::Confirm(
            confirm,
        ))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while fixture
        .handler
        .state
        .maintenance_active
        .load(std::sync::atomic::Ordering::Acquire)
        && Instant::now() < deadline
    {
        std::thread::park_timeout(Duration::from_millis(10));
    }
    let result = fixture
        .request(proto::recording_maintenance_command::Action::Get(
            proto::GetRecordingDeletion {
                job_id: preview.job_id,
            },
        ))
        .unwrap();
    let proto::ok::Result::RecordingDeletionJob(job) = result else {
        panic!("expected progress");
    };
    assert_eq!(job.deleted_count, 1);
    assert_eq!(job.status, proto::RecordingDeletionStatus::Deleted as i32);
    assert!(job.confirmation_nonce.is_none());
    assert!(!fixture.root.join("recording.mp4").exists());
}
