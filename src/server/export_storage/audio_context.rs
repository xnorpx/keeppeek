//! Pins the neighboring source audio needed at corrected export boundaries.

use super::*;
use crate::storage::catalog::readers::LeaseSet;
use anyhow::Context;

const EDGE_PERIODS_MAX: u32 = 32;
const SOURCE_PERIODS_MAX: usize = 512;
const EDGE_BYTES_MAX: u64 = 64 * 1024 * 1024;

pub(super) struct Sources {
    pub fragments: Vec<CatalogMediaFragment>,
    _lease: Option<LeaseSet>,
}

pub(super) fn load(
    attempt: &Attempt,
    video: &[CatalogMediaFragment],
    requested_end_ms: i64,
    catalog: &RecordingCatalogHandle,
) -> anyhow::Result<Sources> {
    if attempt.audio_offset_ms == 0 {
        return Ok(Sources {
            fragments: Vec::new(),
            _lease: None,
        });
    }
    let stream = attempt
        .recording_stream_id
        .as_deref()
        .context("corrected export is missing its captured recording stream")?;
    let (start, end) = edge_window(video, requested_end_ms, attempt.audio_offset_ms)?;
    anyhow::ensure!(!attempt.cancel.load(Ordering::Acquire), "export cancelled");
    let (neighbors, lease) =
        catalog.leased_media_fragments_in_range_bounded(stream, start, end, EDGE_PERIODS_MAX)?;
    validate_edge_bytes(&neighbors)?;
    let mut sources = BTreeMap::new();
    for fragment in video.iter().chain(&neighbors) {
        let key = (fragment.recording_id.clone(), fragment.sequence);
        if let Some(previous) = sources.insert(key, fragment.clone()) {
            anyhow::ensure!(previous == *fragment, "export source metadata changed");
        }
        anyhow::ensure!(
            sources.len() <= SOURCE_PERIODS_MAX,
            "corrected export exceeds 512 source periods"
        );
    }
    anyhow::ensure!(!attempt.cancel.load(Ordering::Acquire), "export cancelled");
    Ok(Sources {
        fragments: sources.into_values().collect(),
        _lease: Some(lease),
    })
}

fn edge_window(
    video: &[CatalogMediaFragment],
    requested_end_ms: i64,
    offset_ms: i32,
) -> anyhow::Result<(i64, i64)> {
    let start = video
        .first()
        .context("export has no video fragments")?
        .start_ms;
    let mut end = start;
    for fragment in video {
        let fragment_end = fragment
            .start_ms
            .checked_add(i64::try_from(fragment.duration_ms)?)
            .context("export fragment end exceeds clock")?;
        end = end.max(fragment_end);
    }
    let end = end.min(requested_end_ms);
    anyhow::ensure!(start < end, "corrected export window must be nonempty");
    match offset_ms.cmp(&0) {
        std::cmp::Ordering::Greater => Ok((
            start
                .checked_sub(i64::from(offset_ms))
                .context("export audio lookbehind exceeds clock")?,
            start,
        )),
        std::cmp::Ordering::Less => Ok((
            end,
            end.checked_sub(i64::from(offset_ms))
                .context("export audio lookahead exceeds clock")?,
        )),
        std::cmp::Ordering::Equal => anyhow::bail!("zero correction needs no audio neighbors"),
    }
}

fn validate_edge_bytes(neighbors: &[CatalogMediaFragment]) -> anyhow::Result<()> {
    let mut initializations = HashSet::new();
    let mut bytes = 0_u64;
    for fragment in neighbors {
        let init = (
            fragment.recording_id.as_str(),
            fragment.init_offset,
            fragment.init_len,
        );
        let init_bytes = if initializations.insert(init) {
            fragment.init_len
        } else {
            0
        };
        anyhow::ensure!(
            fragment.init_len > 0 && fragment.byte_len > 0,
            "export audio neighbor requires complete indexed media"
        );
        bytes = bytes
            .checked_add(init_bytes)
            .and_then(|total| total.checked_add(fragment.byte_len))
            .filter(|total| *total <= EDGE_BYTES_MAX)
            .context("export audio neighbors exceed 64 MiB")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fragment(start_ms: i64, duration_ms: u64) -> CatalogMediaFragment {
        CatalogMediaFragment {
            recording_id: "test".into(),
            recording_started_at_ms: 0,
            path: String::new(),
            init_offset: 0,
            init_len: 32,
            sequence: 0,
            start_ms,
            duration_ms,
            byte_offset: 32,
            byte_len: 64,
            archive: None,
        }
    }

    #[test]
    fn edge_follows_the_aligned_video_window_and_requested_end() {
        let video = [fragment(10_000, 1_000), fragment(11_000, 1_000)];
        assert_eq!(edge_window(&video, 11_500, 200).unwrap(), (9_800, 10_000));
        assert_eq!(edge_window(&video, 11_500, -200).unwrap(), (11_500, 11_700));
        assert_eq!(edge_window(&video, 20_000, -200).unwrap(), (12_000, 12_200));
        assert!(edge_window(&video, 10_000, 200).is_err());
        assert!(edge_window(&video, 11_500, 0).is_err());
        assert!(edge_window(&[fragment(i64::MIN, 1)], i64::MIN + 1, 200).is_err());
        assert!(edge_window(&[fragment(i64::MAX - 1, 1)], i64::MAX, -200).is_err());
    }

    #[test]
    fn edge_bytes_deduplicate_initializations_and_reject_invalid_extents() {
        let first = fragment(0, 1_000);
        let mut second = fragment(1_000, 1_000);
        second.byte_len = EDGE_BYTES_MAX - first.byte_len - first.init_len;
        validate_edge_bytes(&[first.clone(), second.clone()]).unwrap();
        second.byte_len += 1;
        assert!(validate_edge_bytes(&[first.clone(), second]).is_err());
        let mut missing = first;
        missing.init_len = 0;
        assert!(validate_edge_bytes(&[missing]).is_err());
    }
}
