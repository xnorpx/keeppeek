use std::{
    io::{self, BufReader, Read, Seek, SeekFrom},
    time::Instant,
};

const METADATA_BYTES_MAX: u64 = 8 * 1024 * 1024;
const BOX_BYTES_MAX: u64 = 1024 * 1024;
const BOX_COUNT_MAX: usize = 16_384;
const FRAGMENT_COUNT_MAX: usize = 4_096;

impl super::Archive {
    pub(in crate::storage) fn container_index(
        &self,
        observation: &super::Observation,
        deadline: Instant,
    ) -> anyhow::Result<Index> {
        self.revalidate_until(observation, deadline)?;
        let options = content_options();
        let (mut file, metadata) =
            self.open_file_with(&observation.relative, deadline, &options)?;
        anyhow::ensure!(
            super::same_metadata(&metadata, &observation.metadata)?,
            "recording changed before container validation"
        );
        let index = inspect(&mut file, metadata.len(), deadline)?;
        self.revalidate_until(observation, deadline)?;
        Ok(index)
    }
}

fn content_options() -> cap_std::fs::OpenOptions {
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        let mut options = super::observation_options();
        options.access_mode(windows::Win32::Foundation::GENERIC_READ.0);
        options
    }
    #[cfg(not(windows))]
    {
        super::observation_options()
    }
}

pub(in crate::storage) struct Index {
    pub initialization: mp4::Mp4ByteRange,
    pub fragments: Vec<Fragment>,
}

pub(in crate::storage) struct Fragment {
    pub range: mp4::Mp4ByteRange,
    pub first_sample: mp4::Mp4FragmentSampleLocation,
    pub start_ms: u64,
    pub duration_ms: u64,
}

struct MediaRange {
    range: mp4::Mp4ByteRange,
    payload: u64,
}

pub(in crate::storage) fn inspect(
    reader: &mut (impl Read + Seek),
    size: u64,
    deadline: Instant,
) -> anyhow::Result<Index> {
    let (initialization, ranges) = scan(reader, size, deadline)?;
    reader.seek(SeekFrom::Start(0))?;
    let bounded = Bounded {
        reader: BufReader::new(reader),
        remaining: METADATA_BYTES_MAX * 4,
        size,
        deadline,
    };
    let parsed = mp4::Mp4Reader::read_header(bounded, size)?;
    anyhow::ensure!(
        parsed.is_fragmented() && parsed.tracks().len() <= 2,
        "unsupported recording tracks"
    );
    let (&video_id, track) = parsed
        .tracks()
        .iter()
        .find(|(_, track)| {
            matches!(
                track.media_type(),
                Ok(mp4::MediaType::H264 | mp4::MediaType::H265)
            )
        })
        .ok_or_else(|| anyhow::anyhow!("recording has no supported video track"))?;
    anyhow::ensure!(
        track.timescale() > 0 && track.video_decoder_config()?.is_some(),
        "invalid recording decoder"
    );
    let samples = parsed.fragment_first_sample_locations(video_id)?;
    anyhow::ensure!(
        samples.len() == ranges.len() && parsed.moofs.len() == ranges.len(),
        "fragment index mismatch"
    );
    let fragments = indexed_fragments(&parsed, video_id, samples, ranges, deadline)?;
    Ok(Index {
        initialization,
        fragments,
    })
}

fn indexed_fragments<Reader: Read + Seek>(
    parsed: &mp4::Mp4Reader<Reader>,
    video_id: u32,
    samples: Vec<mp4::Mp4FragmentSampleLocation>,
    ranges: Vec<MediaRange>,
    deadline: Instant,
) -> anyhow::Result<Vec<Fragment>> {
    let mut sample_ids = std::collections::HashMap::with_capacity(parsed.tracks().len());
    let mut fragments = Vec::with_capacity(ranges.len());
    for ((range, sample), moof) in ranges.into_iter().zip(samples).zip(&parsed.moofs) {
        super::check_deadline(deadline)?;
        validate_extents(parsed, moof, &range, &mut sample_ids, deadline)?;
        let traf = moof
            .trafs
            .iter()
            .find(|traf| traf.tfhd.track_id == video_id)
            .ok_or_else(|| anyhow::anyhow!("recording fragment lacks video"))?;
        let trun = traf
            .trun
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing sample table"))?;
        let start = traf
            .tfdt
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing decode time"))?
            .base_media_decode_time;
        let duration = trun
            .sample_durations
            .iter()
            .try_fold(0_u64, |total, value| total.checked_add(u64::from(*value)))
            .ok_or_else(|| anyhow::anyhow!("sample duration overflow"))?;
        anyhow::ensure!(
            trun.sample_durations.len() == usize::try_from(trun.sample_count)? && duration > 0,
            "invalid sample timing"
        );
        anyhow::ensure!(
            sample.is_sync
                && sample.location.size > 0
                && sample.location.offset >= range.payload
                && sample
                    .location
                    .offset
                    .checked_add(u64::from(sample.location.size))
                    .is_some_and(|end| end <= range.range.offset + range.range.size),
            "invalid keyframe range"
        );
        let timescale = u64::from(parsed.tracks()[&video_id].timescale());
        fragments.push(Fragment {
            range: range.range,
            first_sample: sample,
            start_ms: start
                .checked_mul(1_000)
                .ok_or_else(|| anyhow::anyhow!("timestamp overflow"))?
                / timescale,
            duration_ms: (duration
                .checked_mul(1_000)
                .ok_or_else(|| anyhow::anyhow!("duration overflow"))?
                / timescale)
                .max(1),
        });
    }
    Ok(fragments)
}

fn validate_extents<Reader: Read + Seek>(
    parsed: &mp4::Mp4Reader<Reader>,
    moof: &mp4::MoofBox,
    range: &MediaRange,
    sample_ids: &mut std::collections::HashMap<u32, u32>,
    deadline: Instant,
) -> anyhow::Result<()> {
    anyhow::ensure!(moof.trafs.len() <= 2, "too many fragment tracks");
    let mut extents = Vec::new();
    for traf in &moof.trafs {
        let count = traf
            .trun
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing samples"))?
            .sample_count;
        anyhow::ensure!(count <= 65_536, "sample limit exceeded");
        let next = sample_ids.entry(traf.tfhd.track_id).or_insert(1);
        for _ in 0..count {
            super::check_deadline(deadline)?;
            let location = parsed
                .sample_location(traf.tfhd.track_id, *next)?
                .ok_or_else(|| anyhow::anyhow!("missing sample extent"))?;
            let end = location
                .offset
                .checked_add(u64::from(location.size))
                .ok_or_else(|| anyhow::anyhow!("sample extent overflow"))?;
            anyhow::ensure!(
                location.size > 0
                    && location.offset >= range.payload
                    && end <= range.range.offset + range.range.size,
                "sample outside media payload"
            );
            extents.push((location.offset, end));
            *next = next
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("sample ID overflow"))?;
        }
    }
    extents.sort_unstable();
    let mut next = range.payload;
    for (start, end) in extents {
        anyhow::ensure!(
            start == next,
            "media samples overlap or leave unindexed bytes"
        );
        next = end;
    }
    anyhow::ensure!(
        next == range.range.offset + range.range.size,
        "unindexed media payload"
    );
    Ok(())
}

fn scan(
    reader: &mut (impl Read + Seek),
    size: u64,
    deadline: Instant,
) -> anyhow::Result<(mp4::Mp4ByteRange, Vec<MediaRange>)> {
    let mut position = 0;
    let mut metadata_bytes = 0;
    let mut boxes = 0;
    let mut movie = false;
    let mut initialization = mp4::Mp4ByteRange { offset: 0, size: 0 };
    let mut fragment = None;
    let mut ranges = Vec::with_capacity(16);
    while position < size {
        super::check_deadline(deadline)?;
        boxes += 1;
        anyhow::ensure!(boxes <= BOX_COUNT_MAX, "recording box limit exceeded");
        let (kind, payload, end) = header(reader, position, size)?;
        if kind != mp4::BoxType::MdatBox {
            let bytes = end - position;
            metadata_bytes += bytes;
            anyhow::ensure!(
                bytes <= BOX_BYTES_MAX && metadata_bytes <= METADATA_BYTES_MAX,
                "recording metadata limit exceeded"
            );
        }
        match kind {
            mp4::BoxType::FtypBox => {
                anyhow::ensure!(position == 0 && end <= 4_096, "invalid file type position");
            }
            mp4::BoxType::MoovBox => {
                anyhow::ensure!(
                    !movie && ranges.is_empty() && fragment.is_none() && end - position <= 65_536,
                    "invalid movie metadata"
                );
                movie = true;
            }
            mp4::BoxType::FreeBox => anyhow::ensure!(
                movie && fragment.is_none() && ranges.is_empty(),
                "unexpected free box"
            ),
            mp4::BoxType::MoofBox => {
                anyhow::ensure!(
                    movie && fragment.is_none() && ranges.len() < FRAGMENT_COUNT_MAX,
                    "invalid fragment sequence"
                );
                if initialization.size == 0 {
                    initialization.size = position;
                }
                guard_samples(reader, payload, end, deadline)?;
                fragment = Some(position);
            }
            mp4::BoxType::MdatBox => {
                ranges.push(media_range(fragment.take(), payload, end)?);
            }
            mp4::BoxType::EmsgBox => {
                anyhow::ensure!(movie && end - position <= 65_536, "invalid event metadata");
            }
            _ => anyhow::bail!("unsupported recording box"),
        }
        position = end;
    }
    anyhow::ensure!(
        movie && !ranges.is_empty() && fragment.is_none(),
        "incomplete recording"
    );
    Ok((initialization, ranges))
}

fn media_range(start: Option<u64>, payload: u64, end: u64) -> anyhow::Result<MediaRange> {
    let start = start.ok_or_else(|| anyhow::anyhow!("unindexed media data"))?;
    anyhow::ensure!(end > payload, "empty media payload");
    Ok(MediaRange {
        range: mp4::Mp4ByteRange {
            offset: start,
            size: end - start,
        },
        payload,
    })
}

fn header(
    reader: &mut (impl Read + Seek),
    start: u64,
    limit: u64,
) -> anyhow::Result<(mp4::BoxType, u64, u64)> {
    anyhow::ensure!(limit.saturating_sub(start) >= 8, "truncated recording box");
    reader.seek(SeekFrom::Start(start))?;
    let header = mp4::BoxHeader::read(reader)?;
    let payload = reader.stream_position()?;
    anyhow::ensure!(header.size >= 8, "zero-length recording box");
    let end = payload
        .checked_add(header.size - 8)
        .ok_or_else(|| anyhow::anyhow!("recording box overflow"))?;
    anyhow::ensure!(end <= limit && end > start, "recording box escapes parent");
    Ok((header.name, payload, end))
}

fn guard_samples(
    reader: &mut (impl Read + Seek),
    start: u64,
    end: u64,
    deadline: Instant,
) -> anyhow::Result<()> {
    let mut pending = vec![(start, end, false)];
    let mut count = 0;
    let mut track_ids = Vec::with_capacity(2);
    while let Some((mut position, limit, nested)) = pending.pop() {
        while position < limit {
            super::check_deadline(deadline)?;
            count += 1;
            anyhow::ensure!(count <= 128, "fragment box limit exceeded");
            let (kind, payload, end) = header(reader, position, limit)?;
            if kind == mp4::BoxType::TrafBox {
                anyhow::ensure!(
                    !nested && pending.len() < 2,
                    "invalid track fragment nesting"
                );
                pending.push((payload, end, true));
            } else if kind == mp4::BoxType::TfdtBox {
                anyhow::ensure!(nested, "decode time must belong to a track");
                guard_timestamp(reader, end - payload)?;
            } else if kind == mp4::BoxType::TfhdBox {
                anyhow::ensure!(nested && end - payload >= 8, "invalid track header");
                let mut fields = [0_u8; 8];
                reader.read_exact(&mut fields)?;
                let flags = u32::from_be_bytes(fields[..4].try_into()?);
                let track_id = u32::from_be_bytes(fields[4..].try_into()?);
                anyhow::ensure!(
                    track_id > 0 && track_ids.len() < 2 && !track_ids.contains(&track_id),
                    "duplicate or invalid fragment track"
                );
                track_ids.push(track_id);
                anyhow::ensure!(
                    flags & 1 == 0 && flags & 0x020000 != 0,
                    "recording requires moof-relative offsets"
                );
            } else if kind == mp4::BoxType::TrunBox {
                anyhow::ensure!(nested && end - payload >= 8, "invalid sample table");
                let mut fields = [0_u8; 8];
                reader.read_exact(&mut fields)?;
                let flags = u32::from_be_bytes(fields[..4].try_into()?);
                let samples = u32::from_be_bytes(fields[4..].try_into()?);
                anyhow::ensure!(
                    flags & 0x301 == 0x301 && (1..=65_536).contains(&samples),
                    "unsupported sample table"
                );
                anyhow::ensure!(end - payload >= 12, "missing media offset");
                let mut offset = [0_u8; 4];
                reader.read_exact(&mut offset)?;
                anyhow::ensure!(
                    i32::from_be_bytes(offset) > 0,
                    "media offset must follow fragment headers"
                );
            }
            position = end;
        }
    }
    Ok(())
}

fn guard_timestamp(reader: &mut impl Read, size: u64) -> anyhow::Result<()> {
    anyhow::ensure!(matches!(size, 8 | 12), "invalid decode time");
    let mut fields = [0_u8; 12];
    reader.read_exact(&mut fields[..usize::try_from(size)?])?;
    let timestamp = match fields[0] {
        0 if size == 8 => u64::from(u32::from_be_bytes(fields[4..8].try_into()?)),
        1 if size == 12 => u64::from_be_bytes(fields[4..12].try_into()?),
        _ => anyhow::bail!("invalid decode time version"),
    };
    anyhow::ensure!(
        timestamp <= u64::MAX / 2_000,
        "decode time exceeds recording limit"
    );
    Ok(())
}

struct Bounded<Reader> {
    reader: Reader,
    remaining: u64,
    size: u64,
    deadline: Instant,
}

impl<Reader: Read> Read for Bounded<Reader> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        super::check_deadline(self.deadline)?;
        if u64::try_from(bytes.len()).map_err(io::Error::other)? > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording metadata read limit exceeded",
            ));
        }
        let read = self.reader.read(bytes)?;
        self.remaining -= u64::try_from(read).map_err(io::Error::other)?;
        Ok(read)
    }
}

impl<Reader: Seek> Seek for Bounded<Reader> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        super::check_deadline(self.deadline)?;
        let offset = self.reader.seek(position)?;
        if offset > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recording seek exceeds file",
            ));
        }
        Ok(offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Cursor, time::Duration};

    fn recording() -> Vec<u8> {
        let config = mp4::Mp4Config {
            major_brand: "iso6".parse().unwrap(),
            minor_version: 1,
            compatible_brands: vec!["iso6".parse().unwrap(), "mp41".parse().unwrap()],
            timescale: 1_000,
        };
        let track = mp4::TrackConfig {
            track_type: mp4::TrackType::Video,
            timescale: 90_000,
            language: "und".to_owned(),
            media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
                width: 320,
                height: 240,
                seq_param_set: vec![0x67, 0x42, 0, 0x1e, 0xe9, 1, 0x40, 0x7b, 0x20],
                pic_param_set: vec![0x68, 0xce, 6, 0xe2],
            }),
        };
        let mut writer =
            mp4::FragmentedMp4Writer::write_start(Cursor::new(Vec::new()), &config, &[track])
                .unwrap();
        writer
            .write_sample(
                1,
                mp4::Mp4Sample {
                    start_time: 0,
                    duration: 90_000,
                    rendering_offset: 0,
                    is_sync: true,
                    bytes: bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65]),
                },
            )
            .unwrap();
        writer.write_end().unwrap();
        writer.into_writer().into_inner()
    }

    #[test]
    fn rejects_duplicate_track_runs_before_shared_parsing() {
        let mut bytes = recording();
        let moof = bytes.windows(4).position(|value| value == b"moof").unwrap() - 4;
        let traf = bytes.windows(4).position(|value| value == b"traf").unwrap() - 4;
        let moof_size = u32::from_be_bytes(bytes[moof..moof + 4].try_into().unwrap());
        let traf_size = u32::from_be_bytes(bytes[traf..traf + 4].try_into().unwrap());
        let duplicate = bytes[traf..traf + traf_size as usize].to_vec();
        let end = moof + moof_size as usize;
        bytes.splice(end..end, duplicate);
        bytes[moof..moof + 4].copy_from_slice(&(moof_size + traf_size).to_be_bytes());
        let deadline = Instant::now() + Duration::from_secs(2);

        assert!(scan(&mut Cursor::new(&bytes), bytes.len() as u64, deadline).is_err());
    }

    #[test]
    fn rejects_overflowing_decode_time_without_panicking() {
        let mut bytes = recording();
        let timestamp = bytes.windows(4).position(|value| value == b"tfdt").unwrap();
        assert_eq!(bytes[timestamp + 4], 1);
        bytes[timestamp + 8..timestamp + 16].copy_from_slice(&u64::MAX.to_be_bytes());
        let deadline = Instant::now() + Duration::from_secs(2);
        assert!(inspect(&mut Cursor::new(&bytes), bytes.len() as u64, deadline).is_err());
    }

    #[test]
    fn rejects_samples_that_point_into_box_headers() {
        let mut bytes = recording();
        let trun = bytes.windows(4).position(|value| value == b"trun").unwrap();
        bytes[trun + 12..trun + 16].copy_from_slice(&0_i32.to_be_bytes());
        let deadline = Instant::now() + Duration::from_secs(2);
        assert!(inspect(&mut Cursor::new(&bytes), bytes.len() as u64, deadline).is_err());
    }

    #[test]
    fn validates_fragmented_recordings_and_rejects_corrupt_boundaries() {
        let bytes = recording();
        let deadline = Instant::now() + Duration::from_secs(2);
        let valid = inspect(&mut Cursor::new(&bytes), bytes.len() as u64, deadline).unwrap();
        assert_eq!(valid.fragments.len(), 1);
        assert!(valid.initialization.size >= 8_192);
        for length in [0, 7, 32, bytes.len() - 1] {
            assert!(inspect(&mut Cursor::new(&bytes[..length]), length as u64, deadline).is_err());
        }
        let mut trailing = bytes.clone();
        trailing.extend([0; 8]);
        assert!(inspect(&mut Cursor::new(&trailing), trailing.len() as u64, deadline).is_err());
        let mut oversized = bytes;
        let trun = oversized
            .windows(4)
            .position(|bytes| bytes == b"trun")
            .unwrap();
        oversized[trun + 4..trun + 8].fill(0);
        oversized[trun + 8..trun + 12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(
            inspect(
                &mut Cursor::new(&oversized),
                oversized.len() as u64,
                deadline
            )
            .is_err()
        );
    }
}
