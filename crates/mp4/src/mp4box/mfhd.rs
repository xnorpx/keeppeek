use crate::mp4box::*;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MfhdBox {
    pub version: u8,
    pub flags: u32,
    pub sequence_number: u32,
}

impl Default for MfhdBox {
    fn default() -> Self {
        Self {
            version: 0,
            flags: 0,
            sequence_number: 1,
        }
    }
}

impl MfhdBox {
    pub const fn get_type(&self) -> BoxType {
        BoxType::MfhdBox
    }

    pub const fn get_size(&self) -> u64 {
        HEADER_SIZE + HEADER_EXT_SIZE + 4
    }
}

impl Mp4Box for MfhdBox {
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
        let s = format!("sequence_number={}", self.sequence_number);
        Ok(s)
    }
}

impl<R: Read + Seek> ReadBox<&mut R> for MfhdBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let end = checked_box_end_with_min(reader, size, HEADER_SIZE + HEADER_EXT_SIZE + 4)?;

        let (version, flags) = read_box_header_ext(reader)?;
        let sequence_number = reader.read_u32::<BigEndian>()?;

        skip_bytes_to(reader, end)?;

        Ok(Self {
            version,
            flags,
            sequence_number,
        })
    }
}

impl<W: Write> WriteBox<&mut W> for MfhdBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;

        write_box_header_ext(writer, self.version, self.flags)?;
        writer.write_u32::<BigEndian>(self.sequence_number)?;

        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_mfhd() {
        let src_box = MfhdBox {
            version: 0,
            flags: 0,
            sequence_number: 1,
        };
        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        assert_eq!(header.name, BoxType::MfhdBox);
        assert_eq!(src_box.box_size(), header.size);

        let dst_box = MfhdBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(src_box, dst_box);
    }

    #[test]
    fn test_mfhd_rejects_overflowing_end() {
        let mut reader = Cursor::new(vec![0; 17]);
        reader.set_position(9);

        let result = MfhdBox::read_box(&mut reader, u64::MAX);

        assert!(matches!(result, Err(Error::InvalidData(_))));
    }
}
