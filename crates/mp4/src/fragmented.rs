//! Fragmented MP4 output with publishable initialization and media byte ranges.
//!
//! Tracks are fixed when the writer starts. Samples are buffered until the caller flushes a
//! fragment, allowing the caller to choose keyframe-aligned boundaries. Each returned range has
//! been flushed to the underlying writer and can be indexed or served independently alongside the
//! initialization range.

use crate::{
    mp4box::{
        mfhd::MfhdBox, mvex::MvexBox, tfdt::TfdtBox, tfhd::TfhdBox, traf::TrafBox, trex::TrexBox,
        trun::TrunBox,
    },
    track::Mp4TrackWriter,
    *,
};
use std::io::{Cursor, Seek, Write};

const SYNC_SAMPLE_FLAGS: u32 = 0x0200_0000;
const NON_SYNC_SAMPLE_FLAGS: u32 = 0x0101_0000;

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

#[derive(Debug)]
struct FragmentTrack {
    track_id: u32,
    track_type: TrackType,
    samples: Vec<Mp4Sample>,
}

#[derive(Debug)]
/// Writes an initialization segment followed by explicitly flushed MP4 fragments.
///
/// Video fragments must begin with a sync sample. The track set cannot change after
/// [`Self::write_start`]; start a new writer when codec parameters or track presence changes.
pub struct FragmentedMp4Writer<W> {
    writer: W,
    tracks: Vec<FragmentTrack>,
    initialization: Mp4ByteRange,
    sequence_number: u32,
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
        mut writer: W,
        config: &Mp4Config,
        track_configs: &[TrackConfig],
    ) -> Result<Self> {
        if track_configs.is_empty() {
            return Err(Error::InvalidData(
                "fragmented MP4 requires at least one track",
            ));
        }
        if config.timescale == 0 || track_configs.iter().any(|track| track.timescale == 0) {
            return Err(Error::InvalidData("MP4 timescales must be non-zero"));
        }

        let init_start = writer.stream_position()?;
        FtypBox {
            major_brand: config.major_brand,
            minor_version: config.minor_version,
            compatible_brands: config.compatible_brands.clone(),
        }
        .write_box(&mut writer)?;

        let mut moov = MoovBox::default();
        moov.mvhd.timescale = config.timescale;
        moov.mvhd.next_track_id = u32::try_from(track_configs.len())
            .ok()
            .and_then(|count| count.checked_add(1))
            .ok_or(Error::InvalidData("too many fragmented MP4 tracks"))?;

        let mut tracks = Vec::with_capacity(track_configs.len());
        let mut trexs = Vec::with_capacity(track_configs.len());
        for (index, track_config) in track_configs.iter().enumerate() {
            let track_id = u32::try_from(index + 1)
                .map_err(|_| Error::InvalidData("too many fragmented MP4 tracks"))?;
            let mut track_writer = Mp4TrackWriter::new(track_id, track_config)?;
            let mut scratch = Cursor::new(Vec::new());
            moov.traks.push(track_writer.write_end(&mut scratch)?);
            trexs.push(TrexBox {
                track_id,
                default_sample_description_index: 1,
                ..TrexBox::default()
            });
            tracks.push(FragmentTrack {
                track_id,
                track_type: track_config.track_type,
                samples: Vec::new(),
            });
        }
        moov.mvex = Some(MvexBox { mehd: None, trexs });
        moov.write_box(&mut writer)?;
        writer.flush()?;

        let init_end = writer.stream_position()?;
        Ok(Self {
            writer,
            tracks,
            initialization: Mp4ByteRange {
                offset: init_start,
                size: init_end - init_start,
            },
            sequence_number: 1,
        })
    }

    /// Adds a sample to the current fragment.
    ///
    /// Samples for each track must have increasing decode timestamps. The first video sample in
    /// every fragment must be a sync sample.
    pub fn write_sample(&mut self, track_id: u32, sample: Mp4Sample) -> Result<()> {
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
        track.samples.push(sample);
        Ok(())
    }

    /// Writes and flushes all pending samples as one `moof`/`mdat` fragment.
    ///
    /// Returns `None` when no samples are pending.
    pub fn flush_fragment(&mut self) -> Result<Option<Mp4FragmentInfo>> {
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

            moof.trafs.push(TrafBox {
                tfhd: TfhdBox {
                    flags: TfhdBox::FLAG_DEFAULT_BASE_IS_MOOF
                        | TfhdBox::FLAG_DEFAULT_SAMPLE_DURATION
                        | TfhdBox::FLAG_DEFAULT_SAMPLE_SIZE
                        | TfhdBox::FLAG_DEFAULT_SAMPLE_FLAGS,
                    track_id: track.track_id,
                    base_data_offset: None,
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
}
