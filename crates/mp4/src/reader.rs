use crate::{meta::MetaBox, mp4box::tfhd::TfhdBox, *};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    time::Duration,
};

fn read_top_level_box_header<R: Read + Seek>(reader: &mut R, end: u64) -> Result<BoxHeader> {
    let start = reader.stream_position()?;
    let remaining = end
        .checked_sub(start)
        .ok_or(Error::InvalidData("reader is past the logical file end"))?;
    if remaining < HEADER_SIZE {
        return Err(Error::InvalidData("top-level box header is truncated"));
    }
    let mut encoded_size = [0; 4];
    reader.read_exact(&mut encoded_size)?;
    reader.seek(SeekFrom::Start(start))?;
    if u32::from_be_bytes(encoded_size) == 1 && remaining < HEADER_SIZE + 8 {
        return Err(Error::InvalidData(
            "top-level extended box header is truncated",
        ));
    }
    BoxHeader::read(reader)
}

#[derive(Debug)]
pub struct Mp4Reader<R> {
    reader: R,
    pub ftyp: FtypBox,
    pub moov: MoovBox,
    pub moofs: Vec<MoofBox>,
    pub emsgs: Vec<EmsgBox>,

    tracks: HashMap<u32, Mp4Track>,
    size: u64,
}

impl<R: Read + Seek> Mp4Reader<R> {
    pub fn read_header(mut reader: R, size: u64) -> Result<Self> {
        let start = reader.stream_position()?;

        let mut ftyp = None;
        let mut moov = None;
        let mut moofs = Vec::new();
        let mut moof_offsets = Vec::new();
        let mut emsgs = Vec::new();

        let mut current = start;
        while current < size {
            // Get box header.
            let header = read_top_level_box_header(&mut reader, size)?;
            let BoxHeader { name, size: s } = header;
            // Break if size zero BoxHeader, which can result in dead-loop.
            if s == 0 {
                break;
            }
            let box_end = box_start(&mut reader)?
                .checked_add(s)
                .ok_or(Error::InvalidData("top-level box size overflow"))?;
            if box_end > size {
                return Err(Error::InvalidData(
                    "file contains a box with a larger size than it",
                ));
            }

            // Match and parse the atom boxes.
            match name {
                BoxType::FtypBox => {
                    ftyp = Some(FtypBox::read_box(&mut reader, s)?);
                }
                BoxType::FreeBox => {
                    skip_box(&mut reader, s)?;
                }
                BoxType::MdatBox => {
                    skip_box(&mut reader, s)?;
                }
                BoxType::MoovBox => {
                    moov = Some(MoovBox::read_box(&mut reader, s)?);
                }
                BoxType::MoofBox => {
                    let moof = MoofBox::read_box(&mut reader, s)?;
                    moofs.push(moof);
                    moof_offsets.push(current);
                }
                BoxType::EmsgBox => {
                    let emsg = EmsgBox::read_box(&mut reader, s)?;
                    emsgs.push(emsg);
                }
                _ => {
                    // XXX warn!()
                    skip_box(&mut reader, s)?;
                }
            }
            current = reader.stream_position()?;
        }

        if ftyp.is_none() {
            return Err(Error::BoxNotFound(BoxType::FtypBox));
        }
        if moov.is_none() {
            return Err(Error::BoxNotFound(BoxType::MoovBox));
        }

        let size = current - start;
        let mut tracks = if let Some(ref moov) = moov {
            if moov.traks.iter().any(|trak| trak.tkhd.track_id == 0) {
                return Err(Error::InvalidData("illegal track id 0"));
            }
            moov.traks
                .iter()
                .map(|trak| (trak.tkhd.track_id, Mp4Track::from(trak)))
                .collect()
        } else {
            HashMap::new()
        };

        // Update tracks if any fragmented (moof) boxes are found.
        if !moofs.is_empty() {
            for (moof, moof_offset) in moofs.iter().zip(moof_offsets) {
                for traf in moof.trafs.iter() {
                    let track_id = traf.tfhd.track_id;
                    if let Some(track) = tracks.get_mut(&track_id) {
                        let sample_count = traf.trun.as_ref().map(|t| t.sample_count).unwrap_or(0);
                        if sample_count > 0 {
                            let last_count = track.traf_sample_counts.last().copied().unwrap_or(0);
                            let cumulative =
                                last_count
                                    .checked_add(sample_count)
                                    .ok_or(Error::InvalidData(
                                        "cumulative trun sample_count overflow",
                                    ))?;
                            track.traf_sample_counts.push(cumulative);
                        }
                        let base_offset = traf.tfhd.base_data_offset.unwrap_or({
                            if traf.tfhd.flags & TfhdBox::FLAG_DEFAULT_BASE_IS_MOOF != 0 {
                                moof_offset
                            } else {
                                0
                            }
                        });
                        track.traf_base_offsets.push(base_offset);
                        track.trafs.push(traf.clone());
                    } else {
                        return Err(Error::TrakNotFound(track_id));
                    }
                }
            }

            if let Some(ref moov) = moov
                && let Some(mvex) = &moov.mvex
            {
                for track in tracks.values_mut() {
                    if let Some(trex) = mvex.trexs.iter().find(|t| t.track_id == track.track_id()) {
                        track.default_sample_duration = trex.default_sample_duration;
                        track.default_sample_size = trex.default_sample_size;
                        track.default_sample_flags = trex.default_sample_flags;
                        track.default_sample_description_index =
                            trex.default_sample_description_index;
                    }
                }
            }

            for track in tracks.values_mut() {
                track.precompute_fragmented_caches();
            }
        }

        Ok(Self {
            reader,
            ftyp: ftyp.unwrap(),
            moov: moov.unwrap(),
            moofs,
            emsgs,
            size,
            tracks,
        })
    }

    pub const fn size(&self) -> u64 {
        self.size
    }

    pub const fn major_brand(&self) -> &FourCC {
        &self.ftyp.major_brand
    }

    pub const fn minor_version(&self) -> u32 {
        self.ftyp.minor_version
    }

    pub fn compatible_brands(&self) -> &[FourCC] {
        &self.ftyp.compatible_brands
    }

    pub fn duration(&self) -> Duration {
        let timescale = u64::from(self.moov.mvhd.timescale);
        if self.moov.mvhd.duration > 0 && timescale > 0 {
            let seconds = self.moov.mvhd.duration / timescale;
            let remainder = self.moov.mvhd.duration % timescale;
            let milliseconds = remainder * 1_000 / timescale;
            return Duration::from_secs(seconds) + Duration::from_millis(milliseconds);
        }
        self.tracks
            .values()
            .map(Mp4Track::duration)
            .max()
            .unwrap_or(Duration::ZERO)
    }

    pub const fn timescale(&self) -> u32 {
        self.moov.mvhd.timescale
    }

    pub const fn is_fragmented(&self) -> bool {
        !self.moofs.is_empty()
    }

    pub const fn tracks(&self) -> &HashMap<u32, Mp4Track> {
        &self.tracks
    }

    pub fn sample_count(&self, track_id: u32) -> Result<u32> {
        self.tracks
            .get(&track_id)
            .map_or(Err(Error::TrakNotFound(track_id)), |track| {
                Ok(track.sample_count())
            })
    }

    pub fn sample_location(
        &self,
        track_id: u32,
        sample_id: u32,
    ) -> Result<Option<Mp4SampleLocation>> {
        self.tracks
            .get(&track_id)
            .map_or(Err(Error::TrakNotFound(track_id)), |track| {
                track.sample_location(sample_id)
            })
    }

    /// Returns the first sample location for each fragment containing the selected track.
    pub fn fragment_first_sample_locations(
        &self,
        track_id: u32,
    ) -> Result<Vec<Mp4FragmentSampleLocation>> {
        let track = self
            .tracks
            .get(&track_id)
            .ok_or(Error::TrakNotFound(track_id))?;
        let mut sample_id = 1u32;
        let mut locations = Vec::new();
        for moof in &self.moofs {
            let Some(traf) = moof
                .trafs
                .iter()
                .find(|traf| traf.tfhd.track_id == track_id)
            else {
                continue;
            };
            let sample_count = traf.trun.as_ref().map_or(0, |trun| trun.sample_count);
            if sample_count == 0 {
                continue;
            }
            let location = track
                .sample_location(sample_id)?
                .ok_or(Error::EntryInTrunNotFound(
                    track_id,
                    BoxType::TrunBox,
                    sample_id,
                ))?;
            locations.push(Mp4FragmentSampleLocation {
                sequence_number: moof.mfhd.sequence_number,
                sample_id,
                sample_description_index: track.sample_description_index(sample_id)?,
                location,
                is_sync: track.is_sync_sample(sample_id),
            });
            sample_id = sample_id
                .checked_add(sample_count)
                .ok_or(Error::InvalidData("fragment sample ID overflow"))?;
        }
        Ok(locations)
    }

    pub fn read_sample(&mut self, track_id: u32, sample_id: u32) -> Result<Option<Mp4Sample>> {
        if let Some(track) = self.tracks.get(&track_id) {
            track.read_sample(&mut self.reader, sample_id)
        } else {
            Err(Error::TrakNotFound(track_id))
        }
    }
}

impl<R> Mp4Reader<R> {
    pub fn metadata(&self) -> impl Metadata<'_> {
        self.moov.udta.as_ref().and_then(|udta| {
            udta.meta.as_ref().and_then(|meta| match meta {
                MetaBox::Mdir { ilst } => ilst.as_ref(),
                _ => None,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4box::mvhd::MvhdBox;
    use std::io::Cursor;

    fn reader_with_movie_duration(duration: u64, timescale: u32) -> Mp4Reader<Cursor<Vec<u8>>> {
        let mut bytes = Vec::new();
        FtypBox::default().write_box(&mut bytes).unwrap();
        MoovBox {
            mvhd: MvhdBox {
                version: u8::from(duration > u64::from(u32::MAX)),
                duration,
                timescale,
                ..MvhdBox::default()
            },
            ..MoovBox::default()
        }
        .write_box(&mut bytes)
        .unwrap();
        let size = bytes.len() as u64;
        Mp4Reader::read_header(Cursor::new(bytes), size).unwrap()
    }

    #[test]
    fn rejects_box_that_extends_past_file_end() {
        let mut bytes = Vec::new();
        FtypBox::default().write_box(&mut bytes).unwrap();
        MoovBox::default().write_box(&mut bytes).unwrap();
        let file_size = bytes.len() as u64 + HEADER_SIZE;
        BoxHeader::new(BoxType::FreeBox, file_size)
            .write(&mut bytes)
            .unwrap();

        let result = Mp4Reader::read_header(Cursor::new(bytes), file_size);

        assert!(matches!(result, Err(Error::InvalidData(_))));
    }

    #[test]
    fn rejects_partial_top_level_header_at_logical_end() {
        let mut bytes = Vec::new();
        FtypBox::default().write_box(&mut bytes).unwrap();
        MoovBox::default().write_box(&mut bytes).unwrap();
        let logical_size = bytes.len() as u64 + 1;
        bytes.extend_from_slice(&[0; 8]);

        let result = Mp4Reader::read_header(Cursor::new(bytes), logical_size);

        assert!(matches!(result, Err(Error::InvalidData(_))));
    }

    #[test]
    fn zero_movie_timescale_returns_zero_duration() {
        let reader = reader_with_movie_duration(1, 0);

        assert_eq!(reader.duration(), Duration::ZERO);
    }

    #[test]
    fn maximal_movie_duration_does_not_overflow() {
        let reader = reader_with_movie_duration(u64::MAX, 1);

        assert_eq!(reader.duration(), Duration::from_secs(u64::MAX));
    }
}
