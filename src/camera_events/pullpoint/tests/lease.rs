use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use onvif::event::Lease;
use test_hikvision::{
    Reply,
    onvif::{FakeOnvif, notification},
};

use super::{Input, LeaseClock, envelope, finish, producer, soap, subscription};

fn pull(remaining: Duration, notifications: &str) -> Vec<u8> {
    let current = chrono::DateTime::parse_from_rfc3339("2000-01-01T00:00:00Z").unwrap();
    let termination = current + chrono::Duration::from_std(remaining).unwrap();
    envelope(&format!("<e:PullMessagesResponse><e:CurrentTime>{}</e:CurrentTime><e:TerminationTime>{}</e:TerminationTime>{notifications}</e:PullMessagesResponse>", current.to_rfc3339(), termination.to_rfc3339())).into_bytes()
}

fn delayed_xml(delay: Duration, body: Vec<u8>) -> Reply {
    Reply::raw(format!("HTTP/1.1 200 OK\r\nContent-Type: application/soap+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).into_bytes())
        .then(delay, body)
}

#[test]
fn delayed_create_uses_request_start_for_its_delivery_deadline() {
    let fake = FakeOnvif::builder()
        .lease(Duration::from_secs(2))
        .notifications(vec![notification(
            "VideoSource/MotionAlarm",
            true,
            "Changed",
            "2000-01-01T00:00:00Z",
            "source-1",
        )])
        .start()
        .unwrap();
    let (_client, existing, _) = subscription(&fake);
    fake.next_response(soap(200, "<e:GetServiceCapabilitiesResponse><e:Capabilities MaxPullPoints='1'/></e:GetServiceCapabilitiesResponse>"))
        .unwrap();
    fake.next_response(soap(200, "<e:GetEventPropertiesResponse/>"))
        .unwrap();
    let created = envelope(&format!(
        "<e:CreatePullPointSubscriptionResponse><e:SubscriptionReference xmlns:a='http://www.w3.org/2005/08/addressing'><a:Address>{}</a:Address><a:ReferenceParameters><f:Identifier xmlns:f='urn:test-hikvision:onvif'>1</f:Identifier></a:ReferenceParameters></e:SubscriptionReference><n:CurrentTime>2000-01-01T00:00:00Z</n:CurrentTime><n:TerminationTime>2000-01-01T00:00:01Z</n:TerminationTime></e:CreatePullPointSubscriptionResponse>",
        existing.endpoint().as_str()
    ));
    fake.next_response(delayed_xml(
        Duration::from_millis(400),
        created.into_bytes(),
    ))
    .unwrap();
    let (mut producer, _received) = producer(&fake);
    producer.camera.events.event_service_url = Some(fake.events_endpoint());
    for _ in 0..32 {
        assert!(producer.slot.try_send(Input::MetadataLost).is_ok());
    }
    let shutdown = producer.shutdown.clone();
    let handle = thread::spawn(move || producer.subscribe());
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(900));

    assert!(finished, "create response time was added back to the lease");
    assert!(result.is_err());
    assert_eq!(fake.pull_count(), 1);
    assert_eq!(fake.renew_count(), 0);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
}

#[test]
fn renewal_uses_camera_remaining_time_minus_the_request_round_trip() {
    let fake = FakeOnvif::builder()
        .lease(Duration::from_secs(2))
        .start()
        .unwrap();
    let (producer, _received) = producer(&fake);
    let (mut client, subscription, started) = subscription(&fake);
    let mut clock = LeaseClock::new(subscription.lease(), started);
    let previous = clock.expires;
    let renewed = envelope(
        "<n:RenewResponse><n:CurrentTime>2000-01-01T00:00:00Z</n:CurrentTime><n:TerminationTime>2000-01-01T00:00:01Z</n:TerminationTime></n:RenewResponse>",
    );
    fake.next_response(delayed_xml(
        Duration::from_millis(200),
        renewed.into_bytes(),
    ))
    .unwrap();

    producer
        .renew(&mut client, &subscription, &mut clock)
        .unwrap();
    let remaining = clock.remaining();
    producer
        .unsubscribe(&mut client, &subscription, clock)
        .unwrap();

    assert!(clock.expires < previous);
    assert!(
        remaining <= Duration::from_millis(850),
        "renewal round trip was not subtracted"
    );
    assert!(remaining > Duration::ZERO);
    let evidence = producer.slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.renewals, 1);
    assert_eq!(evidence.renew_failures, 0);
    assert_eq!(fake.unsubscribe_count(), 1);
}

#[test]
fn malformed_renewal_counts_failure_and_preserves_cleanup_deadline() {
    let fake = FakeOnvif::builder()
        .lease(Duration::from_secs(2))
        .start()
        .unwrap();
    let (producer, _received) = producer(&fake);
    let (mut client, subscription, started) = subscription(&fake);
    let mut clock = LeaseClock::new(subscription.lease(), started);
    let previous = clock.expires;
    fake.next_response(soap(200, "<n:RenewResponse/>")).unwrap();

    let result = producer.renew(&mut client, &subscription, &mut clock);
    producer
        .unsubscribe(&mut client, &subscription, clock)
        .unwrap();

    assert!(result.is_err());
    assert_eq!(clock.expires, previous);
    let evidence = producer.slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.renewals, 0);
    assert_eq!(evidence.renew_failures, 1);
    assert_eq!(fake.unsubscribe_count(), 1);
    assert_eq!(fake.active_subscriptions(), 0);
}

#[test]
fn pull_lease_can_shorten_but_never_extend_the_existing_deadline() {
    let started = Instant::now();
    let lease = Lease::parse(&pull(Duration::from_secs(2), "")).unwrap();
    let mut clock = LeaseClock::new(lease, started);
    let previous = clock;
    clock.pulled(
        Lease::parse(&pull(Duration::from_secs(8), "")).unwrap(),
        started,
    );
    assert_eq!(clock.expires, previous.expires);
    assert_eq!(clock.renew_at, previous.renew_at);

    clock.pulled(
        Lease::parse(&pull(Duration::from_millis(500), "")).unwrap(),
        started,
    );
    assert_eq!(clock.expires, started + Duration::from_millis(500));
    assert!(clock.renew_at < previous.renew_at);
    assert!(clock.budget(Duration::from_secs(5)).unwrap() <= Duration::from_millis(500));
}

#[test]
fn spent_leases_and_sub_millisecond_request_budgets_are_rejected() {
    let lease = Lease::parse(&pull(Duration::from_millis(100), "")).unwrap();
    let spent = LeaseClock::new(lease, Instant::now() - Duration::from_millis(200));
    assert_eq!(spent.remaining(), Duration::ZERO);
    assert!(spent.budget(Duration::from_secs(5)).is_err());

    let live = LeaseClock::new(lease, Instant::now());
    assert!(live.budget(Duration::ZERO).is_err());
    assert!(live.budget(Duration::from_micros(500)).is_err());
}

#[test]
fn malformed_notifications_preserve_current_frames_without_counting_empty_pulls() {
    let fake = FakeOnvif::builder().start().unwrap();
    let (producer, received) = producer(&fake);
    let slot = Arc::clone(&producer.slot);
    let started = Instant::now();
    let mut clock = LeaseClock::new(
        Lease::parse(&pull(Duration::from_secs(90), "")).unwrap(),
        started,
    );
    let valid = notification(
        "VideoSource/MotionAlarm",
        true,
        "Changed",
        "2000-01-01T00:00:00Z",
        "source-1",
    );
    for (messages, valid_count) in [
        ("<n:NotificationMessage/>".to_owned(), 0),
        (format!("<n:NotificationMessage/>{valid}"), 1),
    ] {
        producer
            .deliver(pull(Duration::from_secs(1), &messages), &mut clock, started)
            .unwrap();
        let input = received.recv_timeout(Duration::from_millis(100)).unwrap();
        slot.consumed(&input);
        let Input::Pull { bytes, .. } = input else {
            panic!("current pull frame expected");
        };
        let frame = onvif::event::Pull::parse(&bytes).unwrap();
        assert_eq!(frame.invalid_messages, 1);
        assert_eq!(frame.notifications.len(), valid_count);
    }

    assert_eq!(clock.expires, started + Duration::from_secs(1));
    let evidence = slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.pulls, 2);
    assert_eq!(
        evidence.empty_pulls, 0,
        "malformed notifications are not empty responses"
    );
    assert_eq!(evidence.queue_drops, 0);
}
