use super::*;

fn framed(headers: Vec<(&str, &str)>) -> Request {
    Request::fake_https(
        "POST",
        "/guest/session",
        headers
            .into_iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect(),
        Vec::new(),
    )
}

#[test]
fn token_exchange_requires_one_bounded_decimal_length_and_json_without_transfer_encoding() {
    let valid = [
        ("Content-Length", "55"),
        ("Content-Type", "application/json"),
    ];
    assert_eq!(exchange_length(&framed(valid.to_vec())).unwrap(), 55);
    for headers in [
        vec![("Content-Type", "application/json")],
        vec![
            ("Content-Length", "55"),
            ("Content-Length", "55"),
            ("Content-Type", "application/json"),
        ],
        vec![
            ("Content-Length", "+55"),
            ("Content-Type", "application/json"),
        ],
        vec![
            ("Content-Length", "0"),
            ("Content-Type", "application/json"),
        ],
        vec![
            ("Content-Length", "257"),
            ("Content-Type", "application/json"),
        ],
        vec![
            ("Content-Length", "18446744073709551615"),
            ("Content-Type", "application/json"),
        ],
        vec![("Content-Length", "55"), ("Content-Type", "text/plain")],
        vec![
            ("Content-Length", "55"),
            ("Content-Type", "application/json"),
            ("Content-Encoding", "gzip"),
        ],
        vec![
            ("Content-Length", "55"),
            ("Content-Type", "application/json"),
            ("Transfer-Encoding", "chunked"),
        ],
    ] {
        assert!(exchange_length(&framed(headers)).is_err());
    }
    assert_eq!(
        exchange_length(&framed(vec![
            ("Content-Length", "256"),
            ("Content-Type", "application/json; charset=utf-8")
        ]))
        .unwrap(),
        256
    );
}

#[test]
fn deterministic_exchange_body_fuzz_rejects_malformed_and_oversized_payloads() {
    use super::super::{limits::Action, registry::Registry, registry::tests::Harness};
    use crate::shutdown::Shutdown;
    use std::sync::Arc;

    let registry = Arc::new(Registry::default());
    let shutdown = Shutdown::new();
    let watchdog = registry.start_deadline_watchdog(shutdown.clone()).unwrap();
    let mut harness = Harness::new();
    let guard = registry
        .admit_request(
            "127.0.0.1".parse().unwrap(),
            Action::Exchange,
            harness.request(),
        )
        .unwrap();

    let fixed_bad: &[&[u8]] = &[
        b"{}",
        b"{\"token\":123}",
        b"{\"token\":null}",
        b"{\"token\":\"valid\",\"extra\":true}",
        b"\xff\xfe\xfd",
        b"{\"token\":\"abc\0def\"}",
    ];
    for payload in fixed_bad {
        let req = Request::fake_https(
            "POST",
            "/guest/session",
            vec![
                ("Content-Length".into(), payload.len().to_string()),
                ("Content-Type".into(), "application/json".into()),
            ],
            payload.to_vec(),
        );
        let status = exchange_body(&req, &guard).unwrap_err().status_code;
        assert!((400..500).contains(&status));
    }

    let mut seed = 0x1320_5e55_1000_0001_u64;
    for _ in 0..500 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let len = usize::try_from(seed % 320).unwrap();
        let mut bytes = Vec::with_capacity(len);
        for _ in 0..len {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            bytes.push((seed >> 56) as u8);
        }
        let req = Request::fake_https(
            "POST",
            "/guest/session",
            vec![
                ("Content-Length".into(), len.to_string()),
                ("Content-Type".into(), "application/json".into()),
            ],
            bytes,
        );
        let status = exchange_body(&req, &guard).unwrap_err().status_code;
        assert!((400..500).contains(&status));
    }
    shutdown.cancel();
    watchdog.join().unwrap();
}
