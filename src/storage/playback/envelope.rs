use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportMediaBounds {
    pub start_ms: i64,
    pub end_ms: i64,
}

#[derive(Clone, Copy)]
struct Timing {
    start: u64,
    duration: u32,
    offset: i32,
    timescale: u32,
}

pub(super) struct ExportEnvelope {
    origin_ms: i64,
    pending: [Option<Timing>; 2],
    bounds: Option<ExportMediaBounds>,
    invalid: bool,
}

impl ExportEnvelope {
    pub(super) const fn new(origin_ms: i64) -> Self {
        Self {
            origin_ms,
            pending: [None; 2],
            bounds: None,
            invalid: false,
        }
    }

    pub(super) fn sample(&mut self, track: u32, timescale: u32, sample: &mp4::Mp4Sample) {
        let Some(index) = track.checked_sub(1).and_then(|id| usize::try_from(id).ok()) else {
            self.invalid = true;
            return;
        };
        if index >= self.pending.len() || timescale == 0 || sample.duration == 0 {
            self.invalid = true;
            return;
        }
        let next = Timing {
            start: sample.start_time,
            duration: sample.duration,
            offset: sample.rendering_offset,
            timescale,
        };
        if let Some(mut previous) = self.pending[index].replace(next) {
            // The MP4 writer extends its pending predecessor to the next decode timestamp.
            let duration = next
                .start
                .checked_sub(previous.start)
                .and_then(|delta| u32::try_from(delta).ok())
                .filter(|delta| *delta > 0);
            let Some(duration) = duration.filter(|_| previous.timescale == timescale) else {
                self.invalid = true;
                return;
            };
            previous.duration = duration;
            self.include(previous);
        }
    }

    pub(super) fn flush(&mut self) {
        for index in 0..self.pending.len() {
            if let Some(timing) = self.pending[index].take() {
                self.include(timing);
            }
        }
    }

    pub(super) const fn bounds(&self) -> Option<ExportMediaBounds> {
        if self.invalid { None } else { self.bounds }
    }

    fn include(&mut self, timing: Timing) {
        let Some(bounds) = timing.bounds(self.origin_ms) else {
            self.invalid = true;
            return;
        };
        self.bounds = Some(self.bounds.map_or(bounds, |previous| ExportMediaBounds {
            start_ms: previous.start_ms.min(bounds.start_ms),
            end_ms: previous.end_ms.max(bounds.end_ms),
        }));
    }
}

impl Timing {
    fn bounds(self, origin_ms: i64) -> Option<ExportMediaBounds> {
        let scale = i128::from(self.timescale);
        if scale == 0 || self.duration == 0 {
            return None;
        }
        let start = i128::from(self.start).checked_add(i128::from(self.offset))?;
        let end = start.checked_add(i128::from(self.duration))?;
        let start_scaled = start.checked_mul(1_000)?;
        let end_scaled = end.checked_mul(1_000)?;
        let start_ms = i64::try_from(start_scaled.div_euclid(scale))
            .ok()?
            .checked_add(origin_ms)?;
        let end_ms = end_scaled
            .div_euclid(scale)
            .checked_add(i128::from(end_scaled.rem_euclid(scale) != 0))?;
        let end_ms = i64::try_from(end_ms).ok()?.checked_add(origin_ms)?;
        (start_ms < end_ms).then_some(ExportMediaBounds { start_ms, end_ms })
    }
}
