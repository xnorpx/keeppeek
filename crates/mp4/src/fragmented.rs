//! Fragmented MP4 output with publishable initialization and media byte ranges.
//!
//! Tracks are fixed when the writer starts. A track may add codec sample descriptions between
//! fragments, and each fragment selects one description for all of its samples. Samples are
//! buffered until the caller flushes a fragment, allowing the caller to choose keyframe-aligned
//! boundaries. Each returned range has been flushed to the underlying writer and can be indexed
//! or served independently alongside the initialization range.

use crate::{
    mp4box::{
        mfhd::MfhdBox, mvex::MvexBox, tfdt::TfdtBox, tfhd::TfhdBox, traf::TrafBox, trex::TrexBox,
        trun::TrunBox,
    },
    track::Mp4TrackWriter,
    *,
};
use std::io::{Cursor, Seek, SeekFrom, Write};

const SYNC_SAMPLE_FLAGS: u32 = 0x0200_0000;
const NON_SYNC_SAMPLE_FLAGS: u32 = 0x0101_0000;
const INITIALIZATION_METADATA_CAPACITY: u64 = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// A contiguous byte range in the generated MP4 file.
pub struct Mp4ByteRange {
    /// Absolute byte offset from the beginning of the writer.
    pub offset: u64,
    /// Number of bytes in the range.
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Metadata for one completed `moof`/`mdat` fragment.
pub struct Mp4FragmentInfo {
    /// Monotonically increasing `mfhd` sequence number.
    pub sequence_number: u32,
    /// Exact byte range containing the complete fragment.
    pub range: Mp4ByteRange,
    /// Exact file location of the first video sync sample, when the fragment contains video.
    pub video_keyframe: Option<Mp4SampleLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FragmentedTrackConfig {
    pub track_type: TrackType,
    pub timescale: u32,
    pub language: String,
    pub sample_descriptions: Vec<MediaConfig>,
}

#[derive(Debug)]
struct FragmentTrack {
    track_id: u32,
    track_type: TrackType,
    timescale: u32,
    language: String,
    sample_descriptions: Vec<MediaConfig>,
    sample_description_count: u32,
    pending_sample_description_index: Option<u32>,
    samples: Vec<Mp4Sample>,
}

#[derive(Debug)]
/// Writes an initialization segment followed by explicitly flushed MP4 fragments.
///
/// Video fragments must begin with a sync sample. The track set cannot change after
/// [`Self::write_start`], but a track may register a new sample description between fragments.
pub struct FragmentedMp4Writer<W> {
    writer: W,
    tracks: Vec<FragmentTrack>,
    initialization: Mp4ByteRange,
    movie_timescale: u32,
    moov_offset: u64,
    sequence_number: u32,
    poisoned: bool,
}

impl<W> FragmentedMp4Writer<W> {
    /// Returns the flushed `ftyp`/`moov` initialization range.
    pub const fn initialization(&self) -> Mp4ByteRange {
        self.initialization
    }

    /// Consumes the fragmented writer and returns its underlying writer.
    pub fn into_writer(self) -> W {
        self.writer
    }

    /// Returns whether at least one track has a sample waiting for the next fragment flush.
    pub fn has_pending_samples(&self) -> bool {
        self.tracks.iter().any(|track| !track.samples.is_empty())
    }
}

impl<W: Write + Seek> FragmentedMp4Writer<W> {
    /// Writes and flushes the initialization range for a fixed set of tracks.
    pub fn write_start(
        writer: W,
        config: &Mp4Config,
        track_configs: &[TrackConfig],
    ) -> Result<Self> {
        let track_configs = track_configs
            .iter()
            .map(|track| FragmentedTrackConfig {
                track_type: track.track_type,
                timescale: track.timescale,
                language: track.language.clone(),
                sample_descriptions: vec![track.media_conf.clone()],
            })
            .collect::<Vec<_>>();
        Self::write_start_with_sample_descriptions(writer, config, &track_configs)
    }

    pub fn write_start_with_sample_descriptions(
        mut writer: W,
        config: &Mp4Config,
        track_configs: &[FragmentedTrackConfig],
    ) -> Result<Self> {
        if track_configs.is_empty() {
            return Err(Error::InvalidData(
                "fragmented MP4 requires at least one track",
            ));
        }
        if config.timescale == 0 || track_configs.iter().any(|track| track.timescale == 0) {
            return Err(Error::InvalidData("MP4 timescales must be non-zero"));
        }
        if track_configs
            .iter()
            .any(|track| track.sample_descriptions.is_empty())
        {
            return Err(Error::InvalidData(
                "fragmented MP4 tracks require a sample description",
            ));
        }

        let init_start = writer.stream_position()?;
        FtypBox {
            major_brand: config.major_brand,
            minor_version: config.minor_version,
            compatible_brands: config.compatible_brands.clone(),
        }
        .write_box(&mut writer)?;
        let moov_offset = writer.stream_position()?;

        let mut tracks = Vec::with_capacity(track_configs.len());
        for (index, track_config) in track_configs.iter().enumerate() {
            let track_id = u32::try_from(index + 1)
                .map_err(|_| Error::InvalidData("too many fragmented MP4 tracks"))?;
            let sample_description_count = track_config
                .sample_descriptions
                .len()
                .try_into()
                .map_err(|_| Error::InvalidData("too many sample descriptions"))?;
            tracks.push(FragmentTrack {
                track_id,
                track_type: track_config.track_type,
                timescale: track_config.timescale,
                language: track_config.language.clone(),
                sample_descriptions: track_config.sample_descriptions.clone(),
                sample_description_count,
                pending_sample_description_index: None,
                samples: Vec::new(),
            });
        }
        write_initialization_metadata(&mut writer, moov_offset, config.timescale, &tracks)?;
        let init_end = moov_offset + INITIALIZATION_METADATA_CAPACITY;
        Ok(Self {
            writer,
            tracks,
            initialization: Mp4ByteRange {
                offset: init_start,
                size: init_end - init_start,
            },
            movie_timescale: config.timescale,
            moov_offset,
            sequence_number: 1,
            poisoned: false,
        })
    }

    pub fn add_sample_description(
        &mut self,
        track_id: u32,
        description: MediaConfig,
    ) -> Result<u32> {
        self.ensure_not_poisoned()?;
        let track_index = track_id.saturating_sub(1) as usize;
        let track = self
            .tracks
            .get(track_index)
            .filter(|track| track.track_id == track_id)
            .ok_or(Error::TrakNotFound(track_id))?;
        if media_config_track_type(&description) != track.track_type {
            return Err(Error::InvalidData(
                "sample description does not match the track type",
            ));
        }
        if let Some(index) = track
            .sample_descriptions
            .iter()
            .position(|existing| existing == &description)
        {
            return u32::try_from(index + 1)
                .map_err(|_| Error::InvalidData("too many sample descriptions"));
        }
        if self.has_pending_samples() {
            return Err(Error::InvalidData(
                "sample descriptions can change only at a fragment boundary",
            ));
        }
        let track = &mut self.tracks[track_index];
        track.sample_descriptions.push(description);
        track.sample_description_count = track
            .sample_descriptions
            .len()
            .try_into()
            .map_err(|_| Error::InvalidData("too many sample descriptions"))?;
        let metadata = match initialization_metadata(self.movie_timescale, &self.tracks) {
            Ok(metadata) => metadata,
            Err(error) => {
                let track = &mut self.tracks[track_index];
                track.sample_descriptions.pop();
                track.sample_description_count -= 1;
                return Err(error);
            }
        };
        let media_end = self.writer.stream_position()?;
        if let Err(error) =
            write_initialization_metadata_bytes(&mut self.writer, self.moov_offset, &metadata)
        {
            let track = &mut self.tracks[track_index];
            track.sample_descriptions.pop();
            track.sample_description_count -= 1;
            self.poisoned = true;
            return Err(error);
        }
        self.writer.seek(SeekFrom::Start(media_end))?;
        Ok(self.tracks[track_index].sample_description_count)
    }

    /// Adds a sample to the current fragment.
    ///
    /// Samples for each track must have increasing decode timestamps. The first video sample in
    /// every fragment must be a sync sample.
    pub fn write_sample(&mut self, track_id: u32, sample: Mp4Sample) -> Result<()> {
        self.write_sample_with_description(track_id, 1, sample)
    }

    pub fn write_sample_with_description(
        &mut self,
        track_id: u32,
        sample_description_index: u32,
        sample: Mp4Sample,
    ) -> Result<()> {
        self.ensure_not_poisoned()?;
        if sample.duration == 0 {
            return Err(Error::InvalidData("fragmented MP4 sample duration is zero"));
        }
        u32::try_from(sample.bytes.len())
            .map_err(|_| Error::InvalidData("fragmented MP4 sample exceeds 4 GiB"))?;

        let track = self
            .tracks
            .get_mut(track_id.saturating_sub(1) as usize)
            .filter(|track| track.track_id == track_id)
            .ok_or(Error::TrakNotFound(track_id))?;
        if sample_description_index == 0
            || sample_description_index > track.sample_description_count
        {
            return Err(Error::InvalidData(
                "sample description index is out of range",
            ));
        }
        if track
            .pending_sample_description_index
            .is_some_and(|pending| pending != sample_description_index)
        {
            return Err(Error::InvalidData(
                "sample description can change only at a fragment boundary",
            ));
        }
        if track.track_type == TrackType::Video && track.samples.is_empty() && !sample.is_sync {
            return Err(Error::InvalidData(
                "fragmented MP4 video fragment must start with a sync sample",
            ));
        }
        if let Some(previous) = track.samples.last_mut() {
            let delta = sample
                .start_time
                .checked_sub(previous.start_time)
                .filter(|delta| *delta > 0)
                .ok_or(Error::InvalidData(
                    "fragmented MP4 sample decode times are not increasing",
                ))?;
            previous.duration = u32::try_from(delta)
                .map_err(|_| Error::InvalidData("fragmented MP4 sample duration exceeds u32"))?;
        }
        if track.samples.len() == u32::MAX as usize {
            return Err(Error::InvalidData(
                "fragmented MP4 fragment has too many samples",
            ));
        }
        track.pending_sample_description_index = Some(sample_description_index);
        track.samples.push(sample);
        Ok(())
    }

    /// Writes and flushes all pending samples as one `moof`/`mdat` fragment.
    ///
    /// Returns `None` when no samples are pending.
    pub fn flush_fragment(&mut self) -> Result<Option<Mp4FragmentInfo>> {
        self.ensure_not_poisoned()?;
        if !self.has_pending_samples() {
            return Ok(None);
        }

        let fragment_start = self.writer.stream_position()?;
        let mut moof = MoofBox {
            mfhd: MfhdBox {
                sequence_number: self.sequence_number,
                ..MfhdBox::default()
            },
            trafs: Vec::new(),
        };
        let mut payload_len = 0u64;

        for track in self.tracks.iter().filter(|track| !track.samples.is_empty()) {
            let sample_count = u32::try_from(track.samples.len())
                .map_err(|_| Error::InvalidData("fragmented MP4 fragment has too many samples"))?;
            let first_sample = &track.samples[0];
            let has_composition_offsets = track
                .samples
                .iter()
                .any(|sample| sample.rendering_offset != 0);
            let signed_composition_offsets = track
                .samples
                .iter()
                .any(|sample| sample.rendering_offset < 0);
            let mut flags = TrunBox::FLAG_DATA_OFFSET
                | TrunBox::FLAG_SAMPLE_DURATION
                | TrunBox::FLAG_SAMPLE_SIZE
                | TrunBox::FLAG_SAMPLE_FLAGS;
            if has_composition_offsets {
                flags |= TrunBox::FLAG_SAMPLE_CTS;
            }
            let sample_sizes = track
                .samples
                .iter()
                .map(|sample| u32::try_from(sample.bytes.len()).unwrap())
                .collect::<Vec<_>>();
            payload_len = sample_sizes.iter().try_fold(payload_len, |total, size| {
                total
                    .checked_add(u64::from(*size))
                    .ok_or(Error::InvalidData("fragmented MP4 payload size overflow"))
            })?;

            let has_multiple_descriptions = track.sample_description_count > 1;
            moof.trafs.push(TrafBox {
                tfhd: TfhdBox {
                    flags: TfhdBox::FLAG_DEFAULT_BASE_IS_MOOF
                        | TfhdBox::FLAG_DEFAULT_SAMPLE_DURATION
                        | TfhdBox::FLAG_DEFAULT_SAMPLE_SIZE
                        | TfhdBox::FLAG_DEFAULT_SAMPLE_FLAGS
                        | if has_multiple_descriptions {
                            TfhdBox::FLAG_SAMPLE_DESCRIPTION_INDEX
                        } else {
                            0
                        },
                    track_id: track.track_id,
                    base_data_offset: None,
                    sample_description_index: has_multiple_descriptions
                        .then_some(track.pending_sample_description_index.unwrap()),
                    default_sample_duration: Some(first_sample.duration),
                    default_sample_size: Some(u32::try_from(first_sample.bytes.len()).unwrap()),
                    default_sample_flags: Some(default_sample_flags(track.track_type)),
                    ..TfhdBox::default()
                },
                tfdt: Some(TfdtBox {
                    version: 1,
                    base_media_decode_time: track.samples[0].start_time,
                    ..TfdtBox::default()
                }),
                trun: Some(TrunBox {
                    version: u8::from(signed_composition_offsets),
                    flags,
                    sample_count,
                    data_offset: Some(0),
                    sample_durations: track.samples.iter().map(|sample| sample.duration).collect(),
                    sample_sizes,
                    sample_flags: track
                        .samples
                        .iter()
                        .map(|sample| sample_flags(track.track_type, sample.is_sync))
                        .collect(),
                    sample_cts: if has_composition_offsets {
                        track
                            .samples
                            .iter()
                            .map(|sample| sample.rendering_offset as u32)
                            .collect()
                    } else {
                        Vec::new()
                    },
                    ..TrunBox::default()
                }),
            });
        }

        let mdat_size = HEADER_SIZE
            .checked_add(payload_len)
            .ok_or(Error::InvalidData("fragmented MP4 mdat size overflow"))?;
        if mdat_size > u64::from(u32::MAX) {
            return Err(Error::InvalidData(
                "fragmented MP4 mdat exceeds 32-bit box size",
            ));
        }
        let mut payload_offset = moof
            .box_size()
            .checked_add(HEADER_SIZE)
            .ok_or(Error::InvalidData("fragmented MP4 data offset overflow"))?;
        let mut video_keyframe = None;
        for (traf, track) in moof
            .trafs
            .iter_mut()
            .zip(self.tracks.iter().filter(|track| !track.samples.is_empty()))
        {
            traf.trun.as_mut().unwrap().data_offset = Some(
                i32::try_from(payload_offset)
                    .map_err(|_| Error::InvalidData("fragmented MP4 data offset exceeds i32"))?,
            );
            if video_keyframe.is_none() && track.track_type == TrackType::Video {
                let first_sample = &track.samples[0];
                if first_sample.is_sync {
                    video_keyframe = Some(Mp4SampleLocation {
                        offset: fragment_start
                            .checked_add(payload_offset)
                            .ok_or(Error::InvalidData("keyframe byte offset overflow"))?,
                        size: u32::try_from(first_sample.bytes.len()).map_err(|_| {
                            Error::InvalidData("fragmented MP4 sample exceeds 4 GiB")
                        })?,
                    });
                }
            }
            for sample in &track.samples {
                payload_offset = payload_offset
                    .checked_add(sample.bytes.len() as u64)
                    .ok_or(Error::InvalidData("fragmented MP4 data offset overflow"))?;
            }
        }

        moof.write_box(&mut self.writer)?;
        BoxHeader::new(BoxType::MdatBox, mdat_size).write(&mut self.writer)?;
        for track in self.tracks.iter().filter(|track| !track.samples.is_empty()) {
            for sample in &track.samples {
                self.writer.write_all(&sample.bytes)?;
            }
        }
        self.writer.flush()?;
        let fragment_end = self.writer.stream_position()?;
        for track in &mut self.tracks {
            track.samples.clear();
            track.pending_sample_description_index = None;
        }

        let info = Mp4FragmentInfo {
            sequence_number: self.sequence_number,
            range: Mp4ByteRange {
                offset: fragment_start,
                size: fragment_end - fragment_start,
            },
            video_keyframe,
        };
        self.sequence_number = self
            .sequence_number
            .checked_add(1)
            .ok_or(Error::InvalidData(
                "fragmented MP4 sequence number overflow",
            ))?;
        Ok(Some(info))
    }

    /// Finishes one track's trailing sample at the next sample's decode timestamp, then flushes.
    ///
    /// This is useful for keyframe boundaries where the next sync sample belongs to a new
    /// fragment but its timestamp determines the preceding fragment's final sample duration.
    pub fn flush_fragment_before_sample(
        &mut self,
        track_id: u32,
        next_start_time: u64,
    ) -> Result<Option<Mp4FragmentInfo>> {
        self.ensure_not_poisoned()?;
        let track = self
            .tracks
            .get_mut(track_id.saturating_sub(1) as usize)
            .filter(|track| track.track_id == track_id)
            .ok_or(Error::TrakNotFound(track_id))?;
        if let Some(last) = track.samples.last_mut() {
            let duration = next_start_time
                .checked_sub(last.start_time)
                .filter(|duration| *duration > 0)
                .ok_or(Error::InvalidData(
                    "fragment boundary does not follow the trailing sample",
                ))?;
            last.duration = u32::try_from(duration)
                .map_err(|_| Error::InvalidData("fragmented MP4 sample duration exceeds u32"))?;
        }
        self.flush_fragment()
    }

    /// Flushes the final pending fragment without rewriting the initialization metadata.
    pub fn write_end(&mut self) -> Result<Option<Mp4FragmentInfo>> {
        self.flush_fragment()
    }

    const fn ensure_not_poisoned(&self) -> Result<()> {
        if self.poisoned {
            Err(Error::InvalidData("fragmented MP4 writer is poisoned"))
        } else {
            Ok(())
        }
    }
}

pub fn normalize_fragment_sample_description_indices(fragment: &[u8]) -> Result<Vec<u8>> {
    let mut reader = Cursor::new(fragment);
    let header = BoxHeader::read(&mut reader)?;
    if header.name != BoxType::MoofBox || header.size > fragment.len() as u64 {
        return Err(Error::InvalidData(
            "fragment must begin with one complete moof box",
        ));
    }
    let mut moof = MoofBox::read_box(&mut reader, header.size)?;
    for traf in &mut moof.trafs {
        if traf.tfhd.flags & TfhdBox::FLAG_SAMPLE_DESCRIPTION_INDEX != 0 {
            traf.tfhd.sample_description_index = Some(1);
        }
    }
    let mut normalized = Vec::with_capacity(fragment.len());
    moof.write_box(&mut normalized)?;
    if normalized.len() as u64 != header.size {
        return Err(Error::InvalidData(
            "normalizing sample descriptions changed the moof size",
        ));
    }
    normalized.extend_from_slice(&fragment[header.size as usize..]);
    Ok(normalized)
}

fn write_initialization_metadata<W: Write + Seek>(
    writer: &mut W,
    moov_offset: u64,
    movie_timescale: u32,
    tracks: &[FragmentTrack],
) -> Result<()> {
    let metadata = initialization_metadata(movie_timescale, tracks)?;
    write_initialization_metadata_bytes(writer, moov_offset, &metadata)
}

fn initialization_metadata(movie_timescale: u32, tracks: &[FragmentTrack]) -> Result<Vec<u8>> {
    let mut moov = MoovBox::default();
    moov.mvhd.timescale = movie_timescale;
    moov.mvhd.next_track_id = u32::try_from(tracks.len())
        .ok()
        .and_then(|count| count.checked_add(1))
        .ok_or(Error::InvalidData("too many fragmented MP4 tracks"))?;
    let mut trexs = Vec::with_capacity(tracks.len());
    for track in tracks {
        let mut track_writer = Mp4TrackWriter::new_with_sample_descriptions(
            track.track_id,
            track.track_type,
            track.timescale,
            &track.language,
            &track.sample_descriptions,
        )?;
        let mut scratch = Cursor::new(Vec::new());
        moov.traks.push(track_writer.write_end(&mut scratch)?);
        trexs.push(TrexBox {
            track_id: track.track_id,
            default_sample_description_index: 1,
            ..TrexBox::default()
        });
    }
    moov.mvex = Some(MvexBox { mehd: None, trexs });
    let mut metadata = Cursor::new(Vec::new());
    moov.write_box(&mut metadata)?;
    let metadata = metadata.into_inner();
    let free_size = INITIALIZATION_METADATA_CAPACITY
        .checked_sub(metadata.len() as u64)
        .filter(|size| *size >= HEADER_SIZE)
        .ok_or(Error::InvalidData(
            "fragmented MP4 initialization metadata capacity exceeded",
        ))?;

    let mut output = Vec::with_capacity(INITIALIZATION_METADATA_CAPACITY as usize);
    output.extend_from_slice(&metadata);
    BoxHeader::new(BoxType::FreeBox, free_size).write(&mut output)?;
    output.resize(INITIALIZATION_METADATA_CAPACITY as usize, 0);
    Ok(output)
}

fn write_initialization_metadata_bytes<W: Write + Seek>(
    writer: &mut W,
    moov_offset: u64,
    metadata: &[u8],
) -> Result<()> {
    writer.seek(SeekFrom::Start(moov_offset))?;
    writer.write_all(metadata)?;
    writer.flush()?;
    Ok(())
}

const fn media_config_track_type(config: &MediaConfig) -> TrackType {
    match config {
        MediaConfig::AvcConfig(_) | MediaConfig::HevcConfig(_) | MediaConfig::Vp9Config(_) => {
            TrackType::Video
        }
        MediaConfig::AacConfig(_) => TrackType::Audio,
        MediaConfig::TtxtConfig(_) => TrackType::Subtitle,
    }
}

fn sample_flags(track_type: TrackType, is_sync: bool) -> u32 {
    if track_type == TrackType::Video && !is_sync {
        NON_SYNC_SAMPLE_FLAGS
    } else {
        SYNC_SAMPLE_FLAGS
    }
}

fn default_sample_flags(track_type: TrackType) -> u32 {
    if track_type == TrackType::Video {
        NON_SYNC_SAMPLE_FLAGS
    } else {
        SYNC_SAMPLE_FLAGS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    struct FaultInjectingWriter {
        inner: Cursor<Vec<u8>>,
        bytes_until_failure: Option<usize>,
    }

    impl FaultInjectingWriter {
        fn new() -> Self {
            Self {
                inner: Cursor::new(Vec::new()),
                bytes_until_failure: None,
            }
        }

        fn fail_once_after(&mut self, bytes: usize) {
            self.bytes_until_failure = Some(bytes);
        }
    }

    impl Write for FaultInjectingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let Some(remaining) = self.bytes_until_failure else {
                return self.inner.write(buffer);
            };
            if remaining == 0 {
                self.bytes_until_failure = None;
                return Err(io::Error::other("injected initialization rewrite failure"));
            }
            let written = self.inner.write(&buffer[..buffer.len().min(remaining)])?;
            self.bytes_until_failure = Some(remaining - written);
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for FaultInjectingWriter {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    fn mp4_config() -> Mp4Config {
        Mp4Config {
            major_brand: "iso6".parse().unwrap(),
            minor_version: 1,
            compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
            timescale: 1_000,
        }
    }

    fn video_track() -> TrackConfig {
        TrackConfig {
            track_type: TrackType::Video,
            timescale: 1_000,
            language: "und".to_owned(),
            media_conf: MediaConfig::AvcConfig(AvcConfig {
                width: 320,
                height: 240,
                seq_param_set: Vec::new(),
                pic_param_set: Vec::new(),
            }),
        }
    }

    fn audio_track() -> TrackConfig {
        TrackConfig {
            track_type: TrackType::Audio,
            timescale: 48_000,
            language: "und".to_owned(),
            media_conf: MediaConfig::AacConfig(AacConfig {
                bitrate: 64_000,
                profile: AudioObjectType::AacLowComplexity,
                freq_index: SampleFreqIndex::Freq48000,
                chan_conf: ChannelConfig::Stereo,
            }),
        }
    }

    #[test]
    fn h265_initialization_exposes_decoder_configuration() {
        let track = TrackConfig {
            track_type: TrackType::Video,
            timescale: 90_000,
            language: "und".to_owned(),
            media_conf: MediaConfig::HevcConfig(HevcConfig {
                width: 640,
                height: 360,
                vps: vec![0x40, 0x01, 0x0c],
                sps: vec![0x42, 0x01, 0x01],
                pps: vec![0x44, 0x01, 0xc0],
                decoder_config: Vec::new(),
            }),
        };
        let writer =
            FragmentedMp4Writer::write_start(Cursor::new(Vec::new()), &mp4_config(), &[track])
                .unwrap();
        let mut buffer = writer.into_writer();
        let size = buffer.get_ref().len() as u64;
        buffer.set_position(0);
        let reader = Mp4Reader::read_header(buffer, size).unwrap();
        let decoder = reader.tracks()[&1].video_decoder_config().unwrap().unwrap();
        assert_eq!(decoder.codec, "hev1.0.0.L0.00");
        assert_eq!((decoder.width, decoder.height), (640, 360));
        assert_eq!(decoder.nal_length_size, 4);
        assert!(!decoder.description.is_empty());
    }

    fn sample(start_time: u64, is_sync: bool, bytes: &'static [u8]) -> Mp4Sample {
        Mp4Sample {
            start_time,
            duration: 1_000,
            rendering_offset: 0,
            is_sync,
            bytes: Bytes::from_static(bytes),
        }
    }

    #[test]
    fn fragmented_writer_round_trips_samples_and_ranges() {
        let mut writer = FragmentedMp4Writer::write_start(
            Cursor::new(Vec::new()),
            &mp4_config(),
            &[video_track()],
        )
        .unwrap();
        let initialization = writer.initialization();
        assert_eq!(initialization.offset, 0);
        assert!(initialization.size > 0);

        writer
            .write_sample(1, sample(0, true, b"keyframe-1"))
            .unwrap();
        writer
            .write_sample(1, sample(1_000, false, b"interframe"))
            .unwrap();
        let first = writer.flush_fragment().unwrap().unwrap();
        writer
            .write_sample(1, sample(2_000, true, b"keyframe-2"))
            .unwrap();
        let second = writer.write_end().unwrap().unwrap();
        assert_eq!(first.sequence_number, 1);
        assert_eq!(first.range.offset, initialization.size);
        assert_eq!(second.sequence_number, 2);
        assert_eq!(second.range.offset, first.range.offset + first.range.size);

        let mut buffer = writer.into_writer();
        let size = buffer.get_ref().len() as u64;
        assert_eq!(second.range.offset + second.range.size, size);
        buffer.set_position(0);
        let mut reader = Mp4Reader::read_header(buffer, size).unwrap();
        assert!(reader.is_fragmented());
        assert_eq!(reader.moofs.len(), 2);
        assert_eq!(reader.sample_count(1).unwrap(), 3);

        let first_sample = reader.read_sample(1, 1).unwrap().unwrap();
        assert_eq!(first_sample.start_time, 0);
        assert_eq!(first_sample.bytes, Bytes::from_static(b"keyframe-1"));
        assert!(first_sample.is_sync);
        let second_sample = reader.read_sample(1, 2).unwrap().unwrap();
        assert_eq!(second_sample.start_time, 1_000);
        assert_eq!(second_sample.bytes, Bytes::from_static(b"interframe"));
        assert!(!second_sample.is_sync);
        let third_sample = reader.read_sample(1, 3).unwrap().unwrap();
        assert_eq!(third_sample.start_time, 2_000);
        assert_eq!(third_sample.bytes, Bytes::from_static(b"keyframe-2"));
        assert!(third_sample.is_sync);
    }

    #[test]
    fn video_fragment_rejects_non_sync_first_sample() {
        let mut writer = FragmentedMp4Writer::write_start(
            Cursor::new(Vec::new()),
            &mp4_config(),
            &[video_track()],
        )
        .unwrap();

        let error = writer
            .write_sample(1, sample(0, false, b"interframe"))
            .unwrap_err();
        assert!(error.to_string().contains("must start with a sync sample"));
    }

    #[test]
    fn video_and_audio_tracks_have_independent_payload_offsets() {
        let mut writer = FragmentedMp4Writer::write_start(
            Cursor::new(Vec::new()),
            &mp4_config(),
            &[video_track(), audio_track()],
        )
        .unwrap();
        writer
            .write_sample(1, sample(0, true, b"video-payload"))
            .unwrap();
        writer
            .write_sample(
                2,
                Mp4Sample {
                    start_time: 0,
                    duration: 1_024,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: Bytes::from_static(b"audio-one"),
                },
            )
            .unwrap();
        writer
            .write_sample(
                2,
                Mp4Sample {
                    start_time: 1_024,
                    duration: 1_024,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: Bytes::from_static(b"audio-two"),
                },
            )
            .unwrap();
        writer.write_end().unwrap();

        let mut buffer = writer.into_writer();
        let size = buffer.get_ref().len() as u64;
        buffer.set_position(0);
        let mut reader = Mp4Reader::read_header(buffer, size).unwrap();
        assert_eq!(reader.tracks().len(), 2);
        let video_header = &reader.moofs[0].trafs[0].tfhd;
        assert_eq!(video_header.default_sample_duration, Some(1_000));
        assert_eq!(video_header.default_sample_size, Some(13));
        assert_eq!(
            video_header.default_sample_flags,
            Some(NON_SYNC_SAMPLE_FLAGS)
        );
        let audio_header = &reader.moofs[0].trafs[1].tfhd;
        assert_eq!(audio_header.default_sample_duration, Some(1_024));
        assert_eq!(audio_header.default_sample_size, Some(9));
        assert_eq!(audio_header.default_sample_flags, Some(SYNC_SAMPLE_FLAGS));
        assert_eq!(reader.sample_count(1).unwrap(), 1);
        assert_eq!(reader.sample_count(2).unwrap(), 2);
        assert_eq!(
            reader.read_sample(1, 1).unwrap().unwrap().bytes,
            Bytes::from_static(b"video-payload")
        );
        assert_eq!(
            reader.read_sample(2, 1).unwrap().unwrap().bytes,
            Bytes::from_static(b"audio-one")
        );
        assert_eq!(
            reader.read_sample(2, 2).unwrap().unwrap().bytes,
            Bytes::from_static(b"audio-two")
        );
    }

    #[test]
    fn boundary_timestamp_sets_trailing_sample_duration() {
        let mut writer = FragmentedMp4Writer::write_start(
            Cursor::new(Vec::new()),
            &mp4_config(),
            &[video_track()],
        )
        .unwrap();
        writer
            .write_sample(1, sample(0, true, b"keyframe"))
            .unwrap();
        writer
            .write_sample(1, sample(1_000, false, b"interframe"))
            .unwrap();
        writer.flush_fragment_before_sample(1, 2_500).unwrap();

        let mut buffer = writer.into_writer();
        let size = buffer.get_ref().len() as u64;
        buffer.set_position(0);
        let mut reader = Mp4Reader::read_header(buffer, size).unwrap();
        assert_eq!(reader.read_sample(1, 1).unwrap().unwrap().duration, 1_000);
        assert_eq!(reader.read_sample(1, 2).unwrap().unwrap().duration, 1_500);
    }

    #[test]
    fn fragmented_track_switches_descriptions_only_at_fragment_boundaries() {
        let low = MediaConfig::AvcConfig(AvcConfig {
            width: 640,
            height: 360,
            seq_param_set: vec![0x67, 0x42, 0xc0, 0x1f],
            pic_param_set: vec![0x68, 0xce, 0x3c, 0x80],
        });
        let high = MediaConfig::HevcConfig(HevcConfig {
            width: 3840,
            height: 2160,
            vps: vec![0x40, 0x01, 0x0c],
            sps: vec![0x42, 0x01, 0x01],
            pps: vec![0x44, 0x01, 0xc0],
            decoder_config: Vec::new(),
        });
        let track = FragmentedTrackConfig {
            track_type: TrackType::Video,
            timescale: 90_000,
            language: "und".to_owned(),
            sample_descriptions: vec![low, high],
        };
        let mut writer = FragmentedMp4Writer::write_start_with_sample_descriptions(
            Cursor::new(Vec::new()),
            &mp4_config(),
            &[track],
        )
        .unwrap();

        writer
            .write_sample_with_description(1, 1, sample(0, true, b"sub-keyframe"))
            .unwrap();
        writer
            .write_sample_with_description(1, 1, sample(3_003, false, b"sub-delta"))
            .unwrap();
        writer.flush_fragment_before_sample(1, 7_507).unwrap();

        writer
            .write_sample_with_description(1, 2, sample(7_507, true, b"main-keyframe"))
            .unwrap();
        let error = writer
            .write_sample_with_description(1, 1, sample(11_107, true, b"invalid-switch"))
            .unwrap_err();
        assert!(error.to_string().contains("fragment boundary"));
        writer.flush_fragment_before_sample(1, 11_107).unwrap();

        writer
            .write_sample_with_description(1, 1, sample(11_107, true, b"sub-keyframe-2"))
            .unwrap();
        writer.write_end().unwrap();

        let mut buffer = writer.into_writer();
        let size = buffer.get_ref().len() as u64;
        buffer.set_position(0);
        let mut reader = Mp4Reader::read_header(buffer, size).unwrap();
        let track = &reader.tracks()[&1];
        assert_eq!(track.sample_description_count(), 2);
        assert_eq!(
            track
                .video_decoder_config_for_description(1)
                .unwrap()
                .unwrap()
                .width,
            640
        );
        assert_eq!(
            track
                .video_decoder_config_for_description(2)
                .unwrap()
                .unwrap()
                .width,
            3840
        );
        assert_eq!(track.sample_description_index(1).unwrap(), 1);
        assert_eq!(track.sample_description_index(2).unwrap(), 1);
        assert_eq!(track.sample_description_index(3).unwrap(), 2);
        assert_eq!(track.sample_description_index(4).unwrap(), 1);
        assert_eq!(
            reader
                .fragment_first_sample_locations(1)
                .unwrap()
                .iter()
                .map(|fragment| fragment.sample_description_index)
                .collect::<Vec<_>>(),
            vec![1, 2, 1]
        );
        assert_eq!(reader.read_sample(1, 1).unwrap().unwrap().duration, 3_003);
        assert_eq!(reader.read_sample(1, 2).unwrap().unwrap().duration, 4_504);
        assert_eq!(reader.read_sample(1, 3).unwrap().unwrap().duration, 3_600);
    }

    #[test]
    fn fragmented_reader_uses_trex_description_when_tfhd_omits_it() {
        let mut writer = FragmentedMp4Writer::write_start(
            Cursor::new(Vec::new()),
            &mp4_config(),
            &[video_track()],
        )
        .unwrap();
        writer
            .write_sample(1, sample(0, true, b"keyframe"))
            .unwrap();
        writer.write_end().unwrap();

        let mut buffer = writer.into_writer();
        let size = buffer.get_ref().len() as u64;
        buffer.set_position(0);
        let reader = Mp4Reader::read_header(buffer, size).unwrap();
        assert_eq!(reader.tracks()[&1].sample_description_index(1).unwrap(), 1);
    }

    #[test]
    fn fragmented_writer_registers_a_new_codec_after_media_was_flushed() {
        let mut writer = FragmentedMp4Writer::write_start(
            Cursor::new(Vec::new()),
            &mp4_config(),
            &[video_track()],
        )
        .unwrap();
        let initialization = writer.initialization();
        writer
            .write_sample(1, sample(0, true, b"h264-keyframe"))
            .unwrap();
        let first_fragment = writer.flush_fragment().unwrap().unwrap();
        assert_eq!(first_fragment.range.offset, initialization.size);

        let high = MediaConfig::HevcConfig(HevcConfig {
            width: 3840,
            height: 2160,
            vps: vec![0x40, 0x01, 0x0c],
            sps: vec![0x42, 0x01, 0x01],
            pps: vec![0x44, 0x01, 0xc0],
            decoder_config: Vec::new(),
        });
        let description_index = writer.add_sample_description(1, high.clone()).unwrap();
        assert_eq!(description_index, 2);
        assert_eq!(writer.add_sample_description(1, high).unwrap(), 2);
        assert_eq!(writer.initialization(), initialization);

        writer
            .write_sample_with_description(1, 2, sample(4_500, true, b"h265-keyframe"))
            .unwrap();
        let second_fragment = writer.write_end().unwrap().unwrap();
        assert_eq!(
            second_fragment.range.offset,
            first_fragment.range.offset + first_fragment.range.size
        );

        let mut buffer = writer.into_writer();
        let size = buffer.get_ref().len() as u64;
        buffer.set_position(0);
        let reader = Mp4Reader::read_header(buffer, size).unwrap();
        let track = &reader.tracks()[&1];
        assert_eq!(track.sample_description_count(), 2);
        assert_eq!(track.sample_description_index(1).unwrap(), 1);
        assert_eq!(track.sample_description_index(2).unwrap(), 2);
        assert_eq!(
            track.media_type_for_description(1).unwrap(),
            MediaType::H264
        );
        assert_eq!(
            track.media_type_for_description(2).unwrap(),
            MediaType::H265
        );
        assert_eq!(track.dimensions_for_description(1).unwrap(), (320, 240));
        assert_eq!(track.dimensions_for_description(2).unwrap(), (3840, 2160));
    }

    #[test]
    fn initialization_capacity_failure_leaves_the_writer_usable() {
        let mut writer = FragmentedMp4Writer::write_start(
            Cursor::new(Vec::new()),
            &mp4_config(),
            &[video_track()],
        )
        .unwrap();
        let mut successful_descriptions = 1usize;
        let capacity_error = (1u8..=u8::MAX).find_map(|value| {
            let mut sequence_parameter_set = vec![0x67, 0x42, value, 0x1f];
            sequence_parameter_set.resize(512, value);
            let description = MediaConfig::AvcConfig(AvcConfig {
                width: 320 + u16::from(value),
                height: 240,
                seq_param_set: sequence_parameter_set,
                pic_param_set: vec![0x68, value, 0x3c, 0x80],
            });
            match writer.add_sample_description(1, description) {
                Ok(_) => {
                    successful_descriptions += 1;
                    None
                }
                Err(error) => Some(error),
            }
        });
        assert!(matches!(
            capacity_error,
            Some(Error::InvalidData(
                "fragmented MP4 initialization metadata capacity exceeded"
            ))
        ));

        writer
            .write_sample(1, sample(0, true, b"keyframe-after-capacity-error"))
            .unwrap();
        writer.write_end().unwrap();
        let initialization = writer.initialization();
        let mut buffer = writer.into_writer();
        let initialization_bytes = &buffer.get_ref()[initialization.offset as usize
            ..(initialization.offset + initialization.size) as usize];
        let free_type = initialization_bytes
            .windows(4)
            .rposition(|window| window == b"free")
            .unwrap();
        let free_start = free_type - 4;
        let free_size = u32::from_be_bytes(
            initialization_bytes[free_start..free_start + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        assert!(
            initialization_bytes[free_start + HEADER_SIZE as usize..free_start + free_size]
                .iter()
                .all(|byte| *byte == 0)
        );
        let size = buffer.get_ref().len() as u64;
        buffer.set_position(0);
        let reader = Mp4Reader::read_header(buffer, size).unwrap();
        assert_eq!(
            reader.tracks()[&1].sample_description_count(),
            successful_descriptions
        );
        assert_eq!(reader.sample_count(1).unwrap(), 1);
    }

    #[test]
    fn rewrite_io_failure_poisons_the_writer() {
        let mut writer = FragmentedMp4Writer::write_start(
            FaultInjectingWriter::new(),
            &mp4_config(),
            &[video_track()],
        )
        .unwrap();
        writer
            .write_sample(1, sample(0, true, b"first-keyframe"))
            .unwrap();
        writer.flush_fragment().unwrap();
        writer.writer.fail_once_after(128);

        let error = writer
            .add_sample_description(
                1,
                MediaConfig::HevcConfig(HevcConfig {
                    width: 1920,
                    height: 1080,
                    vps: vec![0x40, 0x01, 0x0c],
                    sps: vec![0x42, 0x01, 0x01],
                    pps: vec![0x44, 0x01, 0xc0],
                    decoder_config: Vec::new(),
                }),
            )
            .unwrap_err();
        assert!(matches!(error, Error::IoError(_)));
        assert!(matches!(
            writer.write_sample(1, sample(1_000, true, b"must-not-write")),
            Err(Error::InvalidData("fragmented MP4 writer is poisoned"))
        ));
        assert!(matches!(
            writer.write_end(),
            Err(Error::InvalidData("fragmented MP4 writer is poisoned"))
        ));
    }

    #[test]
    fn reader_rejects_out_of_range_fragment_description_index() {
        let track = FragmentedTrackConfig {
            track_type: TrackType::Video,
            timescale: 1_000,
            language: "und".to_owned(),
            sample_descriptions: vec![
                video_track().media_conf,
                MediaConfig::AvcConfig(AvcConfig {
                    width: 640,
                    height: 360,
                    seq_param_set: vec![0x67, 0x42, 0, 0x1f],
                    pic_param_set: vec![0x68, 0xce, 0x3c, 0x80],
                }),
            ],
        };
        let mut writer = FragmentedMp4Writer::write_start_with_sample_descriptions(
            Cursor::new(Vec::new()),
            &mp4_config(),
            &[track],
        )
        .unwrap();
        writer
            .write_sample_with_description(1, 2, sample(0, true, b"keyframe"))
            .unwrap();
        writer.write_end().unwrap();
        let valid = writer.into_writer().into_inner();
        let tfhd_type = valid
            .windows(4)
            .position(|window| window == b"tfhd")
            .unwrap();
        for invalid_index in [0u32, 99] {
            let mut bytes = valid.clone();
            bytes[tfhd_type + 12..tfhd_type + 16].copy_from_slice(&invalid_index.to_be_bytes());
            let reader =
                Mp4Reader::read_header(Cursor::new(bytes.clone()), bytes.len() as u64).unwrap();
            assert!(matches!(
                reader.tracks()[&1].sample_description_index(1),
                Err(Error::InvalidData(
                    "sample description index is out of range"
                ))
            ));
        }
    }

    #[test]
    fn normalizes_fragment_description_indices_without_moving_payload() {
        let track = FragmentedTrackConfig {
            track_type: TrackType::Video,
            timescale: 1_000,
            language: "und".to_owned(),
            sample_descriptions: vec![
                video_track().media_conf,
                MediaConfig::AvcConfig(AvcConfig {
                    width: 640,
                    height: 360,
                    seq_param_set: vec![0x67, 0x42, 0, 0x1f],
                    pic_param_set: vec![0x68, 0xce, 0x3c, 0x80],
                }),
            ],
        };
        let mut writer = FragmentedMp4Writer::write_start_with_sample_descriptions(
            Cursor::new(Vec::new()),
            &mp4_config(),
            &[track],
        )
        .unwrap();
        writer
            .write_sample_with_description(1, 2, sample(0, true, b"period-keyframe"))
            .unwrap();
        let fragment = writer.write_end().unwrap().unwrap();
        let bytes = writer.into_writer().into_inner();
        let original = &bytes[fragment.range.offset as usize
            ..(fragment.range.offset + fragment.range.size) as usize];
        let normalized = normalize_fragment_sample_description_indices(original).unwrap();
        assert_eq!(normalized.len(), original.len());

        let mut reader = Cursor::new(normalized.as_slice());
        let header = BoxHeader::read(&mut reader).unwrap();
        let moof = MoofBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(moof.trafs[0].tfhd.sample_description_index, Some(1));
        assert_eq!(
            &normalized[header.size as usize..],
            &original[header.size as usize..]
        );
    }
}
