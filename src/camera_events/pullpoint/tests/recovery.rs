use std::sync::Arc;
use std::thread;
use std::time::Duration;

use test_hikvision::{Reply, onvif::FakeOnvif};

use super::{delayed_unavailable, finish, producer, soap};

#[test]
fn absent_event_service_stops_discovery_with_unsupported_evidence() {
    let fake = FakeOnvif::builder().start().unwrap();
    fake.next_response(soap(
        200,
        "<d:GetServicesResponse xmlns:d='http://www.onvif.org/ver10/device/wsdl'/>",
    ))
    .unwrap();
    let (producer, _received) = producer(&fake);
    let shutdown = producer.shutdown.clone();
    let slot = Arc::clone(&producer.slot);
    let handle = thread::spawn(move || {
        producer.run();
        Ok(())
    });
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(500));

    assert!(finished, "absent service kept the discovery loop alive");
    result.unwrap();
    assert_eq!(
        fake.requests()
            .iter()
            .filter(|request| request.authenticated())
            .count(),
        1
    );
    assert_eq!(fake.subscription_count(), 0);
    let evidence = slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.state, "unsupported");
    assert_eq!(evidence.reconnects, 0);
    assert!(!evidence.pull_capable);
}

#[test]
fn zero_pull_capacity_stops_without_attempting_subscription() {
    let fake = FakeOnvif::builder().start().unwrap();
    fake.next_response(soap(200, "<e:GetServiceCapabilitiesResponse><e:Capabilities MaxPullPoints='0'/></e:GetServiceCapabilitiesResponse>"))
        .unwrap();
    let (mut producer, _received) = producer(&fake);
    producer.camera.events.event_service_url = Some(fake.events_endpoint());
    let shutdown = producer.shutdown.clone();
    let slot = Arc::clone(&producer.slot);
    let handle = thread::spawn(move || {
        producer.run();
        Ok(())
    });
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(500));

    assert!(finished, "zero PullPoint capacity was retried");
    result.unwrap();
    assert_eq!(fake.subscription_count(), 0);
    let evidence = slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.state, "unsupported");
    assert_eq!(evidence.reconnects, 0);
    assert!(!evidence.pull_capable);
}

#[test]
fn authentication_failure_is_terminal_and_visible_without_reconnects() {
    let fake = FakeOnvif::builder().start().unwrap();
    let (mut producer, _received) = producer(&fake);
    producer.camera.password = "wrong".to_owned();
    let shutdown = producer.shutdown.clone();
    let slot = Arc::clone(&producer.slot);
    let handle = thread::spawn(move || {
        producer.run();
        Ok(())
    });
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(500));

    assert!(finished, "rejected credentials were retried");
    result.unwrap();
    assert!(fake.requests().len() <= 3);
    assert_eq!(fake.subscription_count(), 0);
    let evidence = slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.state, "authentication-failed");
    assert_eq!(evidence.reconnects, 0);
    assert!(!evidence.pull_capable);
}

#[test]
fn cleanup_authentication_failure_is_not_hidden_by_a_prior_pull_failure() {
    let fake = FakeOnvif::builder().start().unwrap();
    for _ in 0..2 {
        fake.next_pull_response(Reply::http(503, "text/plain", "unavailable"))
            .unwrap();
    }
    fake.next_pull_response(delayed_unavailable()).unwrap();
    let (producer, _received) = producer(&fake);
    let shutdown = producer.shutdown.clone();
    let slot = Arc::clone(&producer.slot);
    let handle = thread::spawn(move || {
        producer.run();
        Ok(())
    });
    let pending = fake.wait_for_pulls(3, Duration::from_secs(1));
    fake.next_response(Reply::http(401, "text/plain", "denied"))
        .unwrap();
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(1700));

    assert!(pending);
    assert!(finished, "cleanup authentication failure was retried");
    result.unwrap();
    assert_eq!(fake.subscription_count(), 1);
    assert_eq!(fake.unsubscribe_count(), 0);
    let evidence = slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.state, "authentication-failed");
    assert_eq!(evidence.reconnects, 0);
    assert!(!evidence.unsubscribed);
}

#[test]
fn rejected_creation_does_not_advertise_a_working_pullpoint() {
    let fake = FakeOnvif::builder().start().unwrap();
    fake.next_response(soap(200, "<e:GetServiceCapabilitiesResponse><e:Capabilities MaxPullPoints='1'/></e:GetServiceCapabilitiesResponse>"))
        .unwrap();
    fake.next_response(soap(200, "<e:GetEventPropertiesResponse/>"))
        .unwrap();
    fake.next_response(Reply::http(403, "text/plain", "denied"))
        .unwrap();
    let (mut producer, _received) = producer(&fake);
    producer.camera.events.event_service_url = Some(fake.events_endpoint());
    let shutdown = producer.shutdown.clone();
    let slot = Arc::clone(&producer.slot);
    let handle = thread::spawn(move || {
        producer.run();
        Ok(())
    });
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(500));

    assert!(finished);
    result.unwrap();
    assert_eq!(fake.subscription_count(), 0);
    let evidence = slot.evidence.lock().unwrap().clone();
    assert!(
        !evidence.pull_capable,
        "capability was set before creation succeeded"
    );
    assert_eq!(evidence.state, "authentication-failed");
    assert_eq!(evidence.reconnects, 0);
}

#[test]
fn valid_pull_resets_backoff_after_consecutive_failed_subscriptions() {
    let fake = FakeOnvif::builder().start().unwrap();
    for _ in 0..6 {
        fake.next_pull_response(Reply::http(503, "text/plain", "unavailable"))
            .unwrap();
    }
    fake.next_pull_response(soap(200, "<e:PullMessagesResponse><e:CurrentTime>2000-01-01T00:00:00Z</e:CurrentTime><e:TerminationTime>2000-01-01T00:01:30Z</e:TerminationTime></e:PullMessagesResponse>"))
        .unwrap();
    for _ in 0..3 {
        fake.next_pull_response(Reply::http(503, "text/plain", "unavailable"))
            .unwrap();
    }
    fake.next_pull_response(Reply::http(401, "text/plain", "denied"))
        .unwrap();
    let (producer, _received) = producer(&fake);
    let shutdown = producer.shutdown.clone();
    let slot = Arc::clone(&producer.slot);
    let handle = thread::spawn(move || {
        producer.run();
        Ok(())
    });
    let progressed = fake.wait_for_pulls(10, Duration::from_secs(5));
    let retried_promptly = fake.wait_for_pulls(11, Duration::from_secs(2));
    let (finished, result) = finish(handle, &shutdown, Duration::from_millis(200));

    assert!(progressed);
    assert!(
        retried_promptly,
        "valid pull did not reset the growing reconnect backoff"
    );
    assert!(finished);
    result.unwrap();
    assert_eq!(fake.subscription_count(), 4);
    assert_eq!(fake.unsubscribe_count(), 4);
    assert_eq!(fake.active_subscriptions(), 0);
    let evidence = slot.evidence.lock().unwrap().clone();
    assert_eq!(evidence.pulls, 1);
    assert_eq!(evidence.empty_pulls, 1);
    assert_eq!(evidence.reconnects, 3);
    assert_eq!(evidence.state, "authentication-failed");
}
