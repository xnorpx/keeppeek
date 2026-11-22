use super::*;
use crate::{
    config::{self, MetadataBinding},
    shares::{BoundSession, CreateGrant, IssuerBinding},
    shutdown::Shutdown,
    storage::{RecordingCatalog, volumes::runtime::Manager},
};
use sha2::{Digest, Sha256};
use std::{path::PathBuf, sync::Arc, thread::JoinHandle, time::Duration};
use uuid::Uuid;

struct Evidence {
    state: Option<ServerState>,
    root: PathBuf,
    catalog: Option<RecordingCatalog>,
    manager: Option<Manager>,
    shutdown: Shutdown,
    watchdog: Option<JoinHandle<()>>,
    context: SessionContext,
    grant: GrantSummary,
    token: String,
    session: String,
    bound: BoundSession,
}

impl Evidence {
    fn new(allow_download: bool) -> Self {
        let (root, catalog, manager, mut state, request) =
            super::super::super::export_storage_tests::setup(16 * 1024 * 1024);
        let path = super::super::super::export_provenance_tests::configured_source(
            &mut state,
            &root,
            Uuid::new_v4(),
        );
        let mut config = config::load_config(&path).unwrap();
        config.sharing.enabled = true;
        config.sharing.public_origin = Some("https://keeppeek.example".into());
        pin_metadata(&mut state, &manager);
        state.sharing = Arc::new(sharing::Runtime::new(&config));
        state.sharing.open(&state.storage_config).unwrap();
        let principal = super::super::super::ApiPrincipal::local("127.0.0.1".parse().unwrap());
        super::super::super::export_storage::provenance::create_for_principal(
            &state, &principal, request,
        )
        .unwrap();
        wait_for_export(&state);
        let binding = binding(&state);
        let (grant, token) = grant(&state, binding, allow_download);
        let context = sharing::context(&state).unwrap();
        let (bound, session) = exchange(&state, &context, &token);
        let shutdown = Shutdown::new();
        let watchdog = state
            .guest_review
            .start_deadline_watchdog(shutdown.clone())
            .unwrap();
        Self {
            state: Some(state),
            root,
            catalog: Some(catalog),
            manager: Some(manager),
            shutdown,
            watchdog: Some(watchdog),
            context,
            grant,
            token,
            session,
            bound,
        }
    }

    fn state(&self) -> &ServerState {
        self.state.as_ref().unwrap()
    }

    fn snapshot(&self) -> GrantSummary {
        sharing::with_store(self.state(), |store, clock| store.get(self.grant.id, clock))
            .unwrap()
            .unwrap()
    }

    fn request(&self, mode: Mode, range: Option<&str>) -> Request {
        let path = if mode == Mode::Stream {
            "/guest/media"
        } else {
            "/guest/download"
        };
        let mut headers = vec![(
            "Cookie".into(),
            format!("{}={}", super::super::http::COOKIE, self.session),
        )];
        if let Some(range) = range {
            headers.push(("Range".into(), range.into()));
        }
        Request::fake_https(
            "GET",
            format!("{path}?session={}", self.bound.session_id),
            headers,
            Vec::new(),
        )
    }
}

impl Drop for Evidence {
    fn drop(&mut self) {
        self.shutdown.cancel();
        if let Some(thread) = self.watchdog.take() {
            thread.join().unwrap();
        }
        drop(self.state.take());
        drop(self.manager.take());
        if let Some(catalog) = self.catalog.take() {
            catalog.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn pin_metadata(state: &mut ServerState, manager: &Manager) {
    let primary = &manager.configuration().volumes[0];
    let root = crate::storage::volumes::root::Root::open(&primary.root).unwrap();
    let identity = root.identity().clone();
    let handoff = Uuid::new_v4();
    let binding = MetadataBinding {
        volume_id: primary.id.clone(),
        catalog_file: format!("catalog-{handoff}.db"),
        history_file: format!("exports-{handoff}.json"),
        catalog_id: Uuid::new_v4().to_string(),
        generation: 1,
        filesystem: identity.filesystem,
        root_identity: identity.directory,
    };
    root.initialize_history(&binding.history_file).unwrap();
    state.storage_config.recording_catalog_path = primary.root.join(&binding.catalog_file);
    state.storage_config.metadata = Some(binding);
}

fn binding(state: &ServerState) -> ExportBinding {
    let record = super::super::super::export_provenance_tests::current_record(state);
    let bounds = record
        .verified_media_bounds()
        .expect("ready fixture export must have a generation-bound published media proof");
    assert!(bounds.end_ms > bounds.start_ms);
    assert!(bounds.end_ms - bounds.start_ms <= crate::shares::MAX_MEDIA_DURATION_MS);
    let bytes = std::fs::read(record.path.unwrap()).unwrap();
    let timestamp = |value: &Option<prost_types::Timestamp>| {
        super::super::super::required_timestamp_ms(value.as_ref(), "fixture timestamp").unwrap()
    };
    ExportBinding {
        job_id: record.job.job_id,
        artifact_id: record.artifact_id,
        sha256: Sha256::digest(&bytes).into(),
        source_id: record.job.source_id,
        source_identity: record.source_identity.unwrap(),
        start_ms: bounds.start_ms,
        end_ms: bounds.end_ms,
        bytes: bytes.len() as u64,
        available_until_ms: timestamp(&record.job.expires_at),
    }
}

fn grant(
    state: &ServerState,
    binding: ExportBinding,
    allow_download: bool,
) -> (GrantSummary, String) {
    sharing::with_store(state, |store, clock| {
        let issued = store.create(
            CreateGrant {
                request_id: Uuid::new_v4(),
                issuer: IssuerBinding::LocalAdministrator,
                binding,
                label: "Front door evidence".into(),
                note: "creator-only note".into(),
                expires_at_ms: clock.unix_ms + 60_000,
                allow_stream: true,
                allow_download,
                max_views: Some(1),
                max_downloads: Some(1),
            },
            clock,
        )?;
        Ok((issued.grant, issued.token.into_string()))
    })
    .unwrap()
}

fn exchange(state: &ServerState, context: &SessionContext, token: &str) -> (BoundSession, String) {
    sharing::with_guest_store(state, context, |store, clock| {
        let issued = store.exchange(token, context, clock)?;
        let handle = issued.handle.into_string();
        Ok((
            store.inspect_session(&handle, Some(issued.id), context, clock)?,
            handle,
        ))
    })
    .unwrap()
}

fn guard(f: &Evidence, harness: &mut super::super::registry::tests::Harness) -> RequestGuard {
    f.state()
        .guest_review
        .admit_request(
            "127.0.0.1".parse().unwrap(),
            super::super::limits::Action::Media,
            harness.request(),
        )
        .unwrap()
}

#[test]
fn real_export_first_positive_read_commits_once_and_seeks_do_not_consume_another_view() {
    let f = Evidence::new(false);
    let mut harness = super::super::registry::tests::Harness::new();
    let request = f.request(Mode::Stream, Some("bytes=0-31"));
    let response = prepare(
        &request,
        f.state(),
        &f.context,
        guard(&f, &mut harness),
        Mode::Stream,
    )
    .unwrap();
    assert_eq!(response.status_code, 206);
    let (mut reader, size) = response.data.into_reader_and_size();
    assert_eq!(size, Some(32));
    assert_eq!(f.snapshot().views, 0);
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    assert_eq!(f.snapshot().views, 0);
    let mut first = [0; 8];
    assert_eq!(reader.read(&mut first).unwrap(), 8);
    assert_eq!(f.snapshot().views, 1);
    let mut remaining = Vec::new();
    reader.read_to_end(&mut remaining).unwrap();
    assert_eq!(remaining.len(), 24);
    assert_eq!(f.snapshot().views, 1);
    drop(reader);
    harness.finish(0);
    let next = f.request(Mode::Stream, Some("bytes=32-47"));
    let response = prepare(
        &next,
        f.state(),
        &f.context,
        guard(&f, &mut harness),
        Mode::Stream,
    )
    .unwrap();
    let mut reader = response.data.into_reader_and_size().0;
    assert_eq!(reader.read(&mut first).unwrap(), 8);
    assert_eq!(f.snapshot().views, 1);
    let original = std::fs::read(
        super::super::super::export_provenance_tests::current_record(f.state())
            .path
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first, original[32..40]);
}

#[test]
fn failed_durable_first_read_returns_no_bytes_and_releases_unconsumed_reservation() {
    let f = Evidence::new(false);
    let mut harness = super::super::registry::tests::Harness::new();
    let response = prepare(
        &f.request(Mode::Stream, Some("bytes=0-15")),
        f.state(),
        &f.context,
        guard(&f, &mut harness),
        Mode::Stream,
    )
    .unwrap();
    sharing::with_store(f.state(), |store, _| {
        store.fail_next_commit();
        Ok(())
    })
    .unwrap();
    let mut reader = response.data.into_reader_and_size().0;
    let mut output = [0xa5; 16];
    assert!(reader.read(&mut output).is_err());
    assert_eq!(output, [0xa5; 16]);
    assert_eq!(f.snapshot().views, 0);
    drop(reader);
    let (_, next) = exchange(f.state(), &f.context, &f.token);
    sharing::with_guest_store(f.state(), &f.context, |store, clock| {
        store.begin_delivery(
            &next,
            &f.context,
            &f.grant.intent.binding,
            Mode::Stream,
            clock,
        )
    })
    .unwrap();
    assert_eq!(f.snapshot().views, 0);
}

#[test]
fn disabled_download_and_cross_tab_media_id_never_disclose_bytes_or_charge_views() {
    let f = Evidence::new(false);
    let mut harness = super::super::registry::tests::Harness::new();
    let response = prepare(
        &f.request(Mode::Download, None),
        f.state(),
        &f.context,
        guard(&f, &mut harness),
        Mode::Download,
    )
    .unwrap_err();
    assert!(response.status_code >= 400);
    let wrong = Request::fake_https(
        "GET",
        format!("/guest/media?session={}", Uuid::new_v4()),
        vec![(
            "Cookie".into(),
            format!("{}={}", super::super::http::COOKIE, f.session),
        )],
        Vec::new(),
    );
    assert!(
        prepare(
            &wrong,
            f.state(),
            &f.context,
            guard(&f, &mut harness),
            Mode::Stream
        )
        .is_err()
    );
    assert_eq!(f.snapshot().views, 0);
    assert_eq!(f.snapshot().downloads, 0);
}

#[test]
fn revocation_between_response_admission_and_first_read_prevents_delivery() {
    let f = Evidence::new(false);
    let mut harness = super::super::registry::tests::Harness::new();
    let response = prepare(
        &f.request(Mode::Stream, None),
        f.state(),
        &f.context,
        guard(&f, &mut harness),
        Mode::Stream,
    )
    .unwrap();
    sharing::with_store(f.state(), |store, clock| {
        store.revoke(f.grant.id, 1, "withdrawn", clock)
    })
    .unwrap();
    let mut reader = response.data.into_reader_and_size().0;
    let mut output = [0xa5; 32];
    assert!(reader.read(&mut output).is_err());
    assert_eq!(output, [0xa5; 32]);
    assert_eq!(f.snapshot().views, 0);
}

#[test]
fn malformed_range_and_checksum_failure_do_not_count_failed_views() {
    let f = Evidence::new(false);
    let mut harness = super::super::registry::tests::Harness::new();
    let response = prepare(
        &f.request(Mode::Stream, Some("bytes=1-0")),
        f.state(),
        &f.context,
        guard(&f, &mut harness),
        Mode::Stream,
    )
    .unwrap_err();
    assert_eq!(response.status_code, 416);
    let path = super::super::super::export_provenance_tests::current_record(f.state())
        .path
        .unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(path, bytes).unwrap();
    assert!(
        prepare(
            &f.request(Mode::Stream, None),
            f.state(),
            &f.context,
            guard(&f, &mut harness),
            Mode::Stream
        )
        .is_err()
    );
    assert_eq!(f.snapshot().views, 0);
}

fn wait_for_export(state: &ServerState) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let record = super::super::super::export_provenance_tests::current_record(state);
        if record.job.status != crate::api::proto::ExportJobStatus::Running as i32 {
            assert_eq!(
                record.job.status,
                crate::api::proto::ExportJobStatus::Ready as i32
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "guest fixture export did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn status_json(
    f: &Evidence,
    harness: &mut super::super::registry::tests::Harness,
) -> serde_json::Value {
    let request = Request::fake_https(
        "GET",
        format!("/guest/session?session={}", f.bound.session_id),
        vec![(
            "Cookie".into(),
            format!("{}={}", super::super::http::COOKIE, f.session),
        )],
        Vec::new(),
    );
    let response =
        super::super::session::status(&request, f.state(), &f.context, guard(f, harness));
    assert_eq!(response.status_code, 200);
    serde_json::from_reader(response.data.into_reader_and_size().0.take(2048)).unwrap()
}

#[test]
fn guest_status_allowlists_metadata_and_keeps_a_committed_last_view_seekable() {
    let f = Evidence::new(false);
    let mut harness = super::super::registry::tests::Harness::new();
    let ready = status_json(&f, &mut harness);
    let mut keys = ready
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "bytes",
            "can_download",
            "can_stream",
            "display_name",
            "end_ms",
            "expires_at_ms",
            "has_gaps",
            "session_id",
            "start_ms",
            "status"
        ]
    );
    assert_eq!(ready["session_id"], f.bound.session_id.to_string());
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["can_stream"], true);
    assert_eq!(ready["can_download"], false);
    let response = prepare(
        &f.request(Mode::Stream, Some("bytes=0-7")),
        f.state(),
        &f.context,
        guard(&f, &mut harness),
        Mode::Stream,
    )
    .unwrap();
    let mut reader = response.data.into_reader_and_size().0;
    assert_eq!(reader.read(&mut [0; 8]).unwrap(), 8);
    assert_eq!(f.snapshot().views, 1);
    let after = status_json(&f, &mut harness);
    assert_eq!(after["can_stream"], true);
    assert_eq!(after["status"], "ready");
    sharing::with_store(f.state(), |store, clock| {
        store.revoke(f.grant.id, 1, "withdrawn", clock)
    })
    .unwrap();
    let terminal = status_json(&f, &mut harness);
    assert_eq!(
        terminal,
        serde_json::json!({"session_id":f.bound.session_id.to_string(),"status":"revoked"})
    );
}

#[test]
fn allowed_download_commits_one_bounded_transfer_and_never_consumes_a_view() {
    let f = Evidence::new(true);
    let mut harness = super::super::registry::tests::Harness::new();
    let response = prepare(
        &f.request(Mode::Download, Some("bytes=0-31")),
        f.state(),
        &f.context,
        guard(&f, &mut harness),
        Mode::Download,
    )
    .unwrap();
    assert!(response.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("Content-Disposition") && value.starts_with("attachment;")
    }));
    let mut reader = response.data.into_reader_and_size().0;
    assert_eq!(f.snapshot().downloads, 0);
    assert_eq!(reader.read(&mut [0; 8]).unwrap(), 8);
    assert_eq!(f.snapshot().downloads, 1);
    assert_eq!(reader.read(&mut [0; 8]).unwrap(), 8);
    assert_eq!(f.snapshot().downloads, 1);
    assert_eq!(f.snapshot().views, 0);
    drop(reader);
    assert!(
        prepare(
            &f.request(Mode::Download, None),
            f.state(),
            &f.context,
            guard(&f, &mut harness),
            Mode::Download
        )
        .is_err()
    );
    assert_eq!(f.snapshot().downloads, 1);
}
