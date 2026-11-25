//! Administrator-only WebRTC control commands and lazy background scheduler for usage reporting.

use super::{
    ApiPrincipal, ControlCommandError, ServerState, camera_configuration_revision,
    configuration_revision, recording_capacity_estimate, server_health,
};
use crate::{
    access::AccessRole,
    api::proto::{self, ok as control_ok, usage_reporting_command, usage_reporting_result},
    config::{self, usage_reporting::UsageReportingConfig},
    runtime::{FacadeSender, RouterMessage},
    usage_reporting::{
        model::{
            CURRENT_DISCLOSURE_REVISION, MAX_PAYLOAD_BYTES, MAX_RETENTION_DAYS,
            MIN_AGGREGATE_INSTALLATIONS, PUBLISHED_AGGREGATE_DATA_URL, PUBLISHED_DOCUMENTATION_URL,
            PUBLISHED_RECEIVER_SOURCE_URL, PUBLISHED_SCHEMA_URL, SCHEMA_VERSION_V1,
            SchemaValidationError,
        },
        projection::{
            ConsentSelection, ProjectionHealthInput, ProjectionSnapshot, detect_container_runtime,
            project_report,
        },
        sender::{
            Clock, HttpsTelemetryTransport, JitterSource, RandomJitter, SystemClock,
            TelemetryTransport, UsageReportingController,
        },
        state::{
            FailureKind, LifecycleState, PLACEHOLDER_PREVIEW_UUID, StateError,
            TelemetryRuntimeState, default_state_path,
        },
    },
};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

pub(super) const CAPABILITY_ID: &str = "keeppeek.usage-reporting.v1";
const SCHEDULER_POLL_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub(super) struct Runtime {
    config: Arc<RwLock<UsageReportingConfig>>,
    controller: Arc<Mutex<Option<Arc<UsageReportingController>>>>,
    clock: Arc<dyn Clock>,
    jitter: Arc<dyn JitterSource>,
    transport: Arc<dyn TelemetryTransport>,
    worker_started: Arc<AtomicBool>,
    current_disclosure_revision: u32,
}

impl Runtime {
    pub(super) fn new(config: UsageReportingConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            controller: Arc::new(Mutex::new(None)),
            clock: Arc::new(SystemClock),
            jitter: Arc::new(RandomJitter),
            transport: Arc::new(HttpsTelemetryTransport::new()),
            worker_started: Arc::new(AtomicBool::new(false)),
            current_disclosure_revision: CURRENT_DISCLOSURE_REVISION,
        }
    }

    #[cfg(test)]
    pub(super) fn with_test_harness(
        config: UsageReportingConfig,
        clock: Arc<dyn Clock>,
        jitter: Arc<dyn JitterSource>,
        transport: Arc<dyn TelemetryTransport>,
        current_disclosure_revision: u32,
    ) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            controller: Arc::new(Mutex::new(None)),
            clock,
            jitter,
            transport,
            worker_started: Arc::new(AtomicBool::new(false)),
            current_disclosure_revision,
        }
    }

    pub(super) fn current_config(&self) -> UsageReportingConfig {
        self.config
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(super) fn any_enabled(&self) -> bool {
        self.current_config().any_enabled()
    }

    fn controller(&self, config_path: &Path) -> Result<Arc<UsageReportingController>, StateError> {
        let mut guard = self
            .controller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = guard.as_ref() {
            return Ok(Arc::clone(existing));
        }
        let state_path = default_state_path(config_path);
        let opened = Arc::new(UsageReportingController::open(
            state_path,
            Arc::clone(&self.clock),
            Arc::clone(&self.jitter),
            Arc::clone(&self.transport),
        )?);
        *guard = Some(Arc::clone(&opened));
        Ok(opened)
    }

    /// Spawns at most one background scheduler thread strictly when telemetry is enabled.
    pub(super) fn ensure_scheduler_if_enabled(
        &self,
        state: &ServerState,
        router_tx: &FacadeSender<RouterMessage>,
    ) {
        if !self.any_enabled() {
            return;
        }
        let Some(shutdown) = state
            .restart_control
            .as_ref()
            .map(|control| control.shutdown.clone())
        else {
            return;
        };
        if state.camera_config_path.is_none() {
            return;
        }
        if self
            .worker_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        let runtime = self.clone();
        let state_clone = state.clone();
        let router_clone = router_tx.clone();
        let _ = std::thread::Builder::new()
            .name("usage-reporting-scheduler".to_owned())
            .spawn(move || {
                scheduler_loop(&runtime, &state_clone, &router_clone, &shutdown);
                runtime.worker_started.store(false, Ordering::Release);
            });
    }
}

fn scheduler_loop(
    runtime: &Runtime,
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    shutdown: &crate::shutdown::Shutdown,
) {
    while !shutdown.is_cancelled() && runtime.any_enabled() {
        if let Some(config_path) = state.camera_config_path.as_deref()
            && let Ok(controller) = runtime.controller(config_path)
        {
            let cfg = runtime.current_config();
            let runtime_ref = runtime.clone();
            let _ = controller.maybe_send_due(
                &cfg,
                |installation_id, seq| {
                    build_report_from_state(
                        state,
                        router_tx,
                        ConsentSelection::from_config(&cfg),
                        installation_id,
                        seq,
                    )
                },
                move || runtime_ref.any_enabled(),
            );
        }
        let mut waited = Duration::ZERO;
        while waited < SCHEDULER_POLL_INTERVAL && !shutdown.is_cancelled() && runtime.any_enabled()
        {
            std::thread::sleep(Duration::from_millis(250));
            waited += Duration::from_millis(250);
        }
    }
}

pub(super) const fn audit_operation(
    command: &proto::UsageReportingCommand,
) -> Option<&'static str> {
    match command.action {
        Some(usage_reporting_command::Action::Update(_)) => Some("usage_reporting_update"),
        Some(usage_reporting_command::Action::ResetIdentity(_)) => {
            Some("usage_reporting_reset_identity")
        }
        Some(usage_reporting_command::Action::DeleteData(_)) => Some("usage_reporting_delete_data"),
        Some(
            usage_reporting_command::Action::Get(_) | usage_reporting_command::Action::Preview(_),
        )
        | None => None,
    }
}

pub(super) fn dispatch(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    principal: &ApiPrincipal,
    command: proto::UsageReportingCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    if principal.role != AccessRole::Administrator {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "administrator access is required for usage reporting controls",
        ));
    }
    let result = match command.action {
        Some(usage_reporting_command::Action::Get(_)) => {
            usage_reporting_result::Result::State(get_state(state)?)
        }
        Some(usage_reporting_command::Action::Update(update)) => {
            usage_reporting_result::Result::State(update_consent(state, router_tx, update)?)
        }
        Some(usage_reporting_command::Action::Preview(preview)) => {
            usage_reporting_result::Result::Preview(preview_payload(state, router_tx, preview)?)
        }
        Some(usage_reporting_command::Action::ResetIdentity(reset)) => {
            usage_reporting_result::Result::State(reset_identity(state, reset)?)
        }
        Some(usage_reporting_command::Action::DeleteData(delete)) => {
            usage_reporting_result::Result::State(delete_data(state, delete)?)
        }
        None => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "usage reporting command has no action",
            ));
        }
    };
    Ok(control_ok::Result::UsageReportingResult(
        proto::UsageReportingResult {
            result: Some(result),
        },
    ))
}

fn require_config_path(state: &ServerState) -> Result<&Path, ControlCommandError> {
    state.camera_config_path.as_deref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            503,
            "usage reporting requires an application configuration file",
        )
    })
}

fn get_state(state: &ServerState) -> Result<proto::UsageReportingState, ControlCommandError> {
    let config_path = require_config_path(state)?;
    let revision = camera_configuration_revision(state)?;
    let loaded = config::load_config(config_path).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "unable to load usage reporting settings",
        )
    })?;
    let controller = state
        .usage_reporting
        .controller(config_path)
        .map_err(map_state_error)?;
    let runtime_state = controller.snapshot_state();
    Ok(proto_state(
        revision,
        &loaded.usage_reporting,
        &runtime_state,
        state.usage_reporting.current_disclosure_revision,
        controller.is_sending(),
    ))
}

fn update_consent(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    request: proto::UpdateUsageReporting,
) -> Result<proto::UsageReportingState, ControlCommandError> {
    let config_path = require_config_path(state)?;
    let _lock = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let current = config::load_config(config_path).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "unable to load usage reporting settings",
        )
    })?;
    let current_revision = configuration_revision(&current);
    if request.expected_configuration_revision.is_empty()
        || current_revision != request.expected_configuration_revision
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "configuration revision conflicts; reload settings and retry",
        ));
    }

    let target_disclosure = state.usage_reporting.current_disclosure_revision;
    let next_config = build_next_config(&current.usage_reporting, &request, target_disclosure)?;
    let (updated_config, next_revision) = config::usage_reporting::update(
        config_path,
        &request.expected_configuration_revision,
        next_config,
    )
    .map_err(map_config_update_error)?;

    {
        *state
            .usage_reporting
            .config
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            updated_config.usage_reporting.clone();
    }

    let controller = state
        .usage_reporting
        .controller(config_path)
        .map_err(map_state_error)?;
    let runtime_state = controller
        .apply_consent_transition(&updated_config.usage_reporting)
        .map_err(map_state_error)?;
    state
        .usage_reporting
        .ensure_scheduler_if_enabled(state, router_tx);

    Ok(proto_state(
        next_revision,
        &updated_config.usage_reporting,
        &runtime_state,
        target_disclosure,
        controller.is_sending(),
    ))
}

fn build_next_config(
    existing: &UsageReportingConfig,
    request: &proto::UpdateUsageReporting,
    target_disclosure: u32,
) -> Result<UsageReportingConfig, ControlCommandError> {
    if !request.basic && (request.camera_compatibility || request.feature_adoption) {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "basic system data must be enabled before camera compatibility or feature adoption",
        ));
    }
    let consented_disclosure_revision = if !request.basic {
        existing.consented_disclosure_revision
    } else if request.accepted_disclosure_revision == target_disclosure {
        target_disclosure
    } else if existing.basic
        && existing.consented_disclosure_revision >= 1
        && (request.accepted_disclosure_revision == 0
            || request.accepted_disclosure_revision == existing.consented_disclosure_revision)
    {
        // Preserving prior revision lets an Administrator decline a schema renewal without losing unchanged consent.
        existing.consented_disclosure_revision
    } else {
        return Err(ControlCommandError::new(
            proto::ErrorCode::InvalidRequest,
            400,
            "enabling usage reporting requires accepting the current disclosure revision",
        ));
    };

    let next = UsageReportingConfig {
        basic: request.basic,
        camera_compatibility: request.camera_compatibility,
        feature_adoption: request.feature_adoption,
        consented_disclosure_revision,
        receiver_origin: existing.receiver_origin.clone(),
    };
    next.validate_against_disclosure(target_disclosure)
        .map_err(|_| {
            ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "invalid usage reporting consent selection",
            )
        })?;
    Ok(next)
}

fn preview_payload(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    request: proto::PreviewUsageReporting,
) -> Result<proto::UsageReportingPreview, ControlCommandError> {
    if !request.basic {
        return Ok(proto::UsageReportingPreview {
            payload_json: String::new(),
            payload_bytes: 0,
            max_payload_bytes: u32::try_from(MAX_PAYLOAD_BYTES).unwrap_or(65_536),
            uses_placeholder_identity: true,
            valid: true,
            validation_error: proto::UsageReportingFailureKind::None as i32,
        });
    }

    let config_path = require_config_path(state)?;
    let controller = state
        .usage_reporting
        .controller(config_path)
        .map_err(map_state_error)?;
    let runtime_state = controller.snapshot_state();
    let (installation_id, uses_placeholder_identity, next_seq) =
        runtime_state.identity.as_ref().map_or_else(
            || (PLACEHOLDER_PREVIEW_UUID.to_owned(), true, 1),
            |identity| {
                (
                    identity.installation_id.clone(),
                    false,
                    identity.report_sequence.saturating_add(1),
                )
            },
        );
    let disclosure_revision = if request.disclosure_revision == 0 {
        state.usage_reporting.current_disclosure_revision
    } else {
        request
            .disclosure_revision
            .min(state.usage_reporting.current_disclosure_revision)
    };
    let selection = ConsentSelection {
        basic: request.basic,
        camera_compatibility: request.camera_compatibility,
        feature_adoption: request.feature_adoption,
        consented_disclosure_revision: disclosure_revision,
    };

    let built = build_report_from_state(state, router_tx, selection, &installation_id, next_seq)
        .and_then(|report| report.to_bounded_json());
    match built {
        Ok(payload_json) => Ok(proto::UsageReportingPreview {
            payload_bytes: u32::try_from(payload_json.len()).unwrap_or(u32::MAX),
            payload_json,
            max_payload_bytes: u32::try_from(MAX_PAYLOAD_BYTES).unwrap_or(65_536),
            uses_placeholder_identity,
            valid: true,
            validation_error: proto::UsageReportingFailureKind::None as i32,
        }),
        Err(error) => {
            let validation_error = match error {
                SchemaValidationError::PayloadTooLarge { .. } => {
                    proto::UsageReportingFailureKind::PayloadTooLarge as i32
                }
                _ => proto::UsageReportingFailureKind::ValidationFailed as i32,
            };
            Ok(proto::UsageReportingPreview {
                payload_json: String::new(),
                payload_bytes: 0,
                max_payload_bytes: u32::try_from(MAX_PAYLOAD_BYTES).unwrap_or(65_536),
                uses_placeholder_identity,
                valid: false,
                validation_error,
            })
        }
    }
}

fn reset_identity(
    state: &ServerState,
    request: proto::ResetUsageReportingIdentity,
) -> Result<proto::UsageReportingState, ControlCommandError> {
    let config_path = require_config_path(state)?;
    let _lock = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let current = config::load_config(config_path).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "unable to load usage reporting settings",
        )
    })?;
    let current_revision = configuration_revision(&current);
    if request.expected_configuration_revision.is_empty()
        || current_revision != request.expected_configuration_revision
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "configuration revision conflicts; reload settings and retry",
        ));
    }
    let controller = state
        .usage_reporting
        .controller(config_path)
        .map_err(map_state_error)?;
    let runtime_state = controller
        .reset_identity(&current.usage_reporting)
        .map_err(map_state_error)?;
    Ok(proto_state(
        current_revision,
        &current.usage_reporting,
        &runtime_state,
        state.usage_reporting.current_disclosure_revision,
        controller.is_sending(),
    ))
}

fn delete_data(
    state: &ServerState,
    request: proto::DeleteUsageReportingData,
) -> Result<proto::UsageReportingState, ControlCommandError> {
    let config_path = require_config_path(state)?;
    let _lock = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let current = config::load_config(config_path).map_err(|_| {
        ControlCommandError::new(
            proto::ErrorCode::Internal,
            500,
            "unable to load usage reporting settings",
        )
    })?;
    let current_revision = configuration_revision(&current);
    if request.expected_configuration_revision.is_empty()
        || current_revision != request.expected_configuration_revision
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            409,
            "configuration revision conflicts; reload settings and retry",
        ));
    }
    let controller = state
        .usage_reporting
        .controller(config_path)
        .map_err(map_state_error)?;
    let runtime_state = controller
        .process_pending_deletions(current.usage_reporting.receiver_origin.as_deref())
        .map_err(map_state_error)?;
    Ok(proto_state(
        current_revision,
        &current.usage_reporting,
        &runtime_state,
        state.usage_reporting.current_disclosure_revision,
        controller.is_sending(),
    ))
}

fn build_report_from_state(
    state: &ServerState,
    router_tx: &FacadeSender<RouterMessage>,
    selection: ConsentSelection,
    installation_id: &str,
    report_sequence: u64,
) -> Result<crate::usage_reporting::model::UsageReportV1, SchemaValidationError> {
    let config = state
        .camera_config_path
        .as_deref()
        .and_then(|path| config::load_config(path).ok())
        .unwrap_or_default();
    let health = server_health(router_tx, state);
    let health_input = ProjectionHealthInput::from_server_health(&health);
    let cameras: Vec<crate::api::CameraInfo> = state
        .camera_entries()
        .into_iter()
        .map(|entry| entry.info)
        .collect();
    let estimate = recording_capacity_estimate(
        cameras.iter().flat_map(|camera| camera.profiles.iter()),
        state.storage_config.long_term_max_bytes,
    );
    let snapshot = ProjectionSnapshot {
        config: &config,
        health: &health_input,
        cameras: &cameras,
        camera_database: state.camera_database.as_deref(),
        estimated_retention_days: estimate.estimated_retention_days,
        is_container: detect_container_runtime(),
        video_accelerations: &[],
        has_notifications_pushover: state.notifications.is_some(),
        has_notifications_webhook: false,
        has_external_api_events: state.events.is_some(),
    };
    project_report(&snapshot, selection, installation_id, report_sequence)
}

fn proto_state(
    configuration_revision: String,
    consent: &UsageReportingConfig,
    runtime: &TelemetryRuntimeState,
    current_disclosure_revision: u32,
    sending: bool,
) -> proto::UsageReportingState {
    let renewal_required = consent.renewal_required(current_disclosure_revision);
    let pending_renewal_fields = if renewal_required {
        vec![format!("disclosure_revision_{current_disclosure_revision}")]
    } else {
        Vec::new()
    };
    proto::UsageReportingState {
        configuration_revision,
        consent: Some(proto::UsageReportingConsent {
            basic: consent.basic,
            camera_compatibility: consent.camera_compatibility,
            feature_adoption: consent.feature_adoption,
            consented_disclosure_revision: consent.consented_disclosure_revision,
        }),
        disclosure: Some(proto::UsageReportingDisclosure {
            current_disclosure_revision,
            schema_version: SCHEMA_VERSION_V1.to_string(),
            receiver_origin: consent.receiver_origin.clone().unwrap_or_default(),
            schema_url: PUBLISHED_SCHEMA_URL.to_owned(),
            documentation_url: PUBLISHED_DOCUMENTATION_URL.to_owned(),
            aggregate_data_url: PUBLISHED_AGGREGATE_DATA_URL.to_owned(),
            receiver_source_url: PUBLISHED_RECEIVER_SOURCE_URL.to_owned(),
            max_retention_days: MAX_RETENTION_DAYS,
            aggregate_min_installations: MIN_AGGREGATE_INSTALLATIONS,
            max_payload_bytes: u32::try_from(MAX_PAYLOAD_BYTES).unwrap_or(65_536),
            renewal_required,
            pending_renewal_fields,
        }),
        status: Some(proto::UsageReportingDeliveryStatus {
            state: proto_lifecycle(runtime.lifecycle_state(consent.any_enabled(), sending)) as i32,
            last_attempt_at_ms: runtime.last_attempt_at_ms,
            last_success_at_ms: runtime.last_success_at_ms,
            next_scheduled_at_ms: runtime.next_scheduled_at_ms,
            last_failure: proto_failure(runtime.last_failure) as i32,
            pending_deletion_count: u32::try_from(runtime.pending_deletions.len())
                .unwrap_or(u32::MAX),
            identity_configured: runtime.identity.is_some(),
            last_deletion_attempt_at_ms: runtime.last_deletion_attempt_at_ms,
            last_deletion_success_at_ms: runtime.last_deletion_success_at_ms,
            last_deletion_failure: proto_failure(runtime.last_deletion_failure) as i32,
        }),
    }
}

const fn proto_lifecycle(state: LifecycleState) -> proto::UsageReportingLifecycleState {
    match state {
        LifecycleState::Disabled => proto::UsageReportingLifecycleState::Disabled,
        LifecycleState::Scheduled => proto::UsageReportingLifecycleState::Scheduled,
        LifecycleState::Sending => proto::UsageReportingLifecycleState::Sending,
        LifecycleState::Succeeded => proto::UsageReportingLifecycleState::Succeeded,
        LifecycleState::Failed => proto::UsageReportingLifecycleState::Failed,
        LifecycleState::DeletionPending => proto::UsageReportingLifecycleState::DeletionPending,
    }
}

const fn proto_failure(failure: FailureKind) -> proto::UsageReportingFailureKind {
    match failure {
        FailureKind::None => proto::UsageReportingFailureKind::None,
        FailureKind::ReceiverUnconfigured => proto::UsageReportingFailureKind::ReceiverUnconfigured,
        FailureKind::PayloadTooLarge => proto::UsageReportingFailureKind::PayloadTooLarge,
        FailureKind::ValidationFailed => proto::UsageReportingFailureKind::ValidationFailed,
        FailureKind::NetworkUnavailable => proto::UsageReportingFailureKind::NetworkUnavailable,
        FailureKind::DnsFailed => proto::UsageReportingFailureKind::DnsFailed,
        FailureKind::TlsFailed => proto::UsageReportingFailureKind::TlsFailed,
        FailureKind::Timeout => proto::UsageReportingFailureKind::Timeout,
        FailureKind::RateLimited => proto::UsageReportingFailureKind::RateLimited,
        FailureKind::ReceiverRejected => proto::UsageReportingFailureKind::ReceiverRejected,
        FailureKind::ReceiverUnavailable => proto::UsageReportingFailureKind::ReceiverUnavailable,
        FailureKind::InvalidResponse => proto::UsageReportingFailureKind::InvalidResponse,
        FailureKind::DeletionFailed => proto::UsageReportingFailureKind::DeletionFailed,
        FailureKind::StorageError => proto::UsageReportingFailureKind::StorageError,
    }
}

fn map_state_error(error: StateError) -> ControlCommandError {
    match error {
        StateError::PendingDeletionQueueFull => {
            ControlCommandError::new(proto::ErrorCode::Rejected, 409, error.to_string())
        }
        StateError::PersistenceFailed | StateError::CorruptState => {
            ControlCommandError::new(proto::ErrorCode::Internal, 500, error.to_string())
        }
    }
}

fn map_config_update_error(error: config::usage_reporting::UpdateError) -> ControlCommandError {
    match error {
        config::usage_reporting::UpdateError::RevisionConflict => {
            ControlCommandError::new(proto::ErrorCode::Rejected, 409, error.to_string())
        }
        config::usage_reporting::UpdateError::Validation => {
            ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, error.to_string())
        }
        config::usage_reporting::UpdateError::Persistence => {
            ControlCommandError::new(proto::ErrorCode::Internal, 500, error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::AuthenticatedCredential;
    use crate::storage::{RecordingDemand, StorageConfig};
    use crate::usage_reporting::sender::TransportResponse;
    use crate::webrtc::WebRtc;
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicI64, AtomicUsize};

    struct TestClock(AtomicI64);
    impl Clock for TestClock {
        fn now_ms(&self) -> i64 {
            self.0.load(Ordering::Acquire)
        }
    }
    struct ZeroJitter;
    impl JitterSource for ZeroJitter {
        fn non_negative_jitter_ms(&self, _max_ms: i64) -> i64 {
            0
        }
    }
    #[derive(Default)]
    struct CountingTransport {
        deletes: AtomicUsize,
        fail_delete: AtomicBool,
    }
    impl TelemetryTransport for CountingTransport {
        fn put_report(
            &self,
            _origin: &str,
            _installation_id: &str,
            _installation_token: &str,
            _body: &[u8],
        ) -> Result<TransportResponse, FailureKind> {
            Ok(TransportResponse { status: 200 })
        }
        fn delete_installation(
            &self,
            _origin: &str,
            _installation_id: &str,
            _installation_token: &str,
        ) -> Result<TransportResponse, FailureKind> {
            self.deletes.fetch_add(1, Ordering::AcqRel);
            if self.fail_delete.load(Ordering::Acquire) {
                Err(FailureKind::NetworkUnavailable)
            } else {
                Ok(TransportResponse { status: 204 })
            }
        }
    }

    #[test]
    fn authorization_and_lifecycle_enforce_admin_revision_renewal_and_deletion() {
        let dir =
            std::env::temp_dir().join(format!("keeppeek-server-usage-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("config.toml");
        std::fs::write(
            &config_path,
            r#"
host = "127.0.0.1"
port = 8081

[usage_reporting]
basic = false
camera_compatibility = false
feature_adoption = false
consented_disclosure_revision = 0
receiver_origin = "https://usage.example"
"#,
        )
        .unwrap();
        let loaded = config::load_config(&config_path).unwrap();
        let transport = Arc::new(CountingTransport::default());
        let mut state = ServerState::new(
            &loaded,
            &HashMap::new(),
            &HashMap::new(),
            &StorageConfig::default(),
            RecordingDemand::new(Duration::from_secs(5)),
            WebRtc::new(),
        )
        .with_camera_config_path(config_path);
        state.usage_reporting = Runtime::with_test_harness(
            loaded.usage_reporting,
            Arc::new(TestClock(AtomicI64::new(1_790_000_000_000))),
            Arc::new(ZeroJitter),
            transport.clone(),
            CURRENT_DISCLOSURE_REVISION,
        );
        let (_, router_tx) = crate::runtime::Router::new().unwrap();

        let user = ApiPrincipal::credential(AuthenticatedCredential {
            id: uuid::Uuid::new_v4(),
            name: "Viewer".to_owned(),
            role: AccessRole::User,
            revision: 1,
            expires_at_ms: None,
        });
        let admin = ApiPrincipal::local(IpAddr::V4(Ipv4Addr::LOCALHOST));

        // User is strictly denied on every usage reporting command.
        let denied = dispatch(
            &state,
            &router_tx,
            &user,
            proto::UsageReportingCommand {
                action: Some(usage_reporting_command::Action::Get(
                    proto::GetUsageReporting {},
                )),
            },
        )
        .unwrap_err();
        assert_eq!(denied.code, proto::ErrorCode::Rejected);

        // Admin gets disabled default state with no UUID.
        let initial = match dispatch(
            &state,
            &router_tx,
            &admin,
            proto::UsageReportingCommand {
                action: Some(usage_reporting_command::Action::Get(
                    proto::GetUsageReporting {},
                )),
            },
        )
        .unwrap()
        {
            control_ok::Result::UsageReportingResult(proto::UsageReportingResult {
                result: Some(usage_reporting_result::Result::State(value)),
            }) => value,
            other => panic!("unexpected result: {other:?}"),
        };
        assert!(!initial.consent.as_ref().unwrap().basic);
        assert!(!initial.status.as_ref().unwrap().identity_configured);

        // Preview before opt-in uses placeholder UUID and creates no persisted UUID.
        let preview = match dispatch(
            &state,
            &router_tx,
            &admin,
            proto::UsageReportingCommand {
                action: Some(usage_reporting_command::Action::Preview(
                    proto::PreviewUsageReporting {
                        basic: true,
                        camera_compatibility: false,
                        feature_adoption: false,
                        disclosure_revision: 0,
                    },
                )),
            },
        )
        .unwrap()
        {
            control_ok::Result::UsageReportingResult(proto::UsageReportingResult {
                result: Some(usage_reporting_result::Result::Preview(value)),
            }) => value,
            other => panic!("unexpected preview: {other:?}"),
        };
        assert!(preview.valid);
        assert!(preview.uses_placeholder_identity);
        assert!(preview.payload_json.contains(PLACEHOLDER_PREVIEW_UUID));
        assert!(!preview.payload_json.contains("camera_compatibility"));

        // Stale revision fails with FailedPrecondition (409).
        let stale = dispatch(
            &state,
            &router_tx,
            &admin,
            proto::UsageReportingCommand {
                action: Some(usage_reporting_command::Action::Update(
                    proto::UpdateUsageReporting {
                        expected_configuration_revision: "wrong-revision".to_owned(),
                        basic: true,
                        camera_compatibility: false,
                        feature_adoption: false,
                        accepted_disclosure_revision: CURRENT_DISCLOSURE_REVISION,
                    },
                )),
            },
        )
        .unwrap_err();
        assert_eq!(stale.code, proto::ErrorCode::Rejected);

        // Valid opt-in persists consent and creates a dedicated telemetry UUID.
        let enabled = match dispatch(
            &state,
            &router_tx,
            &admin,
            proto::UsageReportingCommand {
                action: Some(usage_reporting_command::Action::Update(
                    proto::UpdateUsageReporting {
                        expected_configuration_revision: initial.configuration_revision,
                        basic: true,
                        camera_compatibility: true,
                        feature_adoption: false,
                        accepted_disclosure_revision: CURRENT_DISCLOSURE_REVISION,
                    },
                )),
            },
        )
        .unwrap()
        {
            control_ok::Result::UsageReportingResult(proto::UsageReportingResult {
                result: Some(usage_reporting_result::Result::State(value)),
            }) => value,
            other => panic!("unexpected state: {other:?}"),
        };
        assert!(enabled.consent.as_ref().unwrap().basic);
        assert!(enabled.consent.as_ref().unwrap().camera_compatibility);
        assert!(enabled.status.as_ref().unwrap().identity_configured);

        // Offline reset queues deletion truthfully; explicit delete retry clears it once online.
        transport.fail_delete.store(true, Ordering::Release);
        let after_reset = match dispatch(
            &state,
            &router_tx,
            &admin,
            proto::UsageReportingCommand {
                action: Some(usage_reporting_command::Action::ResetIdentity(
                    proto::ResetUsageReportingIdentity {
                        expected_configuration_revision: enabled.configuration_revision,
                    },
                )),
            },
        )
        .unwrap()
        {
            control_ok::Result::UsageReportingResult(proto::UsageReportingResult {
                result: Some(usage_reporting_result::Result::State(value)),
            }) => value,
            other => panic!("unexpected reset state: {other:?}"),
        };
        assert_eq!(
            after_reset.status.as_ref().unwrap().pending_deletion_count,
            1
        );

        transport.fail_delete.store(false, Ordering::Release);
        let after_retry = match dispatch(
            &state,
            &router_tx,
            &admin,
            proto::UsageReportingCommand {
                action: Some(usage_reporting_command::Action::DeleteData(
                    proto::DeleteUsageReportingData {
                        expected_configuration_revision: after_reset.configuration_revision,
                    },
                )),
            },
        )
        .unwrap()
        {
            control_ok::Result::UsageReportingResult(proto::UsageReportingResult {
                result: Some(usage_reporting_result::Result::State(value)),
            }) => value,
            other => panic!("unexpected delete retry state: {other:?}"),
        };
        assert_eq!(
            after_retry.status.as_ref().unwrap().pending_deletion_count,
            0
        );
        assert!(transport.deletes.load(Ordering::Acquire) >= 2);

        let _ = std::fs::remove_dir_all(dir);
    }
}
