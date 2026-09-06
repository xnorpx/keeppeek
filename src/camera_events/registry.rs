use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc::{SyncSender, TrySendError},
};
use std::time::Instant;

use serde::Serialize;

use crate::cameras::events::{EventConfig, EventMode, MetadataMode};
use crate::keeppeek::StreamKind;
use crate::shutdown::Shutdown;
use retina::codec::CompressionType;

const QUEUED_BYTES_MAX: usize = 8 * 1024 * 1024;
const SNAPSHOT_ENDPOINTS_MAX: usize = 1024;
const POLICIES_MAX: usize = 1024;

#[derive(Clone, Debug, Default, Serialize)]
pub struct Evidence {
    pub mode: &'static str,
    pub state: &'static str,
    pub kinds: Vec<String>,
    pub pull_advertised: Option<bool>,
    pub pull_capable: bool,
    pub pulls: u64,
    pub empty_pulls: u64,
    pub notifications: u64,
    pub parse_errors: u64,
    pub reconnects: u64,
    pub renewals: u64,
    pub renew_failures: u64,
    pub unsubscribed: bool,
    pub lease_ms: u64,
    pub active: usize,
    pub deduplicated: u64,
    pub metadata_bytes: u64,
    pub metadata_documents: u64,
    pub metadata_loss: u64,
    pub metadata_errors: u64,
    pub metadata_available: bool,
    pub queue_drops: u64,
    pub delivery_stalls: u64,
    pub dropped: u64,
    pub snapshots: u64,
    pub snapshot_failures: u64,
}

#[derive(Clone, Default)]
pub struct Registry {
    slots: Arc<Mutex<HashMap<IpAddr, Arc<Slot>>>>,
    policies: Arc<Mutex<Policies>>,
    observed: Arc<Mutex<HashMap<IpAddr, Vec<String>>>>,
    services: Arc<Mutex<HashMap<IpAddr, onvif::event::Service>>>,
    snapshot_endpoints: Arc<Mutex<HashMap<IpAddr, onvif::event::Endpoint>>>,
}

#[derive(Default)]
struct Policies {
    entries: HashMap<IpAddr, Policy>,
    revision: u64,
}

struct Policy {
    config: EventConfig,
    record_motion: bool,
    shutdown: Shutdown,
    revision: u64,
}

pub(super) struct Slot {
    pub sent: SyncSender<Input>,
    pub evidence: Mutex<Evidence>,
    pub shutdown: Shutdown,
    queued: AtomicUsize,
    policy: EventConfig,
    metadata_owner: Mutex<MetadataOwner>,
    interruptions: Mutex<(Option<Instant>, Option<Instant>)>,
}

#[derive(Default)]
struct MetadataOwner {
    current: Option<(StreamKind, Instant)>,
    reset: Option<Instant>,
}

impl MetadataOwner {
    fn admit(&mut self, profile: StreamKind, received: Instant) -> Option<(Instant, bool)> {
        if self.current.is_some_and(|(current, last)| {
            current != profile
                && received.saturating_duration_since(last) < std::time::Duration::from_secs(5)
        }) {
            return None;
        }
        let received = if self.current.is_none() {
            self.reset
                .filter(|cutoff| *cutoff >= received)
                .map_or(received, |cutoff| {
                    cutoff
                        .checked_add(std::time::Duration::from_nanos(1))
                        .expect("metadata reset leaves room for the next admission")
                })
        } else {
            received
        };
        let changed = self.current.is_some_and(|(current, _)| current != profile);
        let last = self
            .current
            .filter(|(current, _)| *current == profile)
            .map_or(received, |(_, last)| last.max(received));
        self.current = Some((profile, last));
        Some((received, changed))
    }
}

pub(super) enum Input {
    Pull {
        bytes: Vec<u8>,
        received: Instant,
        received_ms: i64,
    },
    Disconnected,
    Metadata {
        bytes: Vec<u8>,
        compression: CompressionType,
        loss: u16,
        received: Instant,
        received_ms: i64,
    },
    MetadataLost,
    Snapshot {
        camera_id: String,
        event_id: String,
        jpeg: Vec<u8>,
    },
}

impl Input {
    pub const fn bytes(&self) -> usize {
        match self {
            Self::Pull { bytes, .. } | Self::Metadata { bytes, .. } => bytes.len(),
            Self::Snapshot { jpeg, .. } => jpeg.len(),
            Self::Disconnected | Self::MetadataLost => 0,
        }
    }
}

impl Registry {
    pub(crate) fn record_snapshot(&self, ip: IpAddr, uri: Option<&str>) -> bool {
        let Some(uri) = uri else {
            return false;
        };
        let address = std::net::SocketAddr::new(ip, 80);
        let endpoint = onvif::event::Endpoint::new(format!("http://{address}/"))
            .and_then(|origin| origin.resolve(uri));
        let Ok(endpoint) = endpoint else {
            return false;
        };
        let Ok(url) = url::Url::parse(uri) else {
            return false;
        };
        let same_ip = match url.host() {
            Some(url::Host::Ipv4(host)) => IpAddr::V4(host) == ip,
            Some(url::Host::Ipv6(host)) => IpAddr::V6(host) == ip,
            _ => false,
        };
        if !same_ip {
            return false;
        }
        let mut endpoints = self
            .snapshot_endpoints
            .lock()
            .expect("snapshot endpoint registry is not poisoned");
        if endpoints.len() >= SNAPSHOT_ENDPOINTS_MAX && !endpoints.contains_key(&ip) {
            tracing::warn!(camera_ip = %ip, "snapshot endpoint evidence capacity exceeded");
            return false;
        }
        endpoints.insert(ip, endpoint);
        true
    }

    pub(super) fn snapshot_endpoint(&self, ip: IpAddr) -> Option<onvif::event::Endpoint> {
        self.snapshot_endpoints
            .lock()
            .expect("snapshot endpoint registry is not poisoned")
            .get(&ip)
            .cloned()
    }

    pub(crate) fn record_service(&self, ip: IpAddr, service: &onvif::event::Service) {
        let mut services = self
            .services
            .lock()
            .expect("event service registry is not poisoned");
        if services.len() >= 1024 && !services.contains_key(&ip) {
            tracing::warn!(camera_ip = %ip, "event service evidence capacity exceeded");
            return;
        }
        services.insert(ip, service.clone());
    }

    pub(crate) fn service(&self, ip: IpAddr) -> Option<onvif::event::Service> {
        self.services
            .lock()
            .expect("event service registry is not poisoned")
            .get(&ip)
            .cloned()
    }

    pub(crate) fn configure(
        &self,
        ip: IpAddr,
        policy: EventConfig,
        record_motion: bool,
        shutdown: Shutdown,
    ) {
        let mut policies = self
            .policies
            .lock()
            .expect("event policy registry is not poisoned");
        if policies.entries.len() >= POLICIES_MAX && !policies.entries.contains_key(&ip) {
            shutdown.cancel();
            tracing::warn!(camera_ip = %ip, "native event policy capacity exceeded");
            return;
        }
        let unchanged = policies
            .entries
            .get(&ip)
            .filter(|previous| {
                previous.config == policy
                    && previous.record_motion == record_motion
                    && !previous.shutdown.is_cancelled()
            })
            .map(|previous| previous.revision);
        let revision = unchanged.unwrap_or_else(|| {
            policies.revision = policies
                .revision
                .checked_add(1)
                .expect("event policy revision exhausted");
            policies.revision
        });
        policies.entries.insert(
            ip,
            Policy {
                config: policy,
                record_motion,
                shutdown,
                revision,
            },
        );
    }

    pub(crate) fn vendor_revision(&self, ip: IpAddr) -> u64 {
        self.policies
            .lock()
            .expect("event policy registry is not poisoned")
            .entries
            .get(&ip)
            .map_or(0, |policy| policy.revision)
    }

    pub(crate) fn enabled(&self, ip: IpAddr) -> bool {
        let policies = self
            .policies
            .lock()
            .expect("event policy registry is not poisoned");
        policies
            .entries
            .get(&ip)
            .map_or(policies.revision == 0, |policy| {
                policy.config.mode != EventMode::Disabled && !policy.shutdown.is_cancelled()
            })
    }

    pub(crate) fn vendor_enabled(&self, ip: IpAddr) -> bool {
        let policies = self
            .policies
            .lock()
            .expect("event policy registry is not poisoned");
        policies
            .entries
            .get(&ip)
            .map_or(policies.revision == 0, |policy| {
                !policy.shutdown.is_cancelled()
                    && matches!(policy.config.mode, EventMode::Auto | EventMode::Vendor)
            })
    }

    pub(crate) fn record_motion(&self, ip: IpAddr, fallback: bool) -> bool {
        self.policies
            .lock()
            .expect("event policy registry is not poisoned")
            .entries
            .get(&ip)
            .map_or(fallback, |policy| policy.record_motion)
    }
    pub(super) fn install(
        &self,
        ip: IpAddr,
        sent: SyncSender<Input>,
        shutdown: Shutdown,
        policy: EventConfig,
    ) -> anyhow::Result<Arc<Slot>> {
        let mode = if policy.mode == EventMode::RtspMetadata {
            "rtsp-metadata"
        } else {
            "onvif-pullpoint"
        };
        let slot = Arc::new(Slot {
            sent,
            evidence: Mutex::new(Evidence {
                mode,
                state: "starting",
                ..Default::default()
            }),
            shutdown,
            queued: AtomicUsize::new(0),
            policy,
            metadata_owner: Mutex::new(MetadataOwner::default()),
            interruptions: Mutex::new((None, None)),
        });
        let mut slots = self
            .slots
            .lock()
            .expect("native event registry is not poisoned");
        anyhow::ensure!(
            slots.contains_key(&ip) || slots.len() < 1024,
            "native event registry capacity exceeded"
        );
        slots.insert(ip, Arc::clone(&slot));
        Ok(slot)
    }

    pub(crate) fn snapshot(&self, ip: IpAddr) -> Option<Evidence> {
        let slot = self
            .slots
            .lock()
            .expect("native event registry is not poisoned")
            .get(&ip)
            .cloned();
        let observed = self
            .observed
            .lock()
            .expect("observed event kinds are not poisoned")
            .get(&ip)
            .cloned();
        let service = self.service(ip);
        let mut evidence = slot
            .map(|slot| {
                slot.evidence
                    .lock()
                    .expect("native event evidence is not poisoned")
                    .clone()
            })
            .or_else(|| {
                observed.as_ref().map(|_| Evidence {
                    mode: "vendor",
                    state: "observed",
                    ..Default::default()
                })
            })
            .or_else(|| {
                service.as_ref().map(|_| Evidence {
                    mode: "onvif-pullpoint",
                    state: "discovered",
                    ..Default::default()
                })
            })?;
        if let Some(service) = service {
            evidence.pull_advertised = service.max_pull_points().map(|capacity| capacity > 0);
            for kind in service.kinds() {
                let kind = kind.to_string();
                if !evidence.kinds.contains(&kind) {
                    evidence.kinds.push(kind);
                }
            }
        }
        for kind in observed.into_iter().flatten() {
            if !evidence.kinds.contains(&kind) {
                evidence.kinds.push(kind);
            }
        }
        Some(evidence)
    }

    pub(crate) fn record_kind(&self, ip: IpAddr, kind: &str) -> bool {
        if kind.len() > 64 {
            return false;
        }
        let mut observed = self
            .observed
            .lock()
            .expect("observed event kinds are not poisoned");
        if observed.len() >= 1024 && !observed.contains_key(&ip) {
            return false;
        }
        let kinds = observed.entry(ip).or_default();
        if kinds.len() < 32 && !kinds.iter().any(|existing| existing == kind) {
            kinds.push(kind.to_owned());
            return true;
        }
        false
    }

    pub(crate) fn remove(&self, ip: IpAddr) {
        self.slots
            .lock()
            .expect("native event registry is not poisoned")
            .remove(&ip);
        self.policies
            .lock()
            .expect("event policy registry is not poisoned")
            .entries
            .remove(&ip);
        self.observed
            .lock()
            .expect("observed event kinds are not poisoned")
            .remove(&ip);
        self.services
            .lock()
            .expect("event service registry is not poisoned")
            .remove(&ip);
        self.snapshot_endpoints
            .lock()
            .expect("snapshot endpoint registry is not poisoned")
            .remove(&ip);
    }

    pub(crate) fn metadata(
        &self,
        ip: IpAddr,
        profile: StreamKind,
        compression: CompressionType,
        loss: u16,
        bytes: &[u8],
        received: Instant,
    ) {
        let slot = self
            .slots
            .lock()
            .expect("native event registry is not poisoned")
            .get(&ip)
            .cloned();
        let Some(slot) = slot else {
            return;
        };
        if slot.shutdown.is_cancelled()
            || matches!(slot.policy.mode, EventMode::Disabled | EventMode::Vendor)
            || slot.policy.metadata_stream == MetadataMode::Disabled
        {
            return;
        }
        if bytes.len() > 256 * 1024
            || !matches!(
                compression,
                CompressionType::Uncompressed | CompressionType::GzipCompressed
            )
        {
            slot.update(|evidence| evidence.metadata_errors += 1);
            return;
        }
        slot.metadata(profile, compression, loss, bytes, received);
    }

    pub(crate) fn metadata_lost(&self, ip: IpAddr, profile: StreamKind) {
        let slot = self
            .slots
            .lock()
            .expect("native event registry is not poisoned")
            .get(&ip)
            .cloned();
        let Some(slot) = slot else {
            return;
        };
        let mut owner = slot
            .metadata_owner
            .lock()
            .expect("metadata owner is not poisoned");
        if owner.current.is_some_and(|(current, _)| current == profile) {
            owner.current = None;
            let _ = slot.try_send(Input::MetadataLost);
            owner.reset = Some(Instant::now());
            slot.update(|evidence| evidence.metadata_loss += 1);
        }
    }
}

impl Slot {
    fn metadata(
        &self,
        profile: StreamKind,
        compression: CompressionType,
        loss: u16,
        bytes: &[u8],
        received: Instant,
    ) {
        let mut owner = self
            .metadata_owner
            .lock()
            .expect("metadata owner is not poisoned");
        let Some((received, changed)) = owner.admit(profile, received) else {
            return;
        };
        self.update(|evidence| evidence.metadata_available = true);
        if changed {
            if let Some(boundary) = received.checked_sub(std::time::Duration::from_nanos(1)) {
                owner.reset = Some(boundary);
                self.interrupt_metadata(boundary);
            }
            self.update(|evidence| evidence.metadata_loss += 1);
        }
        if self
            .try_send(Input::Metadata {
                bytes: bytes.to_vec(),
                compression,
                loss,
                received,
                received_ms: super::unix_ms(),
            })
            .is_err()
        {
            self.update(|evidence| {
                evidence.queue_drops += 1;
                evidence.metadata_loss += 1;
            });
        }
        drop(owner);
    }

    fn interrupt_metadata(&self, boundary: Instant) {
        let mut interruptions = self
            .interruptions
            .lock()
            .expect("event interruption state is not poisoned");
        interruptions.1 = Some(
            interruptions
                .1
                .map_or(boundary, |previous| previous.max(boundary)),
        );
    }

    pub fn take_interruptions(&self) -> (Option<Instant>, Option<Instant>) {
        std::mem::take(
            &mut *self
                .interruptions
                .lock()
                .expect("event interruption state is not poisoned"),
        )
    }

    pub fn try_send(&self, input: Input) -> Result<(), Input> {
        if matches!(input, Input::MetadataLost) {
            self.interrupt(&input);
        }
        let size = input.bytes();
        if self
            .queued
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(size)
                    .filter(|next| *next <= QUEUED_BYTES_MAX)
            })
            .is_err()
        {
            self.interrupt(&input);
            return Err(input);
        }
        match self.sent.try_send(input) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(input) | TrySendError::Disconnected(input)) => {
                self.queued.fetch_sub(size, Ordering::AcqRel);
                self.interrupt(&input);
                Err(input)
            }
        }
    }

    fn interrupt(&self, input: &Input) {
        let mut interruptions = self
            .interruptions
            .lock()
            .expect("event interruption state is not poisoned");
        let (cutoff, received) = match input {
            Input::Disconnected => (&mut interruptions.0, Instant::now()),
            Input::MetadataLost => (&mut interruptions.1, Instant::now()),
            Input::Metadata { received, .. } => (&mut interruptions.1, *received),
            Input::Pull { .. } | Input::Snapshot { .. } => return,
        };
        *cutoff = Some(cutoff.map_or(received, |previous| previous.max(received)));
    }

    pub fn consumed(&self, input: &Input) {
        self.queued.fetch_sub(input.bytes(), Ordering::AcqRel);
    }

    pub fn update(&self, update: impl FnOnce(&mut Evidence)) {
        update(
            &mut self
                .evidence
                .lock()
                .expect("native event evidence is not poisoned"),
        );
    }
}

#[cfg(test)]
mod continuity_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use onvif::{
        event::{Client, Endpoint},
        soap::client::Credentials,
    };
    use std::time::Duration;
    use test_hikvision::onvif::FakeOnvif;

    #[test]
    fn unavailable_pull_limits_do_not_become_positive_advertisement() {
        let fake = FakeOnvif::builder().start().unwrap();
        fake.next_response(test_hikvision::Reply::http(
            503,
            "text/plain",
            "unavailable",
        ))
        .unwrap();
        let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
        let mut client = Client::new(
            endpoint.clone(),
            Credentials {
                username: "test".to_owned(),
                password: "test".to_owned(),
            },
        )
        .unwrap();
        let service = client
            .event_service(endpoint, Duration::from_secs(2))
            .unwrap();
        assert_eq!(service.max_pull_points(), None);
        assert!(
            service.pull_supported(),
            "unknown limits still permit a bounded subscription attempt"
        );
        let registry = Registry::default();
        registry.record_service(fake.address().ip(), &service);
        let evidence = registry.snapshot(fake.address().ip()).unwrap();
        assert_eq!(evidence.pull_advertised, None);
        assert!(!evidence.pull_capable);
    }

    #[test]
    fn policy_capacity_fails_closed_and_removed_generations_are_not_reused() {
        let registry = Registry::default();
        for offset in 0..1024_u32 {
            registry.configure(
                std::net::Ipv4Addr::from(0xc000_0200 + offset).into(),
                EventConfig::default(),
                false,
                Shutdown::new(),
            );
        }
        let rejected_ip = "198.51.100.1".parse().unwrap();
        let rejected = Shutdown::new();
        registry.configure(rejected_ip, EventConfig::default(), false, rejected.clone());
        assert!(rejected.is_cancelled());
        assert!(!registry.vendor_enabled(rejected_ip));
        assert!(!registry.enabled(rejected_ip));
        assert_eq!(registry.policies.lock().unwrap().entries.len(), 1024);
        let first = "192.0.2.0".parse().unwrap();
        let revision = registry.vendor_revision(first);
        registry.remove(first);
        assert!(!registry.vendor_enabled(rejected_ip));
        registry.configure(first, EventConfig::default(), false, Shutdown::new());
        assert!(registry.vendor_revision(first) > revision);
        assert!(registry.vendor_enabled(first));
    }

    #[test]
    fn snapshot_evidence_preserves_queries_and_rejects_unsafe_endpoints() {
        let registry = Registry::default();
        let ip = "127.0.0.1".parse().unwrap();
        for uri in [
            "http://127.0.0.1:8080/private?token=secret",
            "https://127.0.0.1:8443/private?token=secret",
        ] {
            assert!(registry.record_snapshot(ip, Some(uri)));
            let endpoint = registry.snapshot_endpoint(ip).unwrap();
            assert_eq!(endpoint.as_str(), uri);
            assert!(!format!("{endpoint:?} {endpoint}").contains("secret"));
        }
        let previous = registry.snapshot_endpoint(ip).unwrap();
        for uri in [
            "http://192.0.2.1/private",
            "http://localhost/private",
            "http://0.0.0.0/private",
            "http://[::]/private",
            "http://test:secret@127.0.0.1/private",
            "http://test@127.0.0.1/private",
            "http://127.0.0.1/private#fragment",
            "http://127.0.0.1:0/private",
            "http://127.0.0.1:65536/private",
            "ftp://127.0.0.1/private",
            "file:///private",
            "http://127.0.0.1/with space",
            "http://127.0.0.1/with\nnewline",
            "http://127.0.0.1/with\\backslash",
            "//127.0.0.1/private",
            "/private",
            "",
        ] {
            assert!(!registry.record_snapshot(ip, Some(uri)));
            assert_eq!(registry.snapshot_endpoint(ip).unwrap(), previous);
        }
        let oversized = format!("http://127.0.0.1/{}", "a".repeat(4096));
        assert!(!registry.record_snapshot(ip, Some(&oversized)));
        assert!(!registry.record_snapshot(ip, None));
        assert_eq!(registry.snapshot_endpoint(ip).unwrap(), previous);
        assert!(registry.snapshot(ip).is_none());
    }

    #[test]
    fn snapshot_evidence_is_ip_bound_and_removed_across_clones() {
        let registry = Registry::default();
        let cloned = registry.clone();
        let ip = "::1".parse().unwrap();
        let uri = "https://[::1]:8443/private?profile=main";
        assert!(registry.record_snapshot(ip, Some(uri)));
        assert_eq!(cloned.snapshot_endpoint(ip).unwrap().as_str(), uri);
        assert!(!registry.record_snapshot("127.0.0.1".parse().unwrap(), Some(uri)));
        for ip in ["0.0.0.0", "::", "224.0.0.1", "ff02::1", "255.255.255.255"] {
            let ip: IpAddr = ip.parse().unwrap();
            let address = std::net::SocketAddr::new(ip, 80);
            assert!(!registry.record_snapshot(ip, Some(&format!("http://{address}/private"))));
            assert!(registry.snapshot_endpoint(ip).is_none());
        }
        cloned.remove(ip);
        assert!(registry.snapshot_endpoint(ip).is_none());
    }

    #[test]
    fn snapshot_evidence_capacity_allows_replacement_and_reclaimed_entries() {
        let registry = Registry::default();
        for offset in 0..1024_u32 {
            let ip = IpAddr::from(std::net::Ipv4Addr::from(0xc612_0000 + offset));
            assert!(registry.record_snapshot(ip, Some(&format!("http://{ip}/snapshot"))));
        }
        let extra = "198.19.0.1".parse().unwrap();
        assert!(!registry.record_snapshot(extra, Some("http://198.19.0.1/snapshot")));
        assert!(registry.snapshot_endpoint(extra).is_none());
        let existing = "198.18.0.0".parse().unwrap();
        let updated = "https://198.18.0.0:8443/updated?token=secret";
        assert!(registry.record_snapshot(existing, Some(updated)));
        assert_eq!(
            registry.snapshot_endpoint(existing).unwrap().as_str(),
            updated
        );
        registry.remove(existing);
        assert!(registry.snapshot_endpoint(existing).is_none());
        assert!(registry.record_snapshot(extra, Some("http://198.19.0.1/snapshot")));
    }

    #[test]
    fn discovered_service_is_retained_without_claiming_a_working_subscription() {
        let fake = FakeOnvif::builder().start().unwrap();
        let endpoint = Endpoint::new(fake.events_endpoint()).unwrap();
        let mut client = Client::new(
            endpoint.clone(),
            Credentials {
                username: "test".to_owned(),
                password: "test".to_owned(),
            },
        )
        .unwrap();
        let service = client
            .event_service(endpoint, Duration::from_secs(2))
            .unwrap();
        let registry = Registry::default();
        let ip = fake.address().ip();
        assert!(registry.snapshot(ip).is_none());
        registry.record_service(ip, &service);
        assert!(registry.service(ip).is_some());
        let evidence = registry.snapshot(ip).unwrap();
        assert_eq!(evidence.pull_advertised, Some(true));
        assert!(!evidence.pull_capable);
        assert_eq!(evidence.state, "discovered");
        assert!(!serde_json::to_string(&evidence).unwrap().contains("/onvif"));
        registry.remove(ip);
        assert!(registry.service(ip).is_none());
        assert!(registry.snapshot(ip).is_none());
    }
}
