use anyhow::{Context, bail};

/// Positive offsets delay audio. Half-tick ties round away from zero.
/// The caller retains the sample's composition offset, duration and payload.
///
/// # Errors
/// Rejects zero timescales, overflowing arithmetic and positive starts beyond the track clock.
pub(super) fn audio_start(
    normalized_start_ticks: i128,
    rendering_offset: i32,
    offset_ms: i32,
    timescale: u32,
) -> anyhow::Result<Option<u64>> {
    if timescale == 0 {
        bail!("audio timing requires a nonzero timescale");
    }
    let scaled_offset = i128::from(offset_ms)
        .checked_mul(i128::from(timescale))
        .context("audio offset multiplication overflowed")?;
    let rounded_offset = scaled_offset
        .checked_add(if scaled_offset < 0 { -500 } else { 500 })
        .context("audio offset rounding overflowed")?
        / 1_000;
    let decode_start = normalized_start_ticks
        .checked_add(rounded_offset)
        .context("corrected audio decode start overflowed")?;
    if decode_start < 0 {
        // ponytail: Clip whole encoded samples; partial trimming requires a supported re-encoder.
        return Ok(None);
    }
    let presentation_start = decode_start
        .checked_add(i128::from(rendering_offset))
        .context("corrected audio presentation start overflowed")?;
    if presentation_start < 0 {
        return Ok(None);
    }
    let decode_start =
        u64::try_from(decode_start).context("corrected audio decode start exceeds track clock")?;
    u64::try_from(presentation_start)
        .context("corrected audio presentation start exceeds track clock")?;
    Ok(Some(decode_start))
}

#[cfg(test)]
mod tests {
    use super::audio_start;

    #[test]
    fn signed_offsets_follow_the_selected_track_timescale() {
        for (start, offset_ms, timescale, expected) in [
            (10_000, 125, 8_000, 11_000),
            (10_000, -125, 8_000, 9_000),
            (96_000, 250, 48_000, 108_000),
            (96_000, -250, 48_000, 84_000),
            (90_000, 1, 90_000, 90_090),
            (90_000, -1, 90_000, 89_910),
            (44_100, 1, 44_100, 44_144),
            (44_100, -1, 44_100, 44_056),
        ] {
            assert_eq!(
                audio_start(start, 0, offset_ms, timescale).unwrap(),
                Some(expected)
            );
        }
    }

    #[test]
    fn zero_offset_preserves_valid_decode_times_and_composition() {
        for (start, composition) in [(0, 0), (100, -100), (100, 100)] {
            assert_eq!(
                audio_start(start, composition, 0, 48_000).unwrap(),
                Some(u64::try_from(start).unwrap())
            );
        }
        assert_eq!(
            audio_start(i128::from(u64::MAX), -1, 0, 48_000).unwrap(),
            Some(u64::MAX)
        );
    }

    #[test]
    fn fractional_tick_rounding_is_symmetric_with_ties_away_from_zero() {
        for (offset_ms, timescale, expected) in [
            (1, 499, 1_000),
            (-1, 499, 1_000),
            (1, 500, 1_001),
            (-1, 500, 999),
            (1, 1_500, 1_002),
            (-1, 1_500, 998),
            (5, 44_100, 1_221),
            (-5, 44_100, 779),
        ] {
            assert_eq!(
                audio_start(1_000, 0, offset_ms, timescale).unwrap(),
                Some(expected)
            );
        }
    }

    #[test]
    fn negative_decode_or_presentation_start_clips_the_whole_sample() {
        for (start, composition, offset_ms) in [
            (-1, 0, 0),
            (5, 0, -6),
            (5, -10, 0),
            (5, 10, -6),
            (100, i32::MIN, 0),
        ] {
            assert_eq!(
                audio_start(start, composition, offset_ms, 1_000).unwrap(),
                None
            );
        }
        assert_eq!(audio_start(-5, 0, 5, 1_000).unwrap(), Some(0));
        assert_eq!(audio_start(5, -10, 5, 1_000).unwrap(), Some(10));
        assert_eq!(audio_start(0, -1, 1, 1_000).unwrap(), Some(1));
    }

    #[test]
    fn zero_timescale_is_rejected_before_zero_or_negative_mapping() {
        for (start, offset_ms) in [(0, 0), (-1, 0), (1, 1)] {
            assert!(audio_start(start, 0, offset_ms, 0).is_err());
        }
    }

    #[test]
    fn positive_overflow_is_rejected_for_decode_and_presentation() {
        assert!(audio_start(i128::from(u64::MAX), 0, 1, 1_000).is_err());
        assert!(audio_start(i128::from(u64::MAX), 1, 0, 1_000).is_err());
        assert!(audio_start(i128::from(u64::MAX) + 1, 0, 0, 1_000).is_err());
        assert!(audio_start(i128::MAX, 0, i32::MAX, u32::MAX).is_err());
        assert!(audio_start(i128::MAX, i32::MAX, 0, 1_000).is_err());
        assert!(audio_start(i128::MIN, 0, i32::MIN, u32::MAX).is_err());
    }

    #[test]
    fn mapping_has_no_configuration_activation_limit() {
        assert_eq!(audio_start(0, 0, 6_000, 1_000).unwrap(), Some(6_000));
        assert_eq!(
            audio_start(0, 0, i32::MAX, 1_000).unwrap(),
            Some(2_147_483_647)
        );
        assert_eq!(
            audio_start(2_147_483_648, 0, i32::MIN, 1_000).unwrap(),
            Some(0)
        );
    }

    #[test]
    fn mapping_returns_a_new_decode_start_without_changing_the_source_sample() {
        let sample = mp4::Mp4Sample {
            start_time: 90_000,
            duration: 3_600,
            rendering_offset: -90,
            is_sync: true,
            bytes: bytes::Bytes::from_static(b"encoded-aac"),
        };
        assert_eq!(
            audio_start(
                i128::from(sample.start_time),
                sample.rendering_offset,
                40,
                90_000
            )
            .unwrap(),
            Some(93_600)
        );
        assert_eq!(sample.start_time, 90_000);
        assert_eq!(sample.rendering_offset, -90);
        assert_eq!(sample.duration, 3_600);
        assert_eq!(sample.bytes.as_ref(), b"encoded-aac");
    }
}
