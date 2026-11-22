//! Administrator management and live authorization for bounded guest evidence.
mod csp;
mod evidence;
mod management;
#[cfg(test)]
mod tests;
mod wire;

use super::*;
use crate::shares::{Clock, ClockAnchor, GrantSummary, SessionContext, Store};
pub(super) use evidence::{ValidatedEvidence, open_guest, validate_guest};
pub(super) use management::dispatch;

pub(super) const CAPABILITY_ID: &str = "keeppeek.guest-review.v1";

struct Policy {
    settings: config::sharing::SharingConfig,
    epoch: u64,
    error: Option<&'static str>,
    sources: BTreeMap<Uuid, String>,
}

pub(super) struct Runtime {
    policy: RwLock<Policy>,
    store: Mutex<Option<(Store, ClockAnchor)>>,
    next_purge: Mutex<Instant>,
    page_csp: Option<Arc<str>>,
}

impl Runtime {
    pub(super) fn new(config: &Config) -> Self {
        Self {
            policy: RwLock::new(Policy {
                settings: config.sharing.clone(),
                epoch: rand::random::<u64>().max(1),
                error: None,
                sources: sources(config)
                    .expect("camera identities must be validated before startup"),
            }),
            store: Mutex::new(None),
            next_purge: Mutex::new(Instant::now()),
            page_csp: UI_ASSETS
                .get_file("index.html")
                .and_then(|file| std::str::from_utf8(file.contents()).ok())
                .and_then(csp::for_document),
        }
    }

    pub(super) fn open(&self, storage: &StorageConfig) -> anyhow::Result<()> {
        let _root = storage.metadata_root()?;
        let parent = storage
            .recording_catalog_path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("metadata owner is unavailable"))?;
        let clock = raw_clock()?;
        let store = Store::open(
            &parent.join("guest-review").join("grants.db"),
            clock.unix_ms,
        )?;
        _root.revalidate()?;
        let anchor = ClockAnchor::new(clock)?;
        let mut current = self
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("guest store lock failed"))?;
        anyhow::ensure!(current.is_none(), "guest store is already open");
        *current = Some((store, anchor));
        Ok(())
    }
}

fn raw_clock() -> Result<Clock, crate::shares::Error> {
    Ok(Clock {
        unix_ms: i64::try_from(unix_time_ms()).map_err(|_| crate::shares::Error::Denied)?,
        monotonic: Instant::now(),
    })
}

pub(super) fn with_store<T>(
    state: &ServerState,
    operation: impl FnOnce(&mut Store, Clock) -> Result<T, crate::shares::Error>,
) -> Result<T, ControlCommandError> {
    let mut owner = state.sharing.store.lock().map_err(|_| unavailable())?;
    let (store, anchor) = owner.as_mut().ok_or_else(unavailable)?;
    let clock = anchor
        .sample(raw_clock().map_err(domain_error)?)
        .map_err(domain_error)?;
    operation(store, clock).map_err(domain_error)
}

pub(super) fn context(state: &ServerState) -> Result<SessionContext, ControlCommandError> {
    let policy = state.sharing.policy.read().map_err(|_| unavailable())?;
    if !policy.settings.enabled {
        return Err(denied());
    }
    policy_context(&policy)
}

pub(super) fn page_context(state: &ServerState) -> Result<SessionContext, ControlCommandError> {
    let policy = state.sharing.policy.read().map_err(|_| unavailable())?;
    policy_context(&policy)
}

fn policy_context(policy: &Policy) -> Result<SessionContext, ControlCommandError> {
    Ok(SessionContext {
        origin: policy
            .settings
            .public_origin
            .clone()
            .ok_or_else(unavailable)?,
        policy_revision: policy.epoch,
    })
}

pub(super) fn update_sources(state: &ServerState, config: &Config) -> anyhow::Result<()> {
    let next = sources(config)?;
    let mut policy = state
        .sharing
        .policy
        .write()
        .map_err(|_| anyhow::anyhow!("sharing policy lock failed"))?;
    let changed = policy.sources != next;
    policy.sources = next;
    drop(policy);
    if changed {
        // ponytail: camera identity edits stop at most eight guest transfers; narrow this if edits become frequent.
        state.guest_review.abort_all();
    }
    Ok(())
}

pub(super) fn activate(state: &ServerState, config: &Config) -> anyhow::Result<()> {
    state.guest_review.abort_all();
    let mut policy = state
        .sharing
        .policy
        .write()
        .map_err(|_| anyhow::anyhow!("sharing policy lock failed"))?;
    invalidate(state, &mut policy)?;
    policy.sources = sources(config)?;
    policy.settings = config.sharing.clone();
    policy.error = None;
    state.guest_review.abort_all();
    Ok(())
}

fn invalidate(state: &ServerState, policy: &mut Policy) -> anyhow::Result<()> {
    policy.settings.enabled = false;
    policy.sources.clear();
    policy.epoch = policy
        .epoch
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("sharing epoch exhausted"))?;
    let mut owner = state
        .sharing
        .store
        .lock()
        .map_err(|_| anyhow::anyhow!("guest store lock failed"))?;
    if let Some((store, _)) = owner.as_mut() {
        store.invalidate_sessions();
    }
    Ok(())
}

pub(super) fn persistence_failed(state: &ServerState, path: &Path) -> anyhow::Result<()> {
    state.guest_review.abort_all();
    let mut policy = state
        .sharing
        .policy
        .write()
        .map_err(|_| anyhow::anyhow!("sharing policy lock failed"))?;
    invalidate(state, &mut policy)?;
    policy.error = Some(
        "Sharing stopped because a settings write could not be confirmed. Save the policy again to enable it.",
    );
    if let Ok(config) = config::load_config(path) {
        policy.settings = config.sharing;
        policy.settings.enabled = false;
    }
    state.guest_review.abort_all();
    Ok(())
}
fn sources(config: &Config) -> anyhow::Result<BTreeMap<Uuid, String>> {
    config
        .camera_profile_identities()?
        .into_iter()
        .map(|(ip, id)| Ok((Uuid::parse_str(&id)?, ip.to_string())))
        .collect()
}

fn source(state: &ServerState, identity: Uuid) -> Result<String, ControlCommandError> {
    state
        .sharing
        .policy
        .read()
        .map_err(|_| unavailable())?
        .sources
        .get(&identity)
        .cloned()
        .ok_or_else(denied)
}

fn settings(state: &ServerState) -> Result<config::sharing::SharingConfig, ControlCommandError> {
    Ok(state
        .sharing
        .policy
        .read()
        .map_err(|_| unavailable())?
        .settings
        .clone())
}

fn denied() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Rejected,
        403,
        "guest evidence is unavailable",
    )
}

fn unavailable() -> ControlCommandError {
    ControlCommandError::new(
        proto::ErrorCode::Unavailable,
        503,
        "guest review is unavailable",
    )
}

fn invalid(message: &'static str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::InvalidRequest, 400, message)
}

fn domain_error(error: crate::shares::Error) -> ControlCommandError {
    use crate::shares::Error;
    let (code, status) = match error {
        Error::Denied => (proto::ErrorCode::Rejected, 403),
        Error::Invalid => (proto::ErrorCode::InvalidRequest, 400),
        Error::Conflict => (proto::ErrorCode::Rejected, 409),
        Error::Capacity => (proto::ErrorCode::Rejected, 429),
        Error::Storage => (proto::ErrorCode::Unavailable, 503),
    };
    ControlCommandError::new(code, status, error.to_string())
}

/// Keeps a current policy read guard through the serialized guest transition.
pub(super) fn with_guest_store<T>(
    state: &ServerState,
    expected: &SessionContext,
    operation: impl FnOnce(&mut Store, Clock) -> Result<T, crate::shares::Error>,
) -> Result<T, ControlCommandError> {
    let policy = state.sharing.policy.read().map_err(|_| unavailable())?;
    if !policy.settings.enabled
        || policy.epoch != expected.policy_revision
        || policy.settings.public_origin.as_deref() != Some(expected.origin.as_str())
    {
        return Err(denied());
    }
    with_store(state, operation)
}

pub(super) fn refresh_sources(state: &ServerState, path: &Path) -> Result<(), ControlCommandError> {
    let configuration = config::load_config(path).map_err(|_| unavailable());
    match configuration {
        Ok(configuration) => update_sources(state, &configuration).map_err(|_| unavailable()),
        Err(error) => {
            state
                .sharing
                .policy
                .write()
                .map_err(|_| unavailable())?
                .sources
                .clear();
            state.guest_review.abort_all();
            Err(error)
        }
    }
}

pub(super) fn maintain(state: &ServerState) {
    let Ok(mut next) = state.sharing.next_purge.try_lock() else {
        return;
    };
    let now = Instant::now();
    if now < *next {
        return;
    }
    *next = now + Duration::from_secs(60);
    if state
        .sharing
        .store
        .lock()
        .is_ok_and(|store| store.is_none())
    {
        return;
    }
    match with_store(state, |store, clock| store.purge(clock)) {
        Ok(256) => *next = now,
        Ok(_) => {}
        Err(_) => tracing::warn!("guest audit cleanup is unavailable"),
    }
}

pub(super) fn page_csp(state: &ServerState) -> Result<Arc<str>, ControlCommandError> {
    state.sharing.page_csp.clone().ok_or_else(unavailable)
}

pub(super) const fn audit_operation(command: &proto::ShareCommand) -> Option<&'static str> {
    use proto::share_command::Action;
    match command.action {
        Some(Action::ConfigurePolicy(_)) => Some("sharing_policy_update"),
        Some(Action::Create(_)) => Some("sharing_create"),
        Some(Action::Revoke(_)) => Some("sharing_revoke"),
        Some(Action::RevokeEvidence(_)) => Some("sharing_revoke_evidence"),
        _ => None,
    }
}
