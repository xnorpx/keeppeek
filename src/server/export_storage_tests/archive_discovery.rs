//! Recovery through the authenticated export controls after display history expires.

use super::*;
use proto::export_command::Action as ExportAction;

fn handle(
    handler: &ServerControlHandler,
    principal: &ApiPrincipal,
    action: ExportAction,
) -> Result<(control_ok::Result, Vec<OutboundDataMessage>), ControlCommandError> {
    handler.handle_export(
        principal,
        proto::ExportCommand {
            action: Some(action),
        },
    )
}

fn list(handler: &ServerControlHandler, principal: &ApiPrincipal) -> Vec<proto::ExportJob> {
    let (control_ok::Result::ExportJobs(list), _) =
        handle(handler, principal, ExportAction::List(Default::default())).unwrap()
    else {
        panic!("expected the requester's export list");
    };
    list.jobs
}

fn get(id: &str) -> ExportAction {
    ExportAction::Get(proto::GetExportJob { job_id: id.into() })
}

fn download(id: &str) -> ExportAction {
    ExportAction::Download(proto::DownloadExport {
        job_id: id.into(),
        channel: proto::DataChannelKind::ReliableData as i32,
        artifact: 0,
    })
}

#[test]
fn fresh_requester_discovers_archived_export_after_history_pruning() {
    rehearse_discovery(false);
}

#[test]
#[ignore = "paired requester export listing latency on a real verified archive fixture"]
fn measure_requester_export_listing_after_archive_verification() {
    rehearse_discovery(true);
}

fn measure_lists(handler: &ServerControlHandler, owner: &ApiPrincipal, phase: &str) {
    let mut history_only = Vec::new();
    let mut with_archive = Vec::new();
    for ordinal in 0..43 {
        for legacy in [ordinal % 2 == 0, ordinal % 2 != 0] {
            let start = Instant::now();
            if legacy {
                cleanup_expired_exports(&handler.state);
                std::hint::black_box(export_jobs(&handler.state, &owner.id()));
            } else {
                std::hint::black_box(list(handler, owner));
            }
            let elapsed = start.elapsed().as_secs_f64() * 1_000_000.0;
            if ordinal >= 3 {
                if legacy {
                    history_only.push(elapsed);
                } else {
                    with_archive.push(elapsed);
                }
            }
        }
    }
    for (mode, mut samples) in [("history", history_only), ("archive", with_archive)] {
        samples.sort_by(f64::total_cmp);
        assert_eq!(samples.len(), 40);
        let median = (samples[19] + samples[20]) / 2.0;
        println!(
            "fixture={phase} mode={mode} n=40 list_us_median={median:.1} list_us_p95={:.1}",
            samples[37]
        );
    }
}

fn rehearse_discovery(measure: bool) {
    let (root, catalog, manager, mut state, request) = setup(16 * 1024 * 1024);
    let creator = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    let original_id = request.job_id.clone();
    create_export_job(&state, &creator.id(), request).unwrap();
    assert_eq!(
        completed_for(&state, &creator.id()).status,
        proto::ExportJobStatus::Ready as i32
    );
    let record = export_record(&state);
    let original = std::fs::read(record.path.as_ref().unwrap()).unwrap();
    let (worker, transfer) = archive_completed_export(&root, &catalog, &state);
    state.storage_config.archive_runtime = Some(worker.handle());
    let (_, sender) = crate::runtime::Router::new().unwrap();
    let existing = ServerControlHandler::new(state.clone(), sender);
    let before = list(&existing, &creator);
    assert_eq!(before.len(), 1);
    assert_eq!(before[0], record.job);
    if measure {
        measure_lists(&existing, &creator, "retained");
    }

    state
        .export_jobs
        .lock()
        .unwrap()
        .get_mut(&original_id)
        .unwrap()
        .job
        .expires_at = Some(millis_timestamp(0));
    export_storage::history::expire(&state);
    let expired = export_record(&state).job;
    assert_eq!(expired.status, proto::ExportJobStatus::Expired as i32);
    assert_eq!(list(&existing, &creator), [expired]);
    drop(existing);
    state
        .export_jobs
        .lock()
        .unwrap()
        .get_mut(&original_id)
        .unwrap()
        .updated_at_ms = 0;
    export_storage::history::expire(&state);
    assert!(state.export_jobs.lock().unwrap().is_empty());
    assert!(
        manager
            .finish_export_retirement(&record.artifact_id)
            .unwrap()
    );
    assert!(!record.path.as_ref().unwrap().exists());

    let fresh = ApiPrincipal::local("127.0.0.1".parse().unwrap());
    let (_, sender) = crate::runtime::Router::new().unwrap();
    let handler = ServerControlHandler::new(state.clone(), sender);

    let proto::ok::Result::ArchiveResult(result) = super::super::storage_archive::dispatch(
        &state,
        &fresh,
        proto::ArchiveCommand {
            action: Some(proto::archive_command::Action::List(Default::default())),
        },
    )
    .unwrap() else {
        panic!("expected the administrator archive inventory")
    };
    let Some(proto::archive_result::Result::Jobs(page)) = result.result else {
        panic!("expected an archive page");
    };
    let job = page
        .jobs
        .iter()
        .find(|job| job.kind == proto::StorageObjectKind::Export as i32)
        .unwrap();
    assert_eq!(job.job_id, transfer);
    assert_eq!(job.object_id, record.artifact_id);
    for id in [&job.job_id, &job.object_id] {
        assert_eq!(
            handle(&handler, &fresh, download(id)).unwrap_err().code,
            proto::ErrorCode::NotFound
        );
    }
    let (_, messages) = handle(&handler, &fresh, download(&original_id)).unwrap();
    assert_eq!(downloaded_bytes(messages), original);
    let rows = list(&handler, &fresh);
    if measure {
        measure_lists(&handler, &fresh, "pruned");
    }
    let lookup = handle(&handler, &fresh, get(&original_id));
    println!(
        "pruned_history=0 original_download_bytes={} transfer_id_download=404 artifact_id_download=404 owner_list_rows={} owner_get={:?}",
        original.len(),
        rows.len(),
        lookup.as_ref().err().map(|error| error._http_status)
    );
    let other = ApiPrincipal {
        identity: ApiPrincipalIdentity::Credential {
            id: Uuid::new_v4(),
            revision: 1,
        },
        display_name: "Other operator".into(),
        role: AccessRole::User,
        credential_expires_at_ms: None,
    };
    assert!(list(&handler, &other).is_empty());
    assert_eq!(
        handle(&handler, &other, get(&original_id))
            .unwrap_err()
            .code,
        proto::ErrorCode::NotFound
    );
    state
        .privacy
        .replace_profile_overrides(std::collections::BTreeMap::from([(
            "127.0.0.1".into(),
            true,
        )]))
        .unwrap();
    assert!(list(&handler, &fresh).is_empty());
    assert!(handle(&handler, &fresh, get(&original_id)).is_err());
    assert_eq!(
        handle(&handler, &fresh, download(&original_id))
            .unwrap_err()
            .code,
        proto::ErrorCode::Rejected
    );

    drop(handler);
    drop(state);
    worker.shutdown().unwrap();
    drop(manager);
    catalog.shutdown();
    std::fs::remove_dir_all(root).unwrap();

    assert_eq!(
        rows.len(),
        1,
        "fresh owner must be able to discover its verified archive export"
    );
    assert_eq!(rows[0].job_id, original_id);
    assert_eq!(rows[0].sha256, record.job.sha256);
    assert!(rows[0].expires_at.is_none());
    let (control_ok::Result::ExportJob(found), _) = lookup.unwrap() else {
        panic!("expected the durable export description");
    };
    assert_eq!(found, rows[0]);
}
