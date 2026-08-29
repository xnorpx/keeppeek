use crate::mp4box::*;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct DinfBox {
    dref: DrefBox,
}

impl DinfBox {
    pub const fn get_type(&self) -> BoxType {
        BoxType::DinfBox
    }

    pub fn get_size(&self) -> u64 {
        HEADER_SIZE + self.dref.box_size()
    }
}

impl Mp4Box for DinfBox {
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
        let s = String::new();
        Ok(s)
    }
}

impl<R: Read + Seek> ReadBox<&mut R> for DinfBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let end = checked_box_end(reader, size)?;

        let mut dref = None;

        let mut current = reader.stream_position()?;
        while current < end {
            // Get box header.
            let header = read_box_header(reader, end)?;
            let BoxHeader { name, size: s } = header;
            if checked_box_end(reader, s)? > end {
                return Err(Error::InvalidData(
                    "dinf box contains a box with a larger size than it",
                ));
            }

            match name {
                BoxType::DrefBox => {
                    dref = Some(DrefBox::read_box(reader, s)?);
                }
                _ => {
                    // XXX warn!()
                    skip_box(reader, s)?;
                }
            }

            current = reader.stream_position()?;
        }

        if dref.is_none() {
            return Err(Error::BoxNotFound(BoxType::DrefBox));
        }

        skip_bytes_to(reader, end)?;

        Ok(Self {
            dref: dref.unwrap(),
        })
    }
}

impl<W: Write> WriteBox<&mut W> for DinfBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;
        self.dref.write_box(writer)?;
        Ok(size)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct DrefBox {
    pub version: u8,
    pub flags: u32,

    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub url: Option<UrlBox>,
}

impl Default for DrefBox {
    fn default() -> Self {
        Self {
            version: 0,
            flags: 0,
            url: Some(UrlBox::default()),
        }
    }
}

impl DrefBox {
    pub const fn get_type(&self) -> BoxType {
        BoxType::DrefBox
    }

    pub fn get_size(&self) -> u64 {
        let mut size = HEADER_SIZE + HEADER_EXT_SIZE + 4;
        if let Some(ref url) = self.url {
            size += url.box_size();
        }
        size
    }
}

impl Mp4Box for DrefBox {
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
        let s = String::new();
        Ok(s)
    }
}

impl<R: Read + Seek> ReadBox<&mut R> for DrefBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let end = checked_box_end_with_min(reader, size, HEADER_SIZE + HEADER_EXT_SIZE + 4)?;

        let (version, flags) = read_box_header_ext(reader)?;

        let mut url = None;

        let entry_count = reader.read_u32::<BigEndian>()?;
        let remaining = end
            .checked_sub(reader.stream_position()?)
            .ok_or(Error::InvalidData("dref contents exceed its declared size"))?;
        if u64::from(entry_count) > remaining / HEADER_SIZE {
            return Err(Error::InvalidData("dref entry count exceeds box size"));
        }
        for _i in 0..entry_count {
            // Get box header.
            let header = read_box_header(reader, end)?;
            let BoxHeader { name, size: s } = header;
            if checked_box_end(reader, s)? > end {
                return Err(Error::InvalidData(
                    "dinf box contains a box with a larger size than it",
                ));
            }

            match name {
                BoxType::UrlBox => {
                    url = Some(UrlBox::read_box(reader, s)?);
                }
                _ => {
                    skip_box(reader, s)?;
                }
            }
        }

        skip_bytes_to(reader, end)?;

        Ok(Self {
            version,
            flags,
            url,
        })
    }
}

impl<W: Write> WriteBox<&mut W> for DrefBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;

        write_box_header_ext(writer, self.version, self.flags)?;

        writer.write_u32::<BigEndian>(u32::from(self.url.is_some()))?;

        if let Some(ref url) = self.url {
            url.write_box(writer)?;
        }

        Ok(size)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct UrlBox {
    pub version: u8,
    pub flags: u32,
    pub location: String,
}

impl Default for UrlBox {
    fn default() -> Self {
        Self {
            version: 0,
            flags: 1,
            location: String::default(),
        }
    }
}

impl UrlBox {
    pub const fn get_type(&self) -> BoxType {
        BoxType::UrlBox
    }

    pub const fn get_size(&self) -> u64 {
        let mut size = HEADER_SIZE + HEADER_EXT_SIZE;

        if !self.location.is_empty() {
            size += self.location.len() as u64 + 1;
        }

        size
    }
}

impl Mp4Box for UrlBox {
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
        let s = format!("location={}", self.location);
        Ok(s)
    }
}

impl<R: Read + Seek> ReadBox<&mut R> for UrlBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let end = checked_box_end_with_min(reader, size, HEADER_SIZE + HEADER_EXT_SIZE)?;

        let (version, flags) = read_box_header_ext(reader)?;

        let location = if size.saturating_sub(HEADER_SIZE + HEADER_EXT_SIZE) > 0 {
            let buf_size = size - HEADER_SIZE - HEADER_EXT_SIZE - 1;
            let buf_size = usize::try_from(buf_size)
                .map_err(|_| Error::InvalidData("url location is too large"))?;
            let mut buf = vec![0u8; buf_size];
            reader.read_exact(&mut buf)?;
            match String::from_utf8(buf) {
                Ok(t) => {
                    if t.len() != buf_size {
                        return Err(Error::InvalidData("string too small"));
                    }
                    t
                }
                _ => String::default(),
            }
        } else {
            String::default()
        };

        skip_bytes_to(reader, end)?;

        Ok(Self {
            version,
            flags,
            location,
        })
    }
}

impl<W: Write> WriteBox<&mut W> for UrlBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;

        write_box_header_ext(writer, self.version, self.flags)?;

        if !self.location.is_empty() {
            writer.write_all(self.location.as_bytes())?;
            writer.write_u8(0)?;
        }

        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn empty_dref_round_trips() {
        let expected = DrefBox {
            version: 0,
            flags: 0,
            url: None,
        };
        let mut bytes = Vec::new();
        expected.write_box(&mut bytes).unwrap();
        let mut reader = Cursor::new(bytes);
        let header = BoxHeader::read(&mut reader).unwrap();

        let actual = DrefBox::read_box(&mut reader, header.size).unwrap();

        assert_eq!(actual, expected);
    }
}
