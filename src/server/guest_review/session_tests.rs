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
