//! Optional KeepPeek UUID box inside a single fragmented-MP4 track run.
//! All integers use network byte order. Sample indices in this box start at zero.

use super::*;
use std::num::NonZeroU32;

const USER_TYPE: [u8; 16] = [
    0xb6, 0x6e, 0x60, 0x19, 0x3c, 0xb1, 0x49, 0x9b, 0xae, 0xcc, 0xaf, 0x96, 0xaa, 0xc8, 0x98, 0x94,
];
const VERSION: u32 = 1;
const RUN_BYTES: u64 = 4 + 4 + 16 + 4 + 8 + 4 + 8 + 8;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SourceRtpPacket {
    /// Opaque RTSP PLAY attempt, shared by the audio and video tracks of that session.
    pub session_id: [u8; 16],
    /// The source RTP clock rate, independent of the MP4 track timescale.
    pub clock_rate: NonZeroU32,
    /// Original packet timestamp with RTP wraps extended. Its low 32 bits are the wire value.
    pub timestamp: i64,
    /// First access unit after a known source start or packet-loss discontinuity.
    pub discontinuity: bool,
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

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SourceRtpTime {
    /// Calibration at the first accepted sample of this uninterrupted recording epoch.
    pub anchor: SourceRtpAnchor,
    /// Opaque RTSP PLAY attempt, shared by the audio and video tracks of that session.
    pub session_id: [u8; 16],
    /// The source RTP clock rate, independent of the MP4 track timescale.
    pub clock_rate: NonZeroU32,
    /// Original packet timestamp with RTP wraps extended. Its low 32 bits are the wire value.
    pub timestamp: i64,
    /// First access unit after a known source start or packet-loss discontinuity.
    pub discontinuity: bool,
}

impl SourceRtpPacket {
    pub(crate) fn recorded(self, previous: Option<SourceRtpTime>, pts: i64) -> SourceRtpTime {
        let continued = previous.filter(|v| {
            !self.discontinuity
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
pub(crate) struct SourceTiming(Vec<Run>);

impl SourceTiming {
    pub(crate) fn add(&mut self, index: u32, value: SourceRtpTime) {
        if let Some(run) = self.0.last_mut()
            && run.end() == u64::from(index)
            && !value.discontinuity
            && value.anchor == run.first_time.anchor
            && value.session_id == run.first_time.session_id
            && value.clock_rate == run.first_time.clock_rate
            && let Some(delta) = value
                .timestamp
                .checked_sub(run.first_time.timestamp)
                .and_then(|v| i32::try_from(v).ok())
        {
            run.deltas.push(delta);
        } else {
            self.0.push(Run {
                first: index,
                first_time: value,
                deltas: Vec::new(),
            });
        }
    }

    pub(crate) fn sample(&self, index: usize) -> Option<SourceRtpTime> {
        let after = self.0.partition_point(|run| (run.first as usize) <= index);
        let run = self.0.get(after.checked_sub(1)?)?;
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

    pub(super) fn size(&self) -> u64 {
        if self.0.is_empty() {
            return 0;
        }
        HEADER_SIZE
            + 16
            + 4
            + self
                .0
                .iter()
                .map(|r| RUN_BYTES + r.deltas.len() as u64 * 4)
                .sum::<u64>()
    }

    pub(super) fn validate_count(&self, count: u32) -> Result<()> {
        if self.0.last().is_some_and(|r| r.end() > u64::from(count)) {
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
        for run in &self.0 {
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
        Ok(Some(Self(runs)))
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
    fn malformed_metadata_never_produces_a_clock() {
        let source = SourceRtpTime {
            anchor: SourceRtpAnchor {
                timestamp: i64::MAX,
                presentation_time: -17,
            },
            session_id: [4; 16],
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
        for (start, value) in [(32, 0_u32), (32, u32::MAX), (52, 0), (64, 2), (84, 1)] {
            let mut invalid = bytes.clone();
            invalid[start..start + 4].copy_from_slice(&value.to_be_bytes());
            assert!(parse(invalid).is_err());
        }
        bytes.pop();
        assert!(parse(bytes).is_err());
    }
}
