use super::*;
use crate::shares::{CreateGrant, GrantSummary, IssuerBinding};
use rouille::{Request, Response, Server};
use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::mpsc::{self, Receiver, SyncSender},
};

type Handler = Box<dyn Fn(&Request) -> Response + Send + Sync>;

pub(in crate::server::guest_review) struct Harness {
    server: Server<Handler>,
    requests: Receiver<(ResponseAbortHandle, SyncSender<()>)>,
    held: Vec<(TcpStream, SyncSender<()>)>,
}

impl Harness {
    pub(in crate::server::guest_review) fn new() -> Self {
        let (sent, requests) = mpsc::sync_channel(16);
        let handler: Handler = Box::new(move |request| {
            let (release, wait) = mpsc::sync_channel(1);
            sent.send((request.response_abort_handle().unwrap(), release))
                .unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(3));
            Response::text("done")
        });
        Self {
            server: Server::new("127.0.0.1:0", handler).unwrap(),
            requests,
            held: Vec::new(),
        }
    }

    pub(in crate::server::guest_review) fn request(&mut self) -> ResponseAbortHandle {
        let mut client = TcpStream::connect(self.server.server_addr()).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        self.server.poll_once_timeout(Duration::from_secs(1));
        let (abort, release) = self.requests.recv_timeout(Duration::from_secs(1)).unwrap();
        self.held.push((client, release));
        abort
    }

    pub(in crate::server::guest_review) fn finish(&mut self, index: usize) {
        let (client, release) = &mut self.held[index];
        release.send(()).unwrap();
        let mut response = Vec::new();
        client.take(8192).read_to_end(&mut response).unwrap();
        assert!(response.starts_with(b"HTTP/1.1 200"));
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        for (_, release) in &self.held {
            let _ = release.try_send(());
        }
        self.held.clear();
        let _ = self.server.join_timeout(Duration::from_secs(3));
    }
}

fn peer() -> IpAddr {
    "127.0.0.1".parse().unwrap()
}

fn bound() -> BoundSession {
    let wall = i64::try_from(super::super::super::unix_time_ms()).unwrap();
    let intent = CreateGrant {
        request_id: Uuid::new_v4(),
        issuer: IssuerBinding::LocalAdministrator,
        binding: ExportBinding {
            job_id: "test-export".into(),
            artifact_id: Uuid::new_v4().to_string(),
            sha256: [9; 32],
            source_id: "camera".into(),
            source_identity: Uuid::new_v4(),
            start_ms: 1,
            end_ms: 1000,
            bytes: 128,
            available_until_ms: wall + 60_000,
        },
        label: "shared evidence".into(),
        note: String::new(),
        expires_at_ms: wall + 60_000,
        allow_stream: true,
        allow_download: false,
        max_views: None,
        max_downloads: None,
    };
    BoundSession {
        session_id: Uuid::new_v4(),
        grant: GrantSummary {
            id: intent.request_id,
            revision: 1,
            intent,
            created_at_ms: wall,
            revoked_at_ms: None,
            revoke_reason: None,
            views: 0,
            downloads: 0,
            last_access_at_ms: None,
        },
        media_active: true,
        media_deadline: Some(MediaDeadline {
            expires_at_ms: wall + 60_000,
            monotonic: Instant::now() + Duration::from_secs(60),
        }),
        stream_committed: false,
    }
}

fn context() -> SessionContext {
    SessionContext {
        origin: "https://keeppeek.example".into(),
        policy_revision: 1,
    }
}

#[test]
fn stopped_watchdog_denies_new_requests_and_cancels_existing_guards() {
    let registry = Arc::new(Registry::default());
    let mut harness = Harness::new();
    assert!(
        registry
            .admit_request(peer(), Action::Media, harness.request())
            .is_err()
    );
    let shutdown = Shutdown::new();
    let thread = registry.start_deadline_watchdog(shutdown.clone()).unwrap();
    assert!(registry.start_deadline_watchdog(shutdown.clone()).is_err());
    let guard = registry
        .admit_request(peer(), Action::Media, harness.request())
        .unwrap();
    registry.stop();
    assert!(guard.check().is_err());
    assert!(guard.bind(&bound(), &context()).is_err());
    assert!(
        registry
            .admit_request(peer(), Action::Media, harness.request())
            .is_err()
    );
    shutdown.cancel();
    thread.join().unwrap();
    assert!(!registry.lock().unwrap().watchdog);
}

#[test]
fn request_capacity_is_retained_until_the_actual_response_writer_finishes() {
    let registry = Arc::new(Registry::default());
    let shutdown = Shutdown::new();
    let thread = registry.start_deadline_watchdog(shutdown.clone()).unwrap();
    let mut harness = Harness::new();
    let mut first = None;
    for index in 0..8 {
        let abort = harness.request();
        if index == 0 {
            first = Some(abort.clone());
        }
        drop(
            registry
                .admit_request(peer(), Action::Media, abort)
                .unwrap(),
        );
    }
    let rejected = registry.admit_request(peer(), Action::Media, harness.request());
    assert!(matches!(rejected, Err(error) if error.kind() == io::ErrorKind::WouldBlock));
    assert_eq!(registry.lock().unwrap().flows.len(), 8);
    harness.finish(0);
    let deadline = Instant::now() + Duration::from_secs(1);
    while !first.as_ref().unwrap().is_finished().unwrap() && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(first.unwrap().is_finished().unwrap());
    registry
        .admit_request(peer(), Action::Media, harness.request())
        .unwrap();
    shutdown.cancel();
    thread.join().unwrap();
}

#[test]
fn transfer_admission_uses_exact_session_deadline_and_two_transfers_per_session() {
    let registry = Arc::new(Registry::default());
    let shutdown = Shutdown::new();
    let thread = registry.start_deadline_watchdog(shutdown.clone()).unwrap();
    let mut harness = Harness::new();
    let bound = bound();
    let mut guards = Vec::new();
    for _ in 0..2 {
        let guard = registry
            .admit_request(peer(), Action::Media, harness.request())
            .unwrap();
        let transfer = guard.bind(&bound, &context()).unwrap();
        assert_eq!(transfer.deadline, bound.media_deadline.unwrap());
        guards.push(transfer);
    }
    let third = registry
        .admit_request(peer(), Action::Media, harness.request())
        .unwrap();
    assert!(
        matches!(third.bind(&bound, &context()), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    let mut other = bound.clone();
    other.session_id = Uuid::new_v4();
    registry
        .admit_request(peer(), Action::Media, harness.request())
        .unwrap()
        .bind(&other, &context())
        .unwrap();
    registry.abort_session(bound.session_id);
    assert!(guards.iter().all(|guard| guard.check().is_err()));
    shutdown.cancel();
    thread.join().unwrap();
}

#[test]
fn independent_watchdog_cancels_monotonic_expiry_without_live_storage_validation() {
    let registry = Arc::new(Registry::default());
    let shutdown = Shutdown::new();
    let thread = registry.start_deadline_watchdog(shutdown.clone()).unwrap();
    let mut harness = Harness::new();
    let mut bound = bound();
    bound.media_deadline.as_mut().unwrap().monotonic = Instant::now() + Duration::from_millis(10);
    let guard = registry
        .admit_request(peer(), Action::Media, harness.request())
        .unwrap()
        .bind(&bound, &context())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    while !guard.cancelled.load(Ordering::Acquire) && Instant::now() < deadline {
        thread::yield_now();
    }
    assert!(guard.cancelled.load(Ordering::Acquire));
    assert!(guard.check().is_err());
    shutdown.cancel();
    thread.join().unwrap();
}

#[test]
fn hash_reader_permits_are_bounded_and_released_by_drop() {
    let registry = Arc::new(Registry::default());
    let first = registry.reserve_hash().unwrap();
    let second = registry.reserve_hash().unwrap();
    assert!(
        matches!(registry.reserve_hash(), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    drop(first);
    let replacement = registry.reserve_hash().unwrap();
    assert_eq!(registry.lock().unwrap().hash_readers, 2);
    drop((second, replacement));
    assert_eq!(registry.lock().unwrap().hash_readers, 0);
}
