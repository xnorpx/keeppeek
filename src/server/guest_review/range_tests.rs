use super::*;

#[test]
fn absent_range_selects_exact_full_artifact_without_partial_status() {
    assert_eq!(
        parse(None, 1_024).unwrap(),
        ByteRange {
            start: 0,
            end: 1_024,
            partial: false
        }
    );
    assert!(parse(None, 0).is_err());
}

#[test]
fn explicit_open_and_suffix_ranges_remain_within_immutable_artifact() {
    for (header, start, end) in [
        ("bytes=0-0", 0, 1),
        ("bytes=100-199", 100, 200),
        ("bytes=100-", 100, 1_024),
        ("bytes=-10", 1_014, 1_024),
        ("bytes=-2048", 0, 1_024),
        ("bytes=100-2048", 100, 1_024),
        ("bytes=0001-0002", 1, 3),
    ] {
        let range = parse(Some(header), 1_024).unwrap();
        assert_eq!(
            range,
            ByteRange {
                start,
                end,
                partial: true
            },
            "{header}"
        );
        assert!(range.start < range.end);
        assert!(range.end <= 1_024);
    }
}

#[test]
fn malformed_multiple_and_overflowing_ranges_never_select_bytes() {
    for header in [
        "",
        "bytes=",
        "items=0-1",
        "bytes=1",
        "bytes=-",
        "bytes=-0",
        "bytes=2-1",
        "bytes=1024-",
        "bytes=2048-4096",
        "bytes=0-1,3-4",
        "bytes=0-1,",
        "bytes=+1-2",
        "bytes= 1-2",
        "bytes=1- 2",
        "bytes=1-2 ",
        "bytes=0-1\r\nX: attack",
        "bytes=0-1-2",
        "bytes=18446744073709551616-",
        "bytes=0-18446744073709551616",
        "bytes=0-18446744073709551615",
        "bytes=-18446744073709551616",
    ] {
        assert!(parse(Some(header), 1_024).is_err(), "{header:?}");
    }
    assert!(parse(Some(&format!("bytes={}-1", "0".repeat(128))), 1_024).is_err());
}

#[test]
fn small_range_input_corpus_never_exceeds_declared_artifact_bounds() {
    let pieces = [
        "",
        "0",
        "1",
        "1023",
        "1024",
        "18446744073709551615",
        ",",
        "-",
        "a",
    ];
    for left in pieces {
        for right in pieces {
            let header = format!("bytes={left}-{right}");
            if let Ok(range) = parse(Some(&header), 1_024) {
                assert!(range.partial);
                assert!(range.start < range.end, "{header}");
                assert!(range.end <= 1_024, "{header}");
            }
        }
    }
}
