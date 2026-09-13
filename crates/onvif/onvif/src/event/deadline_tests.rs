use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ureq::unversioned::resolver::DefaultResolver;
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport,
};

use super::client::{Client, ClientError, Failure};
use super::deadline::Deadline;
use super::{Endpoint, Request};
use crate::soap::client::Credentials;

#[derive(Debug)]
enum Step {
    Bytes(Duration, Vec<u8>),
    Stall,
}

#[derive(Debug)]
struct Wire {
    now: Instant,
    exchanges: VecDeque<VecDeque<Step>>,
    requests: Vec<Vec<u8>>,
    budgets: Vec<Duration>,
    stalls: Vec<Duration>,
}

#[derive(Debug)]
struct Script(Arc<Mutex<Wire>>);

impl Connector for Script {
    type Out = Connection;

    fn connect(
        &self,
        details: &ConnectionDetails,
        chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        assert!(chained.is_none());
        let mut wire = self.0.lock().unwrap();
        let steps = wire.exchanges.pop_front().expect("unexpected HTTP request");
        let budget = details.config.timeouts().global.unwrap();
        wire.budgets.push(budget);
        let index = wire.requests.len();
        wire.requests.push(Vec::with_capacity(8192));
        Ok(Some(Connection {
            wire: Arc::clone(&self.0),
            steps,
            index,
            budget,
            buffers: LazyBuffers::new(8192, 8192),
        }))
    }
}

#[derive(Debug)]
struct Connection {
    wire: Arc<Mutex<Wire>>,
    steps: VecDeque<Step>,
    index: usize,
    budget: Duration,
    buffers: LazyBuffers,
}

impl Transport for Connection {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        assert_eq!(timeout.reason, ureq::Timeout::Global);
        assert!(*timeout.after <= self.budget);
        let mut wire = self.wire.lock().unwrap();
        let request = &mut wire.requests[self.index];
        assert!(request.len() + amount <= 16 * 1024);
        request.extend_from_slice(&self.buffers.output()[..amount]);
        Ok(())
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        assert_eq!(timeout.reason, ureq::Timeout::Global);
        assert!(*timeout.after <= self.budget);
        let mut wire = self.wire.lock().unwrap();
        match self.steps.pop_front().expect("unexpected response read") {
            Step::Bytes(delay, bytes) => {
                wire.now += delay;
                self.buffers.input_append_buf()[..bytes.len()].copy_from_slice(&bytes);
                self.buffers.input_appended(bytes.len());
                Ok(true)
            }
            Step::Stall => {
                // ureq uses its own clock. Check its I/O bound, then consume the
                // configured budget on the controlled application clock.
                wire.stalls.push(*timeout.after);
                wire.now += self.budget;
                Err(ureq::Error::Timeout(timeout.reason))
            }
        }
    }

    fn is_open(&mut self) -> bool {
        false
    }
}

fn client(exchanges: Vec<Vec<Step>>) -> (Client, Arc<Mutex<Wire>>) {
    let endpoint = Endpoint::new("http://127.0.0.1/onvif/event").unwrap();
    let mut client = Client::new(
        endpoint,
        Credentials {
            username: "test".to_owned(),
            password: "test".to_owned(),
        },
    )
    .unwrap();
    let wire = Arc::new(Mutex::new(Wire {
        now: Instant::now(),
        exchanges: exchanges.into_iter().map(VecDeque::from).collect(),
        requests: Vec::with_capacity(3),
        budgets: Vec::with_capacity(3),
        stalls: Vec::with_capacity(1),
    }));
    client.agent = ureq::Agent::with_parts(
        client.agent.config().clone(),
        Script(Arc::clone(&wire)),
        DefaultResolver::default(),
    );
    (client, wire)
}

fn challenge(nonce: &str, stale: bool, delay: Duration) -> Step {
    Step::Bytes(
        delay,
        format!(
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest realm=\"fake-hikvision\", nonce=\"{nonce}\", algorithm=MD5, qop=\"auth\", stale={stale}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes(),
    )
}

fn assert_network(error: ClientError) {
    assert!(matches!(error.0, Failure::Network));
    assert_eq!(
        error.to_string(),
        "ONVIF event network request failed or timed out"
    );
}

#[test]
fn snapshot_challenges_leave_only_the_original_deadlines_body_budget() {
    let delay = Duration::from_millis(200);
    let (mut client, wire) = client(vec![
        vec![challenge("expired", false, delay)],
        vec![challenge("fresh", true, delay)],
        vec![
            Step::Bytes(Duration::ZERO, b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: image/jpeg\r\nContent-Length: 4\r\n\r\n\xff\xd8".to_vec()),
            Step::Stall,
        ],
    ]);
    let endpoint = client.camera.resolve("/picture").unwrap();
    let started = wire.lock().unwrap().now;
    let error = client
        .snapshot_with_clock(&endpoint, Duration::from_secs(1), || {
            wire.lock().unwrap().now
        })
        .unwrap_err();
    assert_network(error);
    let wire = wire.lock().unwrap();
    assert_eq!(wire.now.duration_since(started), Duration::from_secs(1));
    assert_eq!(wire.budgets, [1000, 800, 600].map(Duration::from_millis));
    assert_eq!(wire.stalls.len(), 1);
    assert!(wire.stalls[0] <= Duration::from_millis(600));
    assert!(wire.exchanges.is_empty());
    let requests = wire
        .requests
        .iter()
        .map(|bytes| std::str::from_utf8(bytes).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 3);
    assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
    assert!(requests[1].contains("nonce=\"expired\""));
    assert!(requests[2].contains("nonce=\"fresh\""));
}

#[test]
fn soap_deadline_reaches_stalled_headers_and_body() {
    for steps in [
        vec![Step::Stall],
        vec![
            Step::Bytes(Duration::ZERO, b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/soap+xml\r\nContent-Length: 1000\r\n\r\n<s:Envelope".to_vec()),
            Step::Stall,
        ],
    ] {
        let (mut client, wire) = client(vec![steps]);
        let request = Request::create(&client.camera, Duration::from_secs(90)).unwrap();
        let timeout = Duration::from_millis(150);
        let started = wire.lock().unwrap().now;
        let error = client.execute_with_clock(&request, timeout, || wire.lock().unwrap().now).unwrap_err();
        assert_network(error);
        let wire = wire.lock().unwrap();
        assert_eq!(wire.now.duration_since(started), timeout);
        assert_eq!(wire.budgets, [timeout]);
        assert_eq!(wire.stalls.len(), 1);
        assert!(wire.stalls[0] <= timeout);
        assert!(wire.exchanges.is_empty());
        assert_eq!(wire.requests.len(), 1);
        assert!(wire.requests[0].starts_with(b"POST /onvif/event HTTP/1.1\r\n"));
    }
}

#[test]
fn deadline_rejects_exact_expiry_and_later_times() {
    let now = std::cell::Cell::new(Instant::now());
    let started = now.get();
    let deadline = Deadline::new(Duration::from_secs(1), || now.get());
    now.set(started + Duration::from_secs(1) - Duration::from_nanos(1));
    assert_eq!(deadline.remaining().unwrap(), Duration::from_nanos(1));
    for elapsed in [Duration::from_secs(1), Duration::from_secs(2)] {
        now.set(started + elapsed);
        assert_network(deadline.remaining().unwrap_err());
    }
}

#[test]
fn an_expired_challenge_cannot_start_another_snapshot_request() {
    let timeout = Duration::from_secs(1);
    let (mut client, wire) = client(vec![vec![challenge("expired", false, timeout)]]);
    let endpoint = client.camera.resolve("/picture").unwrap();
    assert_network(
        client
            .snapshot_with_clock(&endpoint, timeout, || wire.lock().unwrap().now)
            .unwrap_err(),
    );
    let wire = wire.lock().unwrap();
    assert_eq!(wire.requests.len(), 1);
    assert_eq!(wire.budgets, [timeout]);
    assert!(wire.stalls.is_empty());
}

#[test]
fn soap_rejects_a_complete_body_received_at_the_deadline() {
    let timeout = Duration::from_millis(150);
    let body = b"<s:Envelope xmlns:s=\"http://www.w3.org/2003/05/soap-envelope\"><s:Body><ok/></s:Body></s:Envelope>";
    let headers = format!(
        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: application/soap+xml\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let (mut client, wire) = client(vec![vec![
        Step::Bytes(Duration::ZERO, headers.into_bytes()),
        Step::Bytes(timeout, body.to_vec()),
    ]]);
    let request = Request::create(&client.camera, Duration::from_secs(90)).unwrap();
    let error = client
        .execute_with_clock(&request, timeout, || wire.lock().unwrap().now)
        .unwrap_err();
    assert_network(error);
    let wire = wire.lock().unwrap();
    assert_eq!(wire.requests.len(), 1);
    assert_eq!(wire.budgets, [timeout]);
    assert!(wire.stalls.is_empty());
}
