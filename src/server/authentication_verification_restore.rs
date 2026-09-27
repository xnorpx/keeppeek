//! Binds restore authorization to an exact ZIP and a live remote control session.

use super::*;
use crate::access::external_proto;
use std::{fs::File, io::Write};

const MAX_PREPARATIONS: usize = 4;
const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_CHUNK_BYTES: usize = 32 * 1024;

#[derive(Default)]
pub(in crate::server) struct Registry {
    entries: HashMap<Uuid, Preparation>,
    upload_directory: Option<PathBuf>,
}

struct Preparation {
    session: SessionId,
    principal: ApiPrincipalIdentity,
    archive_bytes: u64,
    archive_sha256: String,
    received_bytes: u64,
    target_revision: String,
    expires: Instant,
    expires_at_ms: i64,
    upload: Option<Upload>,
    candidate: Option<Candidate>,
    admitted: Option<Config>,
}

struct Upload {
    path: PathBuf,
    file: Option<File>,
    hash: Sha256,
}

impl Upload {
    fn new(directory: &Path, id: Uuid) -> std::io::Result<Self> {
        let path = directory.join(format!("upload-{id}.tmp"));
        // ponytail: Reuse export-file protection, including the verified Windows DACL.
        let file = crate::backup::create_private_file(&path)?;
        Ok(Self {
            path,
            file: Some(file),
            hash: Sha256::new(),
        })
    }
}

impl Drop for Upload {
    fn drop(&mut self) {
        drop(self.file.take());
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(error_kind = ?error.kind(), "restore verification upload cleanup failed");
        }
    }
}

impl Registry {
    fn upload_directory(&mut self, config_path: &Path) -> Result<&Path, ControlCommandError> {
        if self.upload_directory.is_none() {
            let name = config_path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| rejected("configuration filename is unavailable"))?;
            let path = config_path.with_file_name(format!(".{name}.restore-verification"));
            initialize_upload_directory(&path)
                .map_err(|_| rejected("restore verification temporary directory is unavailable"))?;
            self.upload_directory = Some(path);
        }
        Ok(self
            .upload_directory
            .as_deref()
            .expect("upload directory initialized"))
    }

    pub(in crate::server) fn close_session(&mut self, session: SessionId) {
        self.entries
            .retain(|_, preparation| preparation.session != session);
    }

    pub(in crate::server) fn expire(&mut self, now: Instant, now_ms: i64) {
        self.entries.retain(|_, preparation| {
            now < preparation.expires && now_ms < preparation.expires_at_ms
        });
    }

    fn admit_capacity(
        &mut self,
        session: SessionId,
        bytes: u64,
    ) -> Result<(), ControlCommandError> {
        self.expire(Instant::now(), super::super::now_ms());
        let reserved: u64 = self.entries.values().map(|entry| entry.archive_bytes).sum();
        if self.entries.len() >= MAX_PREPARATIONS
            || self.entries.values().any(|entry| entry.session == session)
            || reserved.saturating_add(bytes) > MAX_ARCHIVE_BYTES
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                429,
                "restore verification capacity exceeded",
            ));
        }
        Ok(())
    }
}

impl Preparation {
    fn validate(
        &self,
        state: &ServerState,
        session: SessionId,
        principal: &ApiPrincipal,
    ) -> Result<(), ControlCommandError> {
        if self.session != session
            || self.principal != principal.identity
            || Instant::now() >= self.expires
            || super::super::now_ms() >= self.expires_at_ms
            || self.target_revision != current_revision(state)?
        {
            return Err(rejected(
                "restore verification is stale or belongs to another session",
            ));
        }
        Ok(())
    }

    fn response(
        &self,
        id: Uuid,
    ) -> Result<proto::ConfigurationRestoreVerification, ControlCommandError> {
        let candidate_authentication = self
            .candidate
            .as_ref()
            .map(|candidate| {
                external_proto::from_root(&candidate.after.source)
                    .map_err(|_| rejected("restore candidate settings are unavailable"))
            })
            .transpose()?
            .flatten();
        Ok(proto::ConfigurationRestoreVerification {
            preparation_id: id.to_string(),
            archive_bytes: self.archive_bytes,
            received_bytes: self.received_bytes,
            expires_at_ms: self.expires_at_ms,
            ready: self.candidate.is_some(),
            requires_administrator_confirmation: self
                .candidate
                .as_ref()
                .map(needs_confirmation)
                .transpose()?
                .unwrap_or(false),
            confirmed: self.admitted.is_some(),
            candidate_authentication,
        })
    }
}

fn config_path(state: &ServerState) -> Result<&Path, ControlCommandError> {
    state
        .camera_config_path
        .as_deref()
        .ok_or_else(|| rejected("configuration storage is unavailable"))
}

fn current_revision(state: &ServerState) -> Result<String, ControlCommandError> {
    crate::backup::target_revision(config_path(state)?)
        .map_err(|_| rejected("restore target revision is unavailable"))
}

fn needs_confirmation(candidate: &Candidate) -> Result<bool, ControlCommandError> {
    let now = super::super::now_ms();
    let inspect = || -> anyhow::Result<bool> {
        Ok(
            migration::has_current_administrator(&candidate.before, now)?
                && !migration::preserves_administrator(&candidate.before, &candidate.after, now)?,
        )
    };
    inspect().map_err(|_| rejected("restore Administrator paths could not be validated"))
}

pub(in crate::server) fn begin(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    request: proto::BeginConfigurationRestoreVerification,
) -> Result<proto::ConfigurationRestoreVerification, ControlCommandError> {
    if request.archive_bytes == 0
        || request.archive_bytes > MAX_ARCHIVE_BYTES
        || request.archive_sha256.len() != 64
        || !request
            .archive_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(rejected("restore archive length or digest is invalid"));
    }
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    session_lifecycle::admit(state, session, || {
        require_remote_owner(state, session, principal)?;
        let path = config_path(state)?;
        let revision = current_revision(state)?;
        let mut registry = state
            .configuration_plans
            .restore_proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.admit_capacity(session, request.archive_bytes)?;
        let id = Uuid::new_v4();
        let upload = Upload::new(registry.upload_directory(path)?, id)
            .map_err(|_| rejected("restore verification upload could not be created"))?;
        let preparation = Preparation {
            session,
            principal: principal.identity.clone(),
            archive_bytes: request.archive_bytes,
            archive_sha256: request.archive_sha256,
            received_bytes: 0,
            target_revision: revision,
            expires: Instant::now() + PROOF_LIFETIME,
            expires_at_ms: super::super::now_ms().saturating_add(300_000),
            upload: Some(upload),
            candidate: None,
            admitted: None,
        };
        let response = preparation.response(id)?;
        assert!(
            registry.entries.insert(id, preparation).is_none(),
            "random restore identifier collision"
        );
        Ok(response)
    })
}

pub(in crate::server) fn append(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    request: proto::AppendConfigurationRestoreVerification,
) -> Result<proto::ConfigurationRestoreVerification, ControlCommandError> {
    let id = parse_id(&request.preparation_id)?;
    if request.data.is_empty() || request.data.len() > MAX_CHUNK_BYTES {
        return Err(rejected(
            "restore upload chunks must contain 1 to 32768 bytes",
        ));
    }
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    session_lifecycle::admit(state, session, || {
        require_remote_owner(state, session, principal)?;
        let mut registry = state
            .configuration_plans
            .restore_proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let preparation = registry
            .entries
            .get_mut(&id)
            .ok_or_else(|| rejected("restore verification is unavailable"))?;
        preparation.validate(state, session, principal)?;
        let result = append_chunk(state, id, preparation, &request);
        if result.is_err() {
            registry.entries.remove(&id);
        }
        result
    })
}

fn append_chunk(
    state: &ServerState,
    id: Uuid,
    preparation: &mut Preparation,
    request: &proto::AppendConfigurationRestoreVerification,
) -> Result<proto::ConfigurationRestoreVerification, ControlCommandError> {
    let length = u64::try_from(request.data.len()).expect("bounded chunk length");
    if request.offset != preparation.received_bytes
        || length
            > preparation
                .archive_bytes
                .saturating_sub(preparation.received_bytes)
    {
        return Err(rejected("restore upload offset or length is invalid"));
    }
    let upload = preparation
        .upload
        .as_mut()
        .ok_or_else(|| rejected("restore upload is already complete"))?;
    upload
        .file
        .as_mut()
        .expect("open upload")
        .write_all(&request.data)
        .map_err(|_| rejected("restore upload could not be saved"))?;
    upload.hash.update(&request.data);
    preparation.received_bytes += length;
    if preparation.received_bytes == preparation.archive_bytes {
        finish_upload(state, id, preparation)?;
    }
    preparation.response(id)
}

fn finish_upload(
    state: &ServerState,
    id: Uuid,
    preparation: &mut Preparation,
) -> Result<(), ControlCommandError> {
    let upload = preparation
        .upload
        .as_mut()
        .expect("upload exists until completion");
    let digest: String = upload
        .hash
        .clone()
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if digest != preparation.archive_sha256 {
        return Err(rejected("restore archive digest does not match"));
    }
    upload
        .file
        .take()
        .expect("open upload")
        .sync_all()
        .map_err(|_| rejected("restore upload could not be saved"))?;
    let path = config_path(state)?;
    let native = crate::backup::inspect_configuration_candidate(&upload.path, path)
        .map_err(|_| rejected("restore archive candidate failed validation"))?;
    let before =
        config::load_config(path).map_err(|_| rejected("current configuration is unavailable"))?;
    preparation.candidate = Some(Candidate {
        plan_id: id.to_string(),
        revision: format!(
            "{}:{}",
            preparation.target_revision, preparation.archive_sha256
        ),
        expires_at_ms: preparation.expires_at_ms,
        path: path.to_owned(),
        before,
        after: native.configuration,
    });
    drop(preparation.upload.take());
    Ok(())
}

pub(super) fn candidate(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    id: &str,
) -> Result<Option<Candidate>, ControlCommandError> {
    let Ok(id) = Uuid::parse_str(id) else {
        return Ok(None);
    };
    let registry = state
        .configuration_plans
        .restore_proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(preparation) = registry.entries.get(&id) else {
        return Ok(None);
    };
    preparation.validate(state, session, principal)?;
    if preparation.admitted.is_some() {
        return Err(rejected("restore preparation is already confirmed"));
    }
    preparation
        .candidate
        .clone()
        .map(Some)
        .ok_or_else(|| rejected("restore upload is incomplete"))
}

pub(in crate::server) fn get(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    request: proto::GetConfigurationRestoreVerification,
) -> Result<proto::ConfigurationRestoreVerification, ControlCommandError> {
    let id = parse_id(&request.preparation_id)?;
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    session_lifecycle::admit(state, session, || {
        require_remote_owner(state, session, principal)?;
        let registry = state
            .configuration_plans
            .restore_proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let preparation = registry
            .entries
            .get(&id)
            .ok_or_else(|| rejected("restore verification is unavailable"))?;
        preparation.validate(state, session, principal)?;
        preparation.response(id)
    })
}

pub(in crate::server) fn confirm(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    request: proto::ConfirmConfigurationRestoreVerification,
) -> Result<proto::ConfigurationRestoreVerification, ControlCommandError> {
    let id = parse_id(&request.preparation_id)?;
    if !request.confirm {
        return Err(rejected("explicit restore confirmation is required"));
    }
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    session_lifecycle::admit(state, session, || {
        require_remote_owner(state, session, principal)?;
        let candidate = candidate(state, session, principal, &request.preparation_id)?
            .ok_or_else(|| rejected("restore verification is unavailable"))?;
        let (admitted, proof) = match request.administrator_confirmation.as_ref() {
            Some(confirmation) => {
                let (admitted, id) =
                    confirmed_configuration(state, session, principal, confirmation, &candidate)?;
                (admitted, Some(id))
            }
            None if !needs_confirmation(&candidate)? => (candidate.after, None),
            None => {
                return Err(rejected(
                    "replacement Administrator verification is required",
                ));
            }
        };
        require_remote_owner(state, session, principal)?;
        let mut registry = state
            .configuration_plans
            .restore_proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let preparation = registry
            .entries
            .get_mut(&id)
            .ok_or_else(|| rejected("restore verification is unavailable"))?;
        preparation.validate(state, session, principal)?;
        preparation.admitted = Some(admitted);
        if let Some(id) = proof {
            consume_proof(state, id);
        }
        preparation.response(id)
    })
}

fn parse_id(value: &str) -> Result<Uuid, ControlCommandError> {
    if value.len() > 36 {
        return Err(rejected("restore verification ID is invalid"));
    }
    Uuid::parse_str(value).map_err(|_| rejected("restore verification ID is invalid"))
}

pub(in crate::server) fn authorize_upload(
    state: &ServerState,
    principal: &ApiPrincipal,
    context: crate::backup::ConfigurationApply<'_>,
) -> anyhow::Result<crate::api::backup_proto::RestoreRecord> {
    let before = config::load_config(context.config_path())?;
    let current = Candidate {
        plan_id: String::new(),
        revision: String::new(),
        expires_at_ms: i64::MAX,
        path: context.config_path().to_owned(),
        before,
        after: context.candidate.configuration.clone(),
    };
    let binding = matching_confirmation(state, principal, &context.archive_sha256)
        .map_err(authorization_error)?;
    if let Some((id, session)) = binding {
        return session_lifecycle::admit(state, session, || {
            require_remote_owner(state, session, principal)?;
            let admitted = take_confirmation(state, session, principal, id, &current)?;
            Ok(stage_with_live_principal(
                state,
                principal,
                &current.before,
                context,
                Some(&admitted),
            ))
        })
        .map_err(authorization_error)?;
    }
    if needs_confirmation(&current).map_err(authorization_error)? {
        return Err(crate::backup::ServiceError::conflict(
            "replacement Administrator verification and restore confirmation are required",
        )
        .into());
    }
    stage_with_live_principal(state, principal, &current.before, context, None)
}

fn matching_confirmation(
    state: &ServerState,
    principal: &ApiPrincipal,
    archive_sha256: &str,
) -> Result<Option<(Uuid, SessionId)>, ControlCommandError> {
    let mut registry = state
        .configuration_plans
        .restore_proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.expire(Instant::now(), super::super::now_ms());
    let mut matches = registry.entries.iter().filter(|(_, entry)| {
        entry.principal == principal.identity
            && entry.archive_sha256 == archive_sha256
            && entry.admitted.is_some()
    });
    let first = matches.next().map(|(id, entry)| (*id, entry.session));
    if matches.next().is_some() {
        return Err(rejected(
            "restore authorization is ambiguous; prepare one confirmation",
        ));
    }
    Ok(first)
}

fn take_confirmation(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    id: Uuid,
    current: &Candidate,
) -> Result<Config, ControlCommandError> {
    let mut registry = state
        .configuration_plans
        .restore_proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let preparation = registry
        .entries
        .get(&id)
        .ok_or_else(|| rejected("restore confirmation is unavailable"))?;
    preparation.validate(state, session, principal)?;
    let candidate = preparation
        .candidate
        .as_ref()
        .ok_or_else(|| rejected("restore candidate is unavailable"))?;
    if fingerprint(&current.path, &current.before, &current.after)?
        != fingerprint(&candidate.path, &candidate.before, &candidate.after)?
    {
        return Err(rejected("restore candidate changed after confirmation"));
    }
    // ponytail: Claim once before staging; a failed stage requires fresh confirmation.
    registry
        .entries
        .remove(&id)
        .and_then(|entry| entry.admitted)
        .ok_or_else(|| rejected("restore confirmation is unavailable"))
}

fn stage_with_live_principal(
    state: &ServerState,
    principal: &ApiPrincipal,
    before: &Config,
    context: crate::backup::ConfigurationApply<'_>,
    admitted: Option<&Config>,
) -> anyhow::Result<crate::api::backup_proto::RestoreRecord> {
    // The configuration lock excludes credential changes; this lock excludes browser logout.
    let registry = state
        .authentication
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let now_ms = super::super::now_ms();
    if registry.config != before.external_auth
        || registry.directory != crate::access::identities::Directory::from_root(&before.source)?
    {
        return Err(crate::backup::ServiceError::conflict(
            "authentication configuration changed on disk",
        )
        .into());
    }
    let active = match principal.identity {
        ApiPrincipalIdentity::Local(_) => true,
        ApiPrincipalIdentity::Credential { id, revision } => {
            registry.config.as_ref().is_none_or(|settings| {
                settings.bearer_enabled
                    && settings
                        .bearer_transition_until_ms
                        .is_some_and(|until| now_ms < until)
            }) && state
                .access_manager
                .credential_is_active(id, revision, now_ms)
        }
        ApiPrincipalIdentity::External {
            id,
            revision,
            browser,
        } => {
            registry.directory.active(id, revision).is_some()
                && registry.sessions.active(
                    browser,
                    crate::access::browser_sessions::Binding {
                        identity_id: id,
                        revision,
                    },
                    Instant::now(),
                )
        }
    };
    if !active || principal.role != AccessRole::Administrator {
        return Err(crate::backup::ServiceError::conflict(
            "restore Administrator is no longer active",
        )
        .into());
    }
    let result = context.stage(admitted);
    drop(registry);
    result
}

fn authorization_error(_error: ControlCommandError) -> anyhow::Error {
    crate::backup::ServiceError::conflict("restore confirmation is missing, stale, or unavailable")
        .into()
}

fn initialize_upload_directory(path: &Path) -> anyhow::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    #[cfg(not(unix))]
    let _ = &mut builder;
    match builder.create(path) {
        Ok(()) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    anyhow::ensure!(
        std::fs::symlink_metadata(path)?.file_type().is_dir(),
        "invalid upload directory"
    );
    let files = std::fs::read_dir(path)?
        .take(MAX_PREPARATIONS + 1)
        .collect::<Result<Vec<_>, _>>()?;
    anyhow::ensure!(
        files.len() <= MAX_PREPARATIONS,
        "unexpected upload directory contents"
    );
    for file in &files {
        let name = file.file_name();
        let valid = name
            .to_str()
            .and_then(|name| name.strip_prefix("upload-"))
            .and_then(|name| name.strip_suffix(".tmp"))
            .is_some_and(|name| name.len() == 36 && Uuid::parse_str(name).is_ok());
        anyhow::ensure!(
            valid && file.file_type()?.is_file(),
            "unexpected upload directory entry"
        );
    }
    for file in files {
        std::fs::remove_file(file.path())?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "authentication_verification_restore_tests.rs"]
mod tests;
