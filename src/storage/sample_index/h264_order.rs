//! Display order for a closed, progressive AVC GOP with type-0 picture-order counts.
use h264_reader::{
    Context,
    avcc::AvcDecoderConfigurationRecord,
    nal::{
        Nal, RefNal, UnitType,
        pps::PicParamSetId,
        slice::{
            DecRefPicMarking, FieldPic, MemoryManagementControlOperation, PicOrderCountLsb,
            SliceHeader,
        },
        sps::{FrameMbsFlags, PicOrderCntType},
    },
};

pub(super) struct PictureOrder<'a> {
    configuration: AvcDecoderConfigurationRecord<'a>,
    context: Context,
    length_size: usize,
    previous_reference: Option<i64>,
    parameter_set: Option<PicParamSetId>,
}

impl<'a> PictureOrder<'a> {
    pub(super) fn new(decoder: &'a mp4::Mp4VideoDecoderConfig) -> Option<Self> {
        if !decoder.codec.starts_with("avc1.") {
            return None;
        }
        let config =
            AvcDecoderConfigurationRecord::try_from(decoder.description.as_slice()).ok()?;
        let length = config.length_size_minus_one() + 1;
        if length != decoder.nal_length_size || !matches!(length, 1 | 2 | 4) {
            return None;
        }
        Some(Self {
            context: config.create_context().ok()?,
            configuration: config,
            length_size: usize::from(length),
            previous_reference: None,
            parameter_set: None,
        })
    }

    pub(super) fn next(&mut self, mut sample: &[u8], sync: bool) -> Option<i64> {
        let mut order = None;
        // Reject unqualified layouts such as in-band parameter replacement and multiple pictures.
        for _ in 0..128 {
            if sample.is_empty() {
                return order;
            }
            let (prefix, tail) = sample.split_at_checked(self.length_size)?;
            let length = prefix
                .iter()
                .fold(0_usize, |n, byte| (n << 8) | usize::from(*byte));
            let (unit, rest) = tail.split_at_checked(length)?;
            sample = rest;
            if unit.is_empty() {
                return None;
            }
            let prefix_len = unit.len().min(2048);
            let nal = RefNal::new(&unit[..prefix_len], &[], prefix_len == unit.len());
            let header = nal.header().ok()?;
            let idr = match header.nal_unit_type() {
                UnitType::SeqParameterSet
                    if self
                        .configuration
                        .sequence_parameter_sets()
                        .any(|p| p.is_ok_and(|bytes| bytes == unit)) =>
                {
                    continue;
                }
                UnitType::PicParameterSet
                    if self
                        .configuration
                        .picture_parameter_sets()
                        .any(|p| p.is_ok_and(|bytes| bytes == unit)) =>
                {
                    continue;
                }
                UnitType::SliceLayerWithoutPartitioningIdr => true,
                UnitType::SliceLayerWithoutPartitioningNonIdr => false,
                UnitType::AccessUnitDelimiter | UnitType::SEI | UnitType::FillerData => continue,
                _ => return None,
            };
            if idr != sync || (!idr && self.previous_reference.is_none()) || order.is_some() {
                return None;
            }
            let (slice, sps, pps) =
                SliceHeader::from_bits(&self.context, &mut nal.rbsp_bits(), header, None).ok()?;
            if idr {
                if self.previous_reference.is_some()
                    && matches!(
                        slice.dec_ref_pic_marking,
                        Some(DecRefPicMarking::Idr {
                            no_output_of_prior_pics_flag: true,
                            ..
                        })
                    )
                {
                    return None;
                }
                self.previous_reference = None;
                self.parameter_set = None;
            }
            if slice.first_mb_in_slice != 0
                || slice.field_pic != FieldPic::Frame
                || !matches!(sps.frame_mbs_flags, FrameMbsFlags::Frames)
                || slice.colour_plane.is_some()
                || slice.redundant_pic_cnt.unwrap_or(0) != 0
                || self
                    .parameter_set
                    .is_some_and(|id| id != pps.pic_parameter_set_id)
                || matches!(&slice.dec_ref_pic_marking, Some(DecRefPicMarking::Adaptive(ops))
                    if ops.iter().any(|op| matches!(op, MemoryManagementControlOperation::AllRefPicturesUnused)))
            {
                return None;
            }
            let PicOrderCntType::TypeZero {
                log2_max_pic_order_cnt_lsb_minus4,
            } = sps.pic_order_cnt
            else {
                return None;
            };
            let Some(PicOrderCountLsb::Frame(lsb)) = slice.pic_order_cnt_lsb else {
                return None;
            };
            if idr && (lsb != 0 || header.nal_ref_idc() == 0) {
                return None;
            }
            let modulus = 1_i64.checked_shl(u32::from(log2_max_pic_order_cnt_lsb_minus4) + 4)?;
            let count = extend_count(
                self.previous_reference.unwrap_or(0),
                i64::from(lsb),
                modulus,
            );
            if header.nal_ref_idc() != 0 {
                self.previous_reference = Some(count);
            }
            self.parameter_set = Some(pps.pic_parameter_set_id);
            order = Some(count);
        }
        None
    }
}

// H.264 section 8.2.1.1 uses the previous reference picture, not the previous decoded B picture.
const fn extend_count(previous_reference: i64, lsb: i64, modulus: i64) -> i64 {
    let previous_lsb = previous_reference.rem_euclid(modulus);
    let mut msb = previous_reference - previous_lsb;
    if previous_lsb - lsb >= modulus / 2 {
        msb += modulus;
    } else if lsb - previous_lsb > modulus / 2 {
        msb -= modulus;
    }
    msb + lsb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_order_the_duplicate_pts_reference_and_b_pictures() {
        let data = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/exact_frame_step/duplicate-pts.mp4"
        ));
        let mut reader =
            mp4::Mp4Reader::read_header(std::io::Cursor::new(data), data.len() as u64).unwrap();
        let decoder = reader.tracks()[&1]
            .video_decoder_config_for_description(1)
            .unwrap()
            .unwrap();
        let mut counts = PictureOrder::new(&decoder).unwrap();
        for first in [1_u32, 7] {
            let mut observed = Vec::new();
            for ordinal in first..first + 6 {
                let picture = reader.read_sample(1, ordinal).unwrap().unwrap();
                observed.push(counts.next(&picture.bytes, picture.is_sync).unwrap());
            }
            // The burned-in frame IDs and FFmpeg's independent pixel oracle put B picture
            // 10 before reference picture 11 even though 11 is decoded first at the same PTS.
            assert_eq!(observed, [0, 6, 2, 4, 10, 8]);
        }
        let mut counts = PictureOrder::new(&decoder).unwrap();
        let track = &reader.tracks()[&1];
        let mut prefixed = Vec::new();
        for set in [
            track.sequence_parameter_set().unwrap(),
            track.picture_parameter_set().unwrap(),
        ] {
            prefixed.extend_from_slice(&u32::try_from(set.len()).unwrap().to_be_bytes());
            prefixed.extend_from_slice(set);
        }
        let sync_picture = reader.read_sample(1, 1).unwrap().unwrap();
        prefixed.extend_from_slice(&sync_picture.bytes);
        assert_eq!(counts.next(&prefixed, true), Some(0));
        // An in-band SPS replacement needs its own qualified configuration.
        prefixed[5] ^= 1;
        let mut counts = PictureOrder::new(&decoder).unwrap();
        assert!(counts.next(&prefixed, true).is_none());
        let mut counts = PictureOrder::new(&decoder).unwrap();
        let non_idr = reader.read_sample(1, 2).unwrap().unwrap();
        assert!(counts.next(&non_idr.bytes, false).is_none());
    }

    #[test]
    fn extends_counts_across_wrap_relative_to_the_last_reference_picture() {
        assert_eq!(extend_count(14, 2, 16), 18);
        assert_eq!(extend_count(18, 15, 16), 15);
        assert_eq!(extend_count(14, 6, 16), 22);
        assert_eq!(extend_count(2, 10, 16), 10);
    }
}
