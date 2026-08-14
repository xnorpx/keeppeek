pub fn annexb_to_avcc(annexb: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(annexb.len());
    let mut i = 0;
    let len = annexb.len();

    while i < len {
        let sc_len = if i + 3 < len
            && annexb[i] == 0
            && annexb[i + 1] == 0
            && annexb[i + 2] == 0
            && annexb[i + 3] == 1
        {
            4
        } else if i + 2 < len && annexb[i] == 0 && annexb[i + 1] == 0 && annexb[i + 2] == 1 {
            3
        } else {
            i += 1;
            continue;
        };

        let nalu_start = i + sc_len;
        let mut nalu_end = len;
        let mut j = nalu_start;
        while j + 2 < len {
            if annexb[j] == 0
                && annexb[j + 1] == 0
                && (annexb[j + 2] == 1 || (j + 3 < len && annexb[j + 2] == 0 && annexb[j + 3] == 1))
            {
                nalu_end = j;
                break;
            }
            j += 1;
        }

        let nalu = &annexb[nalu_start..nalu_end];
        if !nalu.is_empty() {
            result.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
            result.extend_from_slice(nalu);
        }
        i = nalu_end;
    }
    result
}

pub fn avcc_to_annexb(avcc: &[u8]) -> Vec<u8> {
    let mut result = Vec::with_capacity(avcc.len());
    for_each_avcc_nalu(avcc, |nalu| {
        result.extend_from_slice(&[0, 0, 0, 1]);
        result.extend_from_slice(nalu);
    });
    result
}

pub fn extract_h264_sps_pps(avcc: &[u8]) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let mut sps = None;
    let mut pps = None;
    for_each_avcc_nalu(avcc, |nalu| {
        if nalu.is_empty() {
            return;
        }
        let nal_type = nalu[0] & 0x1F;
        match nal_type {
            7 => sps = Some(nalu.to_vec()),
            8 => pps = Some(nalu.to_vec()),
            _ => {}
        }
    });
    (sps, pps)
}

pub fn h264_pixel_dimensions(sps: &[u8], pps: &[u8]) -> Option<(u16, u16)> {
    let parameters = retina::codec::h264::parameters_from_sps_and_pps(
        sps,
        pps,
        retina::codec::h26x::Framing::FourByteLength,
    )
    .ok()?;
    let (width, height) = parameters.pixel_dimensions();
    Some((u16::try_from(width).ok()?, u16::try_from(height).ok()?))
}

pub type H265Params = (Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>);

pub fn extract_h265_params(avcc: &[u8]) -> H265Params {
    let mut vps = None;
    let mut sps = None;
    let mut pps = None;
    for_each_avcc_nalu(avcc, |nalu| {
        if nalu.len() < 2 {
            return;
        }
        let nal_type = (nalu[0] >> 1) & 0x3F;
        match nal_type {
            32 => vps = Some(nalu.to_vec()),
            33 => sps = Some(nalu.to_vec()),
            34 => pps = Some(nalu.to_vec()),
            _ => {}
        }
    });
    (vps, sps, pps)
}

fn for_each_avcc_nalu(data: &[u8], mut f: impl FnMut(&[u8])) {
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let nal_len =
            u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if nal_len == 0 || pos + nal_len > data.len() {
            break;
        }
        f(&data[pos..pos + nal_len]);
        pos += nal_len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annexb_four_byte_start_codes() {
        let annexb = [0, 0, 0, 1, 0x65, 0, 0, 0, 1, 0x41, 0x01];
        let avcc = annexb_to_avcc(&annexb);
        assert_eq!(avcc, vec![0, 0, 0, 1, 0x65, 0, 0, 0, 2, 0x41, 0x01]);
    }

    #[test]
    fn annexb_three_byte_start_codes() {
        let annexb = [0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0x01];
        let avcc = annexb_to_avcc(&annexb);
        assert_eq!(avcc, vec![0, 0, 0, 2, 0x67, 0x42, 0, 0, 0, 2, 0x68, 0x01]);
    }

    #[test]
    fn annexb_empty() {
        assert!(annexb_to_avcc(&[]).is_empty());
    }

    #[test]
    fn annexb_single_nalu() {
        let annexb = [0, 0, 0, 1, 0x65, 0x88, 0x80];
        let avcc = annexb_to_avcc(&annexb);
        assert_eq!(avcc, vec![0, 0, 0, 3, 0x65, 0x88, 0x80]);
    }

    #[test]
    fn avcc_to_annexb_preserves_nalus() {
        let avcc = [0, 0, 0, 2, 0x67, 0x42, 0, 0, 0, 3, 0x65, 0x88, 0x80];
        assert_eq!(
            avcc_to_annexb(&avcc),
            vec![0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 0x88, 0x80]
        );
    }

    #[test]
    fn extract_h264_params_from_avcc() {
        let sps_bytes = [0x67, 0x42, 0x00, 0x1e];
        let pps_bytes = [0x68, 0xce, 0x38, 0x80];
        let idr_bytes = [0x65, 0x88, 0x80];
        let mut avcc = Vec::new();
        avcc.extend_from_slice(&(sps_bytes.len() as u32).to_be_bytes());
        avcc.extend_from_slice(&sps_bytes);
        avcc.extend_from_slice(&(pps_bytes.len() as u32).to_be_bytes());
        avcc.extend_from_slice(&pps_bytes);
        avcc.extend_from_slice(&(idr_bytes.len() as u32).to_be_bytes());
        avcc.extend_from_slice(&idr_bytes);

        let (sps, pps) = extract_h264_sps_pps(&avcc);
        assert_eq!(sps.unwrap(), sps_bytes);
        assert_eq!(pps.unwrap(), pps_bytes);
    }

    #[test]
    fn h264_dimensions_follow_sps_instead_of_advertised_height() {
        let sps = [0x67, 0x42, 0x00, 0x1e, 0xf4, 0x05, 0x01, 0x6c, 0x80];
        let pps = [0x68, 0xce, 0x3c, 0x80];

        assert_eq!(h264_pixel_dimensions(&sps, &pps), Some((640, 352)));
    }

    #[test]
    fn extract_h265_params_from_avcc() {
        let vps_bytes = [0x40, 0x01, 0x0C];
        let sps_bytes = [0x42, 0x01, 0x01];
        let pps_bytes = [0x44, 0x01];
        let mut avcc = Vec::new();
        avcc.extend_from_slice(&(vps_bytes.len() as u32).to_be_bytes());
        avcc.extend_from_slice(&vps_bytes);
        avcc.extend_from_slice(&(sps_bytes.len() as u32).to_be_bytes());
        avcc.extend_from_slice(&sps_bytes);
        avcc.extend_from_slice(&(pps_bytes.len() as u32).to_be_bytes());
        avcc.extend_from_slice(&pps_bytes);

        let (vps, sps, pps) = extract_h265_params(&avcc);
        assert_eq!(vps.unwrap(), vps_bytes);
        assert_eq!(sps.unwrap(), sps_bytes);
        assert_eq!(pps.unwrap(), pps_bytes);
    }

    #[test]
    fn annexb_to_avcc_roundtrip_extract() {
        let annexb = [
            0, 0, 0, 1, 0x67, 0x42, 0x00, 0x1e, // SPS
            0, 0, 0, 1, 0x68, 0xce, 0x38, 0x80, // PPS
            0, 0, 0, 1, 0x65, 0x88, 0x80, // IDR
        ];
        let avcc = annexb_to_avcc(&annexb);
        let (sps, pps) = extract_h264_sps_pps(&avcc);
        assert_eq!(sps.unwrap(), [0x67, 0x42, 0x00, 0x1e]);
        assert_eq!(pps.unwrap(), [0x68, 0xce, 0x38, 0x80]);
    }
}
