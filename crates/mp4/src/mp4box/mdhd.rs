use crate::mp4box::*;
use std::char::{REPLACEMENT_CHARACTER, decode_utf16};

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MdhdBox {
    pub version: u8,
    pub flags: u32,
    pub creation_time: u64,
    pub modification_time: u64,
    pub timescale: u32,
    pub duration: u64,
    pub language: String,
}

impl MdhdBox {
    pub const fn get_type(&self) -> BoxType {
        BoxType::MdhdBox
    }

    pub const fn get_size(&self) -> u64 {
        let mut size = HEADER_SIZE + HEADER_EXT_SIZE;

        if self.version == 1 {
            size += 28;
        } else if self.version == 0 {
            size += 16;
        }
        size += 4;
        size
    }
}

impl Default for MdhdBox {
    fn default() -> Self {
        Self {
            version: 0,
            flags: 0,
            creation_time: 0,
            modification_time: 0,
            timescale: 1000,
            duration: 0,
            language: String::from("und"),
        }
    }
}

impl Mp4Box for MdhdBox {
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
        let s = format!(
            "creation_time={} timescale={} duration={} language={}",
            self.creation_time, self.timescale, self.duration, self.language
        );
        Ok(s)
    }
}

impl<R: Read + Seek> ReadBox<&mut R> for MdhdBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let end = checked_box_end_with_min(reader, size, HEADER_SIZE + HEADER_EXT_SIZE)?;

        let (version, flags) = read_box_header_ext(reader)?;

        let minimum_size = match version {
            0 => HEADER_SIZE + HEADER_EXT_SIZE + 16 + 4,
            1 => HEADER_SIZE + HEADER_EXT_SIZE + 28 + 4,
            _ => return Err(Error::InvalidData("version must be 0 or 1")),
        };
        ensure_box_size(size, minimum_size)?;

        let (creation_time, modification_time, timescale, duration) = if version == 1 {
            (
                reader.read_u64::<BigEndian>()?,
                reader.read_u64::<BigEndian>()?,
                reader.read_u32::<BigEndian>()?,
                reader.read_u64::<BigEndian>()?,
            )
        } else if version == 0 {
            (
                reader.read_u32::<BigEndian>()? as u64,
                reader.read_u32::<BigEndian>()? as u64,
                reader.read_u32::<BigEndian>()?,
                reader.read_u32::<BigEndian>()? as u64,
            )
        } else {
            unreachable!("version was validated above")
        };
        let language_code = reader.read_u16::<BigEndian>()?;
        let language = language_string(language_code);

        skip_bytes_to(reader, end)?;

        Ok(Self {
            version,
            flags,
            creation_time,
            modification_time,
            timescale,
            duration,
            language,
        })
    }
}

impl<W: Write> WriteBox<&mut W> for MdhdBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;

        write_box_header_ext(writer, self.version, self.flags)?;

        if self.version == 1 {
            writer.write_u64::<BigEndian>(self.creation_time)?;
            writer.write_u64::<BigEndian>(self.modification_time)?;
            writer.write_u32::<BigEndian>(self.timescale)?;
            writer.write_u64::<BigEndian>(self.duration)?;
        } else if self.version == 0 {
            writer.write_u32::<BigEndian>(self.creation_time as u32)?;
            writer.write_u32::<BigEndian>(self.modification_time as u32)?;
            writer.write_u32::<BigEndian>(self.timescale)?;
            writer.write_u32::<BigEndian>(self.duration as u32)?;
        } else {
            return Err(Error::InvalidData("version must be 0 or 1"));
        }

        let language_code = language_code(&self.language);
        writer.write_u16::<BigEndian>(language_code)?;
        writer.write_u16::<BigEndian>(0)?; // pre-defined

        Ok(size)
    }
}

fn language_string(language: u16) -> String {
    let mut lang: [u16; 3] = [0; 3];

    lang[0] = ((language >> 10) & 0x1F) + 0x60;
    lang[1] = ((language >> 5) & 0x1F) + 0x60;
    lang[2] = ((language) & 0x1F) + 0x60;

    // Decode utf-16 encoded bytes into a string.
    decode_utf16(lang.iter().cloned())
        .map(|r| r.unwrap_or(REPLACEMENT_CHARACTER))
        .collect::<String>()
}

fn language_code(language: &str) -> u16 {
    let mut lang = language.encode_utf16();
    let mut code = (lang.next().unwrap_or(0) & 0x1F) << 10;
    code += (lang.next().unwrap_or(0) & 0x1F) << 5;
    code += lang.next().unwrap_or(0) & 0x1F;
    code
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn test_language_code(lang: &str) {
        let code = language_code(lang);
        let lang2 = language_string(code);
        assert_eq!(lang, lang2);
    }

    #[test]
    fn test_language_codes() {
        test_language_code("und");
        test_language_code("eng");
        test_language_code("kor");
    }

    #[test]
    fn test_mdhd32() {
        let src_box = MdhdBox {
            version: 0,
            flags: 0,
            creation_time: 100,
            modification_time: 200,
            timescale: 48000,
            duration: 30439936,
            language: String::from("und"),
        };
        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        assert_eq!(header.name, BoxType::MdhdBox);
        assert_eq!(src_box.box_size(), header.size);

        let dst_box = MdhdBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(src_box, dst_box);
    }

    #[test]
    fn test_mdhd64() {
        let src_box = MdhdBox {
            version: 0,
            flags: 0,
            creation_time: 100,
            modification_time: 200,
            timescale: 48000,
            duration: 30439936,
            language: String::from("eng"),
        };
        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        assert_eq!(header.name, BoxType::MdhdBox);
        assert_eq!(src_box.box_size(), header.size);

        let dst_box = MdhdBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(src_box, dst_box);
    }
}
