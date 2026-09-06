use std::time::{Duration, Instant};

use super::super::{FakeOnvif, clock};
use super::lifecycle::{EVENTS_NS, WSNT, create, pull};
use super::transport::{Client, DEVICE, GET_SERVICES, envelope, texts};

#[test]
fn captures_keep_the_newest_256_and_never_stop_device_routing() {
    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = Client::new(&fake);
    for index in 0..260 {
        let body = envelope(GET_SERVICES, &format!("<marker>{index}</marker>"));
        assert_eq!(client.post(DEVICE, &body).0, 200);
    }
    let requests = fake.requests();
    assert_eq!(requests.len(), 256);
    assert!(
        requests[0]
            .body()
            .windows(18)
            .any(|part| part == b"<marker>4</marker>")
    );
    assert!(
        std::str::from_utf8(requests.last().unwrap().body())
            .unwrap()
            .contains("<marker>259</marker>")
    );
    assert!(
        requests
            .iter()
            .all(super::super::CapturedRequest::authenticated)
    );
}

#[test]
fn capture_byte_budget_evicts_old_bodies() {
    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = Client::new(&fake);
    let body = envelope(
        GET_SERVICES,
        &format!("<padding>{}</padding>", "x".repeat(255 * 1024)),
    );
    for _ in 0..40 {
        assert_eq!(client.post(DEVICE, &body).0, 200);
    }
    let requests = fake.requests();
    assert!(requests.len() < 40);
    assert!(
        requests
            .iter()
            .map(super::super::CapturedRequest::byte_len)
            .sum::<usize>()
            <= 8 * 1024 * 1024
    );
}

#[test]
fn timestamps_advance_and_renewals_extend_real_fractional_leases() {
    let fake = FakeOnvif::builder()
        .lease(Duration::from_secs(1))
        .start()
        .unwrap();
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    let (_, first) = client.post(&target, &pull("PT0S", 1, &header));
    let (status, later) = client.post(&target, &pull("PT0.03S", 1, &header));
    assert_eq!(status, 200);
    assert!(
        texts(&later, EVENTS_NS, "CurrentTime")[0] > texts(&first, EVENTS_NS, "CurrentTime")[0]
    );
    let (status, renewed) = client.post(
        &target,
        &envelope(
            "<wsnt:Renew><wsnt:TerminationTime>PT1S</wsnt:TerminationTime></wsnt:Renew>",
            &header,
        ),
    );
    assert_eq!(status, 200);
    assert!(
        texts(&renewed, WSNT, "TerminationTime")[0]
            > texts(&first, EVENTS_NS, "TerminationTime")[0]
    );
    let (status, short) = client.post(
        &target,
        &envelope(
            "<wsnt:Renew><wsnt:TerminationTime>PT0.005S</wsnt:TerminationTime></wsnt:Renew>",
            &header,
        ),
    );
    assert_eq!(status, 200);
    let current = texts(&short, WSNT, "CurrentTime").pop().unwrap();
    let termination = texts(&short, WSNT, "TerminationTime").pop().unwrap();
    let current_seconds = current[17..current.len() - 1].parse::<f64>().unwrap();
    let termination_seconds = termination[17..termination.len() - 1]
        .parse::<f64>()
        .unwrap();
    assert!((termination_seconds - current_seconds - 0.005).abs() < 0.000_001);
}

#[test]
fn timeout_of_one_minute_never_blocks_longer_than_two_seconds() {
    let fake = FakeOnvif::builder().start().unwrap();
    let mut client = Client::new(&fake);
    let (target, header) = create(&mut client);
    let started = Instant::now();
    let (status, body) = client.post(&target, &pull("PT1M", 1, &header));
    assert_eq!(status, 200);
    assert!(started.elapsed() >= Duration::from_millis(1900));
    assert!(started.elapsed() < Duration::from_millis(2500));
    assert!(texts(&body, WSNT, "Topic").is_empty());
}

#[test]
fn fixture_calendar_handles_fractional_time_midnight_and_leap_boundaries() {
    assert_eq!(
        clock::timestamp(Duration::ZERO),
        "2000-01-01T00:00:00.000000000Z"
    );
    assert_eq!(
        clock::timestamp(Duration::from_millis(86_400_001)),
        "2000-01-02T00:00:00.001000000Z"
    );
    assert_eq!(
        clock::timestamp(Duration::from_secs(59 * 86_400)),
        "2000-02-29T00:00:00.000000000Z"
    );
    assert_eq!(
        clock::timestamp(Duration::from_secs(60 * 86_400)),
        "2000-03-01T00:00:00.000000000Z"
    );
    assert_eq!(
        clock::timestamp(Duration::from_secs((36_525 + 59) * 86_400)),
        "2100-03-01T00:00:00.000000000Z"
    );
    assert_eq!(
        clock::timestamp(Duration::from_secs(146_097 * 86_400)),
        "2400-01-01T00:00:00.000000000Z"
    );
}

#[test]
fn duration_parser_rejects_invalid_and_overflowing_values() {
    for invalid in [
        "P",
        "PT",
        "P1DT",
        "PT-1S",
        "PT1S2M",
        "PT1.1.1S",
        "PT1.0000000001S",
        "P1Y",
        "PT18446744073709551616S",
        "PT18446744073709551615H",
    ] {
        assert!(clock::parse_duration(invalid).is_none(), "{invalid}");
    }
    assert_eq!(
        clock::parse_duration("P1DT2H3M4.005S"),
        Some(Duration::from_millis(93_784_005))
    );
    assert_eq!(clock::parse_duration("PT0S"), Some(Duration::ZERO));
    assert_eq!(clock::parse_duration("PT1M"), Some(Duration::from_secs(60)));
}
