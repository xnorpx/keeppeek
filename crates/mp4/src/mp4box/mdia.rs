use crate::mp4box::{hdlr::HdlrBox, mdhd::MdhdBox, minf::MinfBox, *};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MdiaBox {
    pub mdhd: MdhdBox,
    pub hdlr: HdlrBox,
    pub minf: MinfBox,
}

impl MdiaBox {
    pub const fn get_type(&self) -> BoxType {
        BoxType::MdiaBox
    }

    pub fn get_size(&self) -> u64 {
        HEADER_SIZE + self.mdhd.box_size() + self.hdlr.box_size() + self.minf.box_size()
    }
}

impl Mp4Box for MdiaBox {
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

impl<R: Read + Seek> ReadBox<&mut R> for MdiaBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let end = checked_box_end(reader, size)?;

        let mut mdhd = None;
        let mut hdlr = None;
        let mut minf = None;

        let mut current = reader.stream_position()?;
        while current < end {
            // Get box header.
            let header = read_box_header(reader, end)?;
            let BoxHeader { name, size: s } = header;
            if checked_box_end(reader, s)? > end {
                return Err(Error::InvalidData(
                    "mdia box contains a box with a larger size than it",
                ));
            }

            match name {
                BoxType::MdhdBox => {
                    mdhd = Some(MdhdBox::read_box(reader, s)?);
                }
                BoxType::HdlrBox => {
                    hdlr = Some(HdlrBox::read_box(reader, s)?);
                }
                BoxType::MinfBox => {
                    minf = Some(MinfBox::read_box(reader, s)?);
                }
                _ => {
                    // XXX warn!()
                    skip_box(reader, s)?;
                }
            }

            current = reader.stream_position()?;
        }

        if mdhd.is_none() {
            return Err(Error::BoxNotFound(BoxType::MdhdBox));
        }
        if hdlr.is_none() {
            return Err(Error::BoxNotFound(BoxType::HdlrBox));
        }
        if minf.is_none() {
            return Err(Error::BoxNotFound(BoxType::MinfBox));
        }

        skip_bytes_to(reader, end)?;

        Ok(Self {
            mdhd: mdhd.unwrap(),
            hdlr: hdlr.unwrap(),
            minf: minf.unwrap(),
        })
    }
}

impl<W: Write> WriteBox<&mut W> for MdiaBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;

        self.mdhd.write_box(writer)?;
        self.hdlr.write_box(writer)?;
        self.minf.write_box(writer)?;

        Ok(size)
    }
}
