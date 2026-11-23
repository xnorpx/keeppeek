//! Exact output size before reading the next sample's bytes.

use super::*;

pub(super) const fn tfhd_flags(multiple_descriptions: bool) -> u32 {
    let flags = TfhdBox::FLAG_DEFAULT_BASE_IS_MOOF
        | TfhdBox::FLAG_DEFAULT_SAMPLE_DURATION
        | TfhdBox::FLAG_DEFAULT_SAMPLE_SIZE
        | TfhdBox::FLAG_DEFAULT_SAMPLE_FLAGS;
    if multiple_descriptions {
        flags | TfhdBox::FLAG_SAMPLE_DESCRIPTION_INDEX
    } else {
        flags
    }
}

pub(super) const fn trun_flags(has_offsets: bool) -> u32 {
    let flags = TrunBox::FLAG_DATA_OFFSET
        | TrunBox::FLAG_SAMPLE_DURATION
        | TrunBox::FLAG_SAMPLE_SIZE
        | TrunBox::FLAG_SAMPLE_FLAGS;
    if has_offsets {
        flags | TrunBox::FLAG_SAMPLE_CTS
    } else {
        flags
    }
}

impl<W: Write + Seek> FragmentedMp4Writer<W> {
    /// File end after the pending fragment and the next sample's headers, excluding that sample's payload.
    /// Call after flushing any preceding fragment and registering the next sample description.
    pub fn projected_end_before_payload(
        &mut self,
        track_id: u32,
        rendering_offset: i64,
    ) -> Result<u64> {
        self.ensure_not_poisoned()?;
        if !self.tracks.iter().any(|track| track.track_id == track_id) {
            return Err(Error::TrakNotFound(track_id));
        }
        let overhead = MoofBox::default().box_size() + HEADER_SIZE;
        let end = self
            .writer
            .stream_position()?
            .checked_add(overhead)
            .ok_or(Error::InvalidData("fragmented MP4 output size overflow"))?;
        self.tracks.iter().try_fold(end, |end, track| {
            let selected = track.track_id == track_id;
            let count = u32::try_from(track.samples.len())
                .ok()
                .and_then(|count| count.checked_add(u32::from(selected)))
                .ok_or(Error::InvalidData(
                    "fragmented MP4 fragment has too many samples",
                ))?;
            if count == 0 {
                return Ok(end);
            }
            let traf = TrafBox {
                tfhd: TfhdBox {
                    flags: tfhd_flags(track.sample_description_count > 1),
                    ..TfhdBox::default()
                },
                tfdt: Some(TfdtBox {
                    version: 1,
                    ..TfdtBox::default()
                }),
                trun: Some(TrunBox {
                    flags: trun_flags(
                        track.has_composition_offsets || (selected && rendering_offset != 0),
                    ),
                    sample_count: count,
                    ..TrunBox::default()
                }),
            };
            end.checked_add(track.pending_payload)
                .and_then(|n| n.checked_add(traf.box_size()))
                .ok_or(Error::InvalidData("fragmented MP4 output size overflow"))
        })
    }
}
