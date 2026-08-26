use std::time::Duration;

pub fn duration_to_ticks(duration: Duration, timescale: u32) -> u64 {
    let timescale = u64::from(timescale);
    let whole_seconds = duration.as_secs().saturating_mul(timescale);
    let fractional = u64::from(duration.subsec_nanos())
        .saturating_mul(timescale)
        .saturating_add(500_000_000)
        / 1_000_000_000;
    whole_seconds.saturating_add(fractional)
}

pub fn ticks_to_duration(ticks: u64, timescale: u32) -> Duration {
    if timescale == 0 {
        return Duration::ZERO;
    }
    let timescale = u64::from(timescale);
    let seconds = ticks / timescale;
    let remainder = ticks % timescale;
    let nanos = remainder.saturating_mul(1_000_000_000) / timescale;
    Duration::new(seconds, u32::try_from(nanos).unwrap_or(999_999_999))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_fractional_duration_to_nearest_tick() {
        assert_eq!(
            duration_to_ticks(Duration::new(1, 33_333_333), 90_000),
            93_000
        );
    }

    #[test]
    fn converts_ticks_to_duration_without_losing_whole_seconds() {
        assert_eq!(
            ticks_to_duration(93_000, 90_000),
            Duration::new(1, 33_333_333)
        );
        assert_eq!(ticks_to_duration(1, 0), Duration::ZERO);
    }
}
