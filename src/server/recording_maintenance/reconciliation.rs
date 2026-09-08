use super::{
    ApiPrincipal, ControlCommandError, ServerState, catalog_error, error, event_search_catalog,
    proto,
};
use crate::storage::{
    catalog::maintenance::reconciliation::{Kind, Remedy, Report},
    long_term::inspection::Archive,
};
use prost::Message as _;
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[derive(Default)]
pub(in crate::server) struct Registry {
    reports: Mutex<HashMap<String, Stored>>,
    scanning: AtomicBool,
}
struct Stored {
    report: Report,
    applied: HashMap<String, Remedy>,
}

struct Scan<'a>(&'a AtomicBool);
impl Drop for Scan<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) fn inspect(
    state: &ServerState,
    principal: &ApiPrincipal,
) -> Result<proto::ok::Result, ControlCommandError> {
    let registry = &state.maintenance_reconciliation;
    registry
        .scanning
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| {
            error(
                proto::ErrorCode::Rejected,
                429,
                "another catalog scan is running",
            )
        })?;
    let _scan = Scan(&registry.scanning);
    {
        let mut reports = registry.reports.lock().unwrap();
        reports.retain(|_, stored| stored.report.deadline > Instant::now());
        if reports.len() >= 16 {
            return Err(error(
                proto::ErrorCode::Rejected,
                429,
                "catalog report capacity is full",
            ));
        }
    }
    let archive = Archive::open(&state.storage_config.long_term_path)
        .map_err(|_| error(proto::ErrorCode::Unavailable, 503, "archive is unavailable"))?;
    let report = event_search_catalog(state)?
        .recording_reconciliation(&principal.id(), &archive)
        .map_err(catalog_error)?;
    let stored = Stored {
        report,
        applied: HashMap::new(),
    };
    let result = wire(&stored);
    if result.encoded_len() > 48 * 1024 {
        return Err(error(
            proto::ErrorCode::Rejected,
            413,
            "catalog report exceeds response limit",
        ));
    }
    registry
        .reports
        .lock()
        .unwrap()
        .insert(stored.report.id.clone(), stored);
    Ok(proto::ok::Result::RecordingReconciliationReport(result))
}

pub(super) fn apply(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::ApplyRecordingRemedy,
) -> Result<proto::ok::Result, ControlCommandError> {
    if request.report_id.len() != 32 || request.item_id.len() != 32 {
        return Err(error(
            proto::ErrorCode::InvalidRequest,
            400,
            "invalid catalog report identity",
        ));
    }
    let remedy = match proto::RecordingRemedy::try_from(request.remedy) {
        Ok(proto::RecordingRemedy::Ignore) => Remedy::Ignore,
        Ok(proto::RecordingRemedy::RetainTombstone) => Remedy::RetainTombstone,
        Ok(proto::RecordingRemedy::Reindex) => Remedy::Reindex,
        _ => {
            return Err(error(
                proto::ErrorCode::InvalidRequest,
                400,
                "catalog remedy is required",
            ));
        }
    };
    let _permit = (remedy != Remedy::Ignore)
        .then(|| super::worker::admit(state))
        .transpose()?;
    let _configuration = (remedy != Remedy::Ignore)
        .then(|| {
            state.config_update.try_lock().map_err(|_| {
                error(
                    proto::ErrorCode::Rejected,
                    409,
                    "storage coordination is busy",
                )
            })
        })
        .transpose()?;
    if remedy != Remedy::Ignore {
        super::worker::check_restore(state).map_err(|_| {
            error(
                proto::ErrorCode::Rejected,
                409,
                "restore is active or unavailable",
            )
        })?;
    }
    apply_report(state, principal, request, remedy)
}

fn apply_report(
    state: &ServerState,
    principal: &ApiPrincipal,
    request: proto::ApplyRecordingRemedy,
    remedy: Remedy,
) -> Result<proto::ok::Result, ControlCommandError> {
    let mut reports = state.maintenance_reconciliation.reports.lock().unwrap();
    let stored = reports
        .get_mut(&request.report_id)
        .filter(|stored| stored.report.actor == principal.id())
        .ok_or_else(|| {
            error(
                proto::ErrorCode::NotFound,
                404,
                "catalog report is unavailable",
            )
        })?;
    if stored.report.deadline <= Instant::now() {
        return Err(error(
            proto::ErrorCode::Rejected,
            409,
            "catalog report expired; inspect again",
        ));
    }
    if let Some(applied) = stored.applied.get(&request.item_id) {
        if *applied != remedy {
            return Err(error(
                proto::ErrorCode::Rejected,
                409,
                "a different remedy was already applied",
            ));
        }
        return Ok(proto::ok::Result::RecordingReconciliationReport(wire(
            stored,
        )));
    }
    let archive = Archive::open(&state.storage_config.long_term_path)
        .map_err(|_| error(proto::ErrorCode::Unavailable, 503, "archive is unavailable"))?;
    event_search_catalog(state)?
        .apply_recording_reconciliation(
            &principal.id(),
            &stored.report,
            &request.item_id,
            remedy,
            &archive,
        )
        .map_err(catalog_error)?;
    stored.applied.insert(request.item_id, remedy);
    Ok(proto::ok::Result::RecordingReconciliationReport(wire(
        stored,
    )))
}

fn wire(stored: &Stored) -> proto::RecordingReconciliationReport {
    let report = &stored.report;
    proto::RecordingReconciliationReport {
        report_id: report.id.clone(),
        revision: report.revision,
        complete: report.complete,
        inspected: report.inspected,
        items: report
            .items
            .iter()
            .map(|item| proto::RecordingDriftItem {
                item_id: item.id.clone(),
                recording_id: item.recording_id.clone(),
                kind: wire_kind(item.kind) as i32,
                label: item.label.clone(),
                bytes: item.bytes,
                remedies: if !report.complete {
                    Vec::new()
                } else {
                    item.remedies
                        .iter()
                        .map(|remedy| wire_remedy(*remedy) as i32)
                        .collect()
                },
                applied_remedy: stored
                    .applied
                    .get(&item.id)
                    .map(|remedy| wire_remedy(*remedy) as i32),
            })
            .collect(),
    }
}

const fn wire_kind(kind: Kind) -> proto::RecordingDriftKind {
    match kind {
        Kind::MissingFile => proto::RecordingDriftKind::MissingFile,
        Kind::UnknownFile => proto::RecordingDriftKind::UnknownFile,
        Kind::DuplicatePath => proto::RecordingDriftKind::DuplicatePath,
        Kind::SizeMismatch => proto::RecordingDriftKind::SizeMismatch,
        Kind::IdentityMismatch => proto::RecordingDriftKind::IdentityMismatch,
        Kind::PathRejected => proto::RecordingDriftKind::PathRejected,
        Kind::TemporaryFile => proto::RecordingDriftKind::TemporaryFile,
        Kind::InterruptedWork => proto::RecordingDriftKind::InterruptedWork,
        Kind::InspectionFailed => proto::RecordingDriftKind::InspectionFailed,
        Kind::CorruptFile => proto::RecordingDriftKind::CorruptFile,
        Kind::DuplicateIdentity => proto::RecordingDriftKind::DuplicateIdentity,
        Kind::IndexMismatch => proto::RecordingDriftKind::IndexMismatch,
    }
}

const fn wire_remedy(remedy: Remedy) -> proto::RecordingRemedy {
    match remedy {
        Remedy::Ignore => proto::RecordingRemedy::Ignore,
        Remedy::RetainTombstone => proto::RecordingRemedy::RetainTombstone,
        Remedy::Reindex => proto::RecordingRemedy::Reindex,
    }
}
