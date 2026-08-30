use crate::{
    meta::MetaBox,
    mp4box::{mvex::MvexBox, mvhd::MvhdBox, trak::TrakBox, udta::UdtaBox, *},
};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct MoovBox {
    pub mvhd: MvhdBox,

    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub meta: Option<MetaBox>,

    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub mvex: Option<MvexBox>,

    #[cfg_attr(feature = "serde", serde(rename = "trak"))]
    pub traks: Vec<TrakBox>,

    #[cfg_attr(feature = "serde", serde(skip_serializing_if = "Option::is_none"))]
    pub udta: Option<UdtaBox>,
}

impl MoovBox {
    pub const fn get_type(&self) -> BoxType {
        BoxType::MoovBox
    }

    pub fn get_size(&self) -> u64 {
        let mut size = HEADER_SIZE + self.mvhd.box_size();
        for trak in self.traks.iter() {
            size += trak.box_size();
        }
        if let Some(mvex) = &self.mvex {
            size += mvex.box_size();
        }
        if let Some(meta) = &self.meta {
            size += meta.box_size();
        }
        if let Some(udta) = &self.udta {
            size += udta.box_size();
        }
        size
    }
}

impl Mp4Box for MoovBox {
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
        let s = format!("traks={}", self.traks.len());
        Ok(s)
    }
}

impl<R: Read + Seek> ReadBox<&mut R> for MoovBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let end = checked_box_end(reader, size)?;

        let mut mvhd = None;
        let mut meta = None;
        let mut udta = None;
        let mut mvex = None;
        let mut traks = Vec::new();

        let mut current = reader.stream_position()?;
        while current < end {
            // Get box header.
            let header = read_box_header(reader, end)?;
            let BoxHeader { name, size: s } = header;
            if checked_box_end(reader, s)? > end {
                return Err(Error::InvalidData(
                    "moov box contains a box with a larger size than it",
                ));
            }

            match name {
                BoxType::MvhdBox => {
                    mvhd = Some(MvhdBox::read_box(reader, s)?);
                }
                BoxType::MetaBox => {
                    meta = Some(MetaBox::read_box(reader, s)?);
                }
                BoxType::MvexBox => {
                    mvex = Some(MvexBox::read_box(reader, s)?);
                }
                BoxType::TrakBox => {
                    let trak = TrakBox::read_box(reader, s)?;
                    traks.push(trak);
                }
                BoxType::UdtaBox => {
                    udta = Some(UdtaBox::read_box(reader, s)?);
                }
                _ => {
                    // XXX warn!()
                    skip_box(reader, s)?;
                }
            }

            current = reader.stream_position()?;
        }

        if mvhd.is_none() {
            return Err(Error::BoxNotFound(BoxType::MvhdBox));
        }

        skip_bytes_to(reader, end)?;

        Ok(Self {
            mvhd: mvhd.unwrap(),
            meta,
            udta,
            mvex,
            traks,
        })
    }
}

impl<W: Write> WriteBox<&mut W> for MoovBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;

        self.mvhd.write_box(writer)?;
        for trak in self.traks.iter() {
            trak.write_box(writer)?;
        }
        if let Some(mvex) = &self.mvex {
            mvex.write_box(writer)?;
        }
        if let Some(meta) = &self.meta {
            meta.write_box(writer)?;
        }
        if let Some(udta) = &self.udta {
            udta.write_box(writer)?;
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mp4box::trex::TrexBox;
    use std::io::Cursor;

    #[test]
    fn test_moov() {
        let src_box = MoovBox {
            mvhd: MvhdBox::default(),
            mvex: None, // XXX mvex is not written currently
            traks: vec![],
            meta: Some(MetaBox::default()),
            udta: Some(UdtaBox::default()),
        };

        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        assert_eq!(header.name, BoxType::MoovBox);
        assert_eq!(header.size, src_box.box_size());

        let dst_box = MoovBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(dst_box, src_box);
    }

    #[test]
    fn test_moov_empty() {
        let src_box = MoovBox::default();

        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        assert_eq!(header.name, BoxType::MoovBox);
        assert_eq!(header.size, src_box.box_size());

        let dst_box = MoovBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(dst_box, src_box);
    }

    #[test]
    fn test_moov_rejects_child_that_extends_past_parent() {
        let mut buf = Vec::new();
        MoovBox::default().write_box(&mut buf).unwrap();
        let parent_size = u32::try_from(buf.len() + HEADER_SIZE as usize)
            .expect("test moov size must fit in a 32-bit box header");
        buf[..4].copy_from_slice(&parent_size.to_be_bytes());
        BoxHeader::new(BoxType::FreeBox, u64::from(parent_size))
            .write(&mut buf)
            .unwrap();

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        let error = MoovBox::read_box(&mut reader, header.size);

        assert!(matches!(error, Err(Error::InvalidData(_))));
    }

    #[test]
    fn test_moov_rejects_parent_size_overflow() {
        let mut reader = Cursor::new([0; 9]);
        reader.set_position(9);

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            MoovBox::read_box(&mut reader, u64::MAX)
        }));

        assert!(matches!(result, Ok(Err(Error::InvalidData(_)))));
    }

    #[test]
    fn test_moov_rejects_partial_child_header_without_reading_past_parent() {
        let mut reader = Cursor::new([0; 16]);
        reader.set_position(HEADER_SIZE);

        let result = MoovBox::read_box(&mut reader, HEADER_SIZE + 1);

        assert!(matches!(result, Err(Error::InvalidData(_))));
        assert!(reader.position() <= HEADER_SIZE + 1);
    }

    #[test]
    fn test_moov_rejects_partial_extended_child_header_without_reading_past_parent() {
        let mut bytes = vec![0; 24];
        bytes[8..12].copy_from_slice(&1u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&u32::from(BoxType::FreeBox).to_be_bytes());
        let mut reader = Cursor::new(bytes);
        reader.set_position(HEADER_SIZE);

        let result = MoovBox::read_box(&mut reader, HEADER_SIZE + HEADER_SIZE);

        assert!(matches!(result, Err(Error::InvalidData(_))));
        assert!(reader.position() <= HEADER_SIZE + HEADER_SIZE);
    }

    #[test]
    fn test_moov_with_mvex() {
        let src_box = MoovBox {
            mvex: Some(MvexBox {
                mehd: None,
                trexs: vec![TrexBox {
                    track_id: 1,
                    default_sample_description_index: 1,
                    ..TrexBox::default()
                }],
            }),
            ..MoovBox::default()
        };

        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        assert_eq!(header.name, BoxType::MoovBox);
        let dst_box = MoovBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(dst_box, src_box);
    }
}
