//! A loopback-only ONVIF event device independent of KeepPeek and ONVIF client libraries.
//!
//! [`FakeOnvif`] runs SOAP 1.2 over HTTP on `127.0.0.1:0`. The device endpoint is
//! `/onvif/device_service`; `GetServices` advertises Events and Media at the same authority.
//! Events support capability and topic discovery, one pull-point subscription, synchronization,
//! pull, renew and unsubscribe. Media operations, TLS, WS-Security UsernameToken, filtering and
//! inferred property snapshots are not implemented. Supply `Initialized` notifications explicitly
//! when testing synchronization; the synchronization command acknowledges the validated reference.
//!
//! Every normal request requires MD5 Digest with `qop=auth`, the exact POST target including its
//! query, and an increasing nonce count per client nonce. At most 256 distinct client nonces are
//! retained. Debug output never includes credentials, authorization headers, targets or bodies.
//! [`CapturedRequest`] exposes original wire evidence only through explicit accessors.
//!
//! The default lease is 90 seconds. Positive relative termination requests are capped by the
//! configured lease. Timestamps advance from `2000-01-01T00:00:00Z` using monotonic elapsed time,
//! with nanosecond precision and Gregorian calendar rollover. Expired endpoints return a SOAP
//! `ResourceUnknown` fault. Each endpoint is `/onvif/subscription?key=<id>` and its reference
//! parameter is `{urn:test-hikvision:onvif}Identifier`, containing that decimal ID. Subscription
//! requests must include exactly one such direct child in the SOAP Header, regardless of prefix.
//!
//! Notifications remain in FIFO order across subscription replacement. Pulls return all currently
//! available notifications up to `MessageLimit`. Empty pulls wait at most two seconds, even when
//! accepting the default advertised maximum of 60 seconds. Configure lower limits with the builder
//! to exercise `PullMessagesFaultResponse` negotiation. Push, unsubscribe, renew and shutdown wake
//! pending pulls, which recheck the subscription before delivering anything.
//!
//! Notifications and scripted responses share 256 entries and eight MiB. Captures retain the newest
//! 256 requests within another eight MiB. HTTP input is limited to 16 KiB/32 headers and 256 KiB per
//! body; XML input is limited to 16 nesting levels and 4096 elements. Sixteen workers have absolute
//! five-second I/O budgets. The coordinator accepts one connection per iteration until shutdown.
//! Drop closes all active sockets, wakes waiters and joins all workers.
//!
//! [`FakeOnvif::next_response`] bypasses normal routing, but never authentication.
//! [`FakeOnvif::next_pull_response`] additionally requires a live subscription, its reference and
//! valid limits. Overrides do not consume notifications or change leases. Each override supports
//! at most 256 fragments and two seconds of total delays; held-open replies are also bounded.
//! [`Builder::subscription_address`] advertises alternate URLs without making outbound connections.
//!
//! The response roots follow the [ONVIF Events WSDL](https://www.onvif.org/ver10/events/wsdl/event.wsdl).
//! Create and renew times use WS-Notification; pull times use the ONVIF Events namespace.
//!
//! # Examples
//!
//! ```
//! use std::time::Duration;
//! use test_hikvision::onvif::{FakeOnvif, notification};
//!
//! let fake = FakeOnvif::builder()
//!     .credentials("test", "test")
//!     .lease(Duration::from_secs(90))
//!     .notifications(vec![notification(
//!         "VideoSource/MotionAlarm", true, "Initialized",
//!         "2026-09-05T12:00:00Z", "source-1",
//!     )])
//!     .start()?;
//! assert!(fake.address().ip().is_loopback());
//! fake.push(notification(
//!     "VideoSource/MotionAlarm", false, "Changed",
//!     "2026-09-05T12:00:01Z", "source-1",
//! ))?;
//! assert_eq!(fake.active_subscriptions(), 0);
//! # Ok::<(), anyhow::Error>(())
//! ```

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::Reply;

mod clock;
mod server;
mod soap;
mod subscriptions;
mod wire;

const CAPTURE_COUNT_MAX: usize = 256;
const CAPTURE_BYTES_MAX: usize = 8 * 1024 * 1024;
const CONNECTIONS_MAX: usize = 16;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const QUEUE_COUNT_MAX: usize = 256;
const QUEUE_BYTES_MAX: usize = 8 * 1024 * 1024;
const LEASE_MAX: Duration = Duration::from_secs(24 * 60 * 60);
const PULL_WAIT_MAX: Duration = Duration::from_secs(2);
const OBSERVE_WAIT_MAX: Duration = Duration::from_secs(60);

/// Configures a Digest-authenticated, loopback-only ONVIF test device.
pub struct Builder {
    username: String,
    password: String,
    lease: Duration,
    notifications: Vec<String>,
    max_timeout: Duration,
    max_message_limit: u32,
    subscription_address: Option<String>,
}

impl Builder {
    /// Sets synthetic credentials without exposing them through debug output.
    pub fn credentials(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = username.into();
        self.password = password.into();
        self
    }

    /// Caps granted leases at this positive duration, up to one day; the default is 90 seconds.
    pub const fn lease(mut self, lease: Duration) -> Self {
        self.lease = lease;
        self
    }

    /// Seeds the FIFO with caller-supplied notification XML, preserving its exact bytes.
    pub fn notifications(mut self, notifications: Vec<String>) -> Self {
        self.notifications = notifications;
        self
    }

    /// Sets the accepted pull timeout, at most 60 seconds; actual waits never exceed two seconds.
    pub const fn max_timeout(mut self, timeout: Duration) -> Self {
        self.max_timeout = timeout;
        self
    }

    /// Sets the accepted message limit between one and 256; the default is 256.
    pub const fn max_message_limit(mut self, limit: u32) -> Self {
        self.max_message_limit = limit;
        self
    }

    /// Overrides the advertised subscription URL, without changing routing or contacting its destination.
    ///
    /// Expands `{origin}`, `{port}` and `{id}`. Foreign and wildcard hosts are permitted for negative tests.
    pub fn subscription_address(mut self, address: impl Into<String>) -> Self {
        self.subscription_address = Some(address.into());
        self
    }

    /// Starts a local HTTP device on an OS-assigned IPv4 loopback port.
    ///
    /// # Errors
    /// Rejects invalid credentials, lease or pull limits, oversized queues, and unavailable resources.
    pub fn start(self) -> anyhow::Result<FakeOnvif> {
        anyhow::ensure!(
            !self.username.is_empty() && self.username.len() <= 128 && self.password.len() <= 256,
            "invalid fake ONVIF credentials"
        );
        anyhow::ensure!(
            !self.lease.is_zero() && self.lease <= LEASE_MAX,
            "invalid fake ONVIF lease"
        );
        anyhow::ensure!(
            !self.max_timeout.is_zero() && self.max_timeout <= OBSERVE_WAIT_MAX,
            "invalid fake ONVIF maximum timeout"
        );
        anyhow::ensure!(
            (1..=256).contains(&self.max_message_limit),
            "invalid fake ONVIF message limit"
        );
        anyhow::ensure!(
            self.notifications.len() <= QUEUE_COUNT_MAX,
            "fake ONVIF queue count exceeded"
        );
        let mut bytes = 0;
        for notification in &self.notifications {
            anyhow::ensure!(
                notification.len() <= QUEUE_BYTES_MAX - bytes,
                "fake ONVIF queue bytes exceeded"
            );
            bytes += notification.len();
        }
        subscriptions::address(&self, SocketAddr::from(([127, 0, 0, 1], 1)), 1)?;
        server::start(self)
    }
}

impl fmt::Debug for Builder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Builder").finish_non_exhaustive()
    }
}

/// Owns a synthetic ONVIF device and stops all its workers on drop.
pub struct FakeOnvif {
    address: SocketAddr,
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl FakeOnvif {
    /// Uses Digest authentication with the fixture credentials `test` / `test`.
    pub fn builder() -> Builder {
        Builder {
            username: "test".to_owned(),
            password: "test".to_owned(),
            lease: Duration::from_secs(90),
            notifications: Vec::new(),
            max_timeout: Duration::from_secs(60),
            max_message_limit: 256,
            subscription_address: None,
        }
    }

    /// Returns the ephemeral loopback listen address.
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Returns the credential-free HTTP origin.
    pub fn origin(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Returns the advertised Events service URL.
    pub fn events_endpoint(&self) -> String {
        format!("{}/onvif/events_service", self.origin())
    }

    /// Returns the newest 256 captures within an eight-MiB combined byte budget.
    ///
    /// Captures include authentication challenges. Debug output excludes all request contents.
    pub fn requests(&self) -> Vec<CapturedRequest> {
        self.shared
            .state
            .lock()
            .unwrap()
            .requests
            .iter()
            .cloned()
            .collect()
    }

    /// Queues notification XML and wakes pending pulls; XML bytes are not rewritten or validated.
    ///
    /// # Errors
    /// Rejects more than 256 queued entries, more than eight MiB, or a stopped device.
    pub fn push(&self, notification: impl Into<String>) -> anyhow::Result<()> {
        let notification = notification.into();
        let mut state = self.shared.state.lock().unwrap();
        state.check_queue_capacity(notification.len())?;
        state.notifications.push_back(notification);
        self.shared.changed.notify_all();
        Ok(())
    }

    /// Overrides the next authenticated request, before SOAP parsing or normal routing.
    ///
    /// # Errors
    /// Rejects shared queue overflow, over 256 fragments, or total delays exceeding two seconds.
    pub fn next_response(&self, reply: Reply) -> anyhow::Result<()> {
        self.queue_reply(reply, false)
    }

    /// Overrides one valid pull without consuming notifications or changing the subscription.
    ///
    /// Accepts SOAP faults, malformed XML, and raw HTTP through [`crate::Reply`]. Held-open replies
    /// end after at most two seconds. Authentication, reference, expiry and pull-limit checks still apply.
    ///
    /// # Errors
    /// Rejects shared queue overflow, over 256 fragments, or total delays exceeding two seconds.
    pub fn next_pull_response(&self, reply: Reply) -> anyhow::Result<()> {
        self.queue_reply(reply, true)
    }

    fn queue_reply(&self, reply: Reply, pull: bool) -> anyhow::Result<()> {
        anyhow::ensure!(
            reply.fragments.len() <= 256,
            "fake ONVIF fragment count exceeded"
        );
        let mut bytes = 0;
        let mut delay = Duration::ZERO;
        for (fragment_delay, fragment) in &reply.fragments {
            anyhow::ensure!(
                fragment.len() <= QUEUE_BYTES_MAX - bytes,
                "fake ONVIF script bytes exceeded"
            );
            anyhow::ensure!(
                *fragment_delay <= PULL_WAIT_MAX - delay,
                "fake ONVIF script delay exceeded"
            );
            bytes += fragment.len();
            delay += *fragment_delay;
        }
        let mut state = self.shared.state.lock().unwrap();
        state.check_queue_capacity(bytes)?;
        if pull {
            state.pull_responses.push_back(reply);
        } else {
            state.responses.push_back(reply);
        }
        self.shared.changed.notify_all();
        Ok(())
    }

    /// Waits at most 60 seconds for authenticated, reference-validated pull attempts.
    ///
    /// Counts attempts before timeout and message-limit validation. Returns false on timeout or shutdown.
    pub fn wait_for_pulls(&self, count: u64, timeout: Duration) -> bool {
        let (state, _) = self
            .shared
            .changed
            .wait_timeout_while(
                self.shared.state.lock().unwrap(),
                timeout.min(OBSERVE_WAIT_MAX),
                |state| state.pulls < count && !state.stopped,
            )
            .unwrap();
        state.pulls >= count
    }

    /// Returns zero or one, excluding expired subscriptions.
    pub fn active_subscriptions(&self) -> usize {
        let state = self.shared.state.lock().unwrap();
        usize::from(
            state
                .subscription
                .is_some_and(|subscription| subscription.expires > self.shared.started.elapsed()),
        )
    }

    /// Counts successful creations, including replacements after expiry or unsubscribe.
    pub fn subscription_count(&self) -> u64 {
        self.shared.state.lock().unwrap().subscriptions
    }

    /// Counts successful lease renewals.
    pub fn renew_count(&self) -> u64 {
        self.shared.state.lock().unwrap().renews
    }

    /// Counts successful unsubscribe operations.
    pub fn unsubscribe_count(&self) -> u64 {
        self.shared.state.lock().unwrap().unsubscribes
    }

    /// Counts authenticated, reference-validated pull attempts, including limit faults.
    pub fn pull_count(&self) -> u64 {
        self.shared.state.lock().unwrap().pulls
    }

    /// Counts rejected HTTP framing and failed socket operations, without retaining sensitive errors.
    pub fn transport_error_count(&self) -> u64 {
        self.shared.state.lock().unwrap().transport_errors
    }
}

impl fmt::Debug for FakeOnvif {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeOnvif")
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Drop for FakeOnvif {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(handle) = self.handle.take() {
            handle.join().expect("fake ONVIF coordinator panicked");
        }
    }
}

/// A bounded wire capture whose contents are available only through explicit accessors.
#[derive(Clone)]
pub struct CapturedRequest {
    method: String,
    target: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
    authenticated: bool,
}

impl CapturedRequest {
    /// Returns the exact HTTP method.
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Returns the exact received path and query used for Digest verification.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns original body bytes for protocol assertions, not logging.
    pub fn body(&self) -> &[u8] {
        &self.body
    }

    /// Returns a case-insensitive header value for explicit test assertions.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// Reports successful Digest verification, including nonce-count replay protection.
    pub const fn authenticated(&self) -> bool {
        self.authenticated
    }

    fn byte_len(&self) -> usize {
        self.method.len()
            + self.target.len()
            + self.body.len()
            + self
                .headers
                .iter()
                .map(|(name, value)| name.len() + value.len())
                .sum::<usize>()
    }
}

impl fmt::Debug for CapturedRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapturedRequest")
            .field("authenticated", &self.authenticated)
            .field("body_bytes", &self.body.len())
            .finish_non_exhaustive()
    }
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    config: Builder,
    address: SocketAddr,
    started: Instant,
}

#[derive(Default)]
struct State {
    stopped: bool,
    sockets: BTreeMap<u64, TcpStream>,
    next_socket: u64,
    requests: VecDeque<CapturedRequest>,
    digest_counts: BTreeMap<String, u32>,
    transport_errors: u64,
    notifications: VecDeque<String>,
    responses: VecDeque<Reply>,
    pull_responses: VecDeque<Reply>,
    subscription: Option<Subscription>,
    subscriptions: u64,
    renews: u64,
    unsubscribes: u64,
    pulls: u64,
}

#[derive(Clone, Copy)]
struct Subscription {
    id: u64,
    expires: Duration,
}

impl State {
    fn active(&self, id: u64, now: Duration) -> Option<Subscription> {
        self.subscription
            .filter(|subscription| subscription.id == id && subscription.expires > now)
    }

    fn check_queue_capacity(&self, additional_bytes: usize) -> anyhow::Result<()> {
        anyhow::ensure!(!self.stopped, "fake ONVIF device stopped");
        let count = self.notifications.len() + self.responses.len() + self.pull_responses.len();
        let bytes = self.notifications.iter().map(String::len).sum::<usize>()
            + self
                .responses
                .iter()
                .chain(&self.pull_responses)
                .flat_map(|reply| &reply.fragments)
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>();
        assert!(
            bytes <= QUEUE_BYTES_MAX,
            "fake ONVIF queue byte invariant violated"
        );
        anyhow::ensure!(count < QUEUE_COUNT_MAX, "fake ONVIF queue count exceeded");
        anyhow::ensure!(
            additional_bytes <= QUEUE_BYTES_MAX - bytes,
            "fake ONVIF queue bytes exceeded"
        );
        Ok(())
    }
}

impl Shared {
    fn stopped(&self) -> bool {
        self.state.lock().unwrap().stopped
    }

    fn stop(&self) {
        let mut state = self.state.lock().unwrap();
        state.stopped = true;
        let errors = state
            .sockets
            .values()
            .filter_map(|socket| socket.shutdown(Shutdown::Both).err())
            .filter(|error| error.kind() != std::io::ErrorKind::NotConnected)
            .count();
        state.transport_errors += u64::try_from(errors).expect("socket error count fits u64");
        self.changed.notify_all();
    }

    fn wait(&self, duration: Duration) -> bool {
        let (state, _) = self
            .changed
            .wait_timeout_while(self.state.lock().unwrap(), duration, |state| !state.stopped)
            .unwrap();
        state.stopped
    }

    fn capture(&self, request: CapturedRequest) {
        let mut state = self.state.lock().unwrap();
        let mut bytes = state
            .requests
            .iter()
            .map(CapturedRequest::byte_len)
            .sum::<usize>();
        while state.requests.len() >= CAPTURE_COUNT_MAX
            || bytes + request.byte_len() > CAPTURE_BYTES_MAX
        {
            let removed = state
                .requests
                .pop_front()
                .expect("a bounded request fits an empty capture queue");
            bytes -= removed.byte_len();
        }
        state.requests.push_back(request);
        self.changed.notify_all();
    }
}

/// Builds a namespaced notification from XML-compatible fixture text, escaping all supplied values.
///
/// `topic` is the suffix after `tns1:`, such as `VideoSource/MotionAlarm`.
/// The source is a `VideoSourceConfigurationToken`; the boolean data item is `State`.
pub fn notification(
    topic: &str,
    state: bool,
    operation: &str,
    utc_time: &str,
    source_token: &str,
) -> String {
    use xml::escape::{escape_str_attribute, escape_str_pcdata};

    let topic = escape_str_pcdata(topic);
    let operation = escape_str_attribute(operation);
    let utc_time = escape_str_attribute(utc_time);
    let source_token = escape_str_attribute(source_token);
    format!(
        r#"<wsnt:NotificationMessage xmlns:wsnt="http://docs.oasis-open.org/wsn/b-2"
        xmlns:tt="http://www.onvif.org/ver10/schema" xmlns:tns1="http://www.onvif.org/ver10/topics">
        <wsnt:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">tns1:{topic}</wsnt:Topic>
        <wsnt:Message><tt:Message UtcTime="{utc_time}" PropertyOperation="{operation}">
        <tt:Source><tt:SimpleItem Name="VideoSourceConfigurationToken" Value="{source_token}"/></tt:Source>
        <tt:Data><tt:SimpleItem Name="State" Value="{state}"/></tt:Data>
        </tt:Message></wsnt:Message></wsnt:NotificationMessage>"#
    )
}

#[cfg(test)]
mod tests {
    mod bounds;
    mod lifecycle;
    mod runtime;
    mod scenarios;
    mod transport;

    use xml::reader::{EventReader, XmlEvent};

    #[test]
    fn notification_preserves_namespaces_and_escapes_input() {
        let source = "source\"'<>&";
        let operation = "Changed\" & <literal>";
        let utc_time = "2026-09-05T12:00:00Z";
        let body =
            super::notification("VideoSource/MotionAlarm", true, operation, utc_time, source);
        let events: Vec<_> = EventReader::from_str(&body)
            .into_iter()
            .collect::<Result<_, _>>()
            .unwrap();
        let elements: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                XmlEvent::StartElement {
                    name, attributes, ..
                } => Some((name, attributes)),
                _ => None,
            })
            .collect();
        assert_eq!(elements[0].0.local_name, "NotificationMessage");
        assert_eq!(
            elements[0].0.namespace.as_deref(),
            Some("http://docs.oasis-open.org/wsn/b-2")
        );
        let message = elements
            .iter()
            .find(|(name, _)| {
                name.local_name == "Message"
                    && name.namespace.as_deref() == Some("http://www.onvif.org/ver10/schema")
            })
            .unwrap();
        assert!(
            message
                .1
                .iter()
                .any(|attr| attr.name.local_name == "PropertyOperation" && attr.value == operation)
        );
        assert!(
            message
                .1
                .iter()
                .any(|attr| attr.name.local_name == "UtcTime" && attr.value == utc_time)
        );
        assert!(
            elements
                .iter()
                .any(|(name, attrs)| name.local_name == "SimpleItem"
                    && attrs.iter().any(|attr| attr.value == source))
        );
        assert!(events.iter().any(|event| matches!(event, XmlEvent::Characters(text) if text == "tns1:VideoSource/MotionAlarm")));
        assert!(
            elements
                .iter()
                .any(|(name, attrs)| name.local_name == "SimpleItem"
                    && attrs.iter().any(|attr| attr.value == "true"))
        );
    }
}
