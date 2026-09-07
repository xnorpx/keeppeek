use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use onvif::{
    event::{
        Client, ClientError, Endpoint, Lease, Operation, Pull, PullLimits, Request, Service,
        Subscription,
    },
    soap::client::Credentials,
};

use super::registry::{Input, Slot};
use crate::cameras::{Camera, CameraConfig, events::EventMode};
use crate::shutdown::Shutdown;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const SUBSCRIPTION_LIFETIME: Duration = Duration::from_secs(90);
const PRESSURE_TIMEOUT: Duration = Duration::from_secs(2);
const DELIVERY_INTERVAL: Duration = Duration::from_millis(25);
const SYNCHRONIZE_TIMEOUT: Duration = Duration::from_millis(500);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const REQUEST_MIN: Duration = Duration::from_millis(1);
const RECONNECT_MIN: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(30);
const FAILURES_MAX: u8 = 3;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod service_cache_tests {
    use super::*;
    use crate::camera_events::registry::Registry;
    use crate::cameras::configured_cameras;
    use std::collections::HashMap;
    use std::sync::mpsc;
    use test_hikvision::{Reply, onvif::FakeOnvif};

    fn producer(
        fake: &FakeOnvif,
        service: Option<onvif::event::Service>,
    ) -> (Producer, mpsc::Receiver<Input>) {
        let mut config: CameraConfig = toml::from_str(
            "ip='127.0.0.1'\nusername='test'\npassword='test'\n[events]\nmode='onvif-pullpoint'\nsnapshots=false\n",
        )
        .unwrap();
        config.onvif_port = Some(fake.address().port());
        let mut camera = configured_cameras(&HashMap::from([("cameras".to_owned(), vec![config])]))
            .remove(&fake.address().ip())
            .unwrap();
        camera.event_service = service;
        let shutdown = Shutdown::new();
        let (sent, received) = mpsc::sync_channel(32);
        let slot = Registry::default()
            .install(
                camera.config.ip,
                sent,
                shutdown.clone(),
                camera.config.events.clone(),
            )
            .unwrap();
        (Producer::new(&camera, slot, shutdown), received)
    }

    fn calls(fake: &FakeOnvif, operation: &str) -> usize {
        fake.requests()
            .iter()
            .filter(|request| {
                request.authenticated()
                    && request
                        .header("content-type")
                        .is_some_and(|value| value.contains(operation))
            })
            .count()
    }

    #[test]
    fn event_service_discovery_cache_survives_resource_unknown() {
        let fake = FakeOnvif::builder().start().unwrap();
        for _ in 0..3 {
            fake.next_pull_response(Reply::http(500, "application/soap+xml", concat!(
                "<s:Envelope xmlns:s='http://www.w3.org/2003/05/soap-envelope' ",
                "xmlns:r='http://docs.oasis-open.org/wsrf/r-2'><s:Body><s:Fault>",
                "<s:Code><s:Value>s:Sender</s:Value><s:Subcode><s:Value>r:ResourceUnknown</s:Value>",
                "</s:Subcode></s:Code><s:Reason><s:Text xml:lang='en'>expired</s:Text></s:Reason>",
                "</s:Fault></s:Body></s:Envelope>"
            )))
            .unwrap();
        }
        let (mut producer, _received) = producer(&fake, None);

        let error = producer.subscribe().unwrap_err();
        assert!(
            error
                .downcast_ref::<ClientError>()
                .is_some_and(|error| error.is_expired())
        );
        producer.shutdown.cancel();
        producer.subscribe().unwrap();

        assert_eq!(calls(&fake, "GetServices"), 1);
        assert_eq!(calls(&fake, "GetServiceCapabilities"), 1);
        assert_eq!(calls(&fake, "GetEventProperties"), 1);
        assert_eq!(fake.subscription_count(), 2);
        assert_eq!(fake.unsubscribe_count(), 2);
        assert_eq!(fake.active_subscriptions(), 0);
    }

    #[test]
    fn event_service_discovery_uses_camera_cache_unless_overridden() {
        let fake = FakeOnvif::builder().start().unwrap();
        let device = Endpoint::new(format!("{}/onvif/device_service", fake.origin())).unwrap();
        let mut client = Client::new(
            device,
            Credentials {
                username: "test".to_owned(),
                password: "test".to_owned(),
            },
        )
        .unwrap();
        let service = client.discover(REQUEST_TIMEOUT).unwrap().unwrap();
        let (mut producer, _received) = producer(&fake, Some(service));
        producer.shutdown.cancel();

        producer.subscribe().unwrap();

        assert_eq!(calls(&fake, "GetServices"), 1);
        assert_eq!(calls(&fake, "GetServiceCapabilities"), 1);
        assert_eq!(calls(&fake, "GetEventProperties"), 1);
        let override_fake = FakeOnvif::builder().start().unwrap();
        producer.camera.events.event_service_url = Some(override_fake.events_endpoint());

        producer.subscribe().unwrap();

        assert_eq!(override_fake.subscription_count(), 1);
        assert_eq!(override_fake.unsubscribe_count(), 1);
        assert_eq!(calls(&override_fake, "GetServices"), 0);
        assert_eq!(calls(&override_fake, "GetServiceCapabilities"), 1);
        assert_eq!(calls(&fake, "GetServiceCapabilities"), 1);
    }
}

pub(super) struct Producer {
    camera: CameraConfig,
    endpoint: String,
    service: Option<Service>,
    slot: Arc<Slot>,
    shutdown: Shutdown,
}

#[derive(Clone, Copy, Debug)]
struct LeaseClock {
    expires: Instant,
    renew_at: Instant,
}

#[derive(Debug)]
struct Unsupported;

impl std::fmt::Display for Unsupported {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("camera has no usable ONVIF PullPoint event service")
    }
}

impl std::error::Error for Unsupported {}

#[derive(Debug)]
struct DeliveryTimeout {
    deadline: Instant,
}

impl std::fmt::Display for DeliveryTimeout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ONVIF event delivery did not complete before its deadline")
    }
}

impl std::error::Error for DeliveryTimeout {}

impl LeaseClock {
    fn new(lease: Lease, started: Instant) -> Self {
        Self {
            expires: started + lease.remaining(),
            renew_at: started + lease.renew_after(),
        }
    }

    fn remaining(self) -> Duration {
        self.expires.saturating_duration_since(Instant::now())
    }

    fn budget(self, maximum: Duration) -> anyhow::Result<Duration> {
        let timeout = maximum.min(self.remaining());
        anyhow::ensure!(
            timeout >= REQUEST_MIN,
            "ONVIF subscription lease budget exhausted"
        );
        Ok(timeout)
    }

    fn pulled(&mut self, lease: Lease, started: Instant) {
        let reported = Self::new(lease, started);
        self.expires = self.expires.min(reported.expires);
        self.renew_at = self.renew_at.min(reported.renew_at);
    }

    fn remaining_ms(self) -> u64 {
        u64::try_from(self.remaining().as_millis()).expect("validated lease fits milliseconds")
    }
}

fn wire_timeout(timeout: Duration) -> Duration {
    Duration::from_millis(
        u64::try_from(timeout.as_millis()).expect("bounded pull timeout fits milliseconds"),
    )
}

fn reduced_limits(current: PullLimits, offered: PullLimits) -> Option<PullLimits> {
    let reduced = PullLimits {
        timeout: wire_timeout(current.timeout.min(offered.timeout)),
        messages: current.messages.min(offered.messages),
    };
    if reduced.timeout < REQUEST_MIN || reduced.messages == 0 {
        return None;
    }
    (reduced.timeout < current.timeout || reduced.messages < current.messages).then_some(reduced)
}

const fn request_state(error: ClientError) -> Option<&'static str> {
    if error.is_authentication() || matches!(error.http_status(), Some(401 | 403)) {
        Some("authentication-failed")
    } else if matches!(error.http_status(), Some(404 | 405)) {
        Some("unsupported")
    } else {
        None
    }
}

fn terminal_state(error: &anyhow::Error) -> Option<&'static str> {
    if error.is::<Unsupported>() {
        Some("unsupported")
    } else {
        error
            .downcast_ref::<ClientError>()
            .copied()
            .and_then(request_state)
    }
}

impl Producer {
    pub fn new(camera: &Camera, slot: Arc<Slot>, shutdown: Shutdown) -> Self {
        let address = std::net::SocketAddr::new(
            camera.config.ip,
            camera
                .config
                .onvif_port
                .or(camera.ports.onvif)
                .unwrap_or(8000),
        );
        Self {
            camera: camera.config.clone(),
            endpoint: format!("http://{address}/onvif/device_service"),
            service: camera.event_service.clone(),
            slot,
            shutdown,
        }
    }

    pub fn run(mut self) {
        if matches!(
            self.camera.events.mode,
            EventMode::Disabled | EventMode::Vendor | EventMode::RtspMetadata
        ) {
            return;
        }
        let mut backoff = RECONNECT_MIN;
        while !self.shutdown.is_cancelled() {
            let progress = self.progress();
            let result = self.subscribe();
            let reset_deadline = result
                .as_ref()
                .err()
                .and_then(|error| error.downcast_ref::<DeliveryTimeout>())
                .map_or_else(|| Instant::now() + PRESSURE_TIMEOUT, |error| error.deadline);
            if self.send(Input::Disconnected, reset_deadline).is_err() {
                break;
            }
            if self.shutdown.is_cancelled() {
                break;
            }
            if let Some(state) = result.as_ref().err().and_then(terminal_state) {
                self.slot.update(|evidence| {
                    evidence.state = state;
                    if state == "unsupported" {
                        evidence.pull_capable = false;
                    }
                });
                tracing::warn!(name: "camera.events.pull.stopped", camera_ip = %self.camera.ip, state, "ONVIF event producer stopped after a terminal failure");
                break;
            }
            if self.progress() != progress {
                backoff = RECONNECT_MIN;
            }
            self.slot.update(|evidence| {
                evidence.state = "reconnecting";
                evidence.reconnects += 1;
            });
            tracing::warn!(name: "camera.events.pull.retry", camera_ip = %self.camera.ip, backoff_ms = backoff.as_millis(), "ONVIF event subscription unavailable; reconnect scheduled");
            if self.shutdown.wait_timeout(backoff) {
                break;
            }
            backoff = (backoff * 2).min(RECONNECT_MAX);
        }
        if self.shutdown.is_cancelled() {
            self.slot.update(|evidence| evidence.state = "stopped");
        }
    }

    fn progress(&self) -> (u64, u64) {
        let evidence = self
            .slot
            .evidence
            .lock()
            .expect("native event evidence is not poisoned");
        (evidence.pulls, evidence.renewals)
    }

    fn subscribe(&mut self) -> anyhow::Result<()> {
        self.slot.update(|evidence| evidence.state = "discovering");
        let device = Endpoint::new(&self.endpoint)?;
        let mut client = Client::new(
            device.clone(),
            Credentials {
                username: self.camera.username.clone(),
                password: self.camera.password.clone(),
            },
        )?;
        let service = if let Some(endpoint) = &self.camera.events.event_service_url {
            client.event_service(device.resolve(endpoint)?, REQUEST_TIMEOUT)?
        } else if let Some(service) = &self.service {
            service.clone()
        } else {
            let service = client.discover(REQUEST_TIMEOUT)?.ok_or(Unsupported)?;
            self.service = Some(service.clone());
            service
        };
        if !service.pull_supported() {
            return Err(Unsupported.into());
        }
        let started = Instant::now();
        let created = client.execute(
            &Request::create(service.endpoint(), SUBSCRIPTION_LIFETIME)?,
            REQUEST_TIMEOUT,
        )?;
        let subscription = Subscription::parse(service.endpoint(), &created)?;
        let mut clock = LeaseClock::new(subscription.lease(), started);
        self.slot.update(|evidence| {
            evidence.pull_capable = true;
            evidence.unsubscribed = false;
            for kind in service.kinds() {
                let kind = kind.to_string();
                if !evidence.kinds.contains(&kind) {
                    evidence.kinds.push(kind);
                }
            }
        });
        let result = self.observe(&mut client, &subscription, &mut clock);
        let cleanup = self.unsubscribe(&mut client, &subscription, clock);
        match (result, cleanup) {
            (Err(error), Err(cleanup)) => {
                if let Some(timeout) = error.downcast_ref::<DeliveryTimeout>() {
                    Err(cleanup.context(DeliveryTimeout {
                        deadline: timeout.deadline,
                    }))
                } else if terminal_state(&error).is_some() {
                    Err(error.context(cleanup))
                } else {
                    Err(cleanup.context(error))
                }
            }
            (result, Ok(())) | (Ok(()), result) => result,
        }
    }

    fn observe(
        &self,
        client: &mut Client,
        subscription: &Subscription,
        clock: &mut LeaseClock,
    ) -> anyhow::Result<()> {
        self.slot.update(|evidence| {
            evidence.state = "subscribed";
            evidence.lease_ms = clock.remaining_ms();
        });
        self.synchronize(client, subscription, clock)?;
        let mut limits = PullLimits {
            timeout: Duration::from_secs(2),
            messages: 32,
        };
        let mut failures = 0;
        while !self.shutdown.is_cancelled() {
            if Instant::now() >= clock.renew_at {
                self.renew(client, subscription, clock)?;
            }
            let started = Instant::now();
            let budget = clock.budget(clock.renew_at.saturating_duration_since(started))?;
            let pull_timeout = wire_timeout(limits.timeout.min(budget / 2));
            if pull_timeout < REQUEST_MIN {
                clock.renew_at = started;
                continue;
            }
            let response = client.execute(
                &subscription.request(Operation::Pull {
                    timeout: pull_timeout,
                    limit: limits.messages,
                })?,
                budget.min(pull_timeout + Duration::from_secs(2)),
            );
            match response {
                Ok(bytes) => {
                    self.deliver(bytes, clock, started)?;
                    failures = 0;
                }
                Err(error) => {
                    if let Some(offered) = error.pull_limits() {
                        limits = reduced_limits(
                            PullLimits {
                                timeout: pull_timeout,
                                messages: limits.messages,
                            },
                            offered,
                        )
                        .ok_or(error)
                        .context("camera did not lower usable pull limits")?;
                    } else {
                        failures += 1;
                        if error.is_expired()
                            || request_state(error).is_some()
                            || failures >= FAILURES_MAX
                        {
                            return Err(error.into());
                        }
                    }
                }
            }
            self.shutdown.wait_timeout(
                POLL_INTERVAL
                    .saturating_sub(started.elapsed())
                    .min(clock.renew_at.saturating_duration_since(Instant::now())),
            );
        }
        Ok(())
    }

    fn synchronize(
        &self,
        client: &mut Client,
        subscription: &Subscription,
        clock: &mut LeaseClock,
    ) -> anyhow::Result<()> {
        let timeout = clock.budget(SYNCHRONIZE_TIMEOUT.min(clock.remaining() / 3))?;
        if let Err(error) = client.execute(&subscription.request(Operation::Synchronize)?, timeout)
        {
            if error.is_authentication() || matches!(error.http_status(), Some(401 | 403)) {
                return Err(error.into());
            }
            tracing::debug!(name: "camera.events.synchronize.unavailable", camera_ip = %self.camera.ip, "ONVIF synchronization was not available");
            clock.renew_at = Instant::now();
        }
        Ok(())
    }

    fn renew(
        &self,
        client: &mut Client,
        subscription: &Subscription,
        clock: &mut LeaseClock,
    ) -> anyhow::Result<()> {
        let started = Instant::now();
        let result = (|| {
            let timeout = clock.budget(REQUEST_TIMEOUT.min(clock.remaining() / 2))?;
            let bytes = client.execute(
                &subscription.request(Operation::Renew {
                    lifetime: SUBSCRIPTION_LIFETIME,
                })?,
                timeout,
            )?;
            let renewed = LeaseClock::new(Lease::parse(&bytes)?, started);
            renewed.budget(REQUEST_TIMEOUT)?;
            Ok::<_, anyhow::Error>(renewed)
        })();
        self.slot.update(|evidence| {
            evidence.renewals += u64::from(result.is_ok());
            evidence.renew_failures += u64::from(result.is_err());
        });
        *clock = result?;
        self.slot
            .update(|evidence| evidence.lease_ms = clock.remaining_ms());
        Ok(())
    }

    fn deliver(
        &self,
        bytes: Vec<u8>,
        clock: &mut LeaseClock,
        started: Instant,
    ) -> anyhow::Result<()> {
        let received = Instant::now();
        let received_ms = super::unix_ms();
        let received_time = chrono::DateTime::from_timestamp_millis(received_ms)
            .context("ONVIF receipt time is out of range")?;
        let pull = Pull::parse_at(&bytes, received_time).inspect_err(|_| {
            self.slot.update(|evidence| evidence.parse_errors += 1);
        })?;
        clock.pulled(pull.lease, started);
        self.slot.update(|evidence| {
            evidence.pulls += 1;
            evidence.empty_pulls +=
                u64::from(pull.notifications.is_empty() && pull.invalid_messages == 0);
            evidence.lease_ms = clock.remaining_ms();
        });
        drop(pull);
        self.send(
            Input::Pull {
                bytes,
                received,
                received_ms,
            },
            clock.renew_at.min(Instant::now() + PRESSURE_TIMEOUT),
        )?;
        if clock.renew_at.saturating_duration_since(Instant::now()) <= started.elapsed() {
            clock.renew_at = Instant::now();
        }
        Ok(())
    }

    fn unsubscribe(
        &self,
        client: &mut Client,
        subscription: &Subscription,
        clock: LeaseClock,
    ) -> anyhow::Result<()> {
        let result = (|| {
            client.execute(
                &subscription.request(Operation::Unsubscribe)?,
                clock.budget(PRESSURE_TIMEOUT)?,
            )?;
            Ok::<_, anyhow::Error>(())
        })();
        self.slot
            .update(|evidence| evidence.unsubscribed = result.is_ok());
        if result.is_err() {
            tracing::warn!(name: "camera.events.unsubscribe.failed", camera_ip = %self.camera.ip, "ONVIF subscription cleanup failed within its lease budget");
        }
        result
    }

    fn send(&self, mut input: Input, deadline: Instant) -> anyhow::Result<()> {
        let pressure_deadline = Instant::now() + PRESSURE_TIMEOUT;
        let deadline = deadline.min(pressure_deadline);
        let mut stalled = false;
        loop {
            match self.slot.try_send(input) {
                Ok(()) => return Ok(()),
                Err(retry) => input = retry,
            }
            if !stalled {
                self.slot.update(|evidence| evidence.delivery_stalls += 1);
                stalled = true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || self.shutdown.is_cancelled() {
                self.slot.update(|evidence| {
                    evidence.queue_drops += 1;
                    evidence.state = "delivery-stalled";
                });
                tracing::warn!(
                    name: "camera.events.delivery.lost",
                    camera_ip = %self.camera.ip,
                    bytes = input.bytes(),
                    disconnected = matches!(input, Input::Disconnected),
                    "ONVIF event delivery exceeded its bounded wait"
                );
                return Err(DeliveryTimeout { deadline }.into());
            }
            self.shutdown.wait_timeout(remaining.min(DELIVERY_INTERVAL));
        }
    }
}
