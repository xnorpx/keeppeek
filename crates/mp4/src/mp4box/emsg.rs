use crate::mp4box::*;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct EmsgBox {
    pub version: u8,
    pub flags: u32,
    pub timescale: u32,
    pub presentation_time: Option<u64>,
    pub presentation_time_delta: Option<u32>,
    pub event_duration: u32,
    pub id: u32,
    pub scheme_id_uri: String,
    pub value: String,
    pub message_data: Vec<u8>,
}

impl EmsgBox {
    const fn size_without_message(version: u8, scheme_id_uri: &str, value: &str) -> u64 {
        HEADER_SIZE + HEADER_EXT_SIZE +
            4 + // id
            Self::time_size(version) +
            (scheme_id_uri.len() + 1) as u64 +
            (value.len() as u64 + 1)
    }

    const fn time_size(version: u8) -> u64 {
        match version {
            0 => 12,
            1 => 16,
            _ => 0,
        }
    }

    fn validate(&self) -> Result<()> {
        match self.version {
            0 if self.presentation_time_delta.is_none() => {
                return Err(Error::InvalidData(
                    "emsg version 0 requires presentation_time_delta",
                ));
            }
            1 if self.presentation_time.is_none() => {
                return Err(Error::InvalidData(
                    "emsg version 1 requires presentation_time",
                ));
            }
            0 | 1 => {}
            _ => return Err(Error::InvalidData("version must be 0 or 1")),
        }
        if self.scheme_id_uri.contains('\0') || self.value.contains('\0') {
            return Err(Error::InvalidData("emsg strings cannot contain null bytes"));
        }
        Ok(())
    }
}

impl Mp4Box for EmsgBox {
    fn box_type(&self) -> BoxType {
        BoxType::EmsgBox
    }

    fn box_size(&self) -> u64 {
        Self::size_without_message(self.version, &self.scheme_id_uri, &self.value)
            + self.message_data.len() as u64
    }

    #[cfg(feature = "serde")]
    fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(&self).unwrap())
    }

    fn summary(&self) -> Result<String> {
        let s = format!("id={} value={}", self.id, self.value);
        Ok(s)
    }
}

impl<R: Read + Seek> ReadBox<&mut R> for EmsgBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let start = box_start(reader)?;
        let end = start
            .checked_add(size)
            .ok_or(Error::InvalidData("emsg box size overflow"))?;
        if size < HEADER_SIZE + HEADER_EXT_SIZE {
            return Err(Error::InvalidData("emsg box size is too small"));
        }
        let (version, flags) = read_box_header_ext(reader)?;

        let (
            timescale,
            presentation_time,
            presentation_time_delta,
            event_duration,
            id,
            scheme_id_uri,
            value,
        ) = match version {
            0 => {
                let scheme_id_uri = read_null_terminated_utf8_string(reader, end)?;
                let value = read_null_terminated_utf8_string(reader, end)?;
                ensure_bytes_remaining(reader, end, 16)?;
                (
                    reader.read_u32::<BigEndian>()?,
                    None,
                    Some(reader.read_u32::<BigEndian>()?),
                    reader.read_u32::<BigEndian>()?,
                    reader.read_u32::<BigEndian>()?,
                    scheme_id_uri,
                    value,
                )
            }
            1 => {
                ensure_bytes_remaining(reader, end, 20)?;
                (
                    reader.read_u32::<BigEndian>()?,
                    Some(reader.read_u64::<BigEndian>()?),
                    None,
                    reader.read_u32::<BigEndian>()?,
                    reader.read_u32::<BigEndian>()?,
                    read_null_terminated_utf8_string(reader, end)?,
                    read_null_terminated_utf8_string(reader, end)?,
                )
            }
            _ => return Err(Error::InvalidData("version must be 0 or 1")),
        };

        let message_size = end
            .checked_sub(reader.stream_position()?)
            .ok_or(Error::InvalidData("emsg contents exceed box size"))?;
        let message_size = usize::try_from(message_size)
            .map_err(|_| Error::InvalidData("emsg message is too large"))?;
        let mut message_data = vec![0; message_size];
        reader.read_exact(&mut message_data)?;

        skip_bytes_to(reader, end)?;

        Ok(Self {
            version,
            flags,
            timescale,
            presentation_time,
            presentation_time_delta,
            event_duration,
            id,
            scheme_id_uri,
            value,
            message_data,
        })
    }
}

impl<W: Write> WriteBox<&mut W> for EmsgBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        self.validate()?;
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;

        write_box_header_ext(writer, self.version, self.flags)?;
        match self.version {
            0 => {
                write_null_terminated_str(writer, &self.scheme_id_uri)?;
                write_null_terminated_str(writer, &self.value)?;
                writer.write_u32::<BigEndian>(self.timescale)?;
                writer.write_u32::<BigEndian>(self.presentation_time_delta.ok_or(
                    Error::InvalidData("emsg version 0 requires presentation_time_delta"),
                )?)?;
                writer.write_u32::<BigEndian>(self.event_duration)?;
                writer.write_u32::<BigEndian>(self.id)?;
            }
            1 => {
                writer.write_u32::<BigEndian>(self.timescale)?;
                writer.write_u64::<BigEndian>(self.presentation_time.ok_or(
                    Error::InvalidData("emsg version 1 requires presentation_time"),
                )?)?;
                writer.write_u32::<BigEndian>(self.event_duration)?;
                writer.write_u32::<BigEndian>(self.id)?;
                write_null_terminated_str(writer, &self.scheme_id_uri)?;
                write_null_terminated_str(writer, &self.value)?;
            }
            _ => return Err(Error::InvalidData("version must be 0 or 1")),
        }

        for &byte in &self.message_data {
            writer.write_u8(byte)?;
        }

        Ok(size)
    }
}

fn ensure_bytes_remaining<R: Seek>(reader: &mut R, end: u64, needed: u64) -> Result<()> {
    let remaining = end
        .checked_sub(reader.stream_position()?)
        .ok_or(Error::InvalidData("emsg contents exceed box size"))?;
    if remaining < needed {
        return Err(Error::InvalidData("emsg box is truncated"));
    }
    Ok(())
}

fn read_null_terminated_utf8_string<R: Read + Seek>(reader: &mut R, end: u64) -> Result<String> {
    let remaining = end
        .checked_sub(reader.stream_position()?)
        .ok_or(Error::InvalidData("emsg contents exceed box size"))?;
    let mut bytes = Vec::new();
    for _ in 0..remaining {
        let byte = reader.read_u8()?;
        if byte == 0 {
            return String::from_utf8(bytes).map_err(|_| Error::InvalidData("invalid utf8"));
        }
        bytes.push(byte);
    }
    Err(Error::InvalidData("emsg string is not null terminated"))
}

fn write_null_terminated_str<W: Write>(writer: &mut W, string: &str) -> Result<()> {
    for byte in string.bytes() {
        writer.write_u8(byte)?;
    }
    writer.write_u8(0)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_emsg_version0() {
        let src_box = EmsgBox {
            version: 0,
            flags: 0,
            timescale: 48000,
            presentation_time: None,
            presentation_time_delta: Some(100),
            event_duration: 200,
            id: 8,
            scheme_id_uri: String::from("foo"),
            value: String::from("foo"),
            message_data: vec![1, 2, 3],
        };
        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        assert_eq!(header.name, BoxType::EmsgBox);
        assert_eq!(src_box.box_size(), header.size);

        let dst_box = EmsgBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(src_box, dst_box);
    }

    #[test]
    fn test_emsg_version1() {
        let src_box = EmsgBox {
            version: 1,
            flags: 0,
            timescale: 48000,
            presentation_time: Some(50000),
            presentation_time_delta: None,
            event_duration: 200,
            id: 8,
            scheme_id_uri: String::from("foo"),
            value: String::from("foo"),
            message_data: vec![3, 2, 1],
        };
        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        assert_eq!(header.name, BoxType::EmsgBox);
        assert_eq!(src_box.box_size(), header.size);

        let dst_box = EmsgBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(src_box, dst_box);
    }

    #[test]
    fn test_emsg_rejects_contents_larger_than_declared_size() {
        let declared_size = 33;
        let mut buf = Vec::new();
        BoxHeader::new(BoxType::EmsgBox, declared_size)
            .write(&mut buf)
            .unwrap();
        buf.extend_from_slice(&[1, 0, 0, 0]);
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&0u64.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&[0, 0]);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        let error = EmsgBox::read_box(&mut reader, header.size);

        assert!(matches!(error, Err(Error::InvalidData(_))));
        assert_eq!(reader.position(), declared_size);
    }

    #[test]
    fn test_emsg_unterminated_string_stays_within_box() {
        let declared_size = 16;
        let mut buf = Vec::new();
        BoxHeader::new(BoxType::EmsgBox, declared_size)
            .write(&mut buf)
            .unwrap();
        buf.extend_from_slice(&[0, 0, 0, 0]);
        buf.extend_from_slice(b"abcd");
        buf.extend_from_slice(&[0; 32]);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        let error = EmsgBox::read_box(&mut reader, header.size);

        assert!(matches!(error, Err(Error::InvalidData(_))));
        assert_eq!(reader.position(), declared_size);
    }

    #[test]
    fn test_emsg_invalid_public_state_returns_error() {
        let mut invalid_version = EmsgBox {
            version: 2,
            ..EmsgBox::default()
        };
        assert!(std::panic::catch_unwind(|| invalid_version.box_size()).is_ok());
        assert!(matches!(
            invalid_version.write_box(&mut Vec::new()),
            Err(Error::InvalidData(_))
        ));

        invalid_version.version = 0;
        assert!(matches!(
            invalid_version.write_box(&mut Vec::new()),
            Err(Error::InvalidData(_))
        ));

        invalid_version.presentation_time_delta = Some(0);
        invalid_version.scheme_id_uri = "urn:example\0suffix".to_owned();
        assert!(matches!(
            invalid_version.write_box(&mut Vec::new()),
            Err(Error::InvalidData(_))
        ));
    }
}
