use crate::mp4box::*;

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Hev1Box {
    pub data_reference_index: u16,
    pub width: u16,
    pub height: u16,

    #[cfg_attr(feature = "serde", serde(with = "value_u32"))]
    pub horizresolution: FixedPointU16,

    #[cfg_attr(feature = "serde", serde(with = "value_u32"))]
    pub vertresolution: FixedPointU16,
    pub frame_count: u16,
    pub depth: u16,
    pub hvcc: HvcCBox,
}

impl Default for Hev1Box {
    fn default() -> Self {
        Self {
            data_reference_index: 0,
            width: 0,
            height: 0,
            horizresolution: FixedPointU16::new(0x48),
            vertresolution: FixedPointU16::new(0x48),
            frame_count: 1,
            depth: 0x0018,
            hvcc: HvcCBox::default(),
        }
    }
}

impl Hev1Box {
    pub fn new(config: &HevcConfig) -> Self {
        Self {
            data_reference_index: 1,
            width: config.width,
            height: config.height,
            horizresolution: FixedPointU16::new(0x48),
            vertresolution: FixedPointU16::new(0x48),
            frame_count: 1,
            depth: 0x0018,
            hvcc: HvcCBox::from_nalus(&config.vps, &config.sps, &config.pps),
        }
    }

    pub const fn get_type(&self) -> BoxType {
        BoxType::Hev1Box
    }

    pub fn get_size(&self) -> u64 {
        HEADER_SIZE + 8 + 70 + self.hvcc.box_size()
    }
}

impl Mp4Box for Hev1Box {
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
            "data_reference_index={} width={} height={} frame_count={}",
            self.data_reference_index, self.width, self.height, self.frame_count
        );
        Ok(s)
    }
}

impl<R: Read + Seek> ReadBox<&mut R> for Hev1Box {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let start = box_start(reader)?;

        reader.read_u32::<BigEndian>()?; // reserved
        reader.read_u16::<BigEndian>()?; // reserved
        let data_reference_index = reader.read_u16::<BigEndian>()?;

        reader.read_u32::<BigEndian>()?; // pre-defined, reserved
        reader.read_u64::<BigEndian>()?; // pre-defined
        reader.read_u32::<BigEndian>()?; // pre-defined
        let width = reader.read_u16::<BigEndian>()?;
        let height = reader.read_u16::<BigEndian>()?;
        let horizresolution = FixedPointU16::new_raw(reader.read_u32::<BigEndian>()?);
        let vertresolution = FixedPointU16::new_raw(reader.read_u32::<BigEndian>()?);
        reader.read_u32::<BigEndian>()?; // reserved
        let frame_count = reader.read_u16::<BigEndian>()?;
        skip_bytes(reader, 32)?; // compressorname
        let depth = reader.read_u16::<BigEndian>()?;
        reader.read_i16::<BigEndian>()?; // pre-defined

        let header = BoxHeader::read(reader)?;
        let BoxHeader { name, size: s } = header;
        if s > size {
            return Err(Error::InvalidData(
                "hev1 box contains a box with a larger size than it",
            ));
        }
        if name == BoxType::HvcCBox {
            let hvcc = HvcCBox::read_box(reader, s)?;

            skip_bytes_to(reader, start + size)?;

            Ok(Self {
                data_reference_index,
                width,
                height,
                horizresolution,
                vertresolution,
                frame_count,
                depth,
                hvcc,
            })
        } else {
            Err(Error::InvalidData("hvcc not found"))
        }
    }
}

impl<W: Write> WriteBox<&mut W> for Hev1Box {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        self.write_box_with_type(writer, BoxType::Hev1Box)
    }
}

impl Hev1Box {
    pub(crate) fn write_box_with_type<W: Write>(
        &self,
        writer: &mut W,
        box_type: BoxType,
    ) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(box_type, size).write(writer)?;

        writer.write_u32::<BigEndian>(0)?; // reserved
        writer.write_u16::<BigEndian>(0)?; // reserved
        writer.write_u16::<BigEndian>(self.data_reference_index)?;

        writer.write_u32::<BigEndian>(0)?; // pre-defined, reserved
        writer.write_u64::<BigEndian>(0)?; // pre-defined
        writer.write_u32::<BigEndian>(0)?; // pre-defined
        writer.write_u16::<BigEndian>(self.width)?;
        writer.write_u16::<BigEndian>(self.height)?;
        writer.write_u32::<BigEndian>(self.horizresolution.raw_value())?;
        writer.write_u32::<BigEndian>(self.vertresolution.raw_value())?;
        writer.write_u32::<BigEndian>(0)?; // reserved
        writer.write_u16::<BigEndian>(self.frame_count)?;
        // skip compressorname
        write_zeros(writer, 32)?;
        writer.write_u16::<BigEndian>(self.depth)?;
        writer.write_i16::<BigEndian>(-1)?; // pre-defined

        self.hvcc.write_box(writer)?;

        Ok(size)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct HvcCBox {
    pub configuration_version: u8,
    #[cfg_attr(feature = "serde", serde(skip))]
    pub record_data: Vec<u8>,
}

/// Parsed HEVC decoder configuration data carried by an `hvcC` box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HvcCConfiguration {
    /// Number of bytes used for each NAL length prefix in MP4 samples.
    pub nal_length_size: u8,
    /// Video parameter-set NAL units.
    pub vps: Vec<Vec<u8>>,
    /// Sequence parameter-set NAL units.
    pub sps: Vec<Vec<u8>>,
    /// Picture parameter-set NAL units.
    pub pps: Vec<Vec<u8>>,
}

impl HvcCBox {
    pub const fn new() -> Self {
        Self {
            configuration_version: 1,
            record_data: Vec::new(),
        }
    }

    pub fn from_nalus(vps: &[u8], sps: &[u8], pps: &[u8]) -> Self {
        let record_data = build_hvcc_record(vps, sps, pps);
        Self {
            configuration_version: 1,
            record_data,
        }
    }

    /// Decodes the NAL length size and parameter-set arrays from this `hvcC` box.
    pub fn configuration(&self) -> Result<HvcCConfiguration> {
        const HEADER_LEN: usize = 23;
        const ARRAY_COUNT_OFFSET: usize = 22;
        const LENGTH_SIZE_OFFSET: usize = 21;

        let record = &self.record_data;
        if record.len() < HEADER_LEN || record[0] != 1 {
            return Err(Error::InvalidData("invalid hvcC configuration record"));
        }

        let mut offset = HEADER_LEN;
        let mut vps = Vec::new();
        let mut sps = Vec::new();
        let mut pps = Vec::new();
        for _ in 0..usize::from(record[ARRAY_COUNT_OFFSET]) {
            let array_header = *record
                .get(offset)
                .ok_or(Error::InvalidData("invalid hvcC configuration record"))?;
            offset += 1;
            let nal_count = read_hvcc_u16(record, &mut offset)?;
            for _ in 0..usize::from(nal_count) {
                let nal_len = usize::from(read_hvcc_u16(record, &mut offset)?);
                let end = offset
                    .checked_add(nal_len)
                    .filter(|end| *end <= record.len())
                    .ok_or(Error::InvalidData("invalid hvcC configuration record"))?;
                let nal = record[offset..end].to_vec();
                offset = end;
                match array_header & 0x3f {
                    32 => vps.push(nal),
                    33 => sps.push(nal),
                    34 => pps.push(nal),
                    _ => {}
                }
            }
        }
        if offset != record.len() {
            return Err(Error::InvalidData("invalid hvcC configuration record"));
        }

        Ok(HvcCConfiguration {
            nal_length_size: (record[LENGTH_SIZE_OFFSET] & 0x03) + 1,
            vps,
            sps,
            pps,
        })
    }
}

fn read_hvcc_u16(record: &[u8], offset: &mut usize) -> Result<u16> {
    let end = offset
        .checked_add(2)
        .filter(|end| *end <= record.len())
        .ok_or(Error::InvalidData("invalid hvcC configuration record"))?;
    let value = u16::from_be_bytes([record[*offset], record[*offset + 1]]);
    *offset = end;
    Ok(value)
}

fn build_hvcc_record(vps: &[u8], sps: &[u8], pps: &[u8]) -> Vec<u8> {
    if vps.is_empty() && sps.is_empty() && pps.is_empty() {
        return vec![1];
    }

    let (ptl_byte, profile_compat, constraint, level, max_sub_layers, temporal_id_nested) =
        if sps.len() >= 15 {
            let b = sps[2];
            (
                sps[3],
                [sps[4], sps[5], sps[6], sps[7]],
                [sps[8], sps[9], sps[10], sps[11], sps[12], sps[13]],
                sps[14],
                (b >> 1) & 0x07,
                b & 0x01,
            )
        } else {
            (0, [0; 4], [0; 6], 0, 0, 0)
        };

    let num_temporal = (max_sub_layers + 1) & 0x07;
    let mut r = Vec::with_capacity(64 + vps.len() + sps.len() + pps.len());
    r.push(1); // configurationVersion
    r.push(ptl_byte); // general_profile_space | general_tier_flag | general_profile_idc
    r.extend_from_slice(&profile_compat);
    r.extend_from_slice(&constraint);
    r.push(level); // general_level_idc
    r.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc
    r.push(0xFC); // parallelismType
    r.push(0xFC | 1); // chromaFormat (assume 4:2:0)
    r.push(0xF8); // bitDepthLumaMinus8
    r.push(0xF8); // bitDepthChromaMinus8
    r.extend_from_slice(&[0x00, 0x00]); // avgFrameRate
    r.push((num_temporal << 3) | (temporal_id_nested << 2) | 3); // numTemporalLayers | temporalIdNested | lengthSizeMinusOne

    let mut arrays: Vec<(u8, &[u8])> = Vec::new();
    if !vps.is_empty() {
        arrays.push((32, vps));
    }
    if !sps.is_empty() {
        arrays.push((33, sps));
    }
    if !pps.is_empty() {
        arrays.push((34, pps));
    }
    r.push(arrays.len() as u8); // numOfArrays
    for (nal_type, nalu) in &arrays {
        r.push(0x80 | (nal_type & 0x3F)); // array_completeness=1 | NAL_unit_type
        r.extend_from_slice(&1u16.to_be_bytes()); // numNalus
        r.extend_from_slice(&(nalu.len() as u16).to_be_bytes()); // nalUnitLength
        r.extend_from_slice(nalu);
    }
    r
}

impl Mp4Box for HvcCBox {
    fn box_type(&self) -> BoxType {
        BoxType::HvcCBox
    }

    fn box_size(&self) -> u64 {
        if self.record_data.is_empty() {
            HEADER_SIZE + 1
        } else {
            HEADER_SIZE + self.record_data.len() as u64
        }
    }

    #[cfg(feature = "serde")]
    fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(&self).unwrap())
    }

    fn summary(&self) -> Result<String> {
        let s = format!(
            "configuration_version={} record_bytes={}",
            self.configuration_version,
            self.record_data.len()
        );
        Ok(s)
    }
}

impl<R: Read + Seek> ReadBox<&mut R> for HvcCBox {
    fn read_box(reader: &mut R, size: u64) -> Result<Self> {
        let start = box_start(reader)?;
        let content_size = size - HEADER_SIZE;
        if content_size <= 1 {
            skip_bytes_to(reader, start + size)?;
            return Ok(Self::new());
        }
        let configuration_version = reader.read_u8()?;
        let remaining = (content_size - 1) as usize;
        let mut record_tail = vec![0u8; remaining];
        reader.read_exact(&mut record_tail)?;
        let mut record_data = Vec::with_capacity(1 + remaining);
        record_data.push(configuration_version);
        record_data.extend_from_slice(&record_tail);
        skip_bytes_to(reader, start + size)?;
        Ok(Self {
            configuration_version,
            record_data,
        })
    }
}

impl<W: Write> WriteBox<&mut W> for HvcCBox {
    fn write_box(&self, writer: &mut W) -> Result<u64> {
        let size = self.box_size();
        BoxHeader::new(self.box_type(), size).write(writer)?;
        if self.record_data.is_empty() {
            writer.write_u8(self.configuration_version)?;
        } else {
            writer.write_all(&self.record_data)?;
        }
        Ok(size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_hev1() {
        let src_box = Hev1Box {
            data_reference_index: 1,
            width: 320,
            height: 240,
            horizresolution: FixedPointU16::new(0x48),
            vertresolution: FixedPointU16::new(0x48),
            frame_count: 1,
            depth: 24,
            hvcc: HvcCBox {
                configuration_version: 1,
                record_data: Vec::new(),
            },
        };
        let mut buf = Vec::new();
        src_box.write_box(&mut buf).unwrap();
        assert_eq!(buf.len(), src_box.box_size() as usize);

        let mut reader = Cursor::new(&buf);
        let header = BoxHeader::read(&mut reader).unwrap();
        assert_eq!(header.name, BoxType::Hev1Box);
        assert_eq!(src_box.box_size(), header.size);

        let dst_box = Hev1Box::read_box(&mut reader, header.size).unwrap();
        assert_eq!(src_box, dst_box);
    }

    #[test]
    fn hvc_configuration_decodes_parameter_sets() {
        let hvcc = HvcCBox::from_nalus(&[0x40, 0x01], &[0x42, 0x01], &[0x44, 0x01]);
        let configuration = hvcc.configuration().unwrap();

        assert_eq!(configuration.nal_length_size, 4);
        assert_eq!(configuration.vps, vec![vec![0x40, 0x01]]);
        assert_eq!(configuration.sps, vec![vec![0x42, 0x01]]);
        assert_eq!(configuration.pps, vec![vec![0x44, 0x01]]);
    }
}
