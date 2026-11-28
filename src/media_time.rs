use std::time::Duration;

pub fn duration_to_ticks(duration: Duration, timescale: u32) -> u64 {
    let timescale_u64 = u64::from(timescale);
    let whole_seconds = duration.as_secs().saturating_mul(timescale_u64);
    let fractional = u64::from(duration.subsec_nanos())
        .saturating_mul(timescale_u64)
        .saturating_add(500_000_000)
        / 1_000_000_000;
    let ticks = whole_seconds.saturating_add(fractional);
    if tracing::enabled!(tracing::Level::TRACE) {
        tracing::trace!(
            duration_us = duration.as_micros(),
            timescale,
            ticks,
            "media_time duration_to_ticks"
        );
    }
    ticks
}

pub fn ticks_to_duration(ticks: u64, timescale: u32) -> Duration {
    if timescale == 0 {
        tracing::debug!(
            ticks,
            "media_time ticks_to_duration received zero timescale"
        );
        return Duration::ZERO;
    }
    let timescale_u64 = u64::from(timescale);
    let seconds = ticks / timescale_u64;
    let remainder = ticks % timescale_u64;
    let nanos = remainder.saturating_mul(1_000_000_000) / timescale_u64;
    let duration = Duration::new(seconds, u32::try_from(nanos).unwrap_or(999_999_999));
    if tracing::enabled!(tracing::Level::TRACE) {
        tracing::trace!(
            ticks,
            timescale,
            duration_us = duration.as_micros(),
            "media_time ticks_to_duration"
        );
    }
    duration
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

    #[test]
    fn media_time_logs_only_when_trace_is_enabled() {
        use std::sync::{Arc, Mutex};
        use tracing_subscriber::fmt::MakeWriter;

        #[derive(Clone, Default)]
        struct SharedBuf(Arc<Mutex<Vec<u8>>>);
        struct BufWriter(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for BufWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for SharedBuf {
            type Writer = BufWriter;
            fn make_writer(&'a self) -> Self::Writer {
                BufWriter(Arc::clone(&self.0))
            }
        }

        let run_at = |level: tracing::Level| -> String {
            let buf = SharedBuf::default();
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(level)
                .with_ansi(false)
                .with_writer(buf.clone())
                .finish();
            tracing::subscriber::with_default(subscriber, || {
                let ticks = duration_to_ticks(Duration::from_secs(2), 90_000);
                assert_eq!(ticks_to_duration(ticks, 90_000), Duration::from_secs(2));
            });
            String::from_utf8(buf.0.lock().unwrap().clone()).unwrap()
        };

        assert!(run_at(tracing::Level::DEBUG).is_empty());
        let trace_logs = run_at(tracing::Level::TRACE);
        assert!(trace_logs.contains("media_time duration_to_ticks"));
        assert!(trace_logs.contains("media_time ticks_to_duration"));
    }
}
