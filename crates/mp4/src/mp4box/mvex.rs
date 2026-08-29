use crate::mp4box::{mehd::MehdBox, trex::TrexBox, *};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MvexBox {
    pub mehd: Option<MehdBox>,
    pub trexs: Vec<TrexBox>,
}

impl MvexBox {
    pub const fn get_type(&self) -> BoxType {
        BoxType::MvexBox
    }

    pub fn get_size(&self) -> u64 {
        let mut size = HEADER_SIZE + self.mehd.as_ref().map(|x| x.box_size()).unwrap_or(0);
        for trex in &self.trexs {
            size += trex.box_size();
        }
        size
    }
}

impl Mp4Box for MvexBox {
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

impl<R: Read + Seek> ReadBox<&mut R> for MvexBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let end = checked_box_end(reader, size)?;

        let mut mehd = None;
        let mut trexs = Vec::new();

        let mut current = reader.stream_position()?;
        while current < end {
            // Get box header.
            let header = read_box_header(reader, end)?;
            let BoxHeader { name, size: s } = header;
            if checked_box_end(reader, s)? > end {
                return Err(Error::InvalidData(
                    "mvex box contains a box with a larger size than it",
                ));
            }

            match name {
                BoxType::MehdBox => {
                    mehd = Some(MehdBox::read_box(reader, s)?);
                }
                BoxType::TrexBox => {
                    trexs.push(TrexBox::read_box(reader, s)?);
                }
                _ => {
                    // XXX warn!()
                    skip_box(reader, s)?;
                }
            }

            current = reader.stream_position()?;
        }

        if trexs.is_empty() {
            return Err(Error::BoxNotFound(BoxType::TrexBox));
        }

        skip_bytes_to(reader, end)?;

        Ok(Self { mehd, trexs })
    }
}

impl<W: Write> WriteBox<&mut W> for MvexBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;

        if let Some(mehd) = &self.mehd {
            mehd.write_box(writer)?;
        }
        for trex in &self.trexs {
            trex.write_box(writer)?;
        }

        Ok(size)
    }
}
