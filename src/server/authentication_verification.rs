//! Binds replacement Administrator evidence to one live control session and candidate.

use super::{ApiPrincipal, ApiPrincipalIdentity, ServerState};
use crate::{
    access::{AccessRole, migration},
    api::proto,
    config::{self, Config},
    server::{ControlCommandError, configuration, session_lifecycle},
    webrtc::SessionId,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

const MAX_PROOFS: usize = 32;
const MAX_PROOFS_PER_SESSION: usize = 2;
const PROOF_LIFETIME: Duration = Duration::from_secs(300);
const MAX_SECRET_FILE_BYTES: u64 = 1_048_576;

#[path = "authentication_verification_browser.rs"]
pub(in crate::server) mod browser;

#[path = "authentication_verification_oidc.rs"]
pub(in crate::server::authentication) mod oidc_browser;

#[path = "authentication_verification_restore.rs"]
pub(in crate::server) mod restore;

#[derive(Default)]
pub(in crate::server) struct Registry {
    proofs: HashMap<Uuid, Proof>,
    transactions: crate::access::login_transactions::Transactions,
    providers: Arc<Mutex<crate::access::oidc::CandidateCache>>,
}

#[derive(Clone)]
struct Proof {
    session: SessionId,
    principal: ApiPrincipalIdentity,
    plan_id: String,
    revision: String,
    fingerprint: [u8; 32],
    stage: Stage,
    expires: Instant,
    expires_at_ms: i64,
}

#[derive(Clone)]
enum Stage {
    Bearer((Uuid, u64)),
    BrowserStart {
        origin: String,
        provider_id: String,
        challenge: Option<[u8; 32]>,
    },
    External(crate::access::identities::PreparedIdentity),
    OidcPending(Box<oidc_browser::Pending>),
}

#[derive(Clone)]
pub(in crate::server) struct Candidate {
    pub plan_id: String,
    pub revision: String,
    pub expires_at_ms: i64,
    pub path: PathBuf,
    pub before: Config,
    pub after: Config,
}

pub(in crate::server) fn verify_bearer(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    request: proto::VerifyAdministratorBearer,
) -> Result<proto::AdministratorVerification, ControlCommandError> {
    let _update = state
        .config_update
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    session_lifecycle::admit(state, session, || {
        require_remote_owner(state, session, principal)?;
        let candidate =
            resolve_candidate(state, session, principal, &request.configuration_plan_id)?;
        if !migration::same_transport(&candidate.before, &candidate.after) {
            return Err(rejected(
                "replacement verification cannot change transport or network trust",
            ));
        }
        let credential = migration::verify_replacement_bearer(
            &candidate.after,
            &request.access_key,
            super::now_ms(),
        )
        .map_err(|_| {
            rejected("replacement requires a valid permanent Administrator bearer credential")
        })?;
        let proof = Proof::new(session, principal, &candidate, Stage::Bearer(credential))?;
        require_remote_owner(state, session, principal)?;
        state
            .configuration_plans
            .proofs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(proof, Instant::now(), super::now_ms())
    })
}

fn resolve_candidate(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    id: &str,
) -> Result<Candidate, ControlCommandError> {
    restore::candidate(state, session, principal, id)?
        .map_or_else(|| configuration::verification_candidate(state, id), Ok)
}

impl Registry {
    fn insert(
        &mut self,
        proof: Proof,
        now: Instant,
        now_ms: i64,
    ) -> Result<proto::AdministratorVerification, ControlCommandError> {
        self.retain(|proof| now < proof.expires && now_ms < proof.expires_at_ms);
        if self.proofs.len() >= MAX_PROOFS
            || self
                .proofs
                .values()
                .filter(|existing| existing.session == proof.session)
                .count()
                >= MAX_PROOFS_PER_SESSION
        {
            return Err(ControlCommandError::new(
                proto::ErrorCode::Rejected,
                429,
                "Administrator verification capacity exceeded",
            ));
        }
        let id = Uuid::new_v4();
        let response = proof.response(id);
        assert!(
            self.proofs.insert(id, proof).is_none(),
            "random verification identifier collision"
        );
        Ok(response)
    }

    pub(in crate::server) fn close_session(&mut self, session: SessionId) {
        self.retain(|proof| proof.session != session);
    }

    fn retain(&mut self, mut keep: impl FnMut(&Proof) -> bool) {
        let transactions = &mut self.transactions;
        self.proofs.retain(|_, proof| {
            if keep(proof) {
                return true;
            }
            if let Stage::OidcPending(pending) = &proof.stage {
                transactions.revoke_browser(pending.browser);
            }
            false
        });
    }
}

fn require_remote_owner(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
) -> Result<(), ControlCommandError> {
    let owner = state
        .api_session_owners
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&session)
        .cloned();
    let remote = owner.is_some_and(|owner| {
        !owner.classification.local
            && !owner
                .lifecycle
                .closed
                .load(std::sync::atomic::Ordering::Acquire)
            && owner.principal.identity == principal.identity
            && owner.principal.role == AccessRole::Administrator
            && super::now_ms() < owner.absolute_expires_at_ms
            && Instant::now().saturating_duration_since(owner.last_activity)
                < state.api_session_policy.idle_timeout
    });
    if session.as_u64() == 0
        || principal.is_local()
        || principal.role != AccessRole::Administrator
        || !remote
    {
        return Err(ControlCommandError::new(
            proto::ErrorCode::Rejected,
            403,
            "verification requires the live remote Administrator session",
        ));
    }
    if !super::active(state, principal, Instant::now(), super::now_ms()) {
        return Err(rejected("verification Administrator session expired"));
    }
    Ok(())
}

pub(in crate::server) fn commit(
    state: &ServerState,
    actor: Option<(SessionId, &ApiPrincipal)>,
    request: &proto::ApplyConfigurationPlan,
    candidate: &Candidate,
) -> Result<(), ControlCommandError> {
    let confirmation = request
        .administrator_confirmation
        .as_ref()
        .ok_or_else(|| rejected("Administrator confirmation is required"))?;
    let (session, principal) =
        actor.ok_or_else(|| rejected("confirmation requires its original control session"))?;
    session_lifecycle::admit(state, session, || {
        require_remote_owner(state, session, principal)?;
        let (after, id) =
            confirmed_configuration(state, session, principal, confirmation, candidate)?;
        let next =
            super::prepare_configuration(state, &candidate.before, &after).map_err(|_| {
                rejected("authentication configuration changed; prepare verification again")
            })?;
        require_remote_owner(state, session, principal)?;
        // ponytail: config_update serializes all proof consumers; no proof lock spans disk I/O.
        #[cfg(test)]
        super::before_configuration_commit(state);
        super::commit_configuration(state, Some(principal), Some(next), || {
            config::write_configuration_table(&candidate.path, &after.source).map_err(|_| {
                ControlCommandError::new(
                    proto::ErrorCode::Unavailable,
                    503,
                    "configuration could not be saved",
                )
            })
        })?;
        consume_proof(state, id);
        Ok(())
    })
}

fn confirmed_configuration(
    state: &ServerState,
    session: SessionId,
    principal: &ApiPrincipal,
    confirmation: &proto::AdministratorConfirmation,
    candidate: &Candidate,
) -> Result<(Config, Uuid), ControlCommandError> {
    let id = Uuid::parse_str(&confirmation.verification_id)
        .map_err(|_| rejected("Administrator verification is unavailable"))?;
    if !confirmation.confirm {
        return Err(rejected("explicit Administrator confirmation is required"));
    }
    let fingerprint = fingerprint(&candidate.path, &candidate.before, &candidate.after)?;
    let registry = state
        .configuration_plans
        .proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let proof = registry
        .proofs
        .get(&id)
        .ok_or_else(|| rejected("Administrator verification is unavailable"))?;
    proof.validate(
        session,
        principal,
        candidate,
        fingerprint,
        Instant::now(),
        super::now_ms(),
    )?;
    Ok((proof.admitted_configuration(candidate)?, id))
}

fn consume_proof(state: &ServerState, id: Uuid) {
    state
        .configuration_plans
        .proofs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .proofs
        .remove(&id);
}

impl Proof {
    fn new(
        session: SessionId,
        principal: &ApiPrincipal,
        candidate: &Candidate,
        stage: Stage,
    ) -> Result<Self, ControlCommandError> {
        if !migration::same_transport(&candidate.before, &candidate.after) {
            return Err(rejected(
                "replacement verification cannot change transport or network trust",
            ));
        }
        let now_ms = super::now_ms();
        let lifetime_ms = candidate
            .expires_at_ms
            .saturating_sub(now_ms)
            .min(i64::try_from(PROOF_LIFETIME.as_millis()).expect("bounded proof lifetime"));
        if lifetime_ms <= 0 {
            return Err(rejected("verification candidate expired"));
        }
        Ok(Self {
            session,
            principal: principal.identity.clone(),
            plan_id: candidate.plan_id.clone(),
            revision: candidate.revision.clone(),
            fingerprint: fingerprint(&candidate.path, &candidate.before, &candidate.after)?,
            stage,
            expires: Instant::now()
                + Duration::from_millis(
                    u64::try_from(lifetime_ms).expect("positive bounded proof lifetime"),
                ),
            expires_at_ms: now_ms.saturating_add(lifetime_ms),
        })
    }

    fn response(&self, id: Uuid) -> proto::AdministratorVerification {
        proto::AdministratorVerification {
            verification_id: id.to_string(),
            configuration_plan_id: self.plan_id.clone(),
            expires_at_ms: self.expires_at_ms,
            verified: matches!(self.stage, Stage::Bearer(_) | Stage::External(_)),
            browser_start: None,
        }
    }

    fn admitted_configuration(&self, candidate: &Candidate) -> Result<Config, ControlCommandError> {
        let mut after = candidate.after.clone();
        match &self.stage {
            Stage::Bearer(credential) => {
                if !migration::verified_bearer_is_active(&after, *credential, super::now_ms())
                    .map_err(|_| rejected("replacement Administrator could not be revalidated"))?
                {
                    return Err(rejected("replacement Administrator is unavailable"));
                }
            }
            Stage::External(prepared) => {
                let admit = || -> anyhow::Result<toml::Value> {
                    anyhow::ensure!(
                        prepared.identity().role == AccessRole::Administrator,
                        "Administrator is required"
                    );
                    let mut directory =
                        crate::access::identities::Directory::from_root(&after.source)?;
                    directory.admit_prepared(prepared)?;
                    Ok(toml::Value::try_from(directory)?)
                };
                let directory =
                    admit().map_err(|_| rejected("replacement identity could not be admitted"))?;
                after
                    .source
                    .insert(crate::access::identities::SECTION.into(), directory);
            }
            Stage::BrowserStart { .. } | Stage::OidcPending(_) => {
                return Err(rejected("replacement Administrator has not been verified"));
            }
        }
        Ok(after)
    }

    fn validate(
        &self,
        session: SessionId,
        principal: &ApiPrincipal,
        candidate: &Candidate,
        fingerprint: [u8; 32],
        now: Instant,
        now_ms: i64,
    ) -> Result<(), ControlCommandError> {
        let bound = self.session == session
            && self.principal == principal.identity
            && self.plan_id == candidate.plan_id
            && self.revision == candidate.revision
            && self.fingerprint == fingerprint
            && now < self.expires
            && now_ms < self.expires_at_ms
            && now_ms < candidate.expires_at_ms
            && migration::same_transport(&candidate.before, &candidate.after);
        if !bound {
            return Err(rejected(
                "Administrator verification is stale or belongs to another session",
            ));
        }
        Ok(())
    }
}

fn fingerprint(
    path: &Path,
    before: &Config,
    after: &Config,
) -> Result<[u8; 32], ControlCommandError> {
    let inspect = || -> anyhow::Result<[u8; 32]> {
        let mut hash = Sha256::new();
        for bytes in [
            toml::to_string(&before.source)?.into_bytes(),
            toml::to_string(&after.source)?.into_bytes(),
            serde_json::to_vec(&(
                &before.external_auth,
                &after.external_auth,
                before.access_key,
                after.access_key,
            ))?,
            secret_bytes(path)?,
        ] {
            hash.update(
                u64::try_from(bytes.len())
                    .expect("configuration length fits u64")
                    .to_be_bytes(),
            );
            hash.update(bytes);
        }
        Ok(hash.finalize().into())
    };
    inspect().map_err(|_| rejected("verification configuration could not be fingerprinted"))
}

fn secret_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
    let file = match std::fs::File::open(config::secrets_path(path)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_SECRET_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= MAX_SECRET_FILE_BYTES as usize,
        "verification secrets exceed the byte limit"
    );
    Ok(bytes)
}

fn rejected(message: &'static str) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Rejected, 409, message)
}

#[cfg(test)]
#[path = "authentication_verification_tests.rs"]
mod tests;
