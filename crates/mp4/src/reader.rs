use crate::{meta::MetaBox, mp4box::tfhd::TfhdBox, *};
use std::{
    collections::HashMap,
    io::{Read, Seek},
    time::Duration,
};

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
            let header = BoxHeader::read(&mut reader)?;
            let BoxHeader { name, size: s } = header;
            if s > size {
                return Err(Error::InvalidData(
                    "file contains a box with a larger size than it",
                ));
            }

            // Break if size zero BoxHeader, which can result in dead-loop.
            if s == 0 {
                break;
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
        if self.moov.mvhd.duration > 0 {
            return Duration::from_millis(
                self.moov.mvhd.duration * 1000 / self.moov.mvhd.timescale as u64,
            );
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
