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

fn next_u64(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn fuzzed_range_header(seed: &mut u64) -> String {
    const ALPHABET: &[u8] = b"bytes=0123456789-, +/\r\n\t\0xZ";
    match next_u64(seed) % 5 {
        0 => {
            let left = next_u64(seed);
            let right = next_u64(seed);
            format!("bytes={left}-{right}")
        }
        1 => format!("bytes=-{}", next_u64(seed)),
        2 => format!("bytes={}-", next_u64(seed)),
        3 => format!(
            "bytes={}-{},{}-{}",
            next_u64(seed) % 4096,
            next_u64(seed) % 4096,
            next_u64(seed) % 4096,
            next_u64(seed) % 4096
        ),
        _ => {
            let len = usize::try_from(next_u64(seed) % 160).unwrap();
            (0..len)
                .map(|_| {
                    let index = usize::try_from(next_u64(seed) % ALPHABET.len() as u64).unwrap();
                    ALPHABET[index] as char
                })
                .collect()
        }
    }
}

#[test]
fn deterministic_range_fuzz_never_panics_or_exceeds_artifact_bounds() {
    let mut seed = 0x1320_cafe_beef_0001_u64;
    let sizes = [
        0_u64,
        1,
        2,
        65_535,
        65_536,
        65_537,
        crate::shares::MAX_MEDIA_BYTES,
        u64::MAX,
    ];
    let mut accepted = 0_usize;
    for case in 0..2_500 {
        let header = fuzzed_range_header(&mut seed);
        let bytes = sizes[case % sizes.len()];
        let forbidden = header.len() > 128
            || header.contains(',')
            || header.bytes().any(|byte| byte.is_ascii_control());
        if let Ok(range) = parse(Some(&header), bytes) {
            accepted += 1;
            assert!(!forbidden, "accepted forbidden header {header:?}");
            assert!(bytes > 0);
            assert!(range.partial);
            assert!(range.start < range.end, "{header:?} for {bytes}");
            assert!(range.end <= bytes, "{header:?} for {bytes}");
        }
    }
    assert!(
        accepted >= 200,
        "range fuzz accepted too few valid headers: {accepted}"
    );
    assert!(
        accepted < 2_500,
        "range fuzz rejected no malformed headers: {accepted}"
    );
}
