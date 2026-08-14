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
}
