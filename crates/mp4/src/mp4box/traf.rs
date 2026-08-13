use crate::mp4box::{tfdt::TfdtBox, tfhd::TfhdBox, trun::TrunBox, *};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct TrafBox {
    pub tfhd: TfhdBox,
    pub tfdt: Option<TfdtBox>,
    pub trun: Option<TrunBox>,
}

impl TrafBox {
    pub const fn get_type(&self) -> BoxType {
        BoxType::TrafBox
    }

    pub fn get_size(&self) -> u64 {
        let mut size = HEADER_SIZE;
        size += self.tfhd.box_size();
        if let Some(ref tfdt) = self.tfdt {
            size += tfdt.box_size();
        }
        if let Some(ref trun) = self.trun {
            size += trun.box_size();
        }
        size
    }
}

impl Mp4Box for TrafBox {
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

impl<R: Read + Seek> ReadBox<&mut R> for TrafBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let start = box_start(reader)?;

        let mut tfhd = None;
        let mut tfdt = None;
        let mut trun = None;

        let mut current = reader.stream_position()?;
        let end = start + size;
        while current < end {
            // Get box header.
            let header = BoxHeader::read(reader)?;
            let BoxHeader { name, size: s } = header;
            if s > size {
                return Err(Error::InvalidData(
                    "traf box contains a box with a larger size than it",
                ));
            }

            match name {
                BoxType::TfhdBox => {
                    tfhd = Some(TfhdBox::read_box(reader, s)?);
                }
                BoxType::TfdtBox => {
                    tfdt = Some(TfdtBox::read_box(reader, s)?);
                }
                BoxType::TrunBox => {
                    trun = Some(TrunBox::read_box(reader, s)?);
                }
                _ => {
                    // XXX warn!()
                    skip_box(reader, s)?;
                }
            }

            current = reader.stream_position()?;
        }

        if tfhd.is_none() {
            return Err(Error::BoxNotFound(BoxType::TfhdBox));
        }

        skip_bytes_to(reader, start + size)?;

        Ok(Self {
            tfhd: tfhd.unwrap(),
            tfdt,
            trun,
        })
    }
}

impl<W: Write> WriteBox<&mut W> for TrafBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;

        self.tfhd.write_box(writer)?;
        if let Some(tfdt) = &self.tfdt {
            tfdt.write_box(writer)?;
        }
        if let Some(trun) = &self.trun {
            trun.write_box(writer)?;
        }

        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn traf_round_trip_includes_decode_time_and_sample_run() {
        let src_box = TrafBox {
            tfhd: TfhdBox {
                flags: TfhdBox::FLAG_BASE_DATA_OFFSET,
                track_id: 1,
                base_data_offset: Some(4_096),
                ..TfhdBox::default()
            },
            tfdt: Some(TfdtBox {
                version: 1,
                base_media_decode_time: 90_000,
                ..TfdtBox::default()
            }),
            trun: Some(TrunBox {
                flags: TrunBox::FLAG_DATA_OFFSET
                    | TrunBox::FLAG_SAMPLE_DURATION
                    | TrunBox::FLAG_SAMPLE_SIZE
                    | TrunBox::FLAG_SAMPLE_FLAGS,
                sample_count: 2,
                data_offset: Some(128),
                sample_durations: vec![3_000, 3_000],
                sample_sizes: vec![1_024, 512],
                sample_flags: vec![0, 0x0001_0000],
                ..TrunBox::default()
            }),
        };

        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        assert_eq!(header.name, BoxType::TrafBox);
        let dst_box = TrafBox::read_box(&mut reader, header.size).unwrap();
        assert_eq!(dst_box, src_box);
    }
}
