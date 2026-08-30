use crate::mp4box::{avc1::Avc1Box, hev1::Hev1Box, mp4a::Mp4aBox, tx3g::Tx3gBox, vp09::Vp09Box, *};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum SampleEntry {
    Avc1(Avc1Box),
    Hev1(Hev1Box),
    Hvc1(Hev1Box),
    Vp09(Vp09Box),
    Mp4a(Mp4aBox),
    Tx3g(Tx3gBox),
    Unknown { box_type: u32, data: Vec<u8> },
}

impl SampleEntry {
    fn box_size(&self) -> u64 {
        match self {
            Self::Avc1(entry) => entry.box_size(),
            Self::Hev1(entry) | Self::Hvc1(entry) => entry.box_size(),
            Self::Vp09(entry) => entry.box_size(),
            Self::Mp4a(entry) => entry.box_size(),
            Self::Tx3g(entry) => entry.box_size(),
            Self::Unknown { data, .. } => HEADER_SIZE + data.len() as u64,
        }
    }

    fn write_box<W: Write>(&self, writer: &mut W) -> Result<u64> {
        match self {
            Self::Avc1(entry) => entry.write_box(writer),
            Self::Hev1(entry) => entry.write_box(writer),
            Self::Hvc1(entry) => entry.write_box_with_type(writer, BoxType::Hvc1Box),
            Self::Vp09(entry) => entry.write_box(writer),
            Self::Mp4a(entry) => entry.write_box(writer),
            Self::Tx3g(entry) => entry.write_box(writer),
            Self::Unknown { box_type, data } => {
                let size = HEADER_SIZE + data.len() as u64;
                BoxHeader::new(BoxType::from(*box_type), size).write(writer)?;
                writer.write_all(data)?;
                Ok(size)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct StsdBox {
    pub version: u8,
    pub flags: u32,
    pub entries: Vec<SampleEntry>,
}

impl StsdBox {
    pub const fn get_type(&self) -> BoxType {
        BoxType::StsdBox
    }

    pub fn get_size(&self) -> u64 {
        HEADER_SIZE
            + HEADER_EXT_SIZE
            + 4
            + self.entries.iter().map(SampleEntry::box_size).sum::<u64>()
    }

    pub fn entry(&self, index: u32) -> Option<&SampleEntry> {
        index
            .checked_sub(1)
            .and_then(|index| self.entries.get(index as usize))
    }

    pub fn avc1(&self) -> Option<&Avc1Box> {
        self.entries.iter().find_map(|entry| match entry {
            SampleEntry::Avc1(entry) => Some(entry),
            _ => None,
        })
    }

    pub fn hev1(&self) -> Option<&Hev1Box> {
        self.entries.iter().find_map(|entry| match entry {
            SampleEntry::Hev1(entry) => Some(entry),
            _ => None,
        })
    }

    pub fn hvc1(&self) -> Option<&Hev1Box> {
        self.entries.iter().find_map(|entry| match entry {
            SampleEntry::Hvc1(entry) => Some(entry),
            _ => None,
        })
    }

    pub fn vp09(&self) -> Option<&Vp09Box> {
        self.entries.iter().find_map(|entry| match entry {
            SampleEntry::Vp09(entry) => Some(entry),
            _ => None,
        })
    }

    pub fn mp4a(&self) -> Option<&Mp4aBox> {
        self.entries.iter().find_map(|entry| match entry {
            SampleEntry::Mp4a(entry) => Some(entry),
            _ => None,
        })
    }

    pub fn mp4a_mut(&mut self) -> Option<&mut Mp4aBox> {
        self.entries.iter_mut().find_map(|entry| match entry {
            SampleEntry::Mp4a(entry) => Some(entry),
            _ => None,
        })
    }

    pub fn tx3g(&self) -> Option<&Tx3gBox> {
        self.entries.iter().find_map(|entry| match entry {
            SampleEntry::Tx3g(entry) => Some(entry),
            _ => None,
        })
    }
}

impl Mp4Box for StsdBox {
    fn box_type(&self) -> BoxType {
        self.get_type()
    }

    fn box_size(&self) -> u64 {
        self.get_size()
    }

    #[cfg(feature = "serde")]
    fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(&self).unwrap())
    }

    fn summary(&self) -> Result<String> {
        Ok(format!("entry_count={}", self.entries.len()))
    }
}

impl<R: Read + Seek> ReadBox<&mut R> for StsdBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let end = checked_box_end_with_min(reader, size, HEADER_SIZE + HEADER_EXT_SIZE + 4)?;
        let (version, flags) = read_box_header_ext(reader)?;
        let entry_count = reader.read_u32::<BigEndian>()?;
        let entry_bytes = size
            .checked_sub(HEADER_SIZE + HEADER_EXT_SIZE + 4)
            .ok_or(Error::InvalidData("invalid stsd box size"))?;
        if u64::from(entry_count) > entry_bytes / HEADER_SIZE {
            return Err(Error::InvalidData("stsd entry count exceeds box size"));
        }
        let mut entries = Vec::with_capacity(entry_count as usize);
        for _ in 0..entry_count {
            let BoxHeader {
                name,
                size: entry_size,
            } = read_box_header(reader, end)?;
            if checked_box_end(reader, entry_size)? > end {
                return Err(Error::InvalidData("invalid stsd sample entry size"));
            }
            let entry = match name {
                BoxType::Avc1Box => SampleEntry::Avc1(Avc1Box::read_box(reader, entry_size)?),
                BoxType::Hev1Box => SampleEntry::Hev1(Hev1Box::read_box(reader, entry_size)?),
                BoxType::Hvc1Box => SampleEntry::Hvc1(Hev1Box::read_box(reader, entry_size)?),
                BoxType::Vp09Box => SampleEntry::Vp09(Vp09Box::read_box(reader, entry_size)?),
                BoxType::Mp4aBox => SampleEntry::Mp4a(Mp4aBox::read_box(reader, entry_size)?),
                BoxType::Tx3gBox => SampleEntry::Tx3g(Tx3gBox::read_box(reader, entry_size)?),
                _ => {
                    let data_len = usize::try_from(entry_size - HEADER_SIZE)
                        .map_err(|_| Error::InvalidData("stsd sample entry is too large"))?;
                    let mut data = vec![0; data_len];
                    reader.read_exact(&mut data)?;
                    SampleEntry::Unknown {
                        box_type: name.into(),
                        data,
                    }
                }
            };
            entries.push(entry);
        }
        skip_bytes_to(reader, end)?;
        Ok(Self {
            version,
            flags,
            entries,
        })
    }
}

impl<W: Write> WriteBox<&mut W> for StsdBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;

        write_box_header_ext(writer, self.version, self.flags)?;
        writer.write_u32::<BigEndian>(
            self.entries
                .len()
                .try_into()
                .map_err(|_| Error::InvalidData("too many stsd sample entries"))?,
        )?;
        for entry in &self.entries {
            entry.write_box(writer)?;
        }
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn multiple_video_sample_entries_round_trip_in_order() {
        let source = StsdBox {
            entries: vec![
                SampleEntry::Avc1(Avc1Box {
                    width: 640,
                    height: 360,
                    ..Avc1Box::default()
                }),
                SampleEntry::Hvc1(Hev1Box {
                    width: 3840,
                    height: 2160,
                    ..Hev1Box::default()
                }),
            ],
            ..Default::default()
        };
        let mut buffer = Vec::new();
        source.write_box(&mut buffer).unwrap();

        let mut reader = Cursor::new(buffer);
        let header = BoxHeader::read(&mut reader).unwrap();
        let parsed = StsdBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert!(matches!(
            parsed.entry(1),
            Some(SampleEntry::Avc1(entry)) if entry.width == 640
        ));
        assert!(matches!(
            parsed.entry(2),
            Some(SampleEntry::Hvc1(entry)) if entry.width == 3840
        ));
    }

    #[test]
    fn rejects_entry_count_that_cannot_fit_in_the_box() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&16u32.to_be_bytes());
        bytes.extend_from_slice(b"stsd");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&u32::MAX.to_be_bytes());

        let mut reader = Cursor::new(bytes);
        let header = BoxHeader::read(&mut reader).unwrap();
        let error = StsdBox::read_box(&mut reader, header.size).unwrap_err();
        assert!(matches!(
            error,
            Error::InvalidData("stsd entry count exceeds box size")
        ));
    }

    #[test]
    fn rejects_truncated_known_sample_entry() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&24u32.to_be_bytes());
        bytes.extend_from_slice(b"stsd");
        bytes.extend_from_slice(&0u32.to_be_bytes());
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&8u32.to_be_bytes());
        bytes.extend_from_slice(b"avc1");

        let mut reader = Cursor::new(bytes);
        let header = BoxHeader::read(&mut reader).unwrap();
        let error = StsdBox::read_box(&mut reader, header.size).unwrap_err();
        assert!(matches!(error, Error::InvalidData(_)));
    }

    #[test]
    fn truncated_and_mutated_stsd_inputs_never_panic() {
        let source = StsdBox {
            entries: vec![SampleEntry::Avc1(Avc1Box {
                width: 640,
                height: 360,
                ..Avc1Box::default()
            })],
            ..Default::default()
        };
        let mut valid = Vec::new();
        source.write_box(&mut valid).unwrap();
        let parse = |bytes: Vec<u8>| {
            let mut reader = Cursor::new(bytes);
            if let Ok(header) = BoxHeader::read(&mut reader) {
                let _ = StsdBox::read_box(&mut reader, header.size);
            }
        };
        for length in 0..valid.len() {
            let truncated = valid[..length].to_vec();
            assert!(std::panic::catch_unwind(|| parse(truncated)).is_ok());
        }
        for index in 0..valid.len() {
            let mut mutated = valid.clone();
            mutated[index] ^= 0xff;
            assert!(std::panic::catch_unwind(|| parse(mutated)).is_ok());
        }
    }
}
