//! Optional KeepPeek UUID payload inside a standard fragmented-MP4 event message.
//! All integers use network byte order. Sample indices in this box start at zero.

use super::*;
use std::num::NonZeroU32;

pub(crate) const USER_TYPE: [u8; 16] = [
    0xb6, 0x6e, 0x60, 0x19, 0x3c, 0xb1, 0x49, 0x9b, 0xae, 0xcc, 0xaf, 0x96, 0xaa, 0xc8, 0x98, 0x94,
];
const VERSION: u32 = 1;
const RUN_BYTES: u64 = 4 + 4 + 16 + 4 + 8 + 4 + 8 + 8 + 16;
pub(crate) const SCHEME: &str = "urn:keeppeek:original-rtp";
// 512 single-sample runs at 72 bytes each fit in a 37 KiB event, including both headers.
// Chunking bounds the parser allocation even when every packet starts a fresh source epoch.
const EVENT_SAMPLES_MAX: usize = 512;
const UUID_HEADER_BYTES: u64 = HEADER_SIZE + 16 + 4;
// Per chunk: version-1 emsg header + fixed fields (32), scheme + NUL, decimal
// u32 track ID + NUL (at most 11), and the 28-byte UUID header duplicated when
// splitting a track across events. Their sum is below 100 bytes; 192 reserves
// headroom without depending on a particular track ID or first-chunk discount.
const EVENT_WRAPPER_BYTES_MAX: u64 = 192;
pub(crate) const NEXT_SAMPLE_BYTES_MAX: u64 =
    UUID_HEADER_BYTES + RUN_BYTES + EVENT_WRAPPER_BYTES_MAX;

/// Native RTP clock evidence attached to one accepted recording access unit.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SourceRtpPacket {
    /// Opaque RTSP PLAY attempt, shared by the audio and video tracks of that session.
    pub session_id: [u8; 16],
    /// Opaque identity of one uninterrupted RTP clock on one negotiated source track.
    pub epoch_id: [u8; 16],
    /// The source RTP clock rate, independent of the MP4 track timescale.
    pub clock_rate: NonZeroU32,
    /// Original packet timestamp with RTP wraps extended. Its low 32 bits are the wire value.
    pub timestamp: i64,
}

/// Stable calibration for one uninterrupted source epoch in this recording and track.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SourceRtpAnchor {
    /// Original extended RTP timestamp of the first accepted stored unit of this epoch.
    pub timestamp: i64,
    /// Its signed MP4 presentation time, in this track's MP4 timescale.
    pub presentation_time: i64,
}

/// Persisted source reading and its stable track-relative calibration.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SourceRtpTime {
    /// Calibration at the first accepted sample of this uninterrupted recording epoch.
    pub anchor: SourceRtpAnchor,
    /// Opaque RTSP PLAY attempt, shared by the audio and video tracks of that session.
    pub session_id: [u8; 16],
    /// Opaque identity of one uninterrupted RTP clock on one negotiated source track.
    pub epoch_id: [u8; 16],
    /// The source RTP clock rate, independent of the MP4 track timescale.
    pub clock_rate: NonZeroU32,
    /// Original packet timestamp with RTP wraps extended. Its low 32 bits are the wire value.
    pub timestamp: i64,
    /// First stored unit of this recording epoch, including after a loss or gap in evidence.
    pub discontinuity: bool,
}

impl SourceRtpPacket {
    pub(crate) fn recorded(self, previous: Option<SourceRtpTime>, pts: i64) -> SourceRtpTime {
        let continued = previous.filter(|v| {
            self.epoch_id == v.epoch_id
                && self.session_id == v.session_id
                && self.clock_rate == v.clock_rate
        });
        let anchor = continued.map_or(
            SourceRtpAnchor {
                timestamp: self.timestamp,
                presentation_time: pts,
            },
            |v| v.anchor,
        );
        SourceRtpTime {
            anchor,
            session_id: self.session_id,
            epoch_id: self.epoch_id,
            clock_rate: self.clock_rate,
            timestamp: self.timestamp,
            discontinuity: continued.is_none(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
struct Run {
    first: u32,
    first_time: SourceRtpTime,
    // The anchor describes the first unit. Each following delta is relative to the anchor.
    deltas: Vec<i32>,
}

impl Run {
    fn end(&self) -> u64 {
        u64::from(self.first) + 1 + self.deltas.len() as u64
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub(crate) struct SourceTiming {
    runs: Vec<Run>,
    samples: u64,
}

impl SourceTiming {
    pub(crate) fn add(&mut self, index: u32, value: SourceRtpTime) {
        // Use the same run origins in memory and in each independently serialized event.
        // After a split, two individually representable deltas can differ by more than i32.
        let starts_event = self.samples.is_multiple_of(EVENT_SAMPLES_MAX as u64);
        self.samples += 1;
        if !starts_event
            && let Some(run) = self.runs.last_mut()
            && run.end() == u64::from(index)
            && !value.discontinuity
            && value.anchor == run.first_time.anchor
            && value.epoch_id == run.first_time.epoch_id
            && value.session_id == run.first_time.session_id
            && value.clock_rate == run.first_time.clock_rate
            && let Some(delta) = value
                .timestamp
                .checked_sub(run.first_time.timestamp)
                .and_then(|v| i32::try_from(v).ok())
        {
            run.deltas.push(delta);
        } else {
            self.runs.push(Run {
                first: index,
                first_time: value,
                deltas: Vec::new(),
            });
        }
    }

    pub(crate) fn sample(&self, index: usize) -> Option<SourceRtpTime> {
        let after = self
            .runs
            .partition_point(|run| (run.first as usize) <= index);
        let run = self.runs.get(after.checked_sub(1)?)?;
        let offset = index - run.first as usize;
        if offset == 0 {
            return Some(run.first_time);
        }
        Some(SourceRtpTime {
            timestamp: run
                .first_time
                .timestamp
                .checked_add(i64::from(*run.deltas.get(offset - 1)?))?,
            discontinuity: false,
            ..run.first_time
        })
    }

    pub(crate) const fn event_budget(&self) -> (u64, u64) {
        let chunks = self.samples.div_ceil(EVENT_SAMPLES_MAX as u64);
        // The stored runs already include each chunk origin. Reserve its emsg and UUID headers.
        (self.size() + chunks * EVENT_WRAPPER_BYTES_MAX, chunks)
    }

    pub(crate) fn event_bytes(&self, track_id: u32) -> u64 {
        let chunks = self.samples.div_ceil(EVENT_SAMPLES_MAX as u64);
        if chunks == 0 {
            return 0;
        }
        // size() already includes one UUID header and the runs start fresh at each event.
        let wrapper = EmsgBox::size_without_message(1, SCHEME, &track_id.to_string());
        self.size() + (chunks - 1) * UUID_HEADER_BYTES + chunks * wrapper
    }

    /// One bounded ISO event can contain 512 one-sample runs even in the worst case.
    pub(crate) fn events(
        &self,
        track_id: u32,
        seq: u32,
        rate: u32,
        pts: u64,
    ) -> Result<Vec<EmsgBox>> {
        let mut events = Vec::new();
        let mut chunk = Self::default();
        let mut count = 0;
        for run in &self.runs {
            for i in 0..=run.deltas.len() {
                let index = run.first
                    + u32::try_from(i)
                        .map_err(|_| Error::InvalidData("source RTP ordinal overflow"))?;
                let value = if i == 0 {
                    run.first_time
                } else {
                    SourceRtpTime {
                        timestamp: run.first_time.timestamp + i64::from(run.deltas[i - 1]),
                        discontinuity: false,
                        ..run.first_time
                    }
                };
                chunk.add(index, value);
                count += 1;
                if count == EVENT_SAMPLES_MAX {
                    events.push(chunk.event(track_id, seq, rate, pts)?);
                    chunk = Self::default();
                    count = 0;
                }
            }
        }
        if count != 0 {
            events.push(chunk.event(track_id, seq, rate, pts)?);
        }
        Ok(events)
    }

    fn event(&self, track_id: u32, seq: u32, rate: u32, pts: u64) -> Result<EmsgBox> {
        let mut message_data = Vec::with_capacity(self.size() as usize);
        self.write(&mut message_data)?;
        Ok(EmsgBox {
            version: 1,
            flags: 0,
            timescale: rate,
            presentation_time: Some(pts),
            presentation_time_delta: None,
            event_duration: 0,
            id: seq,
            scheme_id_uri: SCHEME.to_owned(),
            value: track_id.to_string(),
            message_data,
        })
    }

    /// The event belongs to the immediately preceding moof and names its track and sequence.
    pub(crate) fn attach(moof: &mut MoofBox, event: &EmsgBox) -> Result<()> {
        if event.id != moof.mfhd.sequence_number {
            return Err(Error::InvalidData("source RTP event sequence mismatch"));
        }
        let track = event
            .value
            .parse::<u32>()
            .map_err(|_| Error::InvalidData("invalid source RTP event track"))?;
        let traf = moof
            .trafs
            .iter_mut()
            .find(|t| t.tfhd.track_id == track)
            .ok_or(Error::TrakNotFound(track))?;
        let mut reader = std::io::Cursor::new(&event.message_data);
        let header = BoxHeader::read(&mut reader)?;
        if header.name != BoxType::UuidBox || header.size != event.message_data.len() as u64 {
            return Err(Error::InvalidData("invalid source RTP event payload"));
        }
        if let Some(mut timing) = Self::read(&mut reader, header.size)? {
            timing.validate_count(traf.trun.as_ref().map_or(0, |t| t.sample_count))?;
            if let Some(previous) = traf.source_timing.runs.last()
                && previous.end() > u64::from(timing.runs[0].first)
            {
                return Err(Error::InvalidData("overlapping source RTP event chunks"));
            }
            traf.source_timing.samples += timing.samples;
            traf.source_timing.runs.append(&mut timing.runs);
        }
        Ok(())
    }

    pub(super) const fn size(&self) -> u64 {
        if self.runs.is_empty() {
            return 0;
        }
        UUID_HEADER_BYTES
            + self.runs.len() as u64 * RUN_BYTES
            + (self.samples - self.runs.len() as u64) * 4
    }

    pub(super) fn validate_count(&self, count: u32) -> Result<()> {
        if self.runs.last().is_some_and(|r| r.end() > u64::from(count)) {
            return Err(Error::InvalidData(
                "source RTP run exceeds track sample count",
            ));
        }
        Ok(())
    }

    pub(super) fn write<W: Write>(&self, writer: &mut W) -> Result<()> {
        let size = self.size();
        if size == 0 {
            return Ok(());
        }
        BoxHeader::new(BoxType::UuidBox, size).write(writer)?;
        writer.write_all(&USER_TYPE)?;
        writer.write_u32::<BigEndian>(VERSION)?;
        for run in &self.runs {
            writer.write_u32::<BigEndian>(run.first)?;
            writer.write_u32::<BigEndian>(
                u32::try_from(1 + run.deltas.len())
                    .map_err(|_| Error::InvalidData("source RTP run too long"))?,
            )?;
            writer.write_all(&run.first_time.session_id)?;
            writer.write_u32::<BigEndian>(run.first_time.clock_rate.get())?;
            writer.write_i64::<BigEndian>(run.first_time.timestamp)?;
            writer.write_u32::<BigEndian>(u32::from(run.first_time.discontinuity))?;
            writer.write_i64::<BigEndian>(run.first_time.anchor.timestamp)?;
            writer.write_i64::<BigEndian>(run.first_time.anchor.presentation_time)?;
            writer.write_all(&run.first_time.epoch_id)?;
            for &delta in &run.deltas {
                writer.write_i32::<BigEndian>(delta)?;
            }
        }
        Ok(())
    }

    pub(super) fn read<R: Read + Seek>(reader: &mut R, size: u64) -> Result<Option<Self>> {
        let end = checked_box_end_with_min(reader, size, HEADER_SIZE + 16)?;
        let mut uuid = [0; 16];
        reader.read_exact(&mut uuid)?;
        if uuid != USER_TYPE {
            skip_bytes_to(reader, end)?;
            return Ok(None);
        }
        ensure_box_bytes_remaining(reader, end, 4)?;
        if reader.read_u32::<BigEndian>()? != VERSION {
            skip_bytes_to(reader, end)?;
            return Ok(None);
        }
        let mut runs = Vec::<Run>::new();
        while reader.stream_position()? < end {
            ensure_box_bytes_remaining(reader, end, RUN_BYTES)?;
            let first = reader.read_u32::<BigEndian>()?;
            let count = reader.read_u32::<BigEndian>()?;
            if count == 0 || runs.last().is_some_and(|r| r.end() > u64::from(first)) {
                return Err(Error::InvalidData("empty or overlapping source RTP run"));
            }
            let mut session_id = [0; 16];
            reader.read_exact(&mut session_id)?;
            let clock_rate = NonZeroU32::new(reader.read_u32::<BigEndian>()?)
                .ok_or(Error::InvalidData("zero source RTP clock rate"))?;
            let timestamp = reader.read_i64::<BigEndian>()?;
            let discontinuity = match reader.read_u32::<BigEndian>()? {
                0 => false,
                1 => true,
                _ => return Err(Error::InvalidData("unrecognized source RTP run flags")),
            };
            let anchor = SourceRtpAnchor {
                timestamp: reader.read_i64::<BigEndian>()?,
                presentation_time: reader.read_i64::<BigEndian>()?,
            };
            if discontinuity && timestamp != anchor.timestamp {
                return Err(Error::InvalidData(
                    "source RTP discontinuity disagrees with its epoch anchor",
                ));
            }
            let mut epoch_id = [0; 16];
            reader.read_exact(&mut epoch_id)?;
            ensure_box_bytes_remaining(reader, end, u64::from(count - 1) * 4)?;
            let mut deltas = Vec::new();
            for _ in 1..count {
                let delta = reader.read_i32::<BigEndian>()?;
                timestamp
                    .checked_add(i64::from(delta))
                    .ok_or(Error::InvalidData("source RTP delta overflows clock"))?;
                deltas.push(delta);
            }
            runs.push(Run {
                first,
                first_time: SourceRtpTime {
                    anchor,
                    session_id,
                    epoch_id,
                    clock_rate,
                    timestamp,
                    discontinuity,
                },
                deltas,
            });
        }
        if runs.is_empty() {
            return Err(Error::InvalidData("empty source RTP timing box"));
        }
        let samples = runs.iter().map(|r| 1 + r.deltas.len() as u64).sum();
        Ok(Some(Self { samples, runs }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn parse(bytes: Vec<u8>) -> Result<Option<SourceTiming>> {
        let mut reader = Cursor::new(bytes);
        let header = BoxHeader::read(&mut reader)?;
        SourceTiming::read(&mut reader, header.size)
    }

    #[test]
    fn event_budget_includes_runs_reanchored_at_each_event_boundary() {
        let packet = SourceRtpPacket {
            session_id: [1; 16],
            epoch_id: [2; 16],
            timestamp: 0,
            clock_rate: 90_000.try_into().unwrap(),
        };
        let timestamps = [i64::from(i32::MAX), 0, -2, 0];
        let mut timing = SourceTiming::default();
        let mut previous = None;
        for index in 0..1_538 {
            let timestamp = if index < 2 {
                0
            } else {
                timestamps[((index - 2) % 4) as usize]
            };
            let clock = SourceRtpPacket {
                timestamp,
                ..packet
            }
            .recorded(previous, i64::from(index));
            timing.add(index, clock);
            previous = Some(clock);
        }
        let events = timing.events(1, 1, 90_000, 0).unwrap();
        let (byte_bound, box_count) = timing.event_budget();
        assert_eq!(events.len() as u64, box_count);
        assert!(events.iter().map(Mp4Box::box_size).sum::<u64>() <= byte_bound);
    }

    #[test]
    fn malformed_metadata_never_produces_a_clock() {
        let source = SourceRtpTime {
            anchor: SourceRtpAnchor {
                timestamp: i64::MAX,
                presentation_time: -17,
            },
            session_id: [4; 16],
            epoch_id: [6; 16],
            clock_rate: 90_000.try_into().unwrap(),
            timestamp: i64::MAX,
            discontinuity: false,
        };
        let mut timing = SourceTiming::default();
        timing.add(0, source);
        timing.add(
            1,
            SourceRtpTime {
                timestamp: i64::MAX - 4,
                ..source
            },
        );
        let mut bytes = Vec::new();
        timing.write(&mut bytes).unwrap();
        assert_eq!(parse(bytes.clone()).unwrap(), Some(timing.clone()));
        assert!(timing.validate_count(1).is_err());
        assert!(timing.validate_count(2).is_ok());
        let mut unknown = bytes.clone();
        unknown[8] ^= 1;
        assert_eq!(parse(unknown).unwrap(), None);
        let mut version = bytes.clone();
        version[27] = 2;
        assert_eq!(parse(version).unwrap(), None);
        for (start, value) in [(32, 0_u32), (32, u32::MAX), (52, 0), (64, 2), (100, 1)] {
            let mut invalid = bytes.clone();
            invalid[start..start + 4].copy_from_slice(&value.to_be_bytes());
            assert!(parse(invalid).is_err());
        }
        bytes.pop();
        assert!(parse(bytes).is_err());
    }
}
