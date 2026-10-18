use super::model::{MAX_CREATOR_GRANTS, MAX_GRANT_LIFETIME_MS};
use super::*;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Directory(PathBuf);

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Fixture {
    store: Store,
    directory: Directory,
    now: Clock,
    intent: CreateGrant,
    context: SessionContext,
}

impl Fixture {
    fn new() -> Self {
        let root = temporary_base().join(format!("keeppeek-shares-{}", Uuid::new_v4()));
        let now = Clock {
            unix_ms: 1_000_000,
            monotonic: Instant::now(),
        };
        let store = Store::open(&root.join("grants.db"), now.unix_ms).unwrap();
        let binding = ExportBinding {
            job_id: Uuid::new_v4().to_string(),
            artifact_id: Uuid::new_v4().to_string(),
            sha256: [17; 32],
            source_id: "camera-1".into(),
            source_identity: Uuid::new_v4(),
            start_ms: 1_000,
            end_ms: 11_000,
            bytes: 1_024,
            available_until_ms: now.unix_ms + 86_400_000,
        };
        Self {
            store,
            directory: Directory(root),
            now,
            intent: CreateGrant {
                request_id: Uuid::new_v4(),
                issuer: IssuerBinding::Credential {
                    id: Uuid::from_u128(1),
                    revision: 1,
                },
                binding,
                label: "Front door evidence".into(),
                note: String::new(),
                expires_at_ms: now.unix_ms + 60_000,
                allow_stream: true,
                allow_download: false,
                max_views: Some(1),
                max_downloads: Some(1),
            },
            context: SessionContext {
                origin: "https://keeppeek.example".into(),
                policy_revision: 1,
            },
        }
    }

    fn grant(&mut self) -> (GrantSummary, String) {
        let issued = self.store.create(self.intent.clone(), self.now).unwrap();
        (issued.grant, issued.token.into_string())
    }

    fn session(&mut self, token: &str) -> String {
        self.store
            .exchange(token, &self.context, self.now)
            .unwrap()
            .handle
            .into_string()
    }

    fn begin(&mut self, session: &str, mode: Mode) -> Result<Ticket, Error> {
        self.store
            .begin_delivery(session, &self.context, &self.intent.binding, mode, self.now)
    }

    fn commit(&mut self, ticket: &Ticket) -> Result<Access, Error> {
        self.store
            .commit_delivery(ticket, &self.context, &self.intent.binding, self.now)
    }

    fn authorize(&mut self, ticket: &Ticket, now: Clock) -> Result<Access, Error> {
        self.store
            .authorize(ticket, &self.context, &self.intent.binding, now)
    }
}

#[test]
fn issued_secrets_are_redacted_and_only_hashes_are_durable() {
    let mut f = Fixture::new();
    let issued = f.store.create(f.intent.clone(), f.now).unwrap();
    let debug = format!("{issued:?} {:?}", f.store);
    let token = issued.token.into_string();
    assert_eq!(
        base64::Engine::decode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &token)
            .unwrap()
            .len(),
        32
    );
    assert!(!debug.contains(&token));
    let session = f.store.exchange(&token, &f.context, f.now).unwrap();
    assert!(!session.id.is_nil());

    let session_debug = format!("{session:?} {:?}", f.store);
    let handle = session.handle.into_string();
    let bound = f
        .store
        .inspect_session(&handle, Some(session.id), &f.context, f.now)
        .unwrap();
    assert_eq!(bound.session_id, session.id);
    assert_eq!(bound.grant.id, issued.grant.id);
    assert!(bound.media_active);
    assert_eq!(
        bound.media_deadline.unwrap().expires_at_ms,
        issued.grant.intent.expires_at_ms
    );
    assert_ne!(token, handle);
    assert!(!session_debug.contains(&handle));
    f.store.checkpoint().unwrap();
    for file in std::fs::read_dir(&f.directory.0).unwrap() {
        let file = file.unwrap();
        if file.file_name() == std::ffi::OsStr::new("grants.db.owner.lock") {
            // The exclusive lock prevents Windows reads; an empty file cannot contain a secret.
            assert_eq!(file.metadata().unwrap().len(), 0);
            continue;
        }
        let bytes = std::fs::read(file.path()).unwrap();
        assert!(
            !bytes
                .windows(token.len())
                .any(|window| window == token.as_bytes())
        );
        assert!(
            !bytes
                .windows(handle.len())
                .any(|window| window == handle.as_bytes())
        );
    }
}

#[test]
fn retrying_creation_never_reveals_or_reissues_a_token() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    assert_eq!(
        f.store.create(f.intent.clone(), f.now).unwrap_err(),
        Error::Conflict
    );
    let mut changed = f.intent.clone();
    changed.label = "Different evidence".into();
    assert_eq!(f.store.create(changed, f.now).unwrap_err(), Error::Conflict);
    let listed = f.store.list(&f.intent.issuer, f.now).unwrap();
    assert_eq!(listed, vec![grant]);
    assert!(
        f.store
            .list(
                &IssuerBinding::ExternalIdentity {
                    id: Uuid::from_u128(1),
                    revision: 1
                },
                f.now
            )
            .unwrap()
            .is_empty()
    );
    assert!(!format!("{listed:?}").contains(&token));
    assert!(f.store.exchange(&token, &f.context, f.now).is_ok());
}

#[test]
fn an_abandoned_view_reservation_releases_capacity_without_charging() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    let first = f.session(&token);
    let second = f.session(&token);
    let pending = f.begin(&first, Mode::Stream).unwrap();
    assert_eq!(f.begin(&second, Mode::Stream).unwrap_err(), Error::Denied);
    assert_eq!(f.authorize(&pending, f.now).unwrap_err(), Error::Denied);
    assert_eq!(f.store.list(&f.intent.issuer, f.now).unwrap()[0].views, 0);
    f.store.finish(pending);
    let accepted = f.begin(&second, Mode::Stream).unwrap();
    f.commit(&accepted).unwrap();
    assert_eq!(f.store.list(&f.intent.issuer, f.now).unwrap()[0].views, 1);
    assert_eq!(
        f.store.list(&f.intent.issuer, f.now).unwrap()[0].id,
        grant.id
    );
}

#[test]
fn first_byte_commit_and_repeated_seeks_charge_one_view_per_session() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let session = f.session(&token);
    let first = f.begin(&session, Mode::Stream).unwrap();
    let parallel = f.begin(&session, Mode::Stream).unwrap();
    f.commit(&first).unwrap();
    f.commit(&first).unwrap();
    f.commit(&parallel).unwrap();
    f.authorize(&first, f.now).unwrap();
    f.store.finish(first);
    f.store.finish(parallel);
    let seek = f.begin(&session, Mode::Stream).unwrap();
    f.commit(&seek).unwrap();
    assert_eq!(f.store.list(&f.intent.issuer, f.now).unwrap()[0].views, 1);
    let another = f.session(&token);
    assert_eq!(f.begin(&another, Mode::Stream).unwrap_err(), Error::Denied);
}

#[test]
fn pending_reservations_expire_without_wall_clock_progress() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let first = f.session(&token);
    let second = f.session(&token);
    let pending = f.begin(&first, Mode::Stream).unwrap();
    f.now.monotonic += Duration::from_secs(31);
    assert_eq!(f.commit(&pending).unwrap_err(), Error::Denied);
    let accepted = f.begin(&second, Mode::Stream).unwrap();
    f.commit(&accepted).unwrap();
    assert_eq!(f.store.list(&f.intent.issuer, f.now).unwrap()[0].views, 1);
}

#[test]
fn downloads_have_separate_permissions_reservations_and_durable_counts() {
    let mut f = Fixture::new();
    f.intent.allow_download = true;
    let (_, token) = f.grant();
    let session = f.session(&token);
    let pending = f.begin(&session, Mode::Download).unwrap();
    assert_eq!(
        f.begin(&session, Mode::Download).unwrap_err(),
        Error::Denied
    );
    f.store.finish(pending);
    let download = f.begin(&session, Mode::Download).unwrap();
    f.commit(&download).unwrap();
    f.commit(&download).unwrap();
    f.store.finish(download);
    assert_eq!(
        f.begin(&session, Mode::Download).unwrap_err(),
        Error::Denied
    );
    let stream = f.begin(&session, Mode::Stream).unwrap();
    f.commit(&stream).unwrap();
    let record = &f.store.list(&f.intent.issuer, f.now).unwrap()[0];
    assert_eq!(record.downloads, 1);
    assert_eq!(record.views, 1);
}

#[test]
fn stream_permission_does_not_grant_download_permission() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let session = f.session(&token);
    assert_eq!(
        f.begin(&session, Mode::Download).unwrap_err(),
        Error::Denied
    );
    assert!(f.begin(&session, Mode::Stream).is_ok());
}

#[test]
fn revocation_invalidates_pending_and_committed_tickets() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    let session = f.session(&token);
    let active = f.begin(&session, Mode::Stream).unwrap();
    f.commit(&active).unwrap();
    let pending = f.begin(&session, Mode::Stream).unwrap();
    assert_eq!(
        f.store
            .revoke(grant.id, grant.revision + 1, "withdrawn", f.now)
            .unwrap_err(),
        Error::Conflict
    );
    let revoked = f
        .store
        .revoke(grant.id, grant.revision, "withdrawn", f.now)
        .unwrap();
    assert_eq!(revoked.revision, grant.revision + 1);
    assert_eq!(f.commit(&pending).unwrap_err(), Error::Denied);
    assert_eq!(f.authorize(&active, f.now).unwrap_err(), Error::Denied);
    assert_eq!(
        f.store.exchange(&token, &f.context, f.now).unwrap_err(),
        Error::Denied
    );
    assert_eq!(
        f.store
            .exchange(&"x".repeat(43), &f.context, f.now)
            .unwrap_err(),
        Error::Denied
    );
}

#[test]
fn expiry_and_monotonic_session_deadlines_fail_closed() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let session = f.session(&token);
    let ticket = f.begin(&session, Mode::Stream).unwrap();
    f.commit(&ticket).unwrap();
    let expiry = Clock {
        unix_ms: f.intent.expires_at_ms,
        monotonic: f.now.monotonic,
    };
    assert_eq!(f.authorize(&ticket, expiry).unwrap_err(), Error::Denied);
    assert_eq!(
        f.store.exchange(&token, &f.context, expiry).unwrap_err(),
        Error::Denied
    );
    let mut other = Fixture::new();
    other.intent.expires_at_ms = other.now.unix_ms + 600_000;
    let (_, token) = other.grant();
    let handle = other.session(&token);
    let ticket = other.begin(&handle, Mode::Stream).unwrap();
    other.commit(&ticket).unwrap();
    let frozen_wall = Clock {
        unix_ms: other.now.unix_ms,
        monotonic: other.now.monotonic + Duration::from_secs(301),
    };
    assert_eq!(
        other.authorize(&ticket, frozen_wall).unwrap_err(),
        Error::Denied
    );
}

#[test]
fn context_and_artifact_generation_must_match_on_every_chunk() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let handle = f.session(&token);
    let ticket = f.begin(&handle, Mode::Stream).unwrap();
    f.commit(&ticket).unwrap();
    let wrong_context = SessionContext {
        origin: "https://other.example".into(),
        ..f.context.clone()
    };
    assert_eq!(
        f.store
            .authorize(&ticket, &wrong_context, &f.intent.binding, f.now)
            .unwrap_err(),
        Error::Denied
    );
    let new_policy = SessionContext {
        policy_revision: 2,
        ..f.context.clone()
    };
    assert_eq!(
        f.store
            .authorize(&ticket, &new_policy, &f.intent.binding, f.now)
            .unwrap_err(),
        Error::Denied
    );
    let mut replacement = f.intent.binding.clone();
    replacement.artifact_id = Uuid::new_v4().to_string();
    assert_eq!(
        f.store
            .authorize(&ticket, &f.context, &replacement, f.now)
            .unwrap_err(),
        Error::Denied
    );
    replacement = f.intent.binding.clone();
    replacement.sha256[0] ^= 1;
    assert_eq!(
        f.store
            .authorize(&ticket, &f.context, &replacement, f.now)
            .unwrap_err(),
        Error::Denied
    );
    assert!(f.authorize(&ticket, f.now).is_ok());
}

#[test]
fn failed_persistence_issues_no_secret_and_no_delivery() {
    let mut f = Fixture::new();
    f.store.fail_next_commit();
    assert_eq!(
        f.store.create(f.intent.clone(), f.now).unwrap_err(),
        Error::Storage
    );
    assert!(f.store.list(&f.intent.issuer, f.now).unwrap().is_empty());
    let (_, token) = f.grant();
    let handle = f.session(&token);
    let ticket = f.begin(&handle, Mode::Stream).unwrap();
    f.store.fail_next_commit();
    assert_eq!(f.commit(&ticket).unwrap_err(), Error::Storage);
    assert_eq!(f.authorize(&ticket, f.now).unwrap_err(), Error::Denied);
    assert_eq!(f.store.list(&f.intent.issuer, f.now).unwrap()[0].views, 0);
    f.commit(&ticket).unwrap();
    assert_eq!(f.store.list(&f.intent.issuer, f.now).unwrap()[0].views, 1);
}

#[test]
fn invalid_inputs_and_artifact_horizons_do_not_create_grants() {
    let mut f = Fixture::new();
    let mut variants = Vec::new();
    let mut invalid = f.intent.clone();
    invalid.expires_at_ms = f.now.unix_ms;
    variants.push(invalid);
    let mut invalid = f.intent.clone();
    invalid.expires_at_ms = f.now.unix_ms + MAX_GRANT_LIFETIME_MS + 1;
    variants.push(invalid);
    let mut invalid = f.intent.clone();
    invalid.binding.available_until_ms = invalid.expires_at_ms - 1;
    variants.push(invalid);
    let mut invalid = f.intent.clone();
    invalid.binding.bytes = MAX_MEDIA_BYTES + 1;
    variants.push(invalid);
    let mut invalid = f.intent.clone();
    invalid.binding.end_ms = invalid.binding.start_ms + 120_001;
    variants.push(invalid);
    let mut invalid = f.intent.clone();
    invalid.label = "x".repeat(129);
    variants.push(invalid);
    let mut invalid = f.intent.clone();
    invalid.note = "x".repeat(513);
    variants.push(invalid);
    let mut invalid = f.intent.clone();
    invalid.max_views = Some(0);
    variants.push(invalid);
    let mut invalid = f.intent.clone();
    invalid.allow_stream = false;
    variants.push(invalid);
    for intent in variants {
        assert_eq!(f.store.create(intent, f.now).unwrap_err(), Error::Invalid);
    }
    assert!(f.store.list(&f.intent.issuer, f.now).unwrap().is_empty());
}

#[test]
fn creator_and_session_capacity_rejects_without_evicting_existing_access() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let handle = f.session(&token);
    let _other = f.session(&token);
    assert_eq!(
        f.store.exchange(&token, &f.context, f.now).unwrap_err(),
        Error::Capacity
    );
    assert!(f.begin(&handle, Mode::Stream).is_ok());
    for _ in 1..MAX_CREATOR_GRANTS {
        let mut intent = f.intent.clone();
        intent.request_id = Uuid::new_v4();
        f.store.create(intent, f.now).unwrap();
    }
    let mut extra = f.intent.clone();
    extra.request_id = Uuid::new_v4();
    assert_eq!(f.store.create(extra, f.now).unwrap_err(), Error::Capacity);
    assert_eq!(
        f.store.list(&f.intent.issuer, f.now).unwrap().len(),
        MAX_CREATOR_GRANTS
    );
}

#[test]
fn owner_lock_rejects_aliases_and_releases_after_drop() {
    let f = Fixture::new();
    let path = f.directory.0.join(".").join("grants.db");
    assert_eq!(
        Store::open(&path, f.now.unix_ms).unwrap_err(),
        Error::Storage
    );
    let Fixture {
        store,
        directory,
        now,
        ..
    } = f;
    drop(store);
    let reopened = Store::open(&directory.0.join("grants.db"), now.unix_ms).unwrap();
    drop(reopened);
}

#[test]
fn restart_preserves_consumption_and_revocation_but_invalidates_sessions() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    let handle = f.session(&token);
    let pending = f.begin(&handle, Mode::Stream).unwrap();
    f.commit(&pending).unwrap();
    let Fixture {
        store,
        directory,
        now,
        context,
        intent,
    } = f;
    drop(store);
    let mut reopened = Store::open(&directory.0.join("grants.db"), now.unix_ms).unwrap();
    assert_eq!(
        reopened
            .begin_delivery(&handle, &context, &intent.binding, Mode::Stream, now)
            .unwrap_err(),
        Error::Denied
    );
    let new_handle = reopened
        .exchange(&token, &context, now)
        .unwrap()
        .handle
        .into_string();
    assert_eq!(
        reopened
            .begin_delivery(&new_handle, &context, &intent.binding, Mode::Stream, now)
            .unwrap_err(),
        Error::Denied
    );
    reopened
        .revoke(grant.id, grant.revision, "withdrawn", now)
        .unwrap();
    drop(reopened);
    let mut reopened = Store::open(&directory.0.join("grants.db"), now.unix_ms).unwrap();
    assert_eq!(
        reopened.exchange(&token, &context, now).unwrap_err(),
        Error::Denied
    );
    assert_eq!(reopened.list(&intent.issuer, now).unwrap()[0].views, 1);
}

#[test]
fn observed_expiry_cannot_be_reactivated_by_clock_rollback_or_restart() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let expired = Clock {
        unix_ms: f.intent.expires_at_ms,
        monotonic: f.now.monotonic,
    };
    assert_eq!(
        f.store.exchange(&token, &f.context, expired).unwrap_err(),
        Error::Denied
    );
    assert_eq!(
        f.store.exchange(&token, &f.context, f.now).unwrap_err(),
        Error::Denied
    );
    let Fixture {
        store,
        directory,
        now,
        context,
        ..
    } = f;
    drop(store);
    let path = directory.0.join("grants.db");
    assert_eq!(Store::open(&path, now.unix_ms).unwrap_err(), Error::Denied);
    let mut reopened = Store::open(&path, expired.unix_ms).unwrap();
    assert_eq!(
        reopened.exchange(&token, &context, expired).unwrap_err(),
        Error::Denied
    );
}

#[test]
fn malformed_evidence_identity_and_interval_inputs_are_rejected() {
    let mut f = Fixture::new();
    let edits: [fn(&mut CreateGrant); 10] = [
        |g| g.binding.start_ms = -1,
        |g| g.binding.end_ms = g.binding.start_ms,
        |g| g.binding.end_ms = i64::MAX,
        |g| g.binding.bytes = 0,
        |g| g.binding.job_id.clear(),
        |g| g.binding.artifact_id = "../replacement".into(),
        |g| g.binding.source_id = "x".repeat(257),
        |g| {
            g.issuer = IssuerBinding::Credential {
                id: Uuid::nil(),
                revision: 1,
            }
        },
        |g| g.request_id = Uuid::nil(),
        |g| g.max_downloads = Some(0),
    ];
    for edit in edits {
        let mut intent = f.intent.clone();
        edit(&mut intent);
        assert_eq!(f.store.create(intent, f.now).unwrap_err(), Error::Invalid);
    }
    assert!(f.store.list(&f.intent.issuer, f.now).unwrap().is_empty());
}

#[test]
fn hardlinked_database_aliases_are_rejected() {
    let f = Fixture::new();
    let alias = f.directory.0.join("alias.db");
    std::fs::hard_link(f.directory.0.join("grants.db"), &alias).unwrap();
    assert_eq!(
        Store::open(&alias, f.now.unix_ms).unwrap_err(),
        Error::Storage
    );
}

#[test]
fn token_lookup_rejects_malformed_or_cross_type_credentials() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let session = f.session(&token);
    for malformed in [
        "".to_owned(),
        "a".repeat(42),
        "a".repeat(44),
        "ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â‚¬Å¾Ã‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¾Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã‚Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã‚Â¾ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â‚¬Å¾Ã‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã‚Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã¢â‚¬Â¦Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã¢â‚¬Â¦Ãƒâ€šÃ‚Â¾ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â‚¬Å¾Ã‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¾Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã¢â‚¬Â¦Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â¦ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â‚¬Å¾Ã‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã‚Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â¦ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã¢â‚¬Â¦Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã‚Â¡ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â©".repeat(43),
        session,
    ] {
        assert_eq!(
            f.store.exchange(&malformed, &f.context, f.now).unwrap_err(),
            Error::Denied
        );
    }
    assert_eq!(f.begin(&token, Mode::Stream).unwrap_err(), Error::Denied);
}

#[test]
fn expiry_observed_on_an_active_transfer_survives_restart_rollback() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let handle = f.session(&token);
    let ticket = f.begin(&handle, Mode::Stream).unwrap();
    f.commit(&ticket).unwrap();
    let expired = Clock {
        unix_ms: f.intent.expires_at_ms,
        monotonic: f.now.monotonic,
    };
    assert_eq!(f.authorize(&ticket, expired).unwrap_err(), Error::Denied);
    let Fixture {
        store,
        directory,
        now,
        ..
    } = f;
    drop(store);
    assert_eq!(
        Store::open(&directory.0.join("grants.db"), now.unix_ms).unwrap_err(),
        Error::Denied
    );
}

#[test]
fn failed_revoke_preserves_authority_and_success_frees_session_capacity() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    let handle = f.session(&token);
    let _second = f.session(&token);
    let ticket = f.begin(&handle, Mode::Stream).unwrap();
    f.commit(&ticket).unwrap();
    f.store.fail_next_commit();
    assert_eq!(
        f.store
            .revoke(grant.id, grant.revision, "withdrawn", f.now)
            .unwrap_err(),
        Error::Storage
    );
    assert!(f.authorize(&ticket, f.now).is_ok());
    f.store
        .revoke(grant.id, grant.revision, "withdrawn", f.now)
        .unwrap();
    assert_eq!(f.authorize(&ticket, f.now).unwrap_err(), Error::Denied);
    for _ in 0..4 {
        f.intent.request_id = Uuid::new_v4();
        let (_, next) = f.grant();
        f.session(&next);
        f.session(&next);
    }
}

#[test]
fn every_immutable_binding_field_is_rechecked() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let handle = f.session(&token);
    let ticket = f.begin(&handle, Mode::Stream).unwrap();
    f.commit(&ticket).unwrap();
    let edits: [fn(&mut ExportBinding); 7] = [
        |b| b.job_id = "replacement-job".into(),
        |b| b.source_id = "other-camera".into(),
        |b| b.source_identity = Uuid::new_v4(),
        |b| b.bytes += 1,
        |b| b.start_ms -= 1,
        |b| b.end_ms += 1,
        |b| b.available_until_ms -= 1,
    ];
    for edit in edits {
        let mut changed = f.intent.binding.clone();
        edit(&mut changed);
        assert_eq!(
            f.store
                .authorize(&ticket, &f.context, &changed, f.now)
                .unwrap_err(),
            Error::Denied
        );
    }
    assert!(f.authorize(&ticket, f.now).is_ok());
}

#[test]
fn named_export_jobs_are_valid_but_artifact_generations_must_be_uuids() {
    let mut f = Fixture::new();
    f.intent.binding.job_id = "named-export".into();
    let (_, token) = f.grant();
    assert!(f.store.exchange(&token, &f.context, f.now).is_ok());
    f.intent.request_id = Uuid::new_v4();
    f.intent.binding.artifact_id = "not-a-uuid".into();
    assert_eq!(
        f.store.create(f.intent.clone(), f.now).unwrap_err(),
        Error::Invalid
    );
    f.intent.binding.artifact_id = Uuid::new_v4().simple().to_string();
    f.intent.binding.job_id = "x".repeat(65);
    assert_eq!(
        f.store.create(f.intent.clone(), f.now).unwrap_err(),
        Error::Invalid
    );
}

#[test]
fn global_session_and_ticket_limits_preserve_existing_viewers() {
    let mut f = Fixture::new();
    f.intent.max_views = None;
    let mut handles = Vec::new();
    for _ in 0..4 {
        f.intent.request_id = Uuid::new_v4();
        let (_, token) = f.grant();
        handles.push(f.session(&token));
        handles.push(f.session(&token));
    }
    f.intent.request_id = Uuid::new_v4();
    let (_, token) = f.grant();
    assert_eq!(
        f.store.exchange(&token, &f.context, f.now).unwrap_err(),
        Error::Capacity
    );
    let tickets: Vec<_> = handles
        .iter()
        .map(|handle| f.begin(handle, Mode::Stream).unwrap())
        .collect();
    assert_eq!(
        f.begin(&handles[0], Mode::Stream).unwrap_err(),
        Error::Capacity
    );
    for ticket in &tickets {
        f.commit(ticket).unwrap();
    }
    for ticket in tickets {
        f.store.finish(ticket);
    }
    assert!(f.begin(&handles[0], Mode::Stream).is_ok());
}

#[test]
fn a_restart_drops_pending_reservations_without_spending_views() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let handle = f.session(&token);
    let _pending = f.begin(&handle, Mode::Stream).unwrap();
    let Fixture {
        store,
        directory,
        now,
        context,
        intent,
    } = f;
    drop(store);
    let mut reopened = Store::open(&directory.0.join("grants.db"), now.unix_ms).unwrap();
    let handle = reopened
        .exchange(&token, &context, now)
        .unwrap()
        .handle
        .into_string();
    let ticket = reopened
        .begin_delivery(&handle, &context, &intent.binding, Mode::Stream, now)
        .unwrap();
    reopened
        .commit_delivery(&ticket, &context, &intent.binding, now)
        .unwrap();
    assert_eq!(reopened.list(&intent.issuer, now).unwrap()[0].views, 1);
}

fn raw_connection(directory: &Directory) -> turso::Connection {
    let path = directory.0.join("grants.db");
    let database =
        pollster::block_on(turso::Builder::new_local(path.to_str().unwrap()).build()).unwrap();
    database.connect().unwrap()
}

struct Seed {
    count: usize,
    terminal: bool,
    group_creators: bool,
    created_ms: i64,
}

fn seed_history(fixture: &Fixture, seed: Seed) {
    let connection = raw_connection(&fixture.directory);
    pollster::block_on(async {
        connection.execute_batch("BEGIN IMMEDIATE").await.unwrap();
        for index in 0..seed.count {
            let mut intent = fixture.intent.clone();
            intent.request_id = Uuid::new_v4();
            if seed.group_creators {
                intent.issuer = IssuerBinding::Credential {
                    id: Uuid::from_u128(u128::try_from(index / MAX_CREATOR_GRANTS + 1).unwrap()),
                    revision: 1,
                };
            }
            let record = super::record::Record {
                intent,
                revision: if seed.terminal { 2 } else { 1 },
                created_at_ms: seed.created_ms,
                revoked_at_ms: seed.terminal.then_some(fixture.now.unix_ms),
                revoke_reason: seed.terminal.then(|| "rotated".into()),
                views: 0,
                downloads: 0,
                last_access_at_ms: None,
            };
            let mut hash = [0u8; 32];
            hash[..8].copy_from_slice(&u64::try_from(index).unwrap().to_le_bytes());
            connection.execute("INSERT INTO guest_grants(id,token_hash,creator,expires_ms,ended_ms,payload) VALUES(?1,?2,?3,?4,?5,?6)",
                turso::params![record.intent.request_id.to_string(), hash.as_slice(), record.intent.issuer.creator_id(),
                    record.intent.expires_at_ms, record.revoked_at_ms, serde_json::to_vec(&record).unwrap()]).await.unwrap();
        }
        connection.execute_batch("COMMIT").await.unwrap();
    });
}
#[test]
fn global_active_grants_are_bounded_independently_of_creator_limits() {
    let mut f = Fixture::new();
    for index in 0..super::model::MAX_ACTIVE_GRANTS {
        f.intent.request_id = Uuid::new_v4();
        f.intent.issuer = IssuerBinding::Credential {
            id: Uuid::from_u128(u128::try_from(index / MAX_CREATOR_GRANTS + 1).unwrap()),
            revision: 1,
        };
        f.grant();
    }
    f.intent.request_id = Uuid::new_v4();
    f.intent.issuer = IssuerBinding::ExternalIdentity {
        id: Uuid::new_v4(),
        revision: 1,
    };
    assert_eq!(
        f.store.create(f.intent.clone(), f.now).unwrap_err(),
        Error::Capacity
    );
    assert_eq!(
        f.store
            .list(
                &IssuerBinding::Credential {
                    id: Uuid::from_u128(1),
                    revision: 1
                },
                f.now
            )
            .unwrap()
            .len(),
        MAX_CREATOR_GRANTS
    );
}

#[test]
fn retained_audit_saturation_rejects_creation_and_purges_in_bounded_batches() {
    let mut f = Fixture::new();
    seed_history(
        &f,
        Seed {
            count: super::model::MAX_GRANT_RECORDS,
            terminal: true,
            group_creators: false,
            created_ms: f.now.unix_ms,
        },
    );
    assert_eq!(
        f.store.create(f.intent.clone(), f.now).unwrap_err(),
        Error::Capacity
    );
    assert_eq!(f.store.list(&f.intent.issuer, f.now).unwrap().len(), 4_096);
    f.now.unix_ms += super::model::AUDIT_RETENTION_MS - 1;
    f.now.monotonic +=
        Duration::from_millis(u64::try_from(super::model::AUDIT_RETENTION_MS - 1).unwrap());
    assert_eq!(f.store.purge(f.now).unwrap(), 0);
    f.now.unix_ms += 1;
    f.now.monotonic += Duration::from_millis(1);
    assert_eq!(f.store.purge(f.now).unwrap(), 256);
    assert_eq!(f.store.list(&f.intent.issuer, f.now).unwrap().len(), 3_840);
    f.intent.expires_at_ms = f.now.unix_ms + 60_000;
    f.intent.binding.available_until_ms = f.intent.expires_at_ms;
    assert!(f.store.create(f.intent.clone(), f.now).is_ok());
}

#[test]
fn revoking_expired_evidence_does_not_extend_its_audit_retention() {
    let mut f = Fixture::new();
    let (grant, _) = f.grant();
    f.now.unix_ms = f.intent.expires_at_ms + 5_000;
    f.store
        .revoke(grant.id, grant.revision, "withdrawn", f.now)
        .unwrap();
    f.now.unix_ms = f.intent.expires_at_ms + super::model::AUDIT_RETENTION_MS;
    assert_eq!(f.store.purge(f.now).unwrap(), 1);
    assert!(f.store.list(&f.intent.issuer, f.now).unwrap().is_empty());
}

#[test]
fn repeated_mutations_truncate_the_wal_and_bound_physical_state() {
    let mut f = Fixture::new();
    for _ in 0..40 {
        f.intent.request_id = Uuid::new_v4();
        let (grant, _) = f.grant();
        f.store
            .revoke(grant.id, grant.revision, "rotated", f.now)
            .unwrap();
        let wal = f.directory.0.join("grants.db-wal");
        if wal.exists() {
            assert_eq!(std::fs::metadata(wal).unwrap().len(), 0);
        }
    }
    assert!(
        std::fs::metadata(f.directory.0.join("grants.db"))
            .unwrap()
            .len()
            <= 64 * 1_024 * 1_024
    );
}

#[test]
fn oversize_database_and_unknown_schema_fail_before_guest_access() {
    let f = Fixture::new();
    let Fixture {
        store,
        directory,
        now,
        ..
    } = f;
    drop(store);
    let path = directory.0.join("grants.db");
    let connection = raw_connection(&directory);
    pollster::block_on(connection.execute_batch("PRAGMA user_version=99")).unwrap();
    drop(connection);
    assert_eq!(Store::open(&path, now.unix_ms).unwrap_err(), Error::Storage);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(64 * 1_024 * 1_024 + 1)
        .unwrap();
    assert_eq!(Store::open(&path, now.unix_ms).unwrap_err(), Error::Storage);
}

#[test]
fn token_hashes_are_not_exposed_by_creator_or_session_debug_views() {
    let mut f = Fixture::new();
    let issued = f.store.create(f.intent.clone(), f.now).unwrap();
    let debug = format!("{issued:?} {:?}", f.store);
    let token = issued.token.into_string();
    let hash = super::secret::digest(&token);
    let session = f.store.exchange(&token, &f.context, f.now).unwrap();
    let listed = f.store.list(&f.intent.issuer, f.now).unwrap();
    let debug = format!("{debug} {session:?} {listed:?} {:?}", f.store);
    assert!(!debug.contains(&format!("{hash:?}")));
    let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
    assert!(!debug.contains(&hex));
}

fn temporary_base() -> PathBuf {
    #[cfg(unix)]
    {
        std::fs::canonicalize(std::env::temp_dir()).unwrap()
    }
    #[cfg(not(unix))]
    {
        std::env::temp_dir()
    }
}

#[test]
fn oversized_legacy_payload_is_denied_without_materializing_the_blob() {
    let mut f = Fixture::new();
    let (grant, token) = f.grant();
    let connection = raw_connection(&f.directory);
    pollster::block_on(async {
        connection.execute_batch("DROP TABLE guest_grants;
            CREATE TABLE guest_grants(id TEXT PRIMARY KEY,token_hash BLOB,creator TEXT,expires_ms INTEGER,ended_ms INTEGER,payload BLOB)").await.unwrap();
        let hash = super::secret::digest(&token);
        connection
            .execute(
                "INSERT INTO guest_grants VALUES(?1,?2,?3,?4,NULL,zeroblob(1048576))",
                turso::params![
                    grant.id.to_string(),
                    hash.as_slice(),
                    grant.intent.issuer.creator_id(),
                    grant.intent.expires_at_ms
                ],
            )
            .await
            .unwrap();
    });
    assert_eq!(
        f.store.exchange(&token, &f.context, f.now).unwrap_err(),
        Error::Storage
    );
    let Fixture {
        store,
        directory,
        now,
        ..
    } = f;
    drop(connection);
    drop(store);
    assert_eq!(
        Store::open(&directory.0.join("grants.db"), now.unix_ms).unwrap_err(),
        Error::Storage
    );
}

#[test]
fn missing_durable_clock_poisoning_denies_existing_delivery() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let handle = f.session(&token);
    let ticket = f.begin(&handle, Mode::Stream).unwrap();
    let connection = raw_connection(&f.directory);
    pollster::block_on(connection.execute("DELETE FROM guest_meta", ())).unwrap();
    assert_eq!(f.commit(&ticket).unwrap_err(), Error::Storage);
    assert_eq!(f.authorize(&ticket, f.now).unwrap_err(), Error::Storage);
}

#[cfg(unix)]
#[test]
fn an_existing_shared_parent_is_rejected_without_changing_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let directory = Directory(temporary_base().join(format!("keeppeek-shared-{}", Uuid::new_v4())));
    std::fs::create_dir(&directory.0).unwrap();
    std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        Store::open(&directory.0.join("grants.db"), 1_000_000).unwrap_err(),
        Error::Storage
    );
    assert_eq!(
        std::fs::metadata(&directory.0)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn restored_state_rejects_excess_active_grants_and_future_security_events() {
    for (count, group_creators, future_created) in
        [(257, true, false), (33, false, false), (1, false, true)]
    {
        let f = Fixture::new();
        seed_history(
            &f,
            Seed {
                count,
                terminal: false,
                group_creators,
                created_ms: f.now.unix_ms + if future_created { 1 } else { 0 },
            },
        );
        let Fixture {
            store,
            directory,
            now,
            ..
        } = f;
        drop(store);
        assert_eq!(
            Store::open(
                &directory.0.join("grants.db"),
                now.unix_ms + MAX_GRANT_LIFETIME_MS
            )
            .unwrap_err(),
            Error::Storage
        );
    }
}

#[test]
fn two_viewer_reservations_contend_for_one_remaining_view() {
    use std::sync::{Arc, Barrier, Mutex};
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let first = f.session(&token);
    let second = f.session(&token);
    let Fixture {
        store,
        directory: _directory,
        now,
        intent,
        context,
    } = f;
    let shared = Arc::new(Mutex::new(store));
    let barrier = Arc::new(Barrier::new(2));
    let mut threads = Vec::new();
    for handle in [first, second] {
        let shared = Arc::clone(&shared);
        let barrier = Arc::clone(&barrier);
        let context = context.clone();
        let binding = intent.binding.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            shared
                .lock()
                .unwrap()
                .begin_delivery(&handle, &context, &binding, Mode::Stream, now)
        }));
    }
    let results: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(Error::Denied)))
            .count(),
        1
    );
    let ticket = results.into_iter().find_map(Result::ok).unwrap();
    let mut store = shared.lock().unwrap();
    let access = store
        .commit_delivery(&ticket, &context, &intent.binding, now)
        .unwrap();
    assert_eq!(access.binding, intent.binding);
    assert_eq!(access.mode, Mode::Stream);
    assert!(access.expires_at_ms <= intent.expires_at_ms);
    assert_eq!(store.list(&intent.issuer, now).unwrap()[0].views, 1);
}

#[test]
fn checkpoint_failure_after_commit_never_refunds_consumption_or_allows_retry() {
    let mut f = Fixture::new();
    let (_, token) = f.grant();
    let handle = f.session(&token);
    let ticket = f.begin(&handle, Mode::Stream).unwrap();
    f.store.fail_next_checkpoint();
    assert_eq!(f.commit(&ticket).unwrap_err(), Error::Storage);
    assert_eq!(f.authorize(&ticket, f.now).unwrap_err(), Error::Storage);
    assert_eq!(f.commit(&ticket).unwrap_err(), Error::Storage);
    let Fixture {
        store,
        directory,
        now,
        context,
        intent,
    } = f;
    drop(store);
    let mut reopened = Store::open(&directory.0.join("grants.db"), now.unix_ms).unwrap();
    assert_eq!(reopened.list(&intent.issuer, now).unwrap()[0].views, 1);
    let handle = reopened
        .exchange(&token, &context, now)
        .unwrap()
        .handle
        .into_string();
    assert_eq!(
        reopened
            .begin_delivery(&handle, &context, &intent.binding, Mode::Stream, now)
            .unwrap_err(),
        Error::Denied
    );
}

#[test]
fn issuer_kind_and_source_generation_round_trip_without_browser_identity() {
    let issuers = [
        IssuerBinding::LocalAdministrator,
        IssuerBinding::Credential {
            id: Uuid::new_v4(),
            revision: 7,
        },
        IssuerBinding::ExternalIdentity {
            id: Uuid::new_v4(),
            revision: 11,
        },
    ];
    for issuer in issuers {
        let mut f = Fixture::new();
        f.intent.issuer = issuer.clone();
        let (grant, _) = f.grant();
        let Fixture {
            store,
            directory,
            now,
            intent,
            ..
        } = f;
        drop(store);
        let mut reopened = Store::open(&directory.0.join("grants.db"), now.unix_ms).unwrap();
        let listed = reopened.list(&issuer, now).unwrap();
        assert_eq!(listed, vec![grant]);
        assert_eq!(listed[0].intent.issuer, issuer);
        assert_eq!(
            listed[0].intent.binding.source_identity,
            intent.binding.source_identity
        );
    }
}

#[test]
fn creator_keys_separate_identity_kinds_but_not_identity_revisions() {
    let mut f = Fixture::new();
    let (original, _) = f.grant();
    f.intent.issuer = IssuerBinding::ExternalIdentity {
        id: Uuid::from_u128(1),
        revision: 1,
    };
    f.intent.request_id = Uuid::new_v4();
    let (external, _) = f.grant();
    assert_eq!(
        f.store.list(&f.intent.issuer, f.now).unwrap(),
        vec![external]
    );
    f.intent.issuer = IssuerBinding::Credential {
        id: Uuid::from_u128(1),
        revision: 2,
    };
    assert_eq!(
        f.store.list(&f.intent.issuer, f.now).unwrap(),
        vec![original]
    );
    for _ in 1..MAX_CREATOR_GRANTS {
        f.intent.request_id = Uuid::new_v4();
        f.grant();
    }
    f.intent.issuer = IssuerBinding::Credential {
        id: Uuid::from_u128(1),
        revision: 999,
    };
    f.intent.request_id = Uuid::new_v4();
    assert_eq!(
        f.store.create(f.intent.clone(), f.now).unwrap_err(),
        Error::Capacity
    );
}

#[test]
fn nil_source_and_invalid_issuer_generations_never_create_authority() {
    let mut f = Fixture::new();
    let invalid_issuers = [
        IssuerBinding::Credential {
            id: Uuid::nil(),
            revision: 1,
        },
        IssuerBinding::Credential {
            id: Uuid::from_u128(1),
            revision: 0,
        },
        IssuerBinding::ExternalIdentity {
            id: Uuid::nil(),
            revision: 1,
        },
        IssuerBinding::ExternalIdentity {
            id: Uuid::from_u128(1),
            revision: 0,
        },
    ];
    for issuer in invalid_issuers {
        let mut intent = f.intent.clone();
        intent.issuer = issuer;
        assert_eq!(f.store.create(intent, f.now).unwrap_err(), Error::Invalid);
    }
    f.intent.binding.source_identity = Uuid::nil();
    assert_eq!(
        f.store.create(f.intent.clone(), f.now).unwrap_err(),
        Error::Invalid
    );
    assert!(f.store.list(&f.intent.issuer, f.now).unwrap().is_empty());
}

#[path = "tests/status.rs"]
mod status;
