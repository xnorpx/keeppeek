use super::*;

const MAXIMUM_TEMPLATES: usize = 64;
const MAXIMUM_TEMPLATE_NAME_BYTES: usize = 128;
const MAXIMUM_TEMPLATE_DESCRIPTION_BYTES: usize = 1_024;
const MAXIMUM_TEMPLATE_DOCUMENT_BYTES: usize = 16 * 1_024;
const MAXIMUM_PLAN_TARGETS: usize = 64;
const MAXIMUM_CONFIGURATION_RESPONSE_BYTES: usize = 60 * 1_024;
const DEFAULT_SNAPSHOT_PAGE_SIZE: usize = 32;
const MAXIMUM_SNAPSHOT_PAGE_SIZE: usize = 64;
const MAXIMUM_RETAINED_PLANS: usize = 128;
const PLAN_TTL: Duration = Duration::from_secs(10 * 60);
const TEMPLATE_DOCUMENT_VERSION: u32 = 1;
const TEMPLATE_CONFIG_SECTION: &str = "configuration_templates";
const LEGACY_TEMPLATE_STORE_FILE: &str = "configuration-templates.json";

#[derive(Clone, Default)]
pub(super) struct Registry {
    plans: Arc<Mutex<HashMap<String, StoredPlan>>>,
    imports: Arc<Mutex<HashMap<String, StoredImportPreview>>>,
}

#[derive(Clone)]
struct StoredPlan {
    plan: proto::ConfigurationPlan,
    candidate: toml::Table,
    target_ids: Vec<String>,
}

#[derive(Clone)]
struct StoredImportPreview {
    preview: proto::ConfigurationTemplateImportPreview,
    document: StoredTemplateDocument,
}

impl Registry {
    fn insert(&self, stored: StoredPlan) {
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        let mut plans = self
            .plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        plans.retain(|_, plan| plan.plan.expires_at_ms > now_ms);
        if plans.len() >= MAXIMUM_RETAINED_PLANS
            && let Some(oldest) = plans
                .iter()
                .min_by_key(|(_, plan)| plan.plan.expires_at_ms)
                .map(|(id, _)| id.clone())
        {
            plans.remove(&oldest);
        }
        plans.insert(stored.plan.plan_id.clone(), stored);
    }

    fn get(&self, plan_id: &str) -> Option<StoredPlan> {
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        let mut plans = self
            .plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let requested = plans.get(plan_id).cloned();
        plans.retain(|id, plan| id == plan_id || plan.plan.expires_at_ms > now_ms);
        requested
    }

    fn remove(&self, plan_id: &str) {
        self.plans
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(plan_id);
    }

    fn insert_import(&self, stored: StoredImportPreview) {
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        let mut imports = self
            .imports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        imports.retain(|_, preview| preview.preview.expires_at_ms > now_ms);
        if imports.len() >= MAXIMUM_RETAINED_PLANS
            && let Some(oldest) = imports
                .iter()
                .min_by_key(|(_, preview)| preview.preview.expires_at_ms)
                .map(|(id, _)| id.clone())
        {
            imports.remove(&oldest);
        }
        imports.insert(stored.preview.preview_id.clone(), stored);
    }

    fn get_import(&self, preview_id: &str) -> Option<StoredImportPreview> {
        let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
        let mut imports = self
            .imports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let requested = imports.get(preview_id).cloned();
        imports.retain(|id, preview| id == preview_id || preview.preview.expires_at_ms > now_ms);
        requested
    }

    fn remove_import(&self, preview_id: &str) {
        self.imports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(preview_id);
    }
}

pub(super) fn dispatch(
    state: &ServerState,
    command: proto::ConfigurationCommand,
) -> Result<control_ok::Result, ControlCommandError> {
    let result = match command.action {
        Some(proto::configuration_command::Action::Get(request)) => {
            proto::configuration_result::Result::Snapshot(locked_configuration_snapshot_page(
                state, request,
            )?)
        }
        Some(proto::configuration_command::Action::SaveTemplate(request)) => {
            proto::configuration_result::Result::Template(save_template(state, request)?)
        }
        Some(proto::configuration_command::Action::DuplicateTemplate(request)) => {
            proto::configuration_result::Result::Template(duplicate_template(state, request)?)
        }
        Some(proto::configuration_command::Action::DeleteTemplate(request)) => {
            proto::configuration_result::Result::Snapshot(delete_template(state, request)?)
        }
        Some(proto::configuration_command::Action::Plan(request)) => {
            proto::configuration_result::Result::Plan(plan_configuration_change(state, request)?)
        }
        Some(proto::configuration_command::Action::Apply(request)) => {
            proto::configuration_result::Result::Applied(apply_configuration_plan(state, request)?)
        }
        Some(proto::configuration_command::Action::ExportTemplates(request)) => {
            proto::configuration_result::Result::ExportedTemplates(export_templates(
                state, request,
            )?)
        }
        Some(proto::configuration_command::Action::PreviewImport(request)) => {
            proto::configuration_result::Result::ImportPreview(preview_template_import(
                state, request,
            )?)
        }
        Some(proto::configuration_command::Action::ApplyImport(request)) => {
            proto::configuration_result::Result::Snapshot(apply_template_import(state, request)?)
        }
        None => {
            return Err(ControlCommandError::new(
                proto::ErrorCode::InvalidRequest,
                400,
                "configuration command has no action",
            ));
        }
    };
    Ok(control_ok::Result::ConfigurationResult(
        proto::ConfigurationResult {
            result: Some(result),
        },
    ))
}

fn plan_configuration_change(
    state: &ServerState,
    request: proto::PlanConfigurationChange,
) -> Result<proto::ConfigurationPlan, ControlCommandError> {
    let _configuration_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (config_path, root, templates, current_revision) = load_revision_state(state)?;
    require_revision(&request.expected_configuration_revision, &current_revision)?;
    let change = request
        .change
        .and_then(|change| change.change)
        .ok_or_else(|| {
            configuration_validation_error(
                &current_revision,
                vec![configuration_issue(
                    "change",
                    "configuration_change_missing",
                    "A configuration change is required.",
                )],
            )
        })?;
    let cameras = loaded_camera_configurations(&config_path, &root)
        .map_err(|error| configuration_internal_error("load cameras", error))?;
    let default_change = matches!(change, proto::configuration_change::Change::Defaults(_));
    let (targets, plan_targets, mut issues) = if default_change {
        all_configuration_targets(cameras)
    } else {
        resolve_configuration_targets(request.targets, cameras, &current_revision)?
    };
    let mut candidate = root.clone();
    let (touched_fields, apply_semantics, impact) =
        apply_change_to_candidate(&config_path, &mut candidate, &targets, &templates, &change)
            .map_err(|change_issues| {
                configuration_validation_error(&current_revision, change_issues)
            })?;
    let candidate_valid = match config::validate_configuration_table(&config_path, &candidate) {
        Ok(()) => true,
        Err(error) => {
            issues.push(configuration_issue(
                "configuration",
                "candidate_validation_failed",
                format!("The complete candidate configuration is invalid: {error}"),
            ));
            false
        }
    };
    let changes = if candidate_valid {
        configuration_changes(
            &config_path,
            &root,
            &candidate,
            &targets,
            &touched_fields,
            matches!(change, proto::configuration_change::Change::TemplateId(_)),
        )
        .map_err(|error| configuration_internal_error("calculate semantic changes", error))?
    } else {
        Vec::new()
    };
    if candidate_valid && changes.is_empty() {
        issues.push(configuration_issue(
            "change",
            "configuration_change_empty",
            "The requested operation does not change any effective or configured value.",
        ));
    }
    let valid = !issues
        .iter()
        .any(|issue| issue.severity == proto::ConfigurationIssueSeverity::Error as i32);
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let expires_at_ms =
        now_ms.saturating_add(i64::try_from(PLAN_TTL.as_millis()).unwrap_or(i64::MAX));
    let plan = proto::ConfigurationPlan {
        plan_id: Uuid::new_v4().simple().to_string(),
        configuration_revision: current_revision,
        expires_at_ms,
        authoritative_target_count: u32::try_from(targets.len()).unwrap_or(u32::MAX),
        targets: plan_targets,
        changes,
        issues,
        impact: impact as i32,
        valid,
        apply_semantics,
    };
    if configuration_response_bytes(proto::configuration_result::Result::Plan(plan.clone()))
        > MAXIMUM_CONFIGURATION_RESPONSE_BYTES
    {
        return Err(configuration_validation_error(
            &plan.configuration_revision,
            vec![configuration_issue(
                "targets",
                "configuration_preview_too_large",
                "The exact preview exceeds the control-message limit. Select fewer cameras or fields.",
            )],
        ));
    }
    if valid {
        state.configuration_plans.insert(StoredPlan {
            plan: plan.clone(),
            candidate,
            target_ids: targets
                .iter()
                .map(|camera| camera.config.ip.to_string())
                .collect(),
        });
    }
    Ok(plan)
}

fn configuration_response_bytes(result: proto::configuration_result::Result) -> usize {
    proto::ControlEnvelope {
        message: Some(proto::control_envelope::Message::Response(
            proto::Response {
                request_id: 1,
                result: Some(control_response::Result::Ok(proto::Ok {
                    result: Some(control_ok::Result::ConfigurationResult(
                        proto::ConfigurationResult {
                            result: Some(result),
                        },
                    )),
                })),
            },
        )),
    }
    .encoded_len()
}

fn apply_configuration_plan(
    state: &ServerState,
    request: proto::ApplyConfigurationPlan,
) -> Result<proto::ConfigurationApplyResult, ControlCommandError> {
    let stored = state
        .configuration_plans
        .get(&request.plan_id)
        .ok_or_else(|| {
            let current_revision = current_configuration_revision(state).unwrap_or_default();
            configuration_error(
                proto::ConfigurationErrorCode::PlanNotFound,
                proto::ErrorCode::NotFound,
                "configuration plan was not found",
                &current_revision,
                Vec::new(),
            )
        })?;
    let configuration_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (config_path, _root, _templates, current_revision) = load_revision_state(state)?;
    require_revision(&request.expected_configuration_revision, &current_revision)?;
    if stored.plan.configuration_revision != current_revision {
        return Err(configuration_error(
            proto::ConfigurationErrorCode::TargetChanged,
            proto::ErrorCode::Rejected,
            "configuration targets changed after this plan was created",
            &current_revision,
            Vec::new(),
        ));
    }
    if stored.plan.expires_at_ms <= i64::try_from(unix_time_ms()).unwrap_or(i64::MAX) {
        state.configuration_plans.remove(&request.plan_id);
        return Err(configuration_error(
            proto::ConfigurationErrorCode::PlanExpired,
            proto::ErrorCode::Rejected,
            "configuration plan expired",
            &current_revision,
            Vec::new(),
        ));
    }
    if !stored.plan.valid {
        return Err(configuration_validation_error(
            &current_revision,
            stored.plan.issues,
        ));
    }
    config::write_configuration_table(&config_path, &stored.candidate)
        .map_err(|error| configuration_internal_error("commit configuration plan", error))?;
    state.configuration_plans.remove(&request.plan_id);

    let saved = config::load_cameras(&config_path)
        .map_err(|error| configuration_internal_error("load committed cameras", error))?
        .into_values()
        .flatten()
        .map(|camera| (camera.ip.to_string(), camera))
        .collect::<HashMap<_, _>>();
    let mut activations = Vec::with_capacity(stored.target_ids.len());
    for camera_id in &stored.target_ids {
        let (status, detail) = match saved.get(camera_id) {
            None => (
                proto::ConfigurationActivationStatus::Failed,
                Some("The committed camera could not be loaded for activation.".to_owned()),
            ),
            Some(_) if state.camera_runtime.is_none() => (
                proto::ConfigurationActivationStatus::RestartRequired,
                Some(
                    "Configuration is committed; restart the server to activate this camera."
                        .to_owned(),
                ),
            ),
            Some(camera) => match start_runtime_camera(state, camera, true, false) {
                Some(_) => (proto::ConfigurationActivationStatus::Applied, None),
                None => (
                    proto::ConfigurationActivationStatus::Failed,
                    Some(
                        "Configuration is committed, but live activation failed; restart the server to recover."
                            .to_owned(),
                    ),
                ),
            },
        };
        activations.push(proto::ConfigurationActivation {
            camera_id: camera_id.clone(),
            status: status as i32,
            detail,
        });
    }
    drop(configuration_update);
    Ok(proto::ConfigurationApplyResult {
        plan_id: request.plan_id,
        configuration_committed: true,
        snapshot: None,
        activations,
        impact: stored.plan.impact,
    })
}

fn current_configuration_revision(state: &ServerState) -> anyhow::Result<String> {
    let config_path = state
        .camera_config_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("configuration persistence is unavailable"))?;
    let root = config::load_configuration_table(config_path)?;
    let templates = load_templates(config_path)?;
    configuration_revision(&root, &templates)
}

fn export_templates(
    state: &ServerState,
    request: proto::ExportConfigurationTemplates,
) -> Result<proto::ConfigurationTemplateDocumentResult, ControlCommandError> {
    let config_path = state.camera_config_path.as_deref().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            409,
            "configuration persistence is unavailable",
        )
    })?;
    let document = load_templates(config_path)
        .map_err(|error| configuration_internal_error("load templates", error))?;
    let selected = if request.template_ids.is_empty() {
        document.templates
    } else {
        let requested = request.template_ids.into_iter().collect::<HashSet<_>>();
        let selected = document
            .templates
            .into_iter()
            .filter(|template| requested.contains(&template.template_id))
            .collect::<Vec<_>>();
        if selected.len() != requested.len() {
            let current_revision = current_configuration_revision(state).unwrap_or_default();
            return Err(configuration_error(
                proto::ConfigurationErrorCode::TemplateNotFound,
                proto::ErrorCode::NotFound,
                "one or more configuration templates were not found",
                &current_revision,
                Vec::new(),
            ));
        }
        selected
    };
    let document_json = serde_json::to_string_pretty(&StoredTemplateDocument {
        document_version: TEMPLATE_DOCUMENT_VERSION,
        templates: selected,
    })
    .map_err(|error| configuration_internal_error("export templates", error.into()))?;
    if document_json.len() > MAXIMUM_TEMPLATE_DOCUMENT_BYTES {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            413,
            "configuration template export exceeds the document limit",
        ));
    }
    Ok(proto::ConfigurationTemplateDocumentResult { document_json })
}

fn preview_template_import(
    state: &ServerState,
    request: proto::PreviewConfigurationTemplateImport,
) -> Result<proto::ConfigurationTemplateImportPreview, ControlCommandError> {
    let _configuration_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (_config_path, _root, existing, current_revision) = load_revision_state(state)?;
    require_revision(&request.expected_configuration_revision, &current_revision)?;
    let mut issues = Vec::new();
    let document = if request.document_json.len() > MAXIMUM_TEMPLATE_DOCUMENT_BYTES {
        issues.push(configuration_issue(
            "document_json",
            "import_document_too_large",
            format!(
                "Template imports must contain at most {MAXIMUM_TEMPLATE_DOCUMENT_BYTES} bytes."
            ),
        ));
        None
    } else {
        match serde_json::from_str::<StoredTemplateDocument>(&request.document_json) {
            Ok(document) => {
                if let Err(error) = validate_stored_templates(&document) {
                    issues.push(configuration_issue(
                        "document_json",
                        "import_document_invalid",
                        error.to_string(),
                    ));
                }
                Some(document)
            }
            Err(error) => {
                issues.push(configuration_issue(
                    "document_json",
                    "import_document_invalid",
                    format!("Template import is not a valid versioned JSON document: {error}"),
                ));
                None
            }
        }
    };
    if let Some(document) = &document {
        if existing
            .templates
            .len()
            .saturating_add(document.templates.len())
            > MAXIMUM_TEMPLATES
        {
            issues.push(configuration_issue(
                "templates",
                "template_count_exceeded",
                format!("At most {MAXIMUM_TEMPLATES} templates are allowed."),
            ));
        }
        for imported in &document.templates {
            if existing
                .templates
                .iter()
                .any(|template| template.template_id == imported.template_id)
            {
                issues.push(configuration_issue(
                    "template_id",
                    "template_id_conflict",
                    format!("Template ID '{}' already exists.", imported.template_id),
                ));
            }
            if existing
                .templates
                .iter()
                .any(|template| template.name.eq_ignore_ascii_case(&imported.name))
            {
                issues.push(configuration_issue(
                    "name",
                    "template_name_conflict",
                    format!("Template name '{}' already exists.", imported.name),
                ));
            }
        }
    }
    let valid = document.is_some()
        && !issues
            .iter()
            .any(|issue| issue.severity == proto::ConfigurationIssueSeverity::Error as i32);
    let preview = proto::ConfigurationTemplateImportPreview {
        preview_id: Uuid::new_v4().simple().to_string(),
        configuration_revision: current_revision,
        expires_at_ms: i64::try_from(unix_time_ms())
            .unwrap_or(i64::MAX)
            .saturating_add(i64::try_from(PLAN_TTL.as_millis()).unwrap_or(i64::MAX)),
        templates: document
            .as_ref()
            .map(|document| {
                document
                    .templates
                    .iter()
                    .map(StoredTemplate::to_proto)
                    .collect()
            })
            .unwrap_or_default(),
        issues,
        valid,
    };
    if valid {
        state
            .configuration_plans
            .insert_import(StoredImportPreview {
                preview: preview.clone(),
                document: document.expect("valid import previews contain a document"),
            });
    }
    Ok(preview)
}

fn apply_template_import(
    state: &ServerState,
    request: proto::ApplyConfigurationTemplateImport,
) -> Result<proto::ConfigurationSnapshot, ControlCommandError> {
    let stored = state
        .configuration_plans
        .get_import(&request.preview_id)
        .ok_or_else(|| {
            let current_revision = current_configuration_revision(state).unwrap_or_default();
            configuration_error(
                proto::ConfigurationErrorCode::PlanNotFound,
                proto::ErrorCode::NotFound,
                "configuration template import preview was not found",
                &current_revision,
                Vec::new(),
            )
        })?;
    let configuration_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (config_path, _root, mut current, current_revision) = load_revision_state(state)?;
    require_revision(&request.expected_configuration_revision, &current_revision)?;
    if stored.preview.configuration_revision != current_revision {
        return Err(configuration_error(
            proto::ConfigurationErrorCode::Conflict,
            proto::ErrorCode::Rejected,
            "configuration changed after this import was previewed",
            &current_revision,
            Vec::new(),
        ));
    }
    if stored.preview.expires_at_ms <= i64::try_from(unix_time_ms()).unwrap_or(i64::MAX) {
        state.configuration_plans.remove_import(&request.preview_id);
        return Err(configuration_error(
            proto::ConfigurationErrorCode::PlanExpired,
            proto::ErrorCode::Rejected,
            "configuration template import preview expired",
            &current_revision,
            Vec::new(),
        ));
    }
    current.templates.extend(stored.document.templates);
    current
        .templates
        .sort_unstable_by(|left, right| left.name.cmp(&right.name));
    persist_templates(&config_path, &current)
        .map_err(|error| configuration_internal_error("apply template import", error))?;
    state.configuration_plans.remove_import(&request.preview_id);
    drop(configuration_update);
    configuration_snapshot_page(state, proto::GetConfigurationSnapshot::default())
}

type ResolvedTargets = (
    Vec<LoadedCameraConfiguration>,
    Vec<proto::ConfigurationPlanTarget>,
    Vec<proto::ConfigurationIssue>,
);

fn all_configuration_targets(cameras: Vec<LoadedCameraConfiguration>) -> ResolvedTargets {
    let targets = cameras.iter().map(proto_plan_target).collect::<Vec<_>>();
    let issues = (cameras.len() > MAXIMUM_PLAN_TARGETS)
        .then(|| {
            configuration_issue(
                "targets",
                "configuration_target_limit_exceeded",
                format!("At most {MAXIMUM_PLAN_TARGETS} cameras can be changed at once."),
            )
        })
        .into_iter()
        .collect();
    (cameras, targets, issues)
}

fn resolve_configuration_targets(
    selector: Option<proto::ConfigurationTargetSelector>,
    cameras: Vec<LoadedCameraConfiguration>,
    current_revision: &str,
) -> Result<ResolvedTargets, ControlCommandError> {
    let selection = selector
        .and_then(|selector| selector.selection)
        .ok_or_else(|| {
            configuration_validation_error(
                current_revision,
                vec![configuration_issue(
                    "targets",
                    "configuration_targets_missing",
                    "An explicit target selector is required.",
                )],
            )
        })?;
    let mut selected = Vec::new();
    let mut plan_targets = Vec::new();
    let mut issues = Vec::new();
    match selection {
        proto::configuration_target_selector::Selection::CameraIds(ids) => {
            let mut requested = ids.camera_ids;
            requested.sort_unstable();
            requested.dedup();
            if requested.len() > MAXIMUM_PLAN_TARGETS {
                return Err(configuration_validation_error(
                    current_revision,
                    vec![configuration_issue(
                        "targets",
                        "configuration_target_limit_exceeded",
                        format!("At most {MAXIMUM_PLAN_TARGETS} cameras can be changed at once."),
                    )],
                ));
            }
            let mut available = cameras
                .into_iter()
                .map(|camera| (camera.config.ip.to_string(), camera))
                .collect::<HashMap<_, _>>();
            for camera_id in requested {
                if let Some(camera) = available.remove(&camera_id) {
                    plan_targets.push(proto_plan_target(&camera));
                    selected.push(camera);
                } else {
                    plan_targets.push(proto::ConfigurationPlanTarget {
                        camera_id: camera_id.clone(),
                        display_name: camera_id.clone(),
                        group_ids: Vec::new(),
                        skipped: true,
                        skip_reason: Some(
                            "Camera is not present in the current revision.".to_owned(),
                        ),
                    });
                    issues.push(configuration_issue_for_camera(
                        camera_id,
                        "targets",
                        "camera_not_found",
                        "The camera is not present in the current revision.",
                    ));
                }
            }
        }
        proto::configuration_target_selector::Selection::FilteredCameras(filter) => {
            if filter.search.chars().count() > 128 {
                return Err(configuration_validation_error(
                    current_revision,
                    vec![configuration_issue(
                        "targets.search",
                        "filter_search_too_long",
                        "Camera filter search must contain at most 128 characters.",
                    )],
                ));
            }
            let backend = match filter.backend {
                Some(value) => Some(camera_backend_from_proto(value).ok_or_else(|| {
                    configuration_validation_error(
                        current_revision,
                        vec![configuration_issue(
                            "targets.backend",
                            "enum_value_invalid",
                            "Camera backend filter contains an unsupported value.",
                        )],
                    )
                })?),
                None => None,
            };
            let recording_mode = match filter.recording_mode {
                Some(value) => Some(camera_recording_mode_from_proto(value).ok_or_else(|| {
                    configuration_validation_error(
                        current_revision,
                        vec![configuration_issue(
                            "targets.recording_mode",
                            "enum_value_invalid",
                            "Recording mode filter contains an unsupported value.",
                        )],
                    )
                })?),
                None => None,
            };
            let search = filter.search.trim().to_lowercase();
            for camera in cameras {
                let matches_search =
                    search.is_empty() || camera_search_text(&camera).contains(&search);
                if matches_search
                    && backend.is_none_or(|backend| camera.config.backend == backend)
                    && recording_mode.is_none_or(|mode| camera.config.recording_mode == mode)
                {
                    plan_targets.push(proto_plan_target(&camera));
                    selected.push(camera);
                }
            }
        }
        proto::configuration_target_selector::Selection::GroupId(group_id) => {
            for camera in cameras {
                if camera.group_ids.iter().any(|group| group == &group_id) {
                    plan_targets.push(proto_plan_target(&camera));
                    selected.push(camera);
                }
            }
        }
        proto::configuration_target_selector::Selection::AllCameras(all) => {
            if !all {
                return Err(configuration_validation_error(
                    current_revision,
                    vec![configuration_issue(
                        "targets",
                        "all_cameras_not_confirmed",
                        "The all-cameras selector must be explicitly confirmed.",
                    )],
                ));
            }
            for camera in cameras {
                plan_targets.push(proto_plan_target(&camera));
                selected.push(camera);
            }
        }
    }
    if selected.len() > MAXIMUM_PLAN_TARGETS {
        return Err(configuration_validation_error(
            current_revision,
            vec![configuration_issue(
                "targets",
                "configuration_target_limit_exceeded",
                format!("At most {MAXIMUM_PLAN_TARGETS} cameras can be changed at once."),
            )],
        ));
    }
    if selected.is_empty() {
        issues.push(configuration_issue(
            "targets",
            "configuration_targets_empty",
            "The target selector did not resolve to a current camera.",
        ));
    }
    selected.sort_unstable_by_key(|camera| camera.config.ip);
    plan_targets.sort_unstable_by(|left, right| left.camera_id.cmp(&right.camera_id));
    Ok((selected, plan_targets, issues))
}

fn camera_search_text(camera: &LoadedCameraConfiguration) -> String {
    let mut values = vec![camera.config.ip.to_string()];
    values.extend(camera.group_ids.iter().cloned());
    values.extend(camera.config.name.iter().cloned());
    values.extend(camera.config.display_name.iter().cloned());
    values.join(" ").to_lowercase()
}

fn proto_plan_target(camera: &LoadedCameraConfiguration) -> proto::ConfigurationPlanTarget {
    proto::ConfigurationPlanTarget {
        camera_id: camera.config.ip.to_string(),
        display_name: camera
            .config
            .display_name()
            .map_or_else(|| camera.config.ip.to_string(), str::to_owned),
        group_ids: camera.group_ids.clone(),
        skipped: false,
        skip_reason: None,
    }
}

fn apply_change_to_candidate(
    config_path: &Path,
    root: &mut toml::Table,
    targets: &[LoadedCameraConfiguration],
    templates: &StoredTemplateDocument,
    change: &proto::configuration_change::Change,
) -> Result<(Vec<String>, String, proto::ConfigurationImpact), Vec<proto::ConfigurationIssue>> {
    let mut issues = Vec::new();
    let (fields, semantics) = match change {
        proto::configuration_change::Change::Patch(patch) => {
            let fields = camera_patch_fields(patch);
            if fields.is_empty() {
                issues.push(configuration_issue(
                    "change.patch",
                    "camera_patch_empty",
                    "Select at least one camera setting to change.",
                ));
            }
            for target in targets {
                apply_to_camera_tables(root, target.config.ip, |table| {
                    apply_camera_patch(config_path, table, patch, &mut issues);
                });
            }
            (
                fields,
                "Named fields become explicit camera overrides; untouched fields and secret references are preserved."
                    .to_owned(),
            )
        }
        proto::configuration_change::Change::TemplateId(template_id) => {
            let Some(template) = templates
                .templates
                .iter()
                .find(|template| template.template_id == *template_id)
            else {
                issues.push(configuration_issue(
                    "change.template_id",
                    "template_not_found",
                    "The selected template is not present in the current revision.",
                ));
                return Err(issues);
            };
            let fields = template_fields(&template.values);
            for target in targets {
                apply_to_camera_tables(root, target.config.ip, |table| {
                    apply_template_values(table, &template.values);
                });
            }
            (
                fields,
                "Applying this template creates explicit camera overrides. Later template edits or deletion do not mutate cameras."
                    .to_owned(),
            )
        }
        proto::configuration_change::Change::Defaults(patch) => {
            let fields = default_patch_fields(patch);
            if fields.is_empty() {
                issues.push(configuration_issue(
                    "change.defaults",
                    "default_patch_empty",
                    "Select at least one shared camera default to change.",
                ));
            }
            let defaults = root
                .entry("camera_defaults".to_owned())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut();
            match defaults {
                Some(defaults) => apply_default_patch(defaults, patch, &mut issues),
                None => issues.push(configuration_issue(
                    "camera_defaults",
                    "default_table_invalid",
                    "The camera_defaults value must be a table.",
                )),
            }
            (
                fields,
                "Shared defaults flow only to cameras without an explicit override. Existing overrides remain unchanged."
                    .to_owned(),
            )
        }
    };
    if !issues.is_empty() {
        return Err(issues);
    }
    Ok((
        fields,
        semantics,
        proto::ConfigurationImpact::ReconnectCamera,
    ))
}

fn apply_to_camera_tables(
    root: &mut toml::Table,
    camera_ip: IpAddr,
    mut apply: impl FnMut(&mut toml::Table),
) {
    let locations = root
        .iter()
        .filter_map(|(namespace, value)| {
            let cameras = value.as_table()?;
            Some(cameras.iter().filter_map(move |(name, value)| {
                let matches = value
                    .as_table()
                    .and_then(|camera| camera.get("ip"))
                    .and_then(toml::Value::as_str)
                    .and_then(|ip| ip.parse::<IpAddr>().ok())
                    .is_some_and(|ip| ip == camera_ip);
                matches.then(|| (namespace.clone(), name.clone()))
            }))
        })
        .flatten()
        .collect::<Vec<_>>();
    for (namespace, name) in locations {
        if let Some(table) = root
            .get_mut(&namespace)
            .and_then(toml::Value::as_table_mut)
            .and_then(|cameras| cameras.get_mut(&name))
            .and_then(toml::Value::as_table_mut)
        {
            apply(table);
        }
    }
}

fn apply_camera_patch(
    config_path: &Path,
    table: &mut toml::Table,
    patch: &proto::CameraConfigurationPatch,
    issues: &mut Vec<proto::ConfigurationIssue>,
) {
    apply_string_update(
        table,
        "display_name",
        "display_name",
        patch.display_name.as_ref(),
        |value| {
            let value = value.trim();
            (!value.is_empty() && !value.chars().any(char::is_control))
                .then(|| value.to_owned())
                .ok_or_else(|| "Display name must contain printable text.".to_owned())
        },
        issues,
    );
    apply_string_update(
        table,
        "manufacturer",
        "manufacturer",
        patch.manufacturer.as_ref(),
        |value| {
            let value = value.trim();
            (!value.is_empty() && value.len() <= 120 && !value.chars().any(char::is_control))
                .then(|| value.to_owned())
                .ok_or_else(|| "Manufacturer must contain 1 to 120 printable bytes.".to_owned())
        },
        issues,
    );
    apply_string_update(
        table,
        "username",
        "username_secret_reference",
        patch.username_secret_reference.as_ref(),
        secret_reference,
        issues,
    );
    apply_string_update(
        table,
        "password",
        "password_secret_reference",
        patch.password_secret_reference.as_ref(),
        secret_reference,
        issues,
    );
    apply_port_update(table, "onvif_port", patch.onvif_port.as_ref(), issues);
    apply_port_update(table, "http_port", patch.http_port.as_ref(), issues);
    apply_string_update(
        table,
        "main_rtsp_url",
        "main_rtsp_url",
        patch.main_rtsp_url.as_ref(),
        |value| valid_rtsp_value(config_path, value, "main RTSP URL"),
        issues,
    );
    apply_string_update(
        table,
        "sub_rtsp_url",
        "sub_rtsp_url",
        patch.sub_rtsp_url.as_ref(),
        |value| valid_rtsp_value(config_path, value, "sub RTSP URL"),
        issues,
    );
    apply_string_update(
        table,
        "uid",
        "uid_secret_reference",
        patch.uid_secret_reference.as_ref(),
        secret_reference,
        issues,
    );
    apply_backend_update(table, "backend", patch.backend.as_ref(), issues);
    apply_transport_update(table, "transport", patch.transport.as_ref(), issues);
    apply_bool_update(
        table,
        "record_generic_motion_events",
        patch.record_generic_motion_events.as_ref(),
        issues,
    );
    apply_recording_mode_update(
        table,
        "recording_mode",
        patch.recording_mode.as_ref(),
        issues,
    );
    apply_duration_update(
        table,
        "event_recording_duration_secs",
        patch.event_recording_duration_secs.as_ref(),
        issues,
    );
}

fn apply_default_patch(
    table: &mut toml::Table,
    patch: &proto::CameraDefaultPatch,
    issues: &mut Vec<proto::ConfigurationIssue>,
) {
    apply_string_update(
        table,
        "username",
        "username_secret_reference",
        patch.username_secret_reference.as_ref(),
        secret_reference,
        issues,
    );
    apply_string_update(
        table,
        "password",
        "password_secret_reference",
        patch.password_secret_reference.as_ref(),
        secret_reference,
        issues,
    );
    apply_backend_update(table, "backend", patch.backend.as_ref(), issues);
    apply_transport_update(table, "transport", patch.transport.as_ref(), issues);
    apply_bool_update(
        table,
        "record_generic_motion_events",
        patch.record_generic_motion_events.as_ref(),
        issues,
    );
    apply_recording_mode_update(
        table,
        "recording_mode",
        patch.recording_mode.as_ref(),
        issues,
    );
    apply_duration_update(
        table,
        "event_recording_duration_secs",
        patch.event_recording_duration_secs.as_ref(),
        issues,
    );
}

fn apply_template_values(table: &mut toml::Table, values: &StoredTemplateValues) {
    insert_optional_string(
        table,
        "username",
        values.username_secret_reference.as_deref(),
    );
    insert_optional_string(
        table,
        "password",
        values.password_secret_reference.as_deref(),
    );
    insert_optional_integer(table, "onvif_port", values.onvif_port.map(u64::from));
    insert_optional_integer(table, "http_port", values.http_port.map(u64::from));
    if let Some(value) = values.backend {
        table.insert(
            "backend".to_owned(),
            toml::Value::String(camera_backend_name(value).to_owned()),
        );
    }
    if let Some(value) = values.transport {
        table.insert(
            "transport".to_owned(),
            toml::Value::String(camera_transport_name(value).to_owned()),
        );
    }
    if let Some(value) = values.record_generic_motion_events {
        table.insert(
            "record_generic_motion_events".to_owned(),
            toml::Value::Boolean(value),
        );
    }
    if let Some(value) = values.recording_mode {
        table.insert(
            "recording_mode".to_owned(),
            toml::Value::String(camera_recording_mode_name(value).to_owned()),
        );
    }
    insert_optional_integer(
        table,
        "event_recording_duration_secs",
        values.event_recording_duration_secs.map(u64::from),
    );
}

fn apply_string_update<F>(
    table: &mut toml::Table,
    key: &str,
    field: &str,
    update: Option<&proto::OptionalStringUpdate>,
    normalize: F,
    issues: &mut Vec<proto::ConfigurationIssue>,
) where
    F: Fn(&str) -> Result<String, String>,
{
    let Some(update) = update else { return };
    match update.value.as_ref() {
        Some(optional_string_update::Value::Set(value)) => match normalize(value) {
            Ok(value) => {
                table.insert(key.to_owned(), toml::Value::String(value));
            }
            Err(message) => issues.push(configuration_issue(field, "value_invalid", message)),
        },
        Some(optional_string_update::Value::Clear(true)) => {
            table.remove(key);
        }
        Some(optional_string_update::Value::Clear(false)) | None => {
            issues.push(configuration_issue(
                field,
                "patch_operation_invalid",
                "The patch must set or clear this field.",
            ));
        }
    }
}

fn apply_port_update(
    table: &mut toml::Table,
    key: &str,
    update: Option<&proto::OptionalUint32Update>,
    issues: &mut Vec<proto::ConfigurationIssue>,
) {
    apply_u32_update(table, key, update, 1, u32::from(u16::MAX), issues);
}

fn apply_duration_update(
    table: &mut toml::Table,
    key: &str,
    update: Option<&proto::OptionalUint32Update>,
    issues: &mut Vec<proto::ConfigurationIssue>,
) {
    apply_u32_update(table, key, update, 1, 3_600, issues);
}

fn apply_u32_update(
    table: &mut toml::Table,
    key: &str,
    update: Option<&proto::OptionalUint32Update>,
    minimum: u32,
    maximum: u32,
    issues: &mut Vec<proto::ConfigurationIssue>,
) {
    let Some(update) = update else { return };
    match update.value {
        Some(proto::optional_uint32_update::Value::Set(value))
            if (minimum..=maximum).contains(&value) =>
        {
            table.insert(key.to_owned(), toml::Value::Integer(i64::from(value)));
        }
        Some(proto::optional_uint32_update::Value::Set(_)) => issues.push(configuration_issue(
            key,
            "number_out_of_range",
            format!("{key} must be between {minimum} and {maximum}."),
        )),
        Some(proto::optional_uint32_update::Value::Clear(true)) => {
            table.remove(key);
        }
        Some(proto::optional_uint32_update::Value::Clear(false)) | None => {
            issues.push(configuration_issue(
                key,
                "patch_operation_invalid",
                "The patch must set or clear this field.",
            ));
        }
    }
}

fn apply_backend_update(
    table: &mut toml::Table,
    key: &str,
    update: Option<&proto::OptionalCameraBackendUpdate>,
    issues: &mut Vec<proto::ConfigurationIssue>,
) {
    let Some(update) = update else { return };
    match update.value {
        Some(proto::optional_camera_backend_update::Value::Set(value)) => {
            if let Some(value) = camera_backend_from_proto(value) {
                table.insert(
                    key.to_owned(),
                    toml::Value::String(camera_backend_name(value).to_owned()),
                );
            } else {
                issues.push(configuration_issue(
                    key,
                    "enum_value_invalid",
                    "Camera backend contains an unsupported value.",
                ));
            }
        }
        Some(proto::optional_camera_backend_update::Value::Clear(true)) => {
            table.remove(key);
        }
        Some(proto::optional_camera_backend_update::Value::Clear(false)) | None => {
            issues.push(configuration_issue(
                key,
                "patch_operation_invalid",
                "The patch must set or clear this field.",
            ));
        }
    }
}

fn apply_transport_update(
    table: &mut toml::Table,
    key: &str,
    update: Option<&proto::OptionalCameraTransportUpdate>,
    issues: &mut Vec<proto::ConfigurationIssue>,
) {
    let Some(update) = update else { return };
    match update.value {
        Some(proto::optional_camera_transport_update::Value::Set(value)) => {
            if let Some(value) = camera_transport_from_proto(value) {
                table.insert(
                    key.to_owned(),
                    toml::Value::String(camera_transport_name(value).to_owned()),
                );
            } else {
                issues.push(configuration_issue(
                    key,
                    "enum_value_invalid",
                    "Camera transport contains an unsupported value.",
                ));
            }
        }
        Some(proto::optional_camera_transport_update::Value::Clear(true)) => {
            table.remove(key);
        }
        Some(proto::optional_camera_transport_update::Value::Clear(false)) | None => {
            issues.push(configuration_issue(
                key,
                "patch_operation_invalid",
                "The patch must set or clear this field.",
            ));
        }
    }
}

fn apply_recording_mode_update(
    table: &mut toml::Table,
    key: &str,
    update: Option<&proto::OptionalCameraRecordingModeUpdate>,
    issues: &mut Vec<proto::ConfigurationIssue>,
) {
    let Some(update) = update else { return };
    match update.value {
        Some(proto::optional_camera_recording_mode_update::Value::Set(value)) => {
            if let Some(value) = camera_recording_mode_from_proto(value) {
                table.insert(
                    key.to_owned(),
                    toml::Value::String(camera_recording_mode_name(value).to_owned()),
                );
            } else {
                issues.push(configuration_issue(
                    key,
                    "enum_value_invalid",
                    "Recording mode contains an unsupported value.",
                ));
            }
        }
        Some(proto::optional_camera_recording_mode_update::Value::Clear(true)) => {
            table.remove(key);
        }
        Some(proto::optional_camera_recording_mode_update::Value::Clear(false)) | None => issues
            .push(configuration_issue(
                key,
                "patch_operation_invalid",
                "The patch must set or clear this field.",
            )),
    }
}

fn apply_bool_update(
    table: &mut toml::Table,
    key: &str,
    update: Option<&proto::OptionalBoolUpdate>,
    issues: &mut Vec<proto::ConfigurationIssue>,
) {
    let Some(update) = update else { return };
    match update.value {
        Some(proto::optional_bool_update::Value::Set(value)) => {
            table.insert(key.to_owned(), toml::Value::Boolean(value));
        }
        Some(proto::optional_bool_update::Value::Clear(true)) => {
            table.remove(key);
        }
        Some(proto::optional_bool_update::Value::Clear(false)) | None => {
            issues.push(configuration_issue(
                key,
                "patch_operation_invalid",
                "The patch must set or clear this field.",
            ));
        }
    }
}

fn secret_reference(value: &str) -> Result<String, String> {
    config::is_secret_reference(value)
        .then(|| value.to_owned())
        .ok_or_else(|| {
            "Use a complete {secret:KEY} reference; inline secret values are not accepted."
                .to_owned()
        })
}

fn valid_rtsp_value(config_path: &Path, value: &str, label: &str) -> Result<String, String> {
    let resolved = config::resolve_secret_references(config_path, value)
        .map_err(|error| format!("{label} secret reference is invalid: {error}"))?;
    normalize_rtsp_url(Some(resolved))
        .map_err(|error| format!("{label} {error}"))?
        .ok_or_else(|| format!("{label} must be nonempty."))?;
    Ok(value.trim().to_owned())
}

fn insert_optional_string(table: &mut toml::Table, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        table.insert(key.to_owned(), toml::Value::String(value.to_owned()));
    }
}

fn insert_optional_integer(table: &mut toml::Table, key: &str, value: Option<u64>) {
    if let Some(value) = value.and_then(|value| i64::try_from(value).ok()) {
        table.insert(key.to_owned(), toml::Value::Integer(value));
    }
}

fn camera_patch_fields(patch: &proto::CameraConfigurationPatch) -> Vec<String> {
    [
        (patch.display_name.is_some(), "display_name"),
        (patch.manufacturer.is_some(), "manufacturer"),
        (patch.username_secret_reference.is_some(), "username"),
        (patch.password_secret_reference.is_some(), "password"),
        (patch.onvif_port.is_some(), "onvif_port"),
        (patch.http_port.is_some(), "http_port"),
        (patch.main_rtsp_url.is_some(), "main_rtsp_url"),
        (patch.sub_rtsp_url.is_some(), "sub_rtsp_url"),
        (patch.uid_secret_reference.is_some(), "uid"),
        (patch.backend.is_some(), "backend"),
        (patch.transport.is_some(), "transport"),
        (
            patch.record_generic_motion_events.is_some(),
            "record_generic_motion_events",
        ),
        (patch.recording_mode.is_some(), "recording_mode"),
        (
            patch.event_recording_duration_secs.is_some(),
            "event_recording_duration_secs",
        ),
    ]
    .into_iter()
    .filter(|(present, _)| *present)
    .map(|(_, field)| field.to_owned())
    .collect()
}

fn default_patch_fields(patch: &proto::CameraDefaultPatch) -> Vec<String> {
    [
        (patch.username_secret_reference.is_some(), "username"),
        (patch.password_secret_reference.is_some(), "password"),
        (patch.backend.is_some(), "backend"),
        (patch.transport.is_some(), "transport"),
        (
            patch.record_generic_motion_events.is_some(),
            "record_generic_motion_events",
        ),
        (patch.recording_mode.is_some(), "recording_mode"),
        (
            patch.event_recording_duration_secs.is_some(),
            "event_recording_duration_secs",
        ),
    ]
    .into_iter()
    .filter(|(present, _)| *present)
    .map(|(_, field)| field.to_owned())
    .collect()
}

fn template_fields(values: &StoredTemplateValues) -> Vec<String> {
    [
        (values.username_secret_reference.is_some(), "username"),
        (values.password_secret_reference.is_some(), "password"),
        (values.onvif_port.is_some(), "onvif_port"),
        (values.http_port.is_some(), "http_port"),
        (values.backend.is_some(), "backend"),
        (values.transport.is_some(), "transport"),
        (
            values.record_generic_motion_events.is_some(),
            "record_generic_motion_events",
        ),
        (values.recording_mode.is_some(), "recording_mode"),
        (
            values.event_recording_duration_secs.is_some(),
            "event_recording_duration_secs",
        ),
    ]
    .into_iter()
    .filter(|(present, _)| *present)
    .map(|(_, field)| field.to_owned())
    .collect()
}

fn configuration_changes(
    config_path: &Path,
    before: &toml::Table,
    after: &toml::Table,
    targets: &[LoadedCameraConfiguration],
    fields: &[String],
    template_source: bool,
) -> anyhow::Result<Vec<proto::ConfigurationFieldChange>> {
    let before_cameras = loaded_camera_configurations(config_path, before)?
        .into_iter()
        .map(|camera| (camera.config.ip, camera))
        .collect::<HashMap<_, _>>();
    let after_cameras = loaded_camera_configurations(config_path, after)?
        .into_iter()
        .map(|camera| (camera.config.ip, camera))
        .collect::<HashMap<_, _>>();
    let after_defaults = after.get("camera_defaults").and_then(toml::Value::as_table);
    let mut changes = Vec::new();
    for target in targets {
        let Some(old) = before_cameras.get(&target.config.ip) else {
            continue;
        };
        let Some(new) = after_cameras.get(&target.config.ip) else {
            continue;
        };
        for field in fields {
            let secret = field_is_secret(field);
            let old_configured = configured_field_value(&old.configured, field, secret);
            let new_configured = configured_field_value(&new.configured, field, secret);
            let old_effective = effective_field_value(&old.config, field, secret);
            let new_effective = effective_field_value(&new.config, field, secret);
            let configured_changed = old.configured.get(field) != new.configured.get(field);
            let effective_changed = effective_field_changed(&old.config, &new.config, field);
            if !configured_changed && !effective_changed {
                continue;
            }
            let source = if template_source {
                proto::ConfigurationValueSource::Template
            } else if new.configured.contains_key(field) {
                proto::ConfigurationValueSource::Override
            } else if after_defaults.is_some_and(|defaults| defaults.contains_key(field)) {
                proto::ConfigurationValueSource::Default
            } else {
                proto::ConfigurationValueSource::BuiltIn
            };
            changes.push(proto::ConfigurationFieldChange {
                camera_id: Some(target.config.ip.to_string()),
                field: field.clone(),
                old_configured_value: old_configured,
                old_effective_value: old_effective,
                new_configured_value: new_configured,
                new_effective_value: new_effective,
                source: source as i32,
                secret,
            });
        }
    }
    changes.sort_unstable_by(|left, right| {
        left.camera_id
            .cmp(&right.camera_id)
            .then_with(|| left.field.cmp(&right.field))
    });
    Ok(changes)
}

fn effective_field_changed(left: &CameraConfig, right: &CameraConfig, field: &str) -> bool {
    match field {
        "display_name" => left.display_name != right.display_name,
        "manufacturer" => left.manufacturer != right.manufacturer,
        "username" => left.username != right.username,
        "password" => left.password != right.password,
        "onvif_port" => left.onvif_port != right.onvif_port,
        "http_port" => left.http_port != right.http_port,
        "main_rtsp_url" => left.main_rtsp_url != right.main_rtsp_url,
        "sub_rtsp_url" => left.sub_rtsp_url != right.sub_rtsp_url,
        "uid" => left.uid != right.uid,
        "backend" => left.backend != right.backend,
        "transport" => left.transport != right.transport,
        "record_generic_motion_events" => {
            left.record_generic_motion_events != right.record_generic_motion_events
        }
        "recording_mode" => left.recording_mode != right.recording_mode,
        "event_recording_duration_secs" => {
            left.event_recording_duration_secs != right.event_recording_duration_secs
        }
        _ => false,
    }
}

fn configured_field_value(table: &toml::Table, field: &str, secret: bool) -> String {
    let Some(value) = table.get(field) else {
        return "inherited".to_owned();
    };
    if secret {
        return "configured reference".to_owned();
    }
    toml_value_label(value)
}

fn effective_field_value(config: &CameraConfig, field: &str, secret: bool) -> String {
    if secret {
        let configured = match field {
            "username" => !config.username.is_empty(),
            "password" => !config.password.is_empty(),
            "main_rtsp_url" => config.main_rtsp_url.is_some(),
            "sub_rtsp_url" => config.sub_rtsp_url.is_some(),
            "uid" => config.uid.is_some(),
            _ => false,
        };
        return if configured {
            "configured"
        } else {
            "not configured"
        }
        .to_owned();
    }
    match field {
        "display_name" => config
            .display_name
            .clone()
            .unwrap_or_else(|| "not configured".to_owned()),
        "manufacturer" => config
            .manufacturer
            .clone()
            .unwrap_or_else(|| "not configured".to_owned()),
        "onvif_port" => config
            .onvif_port
            .map_or_else(|| "automatic".to_owned(), |value| value.to_string()),
        "http_port" => config
            .http_port
            .map_or_else(|| "automatic".to_owned(), |value| value.to_string()),
        "backend" => camera_backend_name(config.backend).to_owned(),
        "transport" => camera_transport_name(config.transport).to_owned(),
        "record_generic_motion_events" => config.record_generic_motion_events.to_string(),
        "recording_mode" => camera_recording_mode_name(config.recording_mode).to_owned(),
        "event_recording_duration_secs" => config.event_recording_duration_secs.to_string(),
        _ => "not configured".to_owned(),
    }
}

fn toml_value_label(value: &toml::Value) -> String {
    match value {
        toml::Value::String(value) => value.clone(),
        toml::Value::Integer(value) => value.to_string(),
        toml::Value::Float(value) => value.to_string(),
        toml::Value::Boolean(value) => value.to_string(),
        toml::Value::Datetime(value) => value.to_string(),
        toml::Value::Array(_) | toml::Value::Table(_) => "structured value".to_owned(),
    }
}

fn field_is_secret(field: &str) -> bool {
    matches!(
        field,
        "username" | "password" | "main_rtsp_url" | "sub_rtsp_url" | "uid"
    )
}

const fn camera_recording_mode_name(value: CameraRecordingMode) -> &'static str {
    match value {
        CameraRecordingMode::Off => "off",
        CameraRecordingMode::Sub => "sub",
        CameraRecordingMode::Main => "main",
        CameraRecordingMode::Both => "both",
        CameraRecordingMode::EventBoost => "event-boost",
    }
}

fn configuration_issue_for_camera(
    camera_id: String,
    field: impl Into<String>,
    code: impl Into<String>,
    message: impl Into<String>,
) -> proto::ConfigurationIssue {
    let mut issue = configuration_issue(field, code, message);
    issue.camera_id = Some(camera_id);
    issue
}

fn save_template(
    state: &ServerState,
    request: proto::SaveConfigurationTemplate,
) -> Result<proto::ConfigurationTemplate, ControlCommandError> {
    let _configuration_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (config_path, root, mut document, current_revision) = load_revision_state(state)?;
    require_revision(&request.expected_configuration_revision, &current_revision)?;
    let template = request.template.ok_or_else(|| {
        configuration_validation_error(
            &current_revision,
            vec![configuration_issue(
                "template",
                "template_missing",
                "A template is required.",
            )],
        )
    })?;
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let existing_index = (!template.template_id.is_empty())
        .then(|| {
            document
                .templates
                .iter()
                .position(|stored| stored.template_id == template.template_id)
        })
        .flatten();
    let (template_id, version, created_at_ms) = match existing_index {
        Some(index) => {
            let existing = &document.templates[index];
            if request.expected_template_version != Some(existing.version) {
                return Err(configuration_error(
                    proto::ConfigurationErrorCode::TemplateConflict,
                    proto::ErrorCode::Rejected,
                    "configuration template version conflict",
                    &current_revision,
                    vec![configuration_issue(
                        "version",
                        "template_version_conflict",
                        format!("The current template version is {}.", existing.version),
                    )],
                ));
            }
            (
                existing.template_id.clone(),
                existing.version.saturating_add(1),
                existing.created_at_ms,
            )
        }
        None if template.template_id.is_empty() => {
            if request.expected_template_version.is_some() {
                return Err(configuration_validation_error(
                    &current_revision,
                    vec![configuration_issue(
                        "expected_template_version",
                        "new_template_version_present",
                        "A new template cannot include an expected version.",
                    )],
                ));
            }
            if document.templates.len() >= MAXIMUM_TEMPLATES {
                return Err(configuration_validation_error(
                    &current_revision,
                    vec![configuration_issue(
                        "template",
                        "template_count_exceeded",
                        format!("At most {MAXIMUM_TEMPLATES} templates are allowed."),
                    )],
                ));
            }
            (Uuid::new_v4().simple().to_string(), 1, now_ms)
        }
        None => {
            return Err(configuration_error(
                proto::ConfigurationErrorCode::TemplateNotFound,
                proto::ErrorCode::NotFound,
                "configuration template was not found",
                &current_revision,
                Vec::new(),
            ));
        }
    };
    let stored = StoredTemplate::from_proto(template, template_id, version, created_at_ms, now_ms)
        .map_err(|issues| configuration_validation_error(&current_revision, issues))?;
    if document
        .templates
        .iter()
        .enumerate()
        .any(|(index, template)| {
            Some(index) != existing_index && template.name.eq_ignore_ascii_case(&stored.name)
        })
    {
        return Err(configuration_validation_error(
            &current_revision,
            vec![configuration_issue(
                "name",
                "template_name_conflict",
                "Template names must be unique.",
            )],
        ));
    }
    match existing_index {
        Some(index) => document.templates[index] = stored.clone(),
        None => document.templates.push(stored.clone()),
    }
    document
        .templates
        .sort_unstable_by(|left, right| left.name.cmp(&right.name));
    persist_templates(&config_path, &document)
        .map_err(|error| configuration_internal_error("save template", error))?;
    let _ = root;
    Ok(stored.to_proto())
}

fn duplicate_template(
    state: &ServerState,
    request: proto::DuplicateConfigurationTemplate,
) -> Result<proto::ConfigurationTemplate, ControlCommandError> {
    let _configuration_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (config_path, _root, mut document, current_revision) = load_revision_state(state)?;
    require_revision(&request.expected_configuration_revision, &current_revision)?;
    if document.templates.len() >= MAXIMUM_TEMPLATES {
        return Err(configuration_validation_error(
            &current_revision,
            vec![configuration_issue(
                "template",
                "template_count_exceeded",
                format!("At most {MAXIMUM_TEMPLATES} templates are allowed."),
            )],
        ));
    }
    let source = document
        .templates
        .iter()
        .find(|template| template.template_id == request.template_id)
        .cloned()
        .ok_or_else(|| {
            configuration_error(
                proto::ConfigurationErrorCode::TemplateNotFound,
                proto::ErrorCode::NotFound,
                "configuration template was not found",
                &current_revision,
                Vec::new(),
            )
        })?;
    let now_ms = i64::try_from(unix_time_ms()).unwrap_or(i64::MAX);
    let duplicate = StoredTemplate::from_proto(
        proto::ConfigurationTemplate {
            name: request.name,
            ..source.to_proto()
        },
        Uuid::new_v4().simple().to_string(),
        1,
        now_ms,
        now_ms,
    )
    .map_err(|issues| configuration_validation_error(&current_revision, issues))?;
    if document
        .templates
        .iter()
        .any(|template| template.name.eq_ignore_ascii_case(&duplicate.name))
    {
        return Err(configuration_validation_error(
            &current_revision,
            vec![configuration_issue(
                "name",
                "template_name_conflict",
                "Template names must be unique.",
            )],
        ));
    }
    document.templates.push(duplicate.clone());
    document
        .templates
        .sort_unstable_by(|left, right| left.name.cmp(&right.name));
    persist_templates(&config_path, &document)
        .map_err(|error| configuration_internal_error("duplicate template", error))?;
    Ok(duplicate.to_proto())
}

fn delete_template(
    state: &ServerState,
    request: proto::DeleteConfigurationTemplate,
) -> Result<proto::ConfigurationSnapshot, ControlCommandError> {
    let _configuration_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (config_path, _root, mut document, current_revision) = load_revision_state(state)?;
    require_revision(&request.expected_configuration_revision, &current_revision)?;
    let original_len = document.templates.len();
    document
        .templates
        .retain(|template| template.template_id != request.template_id);
    if document.templates.len() == original_len {
        return Err(configuration_error(
            proto::ConfigurationErrorCode::TemplateNotFound,
            proto::ErrorCode::NotFound,
            "configuration template was not found",
            &current_revision,
            Vec::new(),
        ));
    }
    persist_templates(&config_path, &document)
        .map_err(|error| configuration_internal_error("delete template", error))?;
    configuration_snapshot_page(state, proto::GetConfigurationSnapshot::default())
}

fn load_revision_state(
    state: &ServerState,
) -> Result<(PathBuf, toml::Table, StoredTemplateDocument, String), ControlCommandError> {
    let config_path = state.camera_config_path.clone().ok_or_else(|| {
        ControlCommandError::new(
            proto::ErrorCode::Unavailable,
            409,
            "configuration persistence is unavailable",
        )
    })?;
    let root = config::load_configuration_table(&config_path)
        .map_err(|error| configuration_internal_error("load configuration", error))?;
    let templates = load_templates(&config_path)
        .map_err(|error| configuration_internal_error("load templates", error))?;
    let revision = configuration_revision(&root, &templates)
        .map_err(|error| configuration_internal_error("calculate revision", error))?;
    Ok((config_path, root, templates, revision))
}

fn require_revision(expected: &str, current: &str) -> Result<(), ControlCommandError> {
    if expected == current {
        Ok(())
    } else {
        Err(configuration_error(
            proto::ConfigurationErrorCode::Conflict,
            proto::ErrorCode::Rejected,
            "configuration changed after this editor was opened",
            current,
            Vec::new(),
        ))
    }
}

pub(super) fn revision_conflict(
    current_revision: &str,
    message: &'static str,
) -> ControlCommandError {
    configuration_error(
        proto::ConfigurationErrorCode::Conflict,
        proto::ErrorCode::Rejected,
        message,
        current_revision,
        Vec::new(),
    )
}

fn configuration_validation_error(
    current_revision: &str,
    issues: Vec<proto::ConfigurationIssue>,
) -> ControlCommandError {
    configuration_error(
        proto::ConfigurationErrorCode::ValidationFailed,
        proto::ErrorCode::InvalidRequest,
        "configuration validation failed",
        current_revision,
        issues,
    )
}

fn configuration_error(
    code: proto::ConfigurationErrorCode,
    error_code: proto::ErrorCode,
    message: &str,
    current_revision: &str,
    issues: Vec<proto::ConfigurationIssue>,
) -> ControlCommandError {
    ControlCommandError::new(error_code, 409, message).with_detail(prost_types::Any {
        type_url: "type.keeppeek.dev/configuration-error.v1".to_owned(),
        value: proto::ConfigurationError {
            code: code as i32,
            current_configuration_revision: current_revision.to_owned(),
            issues,
        }
        .encode_to_vec(),
    })
}

fn configuration_internal_error(operation: &str, error: anyhow::Error) -> ControlCommandError {
    tracing::warn!(%operation, %error, "configuration command failed");
    ControlCommandError::new(
        proto::ErrorCode::Internal,
        500,
        format!("unable to {operation}"),
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredTemplateValues {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    username_secret_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password_secret_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    onvif_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    http_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backend: Option<CameraBackend>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    transport: Option<CameraTransport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    record_generic_motion_events: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    recording_mode: Option<CameraRecordingMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    event_recording_duration_secs: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredTemplate {
    template_id: String,
    version: u64,
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    description: String,
    values: StoredTemplateValues,
    created_at_ms: i64,
    updated_at_ms: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredTemplateDocument {
    document_version: u32,
    templates: Vec<StoredTemplate>,
}

impl Default for StoredTemplateDocument {
    fn default() -> Self {
        Self {
            document_version: TEMPLATE_DOCUMENT_VERSION,
            templates: Vec::new(),
        }
    }
}

impl StoredTemplate {
    fn from_proto(
        template: proto::ConfigurationTemplate,
        template_id: String,
        version: u64,
        created_at_ms: i64,
        updated_at_ms: i64,
    ) -> Result<Self, Vec<proto::ConfigurationIssue>> {
        let template = validate_template(template)?;
        let values = template.values.expect("validated templates contain values");
        Ok(Self {
            template_id,
            version,
            name: template.name.trim().to_owned(),
            description: template.description.trim().to_owned(),
            values: StoredTemplateValues {
                username_secret_reference: values.username_secret_reference,
                password_secret_reference: values.password_secret_reference,
                onvif_port: values.onvif_port.and_then(|port| u16::try_from(port).ok()),
                http_port: values.http_port.and_then(|port| u16::try_from(port).ok()),
                backend: values.backend.and_then(camera_backend_from_proto),
                transport: values.transport.and_then(camera_transport_from_proto),
                record_generic_motion_events: values.record_generic_motion_events,
                recording_mode: values
                    .recording_mode
                    .and_then(camera_recording_mode_from_proto),
                event_recording_duration_secs: values.event_recording_duration_secs,
            },
            created_at_ms,
            updated_at_ms,
        })
    }

    fn to_proto(&self) -> proto::ConfigurationTemplate {
        proto::ConfigurationTemplate {
            template_id: self.template_id.clone(),
            version: self.version,
            name: self.name.clone(),
            description: self.description.clone(),
            values: Some(proto::CameraTemplateValues {
                username_secret_reference: self.values.username_secret_reference.clone(),
                password_secret_reference: self.values.password_secret_reference.clone(),
                onvif_port: self.values.onvif_port.map(u32::from),
                http_port: self.values.http_port.map(u32::from),
                backend: self.values.backend.map(proto_camera_backend),
                transport: self.values.transport.map(proto_camera_transport),
                record_generic_motion_events: self.values.record_generic_motion_events,
                recording_mode: self.values.recording_mode.map(proto_camera_recording_mode),
                event_recording_duration_secs: self.values.event_recording_duration_secs,
            }),
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
        }
    }
}

fn template_store_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name(LEGACY_TEMPLATE_STORE_FILE)
}

fn load_templates(config_path: &Path) -> anyhow::Result<StoredTemplateDocument> {
    let root = config::load_configuration_table(config_path)?;
    let document = root
        .get(TEMPLATE_CONFIG_SECTION)
        .cloned()
        .map(toml::Value::try_into)
        .transpose()?
        .unwrap_or_default();
    validate_stored_templates(&document)?;
    Ok(document)
}

fn persist_templates(config_path: &Path, document: &StoredTemplateDocument) -> anyhow::Result<()> {
    validate_stored_templates(document)?;
    let encoded = toml::to_string(document)?;
    if encoded.len() > MAXIMUM_TEMPLATE_DOCUMENT_BYTES {
        anyhow::bail!(
            "configuration template document exceeds {MAXIMUM_TEMPLATE_DOCUMENT_BYTES} bytes"
        );
    }
    let mut root = config::load_configuration_table(config_path)?;
    root.insert(
        TEMPLATE_CONFIG_SECTION.to_owned(),
        toml::Value::try_from(document)?,
    );
    config::write_configuration_table(config_path, &root)
}

pub(super) fn migrate_template_store(config_path: &Path) -> anyhow::Result<()> {
    let root = config::load_configuration_table(config_path)?;
    let legacy_path = template_store_path(config_path);
    if root.contains_key(TEMPLATE_CONFIG_SECTION) {
        if legacy_path.exists() {
            std::fs::remove_file(legacy_path)?;
        }
        return Ok(());
    }
    let bytes = match std::fs::read(&legacy_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if bytes.len() > MAXIMUM_TEMPLATE_DOCUMENT_BYTES {
        anyhow::bail!(
            "configuration template document exceeds {MAXIMUM_TEMPLATE_DOCUMENT_BYTES} bytes"
        );
    }
    let document: StoredTemplateDocument = serde_json::from_slice(&bytes)?;
    persist_templates(config_path, &document)?;
    std::fs::remove_file(legacy_path)?;
    Ok(())
}

pub(super) fn validate_configuration(root: &toml::Table) -> anyhow::Result<()> {
    let Some(value) = root.get(TEMPLATE_CONFIG_SECTION) else {
        return Ok(());
    };
    let document: StoredTemplateDocument = value.clone().try_into()?;
    validate_stored_templates(&document)
}

fn validate_stored_templates(document: &StoredTemplateDocument) -> anyhow::Result<()> {
    if document.document_version != TEMPLATE_DOCUMENT_VERSION {
        anyhow::bail!(
            "unsupported configuration template document version {}",
            document.document_version
        );
    }
    if document.templates.len() > MAXIMUM_TEMPLATES {
        anyhow::bail!("configuration template count exceeds {MAXIMUM_TEMPLATES}");
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for template in &document.templates {
        if !valid_template_id(&template.template_id) {
            anyhow::bail!("configuration template ID is invalid");
        }
        if !ids.insert(template.template_id.as_str()) {
            anyhow::bail!("configuration template IDs must be unique");
        }
        if !names.insert(template.name.to_lowercase()) {
            anyhow::bail!("configuration template names must be unique");
        }
        validate_template(template.to_proto()).map_err(|issues| {
            anyhow::anyhow!(
                "configuration template '{}' is invalid: {}",
                template.template_id,
                issues
                    .first()
                    .map_or("unknown validation error", |issue| issue.message.as_str())
            )
        })?;
    }
    Ok(())
}

pub(super) fn validate_backup_template_document(bytes: &[u8]) -> anyhow::Result<()> {
    if bytes.len() > MAXIMUM_TEMPLATE_DOCUMENT_BYTES {
        anyhow::bail!(
            "configuration template document exceeds {MAXIMUM_TEMPLATE_DOCUMENT_BYTES} bytes"
        );
    }
    let document: StoredTemplateDocument = serde_json::from_slice(bytes)?;
    validate_stored_templates(&document)
}

fn valid_template_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn camera_backend_from_proto(value: i32) -> Option<CameraBackend> {
    match proto::CameraBackend::try_from(value) {
        Ok(proto::CameraBackend::Auto) => Some(CameraBackend::Auto),
        Ok(proto::CameraBackend::Retina) => Some(CameraBackend::Retina),
        Ok(proto::CameraBackend::ReoProto) => Some(CameraBackend::ReoProto),
        Ok(proto::CameraBackend::Unspecified) | Err(_) => None,
    }
}

fn camera_transport_from_proto(value: i32) -> Option<CameraTransport> {
    match proto::CameraTransport::try_from(value) {
        Ok(proto::CameraTransport::Tcp) => Some(CameraTransport::Tcp),
        Ok(proto::CameraTransport::Udp) => Some(CameraTransport::Udp),
        Ok(proto::CameraTransport::Unspecified) | Err(_) => None,
    }
}

fn camera_recording_mode_from_proto(value: i32) -> Option<CameraRecordingMode> {
    match proto::CameraRecordingMode::try_from(value) {
        Ok(proto::CameraRecordingMode::Off) => Some(CameraRecordingMode::Off),
        Ok(proto::CameraRecordingMode::Sub) => Some(CameraRecordingMode::Sub),
        Ok(proto::CameraRecordingMode::Main) => Some(CameraRecordingMode::Main),
        Ok(proto::CameraRecordingMode::Both) => Some(CameraRecordingMode::Both),
        Ok(proto::CameraRecordingMode::EventBoost) => Some(CameraRecordingMode::EventBoost),
        Ok(proto::CameraRecordingMode::Unspecified) | Err(_) => None,
    }
}

const fn proto_camera_backend(value: CameraBackend) -> i32 {
    match value {
        CameraBackend::Auto => proto::CameraBackend::Auto as i32,
        CameraBackend::Retina => proto::CameraBackend::Retina as i32,
        CameraBackend::ReoProto => proto::CameraBackend::ReoProto as i32,
    }
}

const fn proto_camera_transport(value: CameraTransport) -> i32 {
    match value {
        CameraTransport::Tcp => proto::CameraTransport::Tcp as i32,
        CameraTransport::Udp => proto::CameraTransport::Udp as i32,
    }
}

const fn proto_camera_recording_mode(value: CameraRecordingMode) -> i32 {
    match value {
        CameraRecordingMode::Off => proto::CameraRecordingMode::Off as i32,
        CameraRecordingMode::Sub => proto::CameraRecordingMode::Sub as i32,
        CameraRecordingMode::Main => proto::CameraRecordingMode::Main as i32,
        CameraRecordingMode::Both => proto::CameraRecordingMode::Both as i32,
        CameraRecordingMode::EventBoost => proto::CameraRecordingMode::EventBoost as i32,
    }
}

fn validate_template(
    template: proto::ConfigurationTemplate,
) -> Result<proto::ConfigurationTemplate, Vec<proto::ConfigurationIssue>> {
    let mut issues = Vec::new();
    let name = template.name.trim();
    if name.is_empty()
        || name.len() > MAXIMUM_TEMPLATE_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        issues.push(configuration_issue(
            "name",
            "template_name_invalid",
            format!(
                "Template names must contain 1 to {MAXIMUM_TEMPLATE_NAME_BYTES} bytes without control characters."
            ),
        ));
    }
    if template.description.len() > MAXIMUM_TEMPLATE_DESCRIPTION_BYTES
        || template.description.chars().any(char::is_control)
    {
        issues.push(configuration_issue(
            "description",
            "template_description_invalid",
            format!(
                "Template descriptions must contain at most {MAXIMUM_TEMPLATE_DESCRIPTION_BYTES} bytes without control characters."
            ),
        ));
    }
    let Some(values) = template.values.as_ref() else {
        issues.push(configuration_issue(
            "values",
            "template_values_missing",
            "A template must contain at least one camera setting.",
        ));
        return Err(issues);
    };
    for (field, reference) in [
        (
            "username_secret_reference",
            values.username_secret_reference.as_deref(),
        ),
        (
            "password_secret_reference",
            values.password_secret_reference.as_deref(),
        ),
    ] {
        if reference.is_some_and(|reference| !config::is_secret_reference(reference)) {
            issues.push(configuration_issue(
                field,
                "secret_reference_required",
                "Templates accept a complete {secret:KEY} reference, not an inline credential.",
            ));
        }
    }
    for (field, port) in [
        ("onvif_port", values.onvif_port),
        ("http_port", values.http_port),
    ] {
        if port.is_some_and(|port| port == 0 || port > u32::from(u16::MAX)) {
            issues.push(configuration_issue(
                field,
                "port_out_of_range",
                "Camera ports must be between 1 and 65535.",
            ));
        }
    }
    if values
        .event_recording_duration_secs
        .is_some_and(|seconds| seconds == 0 || seconds > 3_600)
    {
        issues.push(configuration_issue(
            "event_recording_duration_secs",
            "duration_out_of_range",
            "Event recording duration must be between 1 and 3600 seconds.",
        ));
    }
    validate_template_enum::<proto::CameraBackend>(values.backend, "backend", &mut issues);
    validate_template_enum::<proto::CameraTransport>(values.transport, "transport", &mut issues);
    validate_template_enum::<proto::CameraRecordingMode>(
        values.recording_mode,
        "recording_mode",
        &mut issues,
    );
    if values.username_secret_reference.is_none()
        && values.password_secret_reference.is_none()
        && values.onvif_port.is_none()
        && values.http_port.is_none()
        && values.backend.is_none()
        && values.transport.is_none()
        && values.record_generic_motion_events.is_none()
        && values.recording_mode.is_none()
        && values.event_recording_duration_secs.is_none()
    {
        issues.push(configuration_issue(
            "values",
            "template_values_empty",
            "A template must contain at least one camera setting.",
        ));
    }
    if issues.is_empty() {
        Ok(template)
    } else {
        Err(issues)
    }
}

fn validate_template_enum<T>(
    value: Option<i32>,
    field: &str,
    issues: &mut Vec<proto::ConfigurationIssue>,
) where
    T: TryFrom<i32>,
{
    if value.is_some_and(|value| value == 0 || T::try_from(value).is_err()) {
        issues.push(configuration_issue(
            field,
            "enum_value_invalid",
            format!("{field} contains an unsupported value."),
        ));
    }
}

fn configuration_issue(
    field: impl Into<String>,
    code: impl Into<String>,
    message: impl Into<String>,
) -> proto::ConfigurationIssue {
    proto::ConfigurationIssue {
        camera_id: None,
        field: field.into(),
        severity: proto::ConfigurationIssueSeverity::Error as i32,
        code: code.into(),
        message: message.into(),
        required_capability: None,
    }
}

struct LoadedCameraConfiguration {
    config: CameraConfig,
    group_ids: Vec<String>,
    configured: toml::Table,
}

fn configuration_snapshot(state: &ServerState) -> anyhow::Result<proto::ConfigurationSnapshot> {
    let config_path = state
        .camera_config_path
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("configuration persistence is unavailable"))?;
    let root = config::load_configuration_table(config_path)?;
    let defaults = config::load_camera_defaults(config_path)?;
    let templates = load_templates(config_path)?;
    let cameras = loaded_camera_configurations(config_path, &root)?;
    let revision = configuration_revision(&root, &templates)?;
    let raw_defaults = root.get("camera_defaults").and_then(toml::Value::as_table);
    let defaults_proto = proto::CameraDefaultValues {
        username_configured: raw_defaults
            .and_then(|table| table.get("username"))
            .and_then(toml::Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        password_configured: raw_defaults
            .and_then(|table| table.get("password"))
            .and_then(toml::Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        configured_backend: defaults.backend.map(proto_camera_backend),
        effective_backend: proto_camera_backend(defaults.backend.unwrap_or_default()),
        configured_transport: defaults.transport.map(proto_camera_transport),
        effective_transport: proto_camera_transport(defaults.transport.unwrap_or_default()),
        configured_record_generic_motion_events: defaults.record_generic_motion_events,
        effective_record_generic_motion_events: defaults
            .record_generic_motion_events
            .unwrap_or_default(),
        configured_recording_mode: defaults.recording_mode.map(proto_camera_recording_mode),
        effective_recording_mode: proto_camera_recording_mode(
            defaults.recording_mode.unwrap_or_default(),
        ),
        configured_event_recording_duration_secs: defaults
            .event_recording_duration_secs
            .and_then(|seconds| u32::try_from(seconds).ok()),
        effective_event_recording_duration_secs: u32::try_from(
            defaults
                .event_recording_duration_secs
                .unwrap_or_else(crate::cameras::default_event_recording_duration_secs),
        )
        .unwrap_or(u32::MAX),
    };
    let cameras = cameras
        .into_iter()
        .map(|camera| proto_effective_camera(state, config_path, &defaults, raw_defaults, camera))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let total_camera_count = u32::try_from(cameras.len()).unwrap_or(u32::MAX);
    Ok(proto::ConfigurationSnapshot {
        contract_version: 1,
        configuration_revision: revision,
        defaults: Some(defaults_proto),
        cameras,
        templates: templates
            .templates
            .iter()
            .map(StoredTemplate::to_proto)
            .collect(),
        limits: Some(proto::ConfigurationLimits {
            maximum_templates: u32::try_from(MAXIMUM_TEMPLATES).unwrap_or(u32::MAX),
            maximum_template_name_bytes: u32::try_from(MAXIMUM_TEMPLATE_NAME_BYTES)
                .unwrap_or(u32::MAX),
            maximum_template_description_bytes: u32::try_from(MAXIMUM_TEMPLATE_DESCRIPTION_BYTES)
                .unwrap_or(u32::MAX),
            maximum_plan_targets: u32::try_from(MAXIMUM_PLAN_TARGETS).unwrap_or(u32::MAX),
            maximum_import_bytes: u32::try_from(MAXIMUM_TEMPLATE_DOCUMENT_BYTES)
                .unwrap_or(u32::MAX),
        }),
        domains: configuration_domains(state),
        total_camera_count,
        next_page_token: String::new(),
    })
}

fn locked_configuration_snapshot_page(
    state: &ServerState,
    request: proto::GetConfigurationSnapshot,
) -> Result<proto::ConfigurationSnapshot, ControlCommandError> {
    let _configuration_update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    configuration_snapshot_page(state, request)
}

fn configuration_snapshot_page(
    state: &ServerState,
    request: proto::GetConfigurationSnapshot,
) -> Result<proto::ConfigurationSnapshot, ControlCommandError> {
    let snapshot = configuration_snapshot(state)
        .map_err(|error| configuration_internal_error("load snapshot", error))?;
    paginate_configuration_snapshot(snapshot, request)
}

fn paginate_configuration_snapshot(
    snapshot: proto::ConfigurationSnapshot,
    request: proto::GetConfigurationSnapshot,
) -> Result<proto::ConfigurationSnapshot, ControlCommandError> {
    let total = snapshot.cameras.len();
    let offset = if request.page_token.is_empty() {
        0
    } else {
        let Some((revision, offset)) = request.page_token.rsplit_once(':') else {
            return Err(configuration_validation_error(
                &snapshot.configuration_revision,
                vec![configuration_issue(
                    "page_token",
                    "snapshot_page_token_invalid",
                    "The configuration snapshot page token is invalid.",
                )],
            ));
        };
        if revision != snapshot.configuration_revision {
            return Err(revision_conflict(
                &snapshot.configuration_revision,
                "configuration changed while snapshot pages were loading; reload the snapshot",
            ));
        }
        offset
            .parse::<usize>()
            .ok()
            .filter(|offset| *offset <= total)
            .ok_or_else(|| {
                configuration_validation_error(
                    &snapshot.configuration_revision,
                    vec![configuration_issue(
                        "page_token",
                        "snapshot_page_token_invalid",
                        "The configuration snapshot page token is invalid.",
                    )],
                )
            })?
    };
    let requested = request
        .page_size
        .and_then(|size| usize::try_from(size).ok())
        .unwrap_or(DEFAULT_SNAPSHOT_PAGE_SIZE)
        .clamp(1, MAXIMUM_SNAPSHOT_PAGE_SIZE);
    let mut count = requested.min(total.saturating_sub(offset));
    loop {
        let end = offset.saturating_add(count).min(total);
        let mut page = snapshot.clone();
        page.cameras = snapshot.cameras[offset..end].to_vec();
        if offset > 0 {
            page.templates.clear();
            page.domains.clear();
        }
        page.total_camera_count = u32::try_from(total).unwrap_or(u32::MAX);
        page.next_page_token = if end < total {
            format!("{}:{end}", snapshot.configuration_revision)
        } else {
            String::new()
        };
        if configuration_response_bytes(proto::configuration_result::Result::Snapshot(page.clone()))
            <= MAXIMUM_CONFIGURATION_RESPONSE_BYTES
        {
            return Ok(page);
        }
        if count <= 1 {
            return Err(configuration_validation_error(
                &snapshot.configuration_revision,
                vec![configuration_issue(
                    "snapshot",
                    "configuration_snapshot_too_large",
                    "One configuration snapshot page exceeds the control-message limit.",
                )],
            ));
        }
        count = count.div_ceil(2);
    }
}

fn loaded_camera_configurations(
    config_path: &Path,
    root: &toml::Table,
) -> anyhow::Result<Vec<LoadedCameraConfiguration>> {
    let grouped = config::cameras_from_configuration_table(config_path, root)?;
    let mut by_ip = HashMap::<IpAddr, LoadedCameraConfiguration>::new();
    for (group_id, cameras) in grouped {
        for camera in cameras {
            let name = camera
                .name
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("camera configuration key is missing"))?;
            let configured = root
                .get(&group_id)
                .and_then(toml::Value::as_table)
                .and_then(|group| group.get(name))
                .and_then(toml::Value::as_table)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("camera {name} is not a configuration table"))?;
            match by_ip.entry(camera.ip) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    let existing = entry.get_mut();
                    if !equivalent_camera_configurations(&existing.config, &camera)? {
                        anyhow::bail!(
                            "camera {} appears more than once in configuration",
                            camera.ip
                        );
                    }
                    existing.group_ids.push(group_id.clone());
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(LoadedCameraConfiguration {
                        config: camera,
                        group_ids: vec![group_id.clone()],
                        configured,
                    });
                }
            }
        }
    }
    let mut cameras = by_ip.into_values().collect::<Vec<_>>();
    for camera in &mut cameras {
        camera.group_ids.sort_unstable();
        camera.group_ids.dedup();
    }
    cameras.sort_unstable_by_key(|camera| camera.config.ip);
    Ok(cameras)
}

fn equivalent_camera_configurations(
    left: &CameraConfig,
    right: &CameraConfig,
) -> anyhow::Result<bool> {
    let mut left = left.clone();
    let mut right = right.clone();
    left.name = None;
    right.name = None;
    Ok(toml::Value::try_from(left)? == toml::Value::try_from(right)?)
}

fn proto_effective_camera(
    state: &ServerState,
    config_path: &Path,
    defaults: &config::CameraCredentialDefaults,
    raw_defaults: Option<&toml::Table>,
    camera: LoadedCameraConfiguration,
) -> anyhow::Result<proto::CameraEffectiveConfiguration> {
    let camera_id = camera.config.ip.to_string();
    let live = state.camera(&camera_id).map(|entry| entry.configuration);
    let model = state.camera(&camera_id).and_then(|entry| entry.info.model);
    let username_override = configured_string(&camera.configured, "username");
    let password_override = configured_string(&camera.configured, "password");
    let username_default = raw_defaults.and_then(|table| configured_string(table, "username"));
    let password_default = raw_defaults.and_then(|table| configured_string(table, "password"));
    let backend_override = configured_value::<CameraBackend>(&camera.configured, "backend");
    let transport_override = configured_value::<CameraTransport>(&camera.configured, "transport");
    let generic_motion_override =
        configured_value::<bool>(&camera.configured, "record_generic_motion_events");
    let recording_mode_override =
        configured_value::<CameraRecordingMode>(&camera.configured, "recording_mode");
    let event_duration_override =
        configured_value::<u64>(&camera.configured, "event_recording_duration_secs");
    let camera_proto = proto::CameraSettings {
        id: camera_id.clone(),
        ip: camera_id,
        display_name: camera.config.display_name.clone(),
        manufacturer_override: camera.config.manufacturer_override().map(str::to_owned),
        username_configured: !camera.config.username.is_empty(),
        password_configured: !camera.config.password.is_empty(),
        onvif_port: camera.config.onvif_port.map(u32::from),
        http_port: camera.config.http_port.map(u32::from),
        main_rtsp_url: camera.config.main_rtsp_url.as_deref().map(|resolved| {
            config::camera_reference_or_value(
                config_path,
                camera.config.ip,
                "main_rtsp_url",
                resolved,
            )
            .unwrap_or_else(|_| resolved.to_owned())
        }),
        sub_rtsp_url: camera.config.sub_rtsp_url.as_deref().map(|resolved| {
            config::camera_reference_or_value(
                config_path,
                camera.config.ip,
                "sub_rtsp_url",
                resolved,
            )
            .unwrap_or_else(|_| resolved.to_owned())
        }),
        uid_configured: camera.config.uid.is_some(),
        backend: proto_camera_backend(camera.config.backend),
        transport: proto_camera_transport(camera.config.transport),
        health: None,
        model,
        record_generic_motion_events: camera.config.record_generic_motion_events,
        recording_mode: proto_camera_recording_mode(camera.config.recording_mode),
        event_recording_duration_secs: u32::try_from(camera.config.event_recording_duration_secs)
            .unwrap_or(u32::MAX),
    };
    Ok(proto::CameraEffectiveConfiguration {
        camera: Some(camera_proto),
        group_ids: camera.group_ids,
        username: Some(proto_secret_value(
            username_default,
            username_override,
            !camera.config.username.is_empty(),
            live.as_ref()
                .is_some_and(|live| live.username == camera.config.username),
        )),
        password: Some(proto_secret_value(
            password_default,
            password_override,
            !camera.config.password.is_empty(),
            live.as_ref()
                .is_some_and(|live| live.password == camera.config.password),
        )),
        backend: Some(proto::EffectiveCameraBackendValue {
            configured_default: defaults.backend.map(proto_camera_backend),
            camera_override: backend_override.map(proto_camera_backend),
            effective: proto_camera_backend(camera.config.backend),
            source: configured_source(backend_override.is_some(), defaults.backend.is_some()),
            runtime_applied: live
                .as_ref()
                .is_some_and(|live| live.backend == camera.config.backend),
            warning: runtime_warning(
                live.as_ref()
                    .is_some_and(|live| live.backend == camera.config.backend),
            ),
        }),
        transport: Some(proto::EffectiveCameraTransportValue {
            configured_default: defaults.transport.map(proto_camera_transport),
            camera_override: transport_override.map(proto_camera_transport),
            effective: proto_camera_transport(camera.config.transport),
            source: configured_source(transport_override.is_some(), defaults.transport.is_some()),
            runtime_applied: live
                .as_ref()
                .is_some_and(|live| live.transport == camera.config.transport),
            warning: runtime_warning(
                live.as_ref()
                    .is_some_and(|live| live.transport == camera.config.transport),
            ),
        }),
        record_generic_motion_events: Some(proto::EffectiveBoolValue {
            configured_default: defaults.record_generic_motion_events,
            camera_override: generic_motion_override,
            effective: camera.config.record_generic_motion_events,
            source: configured_source(
                generic_motion_override.is_some(),
                defaults.record_generic_motion_events.is_some(),
            ),
            runtime_applied: live.as_ref().is_some_and(|live| {
                live.record_generic_motion_events == camera.config.record_generic_motion_events
            }),
            warning: runtime_warning(live.as_ref().is_some_and(|live| {
                live.record_generic_motion_events == camera.config.record_generic_motion_events
            })),
        }),
        recording_mode: Some(proto::EffectiveCameraRecordingModeValue {
            configured_default: defaults.recording_mode.map(proto_camera_recording_mode),
            camera_override: recording_mode_override.map(proto_camera_recording_mode),
            effective: proto_camera_recording_mode(camera.config.recording_mode),
            source: configured_source(
                recording_mode_override.is_some(),
                defaults.recording_mode.is_some(),
            ),
            runtime_applied: live
                .as_ref()
                .is_some_and(|live| live.recording_mode == camera.config.recording_mode),
            warning: runtime_warning(
                live.as_ref()
                    .is_some_and(|live| live.recording_mode == camera.config.recording_mode),
            ),
        }),
        event_recording_duration_secs: Some(proto::EffectiveUint32Value {
            configured_default: defaults
                .event_recording_duration_secs
                .and_then(|value| u32::try_from(value).ok()),
            camera_override: event_duration_override.and_then(|value| u32::try_from(value).ok()),
            effective: u32::try_from(camera.config.event_recording_duration_secs)
                .unwrap_or(u32::MAX),
            source: configured_source(
                event_duration_override.is_some(),
                defaults.event_recording_duration_secs.is_some(),
            ),
            runtime_applied: live.as_ref().is_some_and(|live| {
                live.event_recording_duration_secs == camera.config.event_recording_duration_secs
            }),
            warning: runtime_warning(live.as_ref().is_some_and(|live| {
                live.event_recording_duration_secs == camera.config.event_recording_duration_secs
            })),
        }),
    })
}

fn configured_string(table: &toml::Table, key: &str) -> Option<String> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn configured_value<T>(table: &toml::Table, key: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    table
        .get(key)
        .cloned()
        .and_then(|value| value.try_into().ok())
}

fn proto_secret_value(
    configured_default: Option<String>,
    camera_override: Option<String>,
    effective_configured: bool,
    runtime_applied: bool,
) -> proto::EffectiveSecretValue {
    proto::EffectiveSecretValue {
        default_configured: configured_default.is_some(),
        override_configured: camera_override.is_some(),
        effective_configured,
        source: configured_source(camera_override.is_some(), configured_default.is_some()),
        runtime_applied,
        warning: runtime_warning(runtime_applied),
    }
}

const fn configured_source(has_override: bool, has_default: bool) -> i32 {
    if has_override {
        proto::ConfigurationValueSource::Override as i32
    } else if has_default {
        proto::ConfigurationValueSource::Default as i32
    } else {
        proto::ConfigurationValueSource::BuiltIn as i32
    }
}

fn runtime_warning(runtime_applied: bool) -> Option<String> {
    (!runtime_applied).then(|| "The persisted value is not currently applied.".to_owned())
}

fn configuration_revision(
    root: &toml::Table,
    templates: &StoredTemplateDocument,
) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(toml::to_string(root)?.as_bytes());
    hasher.update([0]);
    hasher.update(serde_json::to_vec(templates)?);
    let digest = hasher.finalize();
    let mut revision = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        write!(&mut revision, "{byte:02x}")?;
    }
    Ok(revision)
}

fn configuration_domains(state: &ServerState) -> Vec<proto::ConfigurationDomain> {
    [
        (
            "cameras",
            "Camera defaults and fleet",
            "/cameras",
            CONFIGURATION_CAPABILITY_ID,
            true,
        ),
        (
            "storage",
            "Storage and retention",
            "/settings#storage",
            "keeppeek.runtime-config.v1",
            true,
        ),
        (
            "groups",
            "Groups",
            "/groups",
            "keeppeek.group-admin.v1",
            false,
        ),
        (
            "layouts",
            "Dashboard layouts",
            "/settings#dashboards",
            peek_layouts::CAPABILITY_ID,
            true,
        ),
        (
            "events",
            "Event sources",
            "/settings#events",
            "keeppeek.runtime-config.v1",
            true,
        ),
        (
            "integrations",
            "Integrations",
            "/settings#integrations",
            "keeppeek.mqtt-forwarder.v1",
            state.event_forwarder.is_some(),
        ),
        (
            "notifications",
            "Notifications",
            "/settings#notifications",
            "keeppeek.rules.v1",
            state.notifications.is_some(),
        ),
        (
            "access",
            "Access",
            "/settings#access",
            "keeppeek.identity.v1",
            true,
        ),
        (
            "logging",
            "Logging",
            "/settings#logs",
            "keeppeek.runtime-config.v1",
            true,
        ),
        (
            "appearance",
            "Appearance",
            "/settings#appearance",
            "keeppeek.device-preferences.v1",
            false,
        ),
    ]
    .into_iter()
    .map(
        |(domain_id, label, owner_path, capability_id, mutable)| proto::ConfigurationDomain {
            domain_id: domain_id.to_owned(),
            label: label.to_owned(),
            owner_path: owner_path.to_owned(),
            capability_id: capability_id.to_owned(),
            readable: true,
            mutable,
            unavailable_reason: (!mutable)
                .then(|| format!("Server update required · {capability_id}")),
        },
    )
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTROL_MESSAGE_BUDGET_BYTES: usize = 64 * 1_024;

    fn template(username: &str, name: &str) -> proto::ConfigurationTemplate {
        proto::ConfigurationTemplate {
            template_id: String::new(),
            version: 0,
            name: name.to_owned(),
            description: "Outdoor camera recording policy".to_owned(),
            values: Some(proto::CameraTemplateValues {
                username_secret_reference: Some(username.to_owned()),
                backend: Some(proto::CameraBackend::ReoProto as i32),
                recording_mode: Some(proto::CameraRecordingMode::EventBoost as i32),
                event_recording_duration_secs: Some(90),
                ..Default::default()
            }),
            created_at_ms: 0,
            updated_at_ms: 0,
        }
    }

    #[test]
    fn template_validation_bounds_metadata_and_rejects_inline_secrets() {
        let valid = validate_template(template("{secret:OUTDOOR_USERNAME}", "Outdoor"));
        assert!(valid.is_ok());

        let inline = validate_template(template("admin", "Outdoor")).unwrap_err();
        assert_eq!(inline[0].field, "username_secret_reference");

        let overlong = validate_template(template(
            "{secret:OUTDOOR_USERNAME}",
            &"x".repeat(MAXIMUM_TEMPLATE_NAME_BYTES + 1),
        ))
        .unwrap_err();
        assert_eq!(overlong[0].field, "name");
    }

    #[test]
    fn template_store_round_trips_versioned_human_readable_documents() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-templates-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "host = \"127.0.0.1\"\n").unwrap();
        std::fs::write(
            directory.join("secrets.toml"),
            "OUTDOOR_USERNAME = \"operator\"\n",
        )
        .unwrap();
        let stored = StoredTemplate::from_proto(
            validate_template(template("{secret:OUTDOOR_USERNAME}", "Outdoor")).unwrap(),
            "outdoor".to_owned(),
            3,
            100,
            200,
        )
        .unwrap();

        persist_templates(
            &config_path,
            &StoredTemplateDocument {
                document_version: 1,
                templates: vec![stored],
            },
        )
        .unwrap();
        let loaded = load_templates(&config_path).unwrap();
        assert_eq!(loaded.document_version, 1);
        assert_eq!(loaded.templates[0].version, 3);
        assert_eq!(
            loaded.templates[0].values.backend,
            Some(CameraBackend::ReoProto)
        );

        let serialized = std::fs::read_to_string(&config_path).unwrap();
        assert!(serialized.contains("configuration_templates"));
        assert!(serialized.contains("\"reo-proto\""));
        assert!(serialized.contains("{secret:OUTDOOR_USERNAME}"));
        assert!(!serialized.contains("resolved-password"));
        assert!(!directory.join("configuration-templates.json").exists());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn snapshot_distinguishes_defaults_overrides_effective_values_and_runtime_state() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-snapshot-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(
            &config_path,
            r#"
                [camera_defaults]
                username = "operator"
                password = "password"
                backend = "reo-proto"
                transport = "udp"
                recording_mode = "main"
                event_recording_duration_secs = 90

                [exterior.front]
                ip = "192.0.2.10"
                transport = "tcp"
            "#,
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path);

        let snapshot = configuration_snapshot(&state).unwrap();

        assert_eq!(snapshot.contract_version, 1);
        assert!(!snapshot.configuration_revision.is_empty());
        let defaults = snapshot.defaults.unwrap();
        assert_eq!(
            defaults.configured_backend,
            Some(proto::CameraBackend::ReoProto as i32)
        );
        let camera = &snapshot.cameras[0];
        assert_eq!(camera.group_ids, ["exterior"]);
        let backend = camera.backend.as_ref().unwrap();
        assert_eq!(backend.camera_override, None);
        assert_eq!(backend.effective, proto::CameraBackend::ReoProto as i32);
        assert_eq!(
            backend.source,
            proto::ConfigurationValueSource::Default as i32
        );
        assert!(!backend.runtime_applied);
        let transport = camera.transport.as_ref().unwrap();
        assert_eq!(
            transport.camera_override,
            Some(proto::CameraTransport::Tcp as i32)
        );
        assert_eq!(
            transport.source,
            proto::ConfigurationValueSource::Override as i32
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn template_create_is_versioned_and_stale_writes_return_current_revision() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-template-create-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "host = \"127.0.0.1\"\n").unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let initial_revision = configuration_snapshot(&state)
            .unwrap()
            .configuration_revision;
        let create = proto::ConfigurationCommand {
            action: Some(proto::configuration_command::Action::SaveTemplate(
                proto::SaveConfigurationTemplate {
                    expected_configuration_revision: initial_revision.clone(),
                    template: Some(template("{secret:OUTDOOR_USERNAME}", "Outdoor")),
                    expected_template_version: None,
                },
            )),
        };

        let result = dispatch(&state, create.clone()).unwrap();
        let control_ok::Result::ConfigurationResult(result) = result else {
            panic!("expected a configuration result");
        };
        let Some(proto::configuration_result::Result::Template(created)) = result.result else {
            panic!("expected a created template");
        };
        assert!(!created.template_id.is_empty());
        assert_eq!(created.version, 1);

        let conflict = dispatch(&state, create).unwrap_err();
        assert_eq!(conflict.code, proto::ErrorCode::Rejected);
        let detail =
            proto::ConfigurationError::decode(conflict.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.code, proto::ConfigurationErrorCode::Conflict as i32);
        assert_ne!(detail.current_configuration_revision, initial_revision);
        assert_eq!(load_templates(&config_path).unwrap().templates.len(), 1);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn plan_resolves_authoritative_group_targets_without_writing_configuration() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-plan-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(
            &config_path,
            r#"
                [camera_defaults]
                username = "operator"
                password = "password"

                [exterior.front]
                ip = "192.0.2.10"

                [interior.hall]
                ip = "192.0.2.11"
            "#,
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let revision = configuration_snapshot(&state)
            .unwrap()
            .configuration_revision;
        let original = std::fs::read_to_string(&config_path).unwrap();

        let result = dispatch(
            &state,
            proto::ConfigurationCommand {
                action: Some(proto::configuration_command::Action::Plan(
                    proto::PlanConfigurationChange {
                        expected_configuration_revision: revision,
                        targets: Some(proto::ConfigurationTargetSelector {
                            selection: Some(
                                proto::configuration_target_selector::Selection::GroupId(
                                    "exterior".to_owned(),
                                ),
                            ),
                        }),
                        change: Some(proto::ConfigurationChange {
                            change: Some(proto::configuration_change::Change::Patch(
                                proto::CameraConfigurationPatch {
                                    backend: Some(proto::OptionalCameraBackendUpdate {
                                        value: Some(
                                            proto::optional_camera_backend_update::Value::Set(
                                                proto::CameraBackend::Retina as i32,
                                            ),
                                        ),
                                    }),
                                    ..Default::default()
                                },
                            )),
                        }),
                    },
                )),
            },
        )
        .unwrap();
        let control_ok::Result::ConfigurationResult(result) = result else {
            panic!("expected a configuration result");
        };
        let Some(proto::configuration_result::Result::Plan(plan)) = result.result else {
            panic!("expected a configuration plan");
        };
        assert!(plan.valid);
        assert!(!plan.plan_id.is_empty());
        assert!(plan.expires_at_ms > i64::try_from(unix_time_ms()).unwrap());
        assert_eq!(plan.authoritative_target_count, 1);
        assert_eq!(plan.targets[0].camera_id, "192.0.2.10");
        assert_eq!(plan.changes[0].field, "backend");
        assert_eq!(plan.changes[0].old_effective_value, "auto");
        assert_eq!(plan.changes[0].new_effective_value, "retina");
        assert_eq!(
            plan.impact,
            proto::ConfigurationImpact::ReconnectCamera as i32
        );
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn apply_commits_atomic_candidate_and_preserves_unknown_and_secret_values() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-apply-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        config::write_private_file(
            &config_path,
            br#"
                [camera_defaults]
                username = "{secret:CAMERA_USERNAME}"
                password = "{secret:CAMERA_PASSWORD}"

                [cameras.front]
                ip = "192.0.2.10"
                future_setting = "keep-me"
            "#,
        )
        .unwrap();
        config::write_private_file(
            &config_path.with_file_name("secrets.toml"),
            b"CAMERA_USERNAME = \"operator\"\nCAMERA_PASSWORD = \"password\"\n",
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let revision = configuration_snapshot(&state)
            .unwrap()
            .configuration_revision;
        let planned = plan_configuration_change(
            &state,
            proto::PlanConfigurationChange {
                expected_configuration_revision: revision.clone(),
                targets: Some(proto::ConfigurationTargetSelector {
                    selection: Some(proto::configuration_target_selector::Selection::CameraIds(
                        proto::CameraIdList {
                            camera_ids: vec!["192.0.2.10".to_owned()],
                        },
                    )),
                }),
                change: Some(proto::ConfigurationChange {
                    change: Some(proto::configuration_change::Change::Patch(
                        proto::CameraConfigurationPatch {
                            backend: Some(proto::OptionalCameraBackendUpdate {
                                value: Some(proto::optional_camera_backend_update::Value::Set(
                                    proto::CameraBackend::Retina as i32,
                                )),
                            }),
                            ..Default::default()
                        },
                    )),
                }),
            },
        )
        .unwrap();

        let applied = apply_configuration_plan(
            &state,
            proto::ApplyConfigurationPlan {
                plan_id: planned.plan_id,
                expected_configuration_revision: revision,
            },
        )
        .unwrap();

        assert!(applied.configuration_committed);
        assert_eq!(applied.activations.len(), 1);
        assert_eq!(
            applied.activations[0].status,
            proto::ConfigurationActivationStatus::RestartRequired as i32
        );
        let root = config::load_configuration_table(&config_path).unwrap();
        let camera = root["cameras"]["front"].as_table().unwrap();
        assert_eq!(camera["backend"].as_str(), Some("retina"));
        assert_eq!(camera["future_setting"].as_str(), Some("keep-me"));
        let defaults = root["camera_defaults"].as_table().unwrap();
        assert_eq!(
            defaults["username"].as_str(),
            Some("{secret:CAMERA_USERNAME}")
        );
        assert_eq!(
            defaults["password"].as_str(),
            Some("{secret:CAMERA_PASSWORD}")
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn apply_reports_failed_worker_activation_with_restart_recovery() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-activation-failure-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        config::write_private_file(
            &config_path,
            br#"
                [camera_defaults]
                username = "operator"
                password = "password"

                [cameras.front]
                ip = "192.0.2.10"
                main_rtsp_url = "rtsp://192.0.2.10/main"
            "#,
        )
        .unwrap();
        let loop_ = crate::keeppeek::KeepPeekLoop::new(Shutdown::new(), None);
        let stopped_runtime = loop_.control();
        drop(loop_);
        let state = ServerState::empty()
            .with_camera_config_path(config_path.clone())
            .with_camera_runtime(stopped_runtime);
        let revision = current_configuration_revision(&state).unwrap();
        let plan = plan_configuration_change(
            &state,
            proto::PlanConfigurationChange {
                expected_configuration_revision: revision.clone(),
                targets: Some(proto::ConfigurationTargetSelector {
                    selection: Some(proto::configuration_target_selector::Selection::CameraIds(
                        proto::CameraIdList {
                            camera_ids: vec!["192.0.2.10".to_owned()],
                        },
                    )),
                }),
                change: Some(proto::ConfigurationChange {
                    change: Some(proto::configuration_change::Change::Patch(
                        proto::CameraConfigurationPatch {
                            recording_mode: Some(proto::OptionalCameraRecordingModeUpdate {
                                value: Some(
                                    proto::optional_camera_recording_mode_update::Value::Set(
                                        proto::CameraRecordingMode::Main as i32,
                                    ),
                                ),
                            }),
                            ..Default::default()
                        },
                    )),
                }),
            },
        )
        .unwrap();

        let applied = apply_configuration_plan(
            &state,
            proto::ApplyConfigurationPlan {
                plan_id: plan.plan_id,
                expected_configuration_revision: revision,
            },
        )
        .unwrap();

        assert!(applied.configuration_committed);
        assert_eq!(
            applied.activations[0].status,
            proto::ConfigurationActivationStatus::Failed as i32
        );
        assert!(
            applied.activations[0]
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("restart the server to recover"))
        );
        let saved = config::load_configuration_table(&config_path).unwrap();
        assert_eq!(
            saved["cameras"]["front"]["recording_mode"].as_str(),
            Some("main")
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn template_export_and_previewed_import_round_trip_secret_references() {
        let source_directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-export-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&source_directory).unwrap();
        let source_config = source_directory.join("config.toml");
        std::fs::write(&source_config, "host = \"127.0.0.1\"\n").unwrap();
        persist_templates(
            &source_config,
            &StoredTemplateDocument {
                document_version: 1,
                templates: vec![
                    StoredTemplate::from_proto(
                        validate_template(template("{secret:OUTDOOR_USERNAME}", "Outdoor"))
                            .unwrap(),
                        "outdoor".to_owned(),
                        1,
                        100,
                        100,
                    )
                    .unwrap(),
                ],
            },
        )
        .unwrap();
        let source_state = ServerState::empty().with_camera_config_path(source_config);
        let exported = export_templates(
            &source_state,
            proto::ExportConfigurationTemplates {
                template_ids: Vec::new(),
            },
        )
        .unwrap();
        assert!(exported.document_json.contains("{secret:OUTDOOR_USERNAME}"));
        assert!(!exported.document_json.contains("resolved-secret"));

        let target_directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-import-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&target_directory).unwrap();
        let target_config = target_directory.join("config.toml");
        std::fs::write(&target_config, "host = \"127.0.0.1\"\n").unwrap();
        let target_state = ServerState::empty().with_camera_config_path(target_config.clone());
        let revision = configuration_snapshot(&target_state)
            .unwrap()
            .configuration_revision;
        let preview = preview_template_import(
            &target_state,
            proto::PreviewConfigurationTemplateImport {
                expected_configuration_revision: revision.clone(),
                document_json: exported.document_json,
            },
        )
        .unwrap();
        assert!(preview.valid);
        assert_eq!(load_templates(&target_config).unwrap().templates.len(), 0);

        apply_template_import(
            &target_state,
            proto::ApplyConfigurationTemplateImport {
                preview_id: preview.preview_id,
                expected_configuration_revision: revision,
            },
        )
        .unwrap();
        let imported = load_templates(&target_config).unwrap();
        assert_eq!(imported.templates.len(), 1);
        assert_eq!(
            imported.templates[0]
                .values
                .username_secret_reference
                .as_deref(),
            Some("{secret:OUTDOOR_USERNAME}")
        );

        std::fs::remove_dir_all(source_directory).unwrap();
        std::fs::remove_dir_all(target_directory).unwrap();
    }

    #[test]
    fn template_import_rejects_unknown_fields_without_mutation() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-import-unknown-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "host = \"127.0.0.1\"\n").unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let revision = current_configuration_revision(&state).unwrap();

        let preview = preview_template_import(
            &state,
            proto::PreviewConfigurationTemplateImport {
                expected_configuration_revision: revision,
                document_json: r#"{
                    "document_version": 1,
                    "templates": [{
                        "template_id": "future",
                        "version": 1,
                        "name": "Future",
                        "values": { "backend": "auto", "future_field": true },
                        "created_at_ms": 1,
                        "updated_at_ms": 1
                    }]
                }"#
                .to_owned(),
            },
        )
        .unwrap();

        assert!(!preview.valid);
        assert!(
            preview
                .issues
                .iter()
                .any(|issue| issue.code == "import_document_invalid")
        );
        assert!(load_templates(&config_path).unwrap().templates.is_empty());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stale_apply_preserves_current_state_and_fresh_clear_restores_inheritance() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-conflict-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        config::write_private_file(
            &config_path,
            br#"
                [camera_defaults]
                username = "operator"
                password = "password"
                backend = "reo-proto"

                [cameras.front]
                ip = "192.0.2.10"
                backend = "retina"
            "#,
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let original_revision = current_configuration_revision(&state).unwrap();
        let plan_request = |revision: String| proto::PlanConfigurationChange {
            expected_configuration_revision: revision,
            targets: Some(proto::ConfigurationTargetSelector {
                selection: Some(proto::configuration_target_selector::Selection::CameraIds(
                    proto::CameraIdList {
                        camera_ids: vec!["192.0.2.10".to_owned()],
                    },
                )),
            }),
            change: Some(proto::ConfigurationChange {
                change: Some(proto::configuration_change::Change::Patch(
                    proto::CameraConfigurationPatch {
                        backend: Some(proto::OptionalCameraBackendUpdate {
                            value: Some(proto::optional_camera_backend_update::Value::Clear(true)),
                        }),
                        ..Default::default()
                    },
                )),
            }),
        };
        let stale_plan =
            plan_configuration_change(&state, plan_request(original_revision.clone())).unwrap();
        let mut externally_updated = config::load_configuration_table(&config_path).unwrap();
        externally_updated.insert(
            "future_server_setting".to_owned(),
            toml::Value::String("keep-current".to_owned()),
        );
        config::write_configuration_table(&config_path, &externally_updated).unwrap();

        let conflict = apply_configuration_plan(
            &state,
            proto::ApplyConfigurationPlan {
                plan_id: stale_plan.plan_id,
                expected_configuration_revision: original_revision,
            },
        )
        .unwrap_err();
        let detail =
            proto::ConfigurationError::decode(conflict.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.code, proto::ConfigurationErrorCode::Conflict as i32);
        let current = config::load_configuration_table(&config_path).unwrap();
        assert_eq!(
            current["future_server_setting"].as_str(),
            Some("keep-current")
        );
        assert_eq!(
            current["cameras"]["front"]["backend"].as_str(),
            Some("retina")
        );

        let current_revision = current_configuration_revision(&state).unwrap();
        let fresh_plan =
            plan_configuration_change(&state, plan_request(current_revision.clone())).unwrap();
        apply_configuration_plan(
            &state,
            proto::ApplyConfigurationPlan {
                plan_id: fresh_plan.plan_id,
                expected_configuration_revision: current_revision,
            },
        )
        .unwrap();
        let saved = config::load_configuration_table(&config_path).unwrap();
        assert!(
            !saved["cameras"]["front"]
                .as_table()
                .unwrap()
                .contains_key("backend")
        );
        let snapshot = configuration_snapshot(&state).unwrap();
        let backend = snapshot.cameras[0].backend.as_ref().unwrap();
        assert_eq!(backend.camera_override, None);
        assert_eq!(backend.effective, proto::CameraBackend::ReoProto as i32);
        assert_eq!(
            backend.source,
            proto::ConfigurationValueSource::Default as i32
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn missing_explicit_target_is_skipped_and_cannot_be_applied() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-missing-target-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        std::fs::write(&config_path, "host = \"127.0.0.1\"\n").unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path);
        let revision = current_configuration_revision(&state).unwrap();

        let plan = plan_configuration_change(
            &state,
            proto::PlanConfigurationChange {
                expected_configuration_revision: revision.clone(),
                targets: Some(proto::ConfigurationTargetSelector {
                    selection: Some(proto::configuration_target_selector::Selection::CameraIds(
                        proto::CameraIdList {
                            camera_ids: vec!["192.0.2.99".to_owned()],
                        },
                    )),
                }),
                change: Some(proto::ConfigurationChange {
                    change: Some(proto::configuration_change::Change::Patch(
                        proto::CameraConfigurationPatch {
                            transport: Some(proto::OptionalCameraTransportUpdate {
                                value: Some(proto::optional_camera_transport_update::Value::Set(
                                    proto::CameraTransport::Udp as i32,
                                )),
                            }),
                            ..Default::default()
                        },
                    )),
                }),
            },
        )
        .unwrap();

        assert!(!plan.valid);
        assert_eq!(plan.authoritative_target_count, 0);
        assert!(plan.targets[0].skipped);
        assert!(
            plan.issues
                .iter()
                .any(|issue| issue.code == "camera_not_found")
        );
        let error = apply_configuration_plan(
            &state,
            proto::ApplyConfigurationPlan {
                plan_id: plan.plan_id,
                expected_configuration_revision: revision,
            },
        )
        .unwrap_err();
        let detail = proto::ConfigurationError::decode(error.details[0].value.as_slice()).unwrap();
        assert_eq!(
            detail.code,
            proto::ConfigurationErrorCode::PlanNotFound as i32
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn secret_default_changes_are_detected_without_exposing_values() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-secret-diff-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        config::write_private_file(
            &config_path,
            br#"
                [camera_defaults]
                username = "{secret:OLD_USERNAME}"
                password = "password"

                [cameras.front]
                ip = "192.0.2.10"
            "#,
        )
        .unwrap();
        config::write_private_file(
            &config_path.with_file_name("secrets.toml"),
            b"OLD_USERNAME = \"old-operator\"\nNEW_USERNAME = \"new-operator\"\n",
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path);
        let revision = current_configuration_revision(&state).unwrap();

        let plan = plan_configuration_change(
            &state,
            proto::PlanConfigurationChange {
                expected_configuration_revision: revision,
                targets: None,
                change: Some(proto::ConfigurationChange {
                    change: Some(proto::configuration_change::Change::Defaults(
                        proto::CameraDefaultPatch {
                            username_secret_reference: Some(proto::OptionalStringUpdate {
                                value: Some(optional_string_update::Value::Set(
                                    "{secret:NEW_USERNAME}".to_owned(),
                                )),
                            }),
                            ..Default::default()
                        },
                    )),
                }),
            },
        )
        .unwrap();

        assert!(plan.valid);
        assert_eq!(plan.changes.len(), 1);
        assert!(plan.changes[0].secret);
        assert_eq!(plan.changes[0].old_effective_value, "configured");
        assert_eq!(plan.changes[0].new_effective_value, "configured");
        let serialized = format!("{plan:?}");
        assert!(!serialized.contains("old-operator"));
        assert!(!serialized.contains("new-operator"));

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn unresolved_secret_candidate_returns_an_invalid_preview_not_an_internal_error() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-invalid-secret-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        config::write_private_file(
            &config_path,
            br#"
                [camera_defaults]
                username = "operator"
                password = "password"

                [cameras.front]
                ip = "192.0.2.10"
            "#,
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path);
        let revision = current_configuration_revision(&state).unwrap();

        let plan = plan_configuration_change(
            &state,
            proto::PlanConfigurationChange {
                expected_configuration_revision: revision,
                targets: None,
                change: Some(proto::ConfigurationChange {
                    change: Some(proto::configuration_change::Change::Defaults(
                        proto::CameraDefaultPatch {
                            username_secret_reference: Some(proto::OptionalStringUpdate {
                                value: Some(optional_string_update::Value::Set(
                                    "{secret:MISSING_USERNAME}".to_owned(),
                                )),
                            }),
                            ..Default::default()
                        },
                    )),
                }),
            },
        )
        .unwrap();

        assert!(!plan.valid);
        assert!(
            plan.issues
                .iter()
                .any(|issue| issue.code == "candidate_validation_failed")
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn camera_plan_impact_matches_the_worker_reconnect_that_apply_performs() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-impact-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        config::write_private_file(
            &config_path,
            br#"
                [camera_defaults]
                username = "operator"
                password = "password"

                [cameras.front]
                ip = "192.0.2.10"
            "#,
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path);
        let revision = current_configuration_revision(&state).unwrap();

        let plan = plan_configuration_change(
            &state,
            proto::PlanConfigurationChange {
                expected_configuration_revision: revision,
                targets: Some(proto::ConfigurationTargetSelector {
                    selection: Some(proto::configuration_target_selector::Selection::CameraIds(
                        proto::CameraIdList {
                            camera_ids: vec!["192.0.2.10".to_owned()],
                        },
                    )),
                }),
                change: Some(proto::ConfigurationChange {
                    change: Some(proto::configuration_change::Change::Patch(
                        proto::CameraConfigurationPatch {
                            display_name: Some(proto::OptionalStringUpdate {
                                value: Some(optional_string_update::Value::Set(
                                    "Front entrance".to_owned(),
                                )),
                            }),
                            ..Default::default()
                        },
                    )),
                }),
            },
        )
        .unwrap();

        assert_eq!(
            plan.impact,
            proto::ConfigurationImpact::ReconnectCamera as i32
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn fleet_snapshot_and_plan_fit_the_control_message_budget() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-transport-budget-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        let mut source =
            String::from("[camera_defaults]\nusername = \"operator\"\npassword = \"password\"\n");
        for index in 1..=127 {
            use std::fmt::Write as _;
            write!(
                &mut source,
                "\n[cameras.camera_{index:03}]\nip = \"192.0.2.{index}\"\ndisplay_name = \"Camera {index:03} at the extended perimeter entrance\"\n"
            )
            .unwrap();
        }
        config::write_private_file(&config_path, source.as_bytes()).unwrap();
        let mut templates = StoredTemplateDocument::default();
        for index in 0..MAXIMUM_TEMPLATES {
            let stored = StoredTemplate::from_proto(
                validate_template(proto::ConfigurationTemplate {
                    name: format!("Template {index:02}"),
                    description: "D".repeat(900),
                    values: Some(proto::CameraTemplateValues {
                        backend: Some(proto::CameraBackend::Auto as i32),
                        ..Default::default()
                    }),
                    ..Default::default()
                })
                .unwrap(),
                format!("template-{index:02}"),
                1,
                1,
                1,
            )
            .unwrap();
            templates.templates.push(stored);
            if serde_json::to_vec_pretty(&templates).unwrap().len()
                > MAXIMUM_TEMPLATE_DOCUMENT_BYTES
            {
                templates.templates.pop();
                break;
            }
        }
        persist_templates(&config_path, &templates).unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path);
        let snapshot = configuration_snapshot(&state).unwrap();
        let mut page_token = String::new();
        let mut camera_count = 0;
        loop {
            let page = paginate_configuration_snapshot(
                snapshot.clone(),
                proto::GetConfigurationSnapshot {
                    page_size: Some(u32::try_from(MAXIMUM_SNAPSHOT_PAGE_SIZE).unwrap()),
                    page_token,
                },
            )
            .unwrap();
            let snapshot_bytes = configuration_response_bytes(
                proto::configuration_result::Result::Snapshot(page.clone()),
            );
            assert!(
                snapshot_bytes <= CONTROL_MESSAGE_BUDGET_BYTES,
                "127-camera snapshot page uses {snapshot_bytes} bytes"
            );
            camera_count += page.cameras.len();
            page_token = page.next_page_token;
            if page_token.is_empty() {
                break;
            }
        }
        assert_eq!(camera_count, 127);
        let plan = plan_configuration_change(
            &state,
            proto::PlanConfigurationChange {
                expected_configuration_revision: snapshot.configuration_revision,
                targets: Some(proto::ConfigurationTargetSelector {
                    selection: Some(proto::configuration_target_selector::Selection::CameraIds(
                        proto::CameraIdList {
                            camera_ids: (1..=MAXIMUM_PLAN_TARGETS)
                                .map(|index| format!("192.0.2.{index}"))
                                .collect(),
                        },
                    )),
                }),
                change: Some(proto::ConfigurationChange {
                    change: Some(proto::configuration_change::Change::Patch(
                        proto::CameraConfigurationPatch {
                            display_name: Some(proto::OptionalStringUpdate {
                                value: Some(optional_string_update::Value::Set(
                                    "Perimeter camera".to_owned(),
                                )),
                            }),
                            manufacturer: Some(proto::OptionalStringUpdate {
                                value: Some(optional_string_update::Value::Set(
                                    "ONVIF camera".to_owned(),
                                )),
                            }),
                            onvif_port: Some(proto::OptionalUint32Update {
                                value: Some(proto::optional_uint32_update::Value::Set(8000)),
                            }),
                            http_port: Some(proto::OptionalUint32Update {
                                value: Some(proto::optional_uint32_update::Value::Set(80)),
                            }),
                            backend: Some(proto::OptionalCameraBackendUpdate {
                                value: Some(proto::optional_camera_backend_update::Value::Set(
                                    proto::CameraBackend::Retina as i32,
                                )),
                            }),
                            transport: Some(proto::OptionalCameraTransportUpdate {
                                value: Some(proto::optional_camera_transport_update::Value::Set(
                                    proto::CameraTransport::Udp as i32,
                                )),
                            }),
                            record_generic_motion_events: Some(proto::OptionalBoolUpdate {
                                value: Some(proto::optional_bool_update::Value::Set(true)),
                            }),
                            recording_mode: Some(proto::OptionalCameraRecordingModeUpdate {
                                value: Some(
                                    proto::optional_camera_recording_mode_update::Value::Set(
                                        proto::CameraRecordingMode::Main as i32,
                                    ),
                                ),
                            }),
                            event_recording_duration_secs: Some(proto::OptionalUint32Update {
                                value: Some(proto::optional_uint32_update::Value::Set(120)),
                            }),
                            ..Default::default()
                        },
                    )),
                }),
            },
        )
        .unwrap();
        let plan_bytes =
            configuration_response_bytes(proto::configuration_result::Result::Plan(plan));
        assert!(
            plan_bytes <= CONTROL_MESSAGE_BUDGET_BYTES,
            "maximum 64-camera plan uses {plan_bytes} bytes"
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn snapshot_page_token_rejects_a_new_configuration_revision() {
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-page-conflict-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        config::write_private_file(
            &config_path,
            br#"
                [camera_defaults]
                username = "operator"
                password = "password"

                [cameras.front]
                ip = "192.0.2.10"

                [cameras.back]
                ip = "192.0.2.11"
            "#,
        )
        .unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path.clone());
        let first = locked_configuration_snapshot_page(
            &state,
            proto::GetConfigurationSnapshot {
                page_size: Some(1),
                page_token: String::new(),
            },
        )
        .unwrap();
        assert!(!first.next_page_token.is_empty());
        let mut current = config::load_configuration_table(&config_path).unwrap();
        current.insert(
            "future_setting".to_owned(),
            toml::Value::String("changed".to_owned()),
        );
        config::write_configuration_table(&config_path, &current).unwrap();

        let error = locked_configuration_snapshot_page(
            &state,
            proto::GetConfigurationSnapshot {
                page_size: Some(1),
                page_token: first.next_page_token,
            },
        )
        .unwrap_err();

        let detail = proto::ConfigurationError::decode(error.details[0].value.as_slice()).unwrap();
        assert_eq!(detail.code, proto::ConfigurationErrorCode::Conflict as i32);
        assert_ne!(
            detail.current_configuration_revision,
            first.configuration_revision
        );

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore = "measures maximum-target configuration planning latency"]
    fn configuration_planning_benchmark() {
        const P95_BUDGET_MS: f64 = 250.0;
        let samples = std::env::var("KEEPPEEK_CONFIGURATION_BENCH_SAMPLES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(30);
        assert!(samples > 0, "benchmark samples must be greater than zero");
        let directory = std::env::temp_dir().join(format!(
            "keeppeek-configuration-benchmark-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let config_path = directory.join("config.toml");
        let mut source =
            String::from("[camera_defaults]\nusername = \"operator\"\npassword = \"password\"\n");
        for index in 1..=MAXIMUM_PLAN_TARGETS {
            use std::fmt::Write as _;
            write!(
                &mut source,
                "\n[cameras.camera_{index:03}]\nip = \"192.0.2.{index}\"\ndisplay_name = \"Camera {index:03}\"\n"
            )
            .unwrap();
        }
        config::write_private_file(&config_path, source.as_bytes()).unwrap();
        let state = ServerState::empty().with_camera_config_path(config_path);
        let revision = current_configuration_revision(&state).unwrap();
        let plan_request = || proto::PlanConfigurationChange {
            expected_configuration_revision: revision.clone(),
            targets: Some(proto::ConfigurationTargetSelector {
                selection: Some(proto::configuration_target_selector::Selection::AllCameras(
                    true,
                )),
            }),
            change: Some(proto::ConfigurationChange {
                change: Some(proto::configuration_change::Change::Patch(
                    proto::CameraConfigurationPatch {
                        backend: Some(proto::OptionalCameraBackendUpdate {
                            value: Some(proto::optional_camera_backend_update::Value::Set(
                                proto::CameraBackend::Retina as i32,
                            )),
                        }),
                        transport: Some(proto::OptionalCameraTransportUpdate {
                            value: Some(proto::optional_camera_transport_update::Value::Set(
                                proto::CameraTransport::Udp as i32,
                            )),
                        }),
                        record_generic_motion_events: Some(proto::OptionalBoolUpdate {
                            value: Some(proto::optional_bool_update::Value::Set(true)),
                        }),
                        recording_mode: Some(proto::OptionalCameraRecordingModeUpdate {
                            value: Some(proto::optional_camera_recording_mode_update::Value::Set(
                                proto::CameraRecordingMode::Main as i32,
                            )),
                        }),
                        event_recording_duration_secs: Some(proto::OptionalUint32Update {
                            value: Some(proto::optional_uint32_update::Value::Set(120)),
                        }),
                        ..Default::default()
                    },
                )),
            }),
        };
        let _ =
            locked_configuration_snapshot_page(&state, proto::GetConfigurationSnapshot::default())
                .unwrap();
        let _ = plan_configuration_change(&state, plan_request()).unwrap();
        let mut snapshot_nanoseconds = Vec::with_capacity(samples);
        let mut plan_nanoseconds = Vec::with_capacity(samples);
        let mut final_plan = None;
        for _ in 0..samples {
            let started_at = Instant::now();
            let _ = locked_configuration_snapshot_page(
                &state,
                proto::GetConfigurationSnapshot::default(),
            )
            .unwrap();
            snapshot_nanoseconds.push(started_at.elapsed().as_nanos());

            let started_at = Instant::now();
            final_plan = Some(plan_configuration_change(&state, plan_request()).unwrap());
            plan_nanoseconds.push(started_at.elapsed().as_nanos());
        }
        snapshot_nanoseconds.sort_unstable();
        plan_nanoseconds.sort_unstable();
        let snapshot_p50 = nearest_rank(&snapshot_nanoseconds, 50);
        let snapshot_p95 = nearest_rank(&snapshot_nanoseconds, 95);
        let plan_p50 = nearest_rank(&plan_nanoseconds, 50);
        let plan_p95 = nearest_rank(&plan_nanoseconds, 95);
        let plan = final_plan.expect("benchmark must produce a plan");
        let plan_bytes =
            configuration_response_bytes(proto::configuration_result::Result::Plan(plan));
        println!("camera_count={MAXIMUM_PLAN_TARGETS}");
        println!("samples={samples}");
        println!("snapshot_p50_ms={:.3}", snapshot_p50 as f64 / 1_000_000.0);
        println!("snapshot_p95_ms={:.3}", snapshot_p95 as f64 / 1_000_000.0);
        println!("plan_p50_ms={:.3}", plan_p50 as f64 / 1_000_000.0);
        println!("plan_p95_ms={:.3}", plan_p95 as f64 / 1_000_000.0);
        println!(
            "plan_p95_delta_ms={:.3}",
            plan_p95.saturating_sub(snapshot_p95) as f64 / 1_000_000.0
        );
        println!("plan_encoded_bytes={plan_bytes}");
        println!("plan_encoded_budget_bytes={CONTROL_MESSAGE_BUDGET_BYTES}");
        println!("plan_p95_budget_ms={P95_BUDGET_MS:.3}");
        assert!(
            plan_p95 as f64 / 1_000_000.0 <= P95_BUDGET_MS,
            "configuration plan p95 exceeds {P95_BUDGET_MS:.3} ms"
        );
        assert!(plan_bytes <= CONTROL_MESSAGE_BUDGET_BYTES);

        std::fs::remove_dir_all(directory).unwrap();
    }

    fn nearest_rank(sorted: &[u128], percentile: usize) -> u128 {
        let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
        sorted[rank.saturating_sub(1)]
    }
}
