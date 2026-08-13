pub struct AdtsInfo {
    pub header_len: usize,
    pub profile: u8,
    pub sample_rate: u32,
    pub channels: u8,
}

const SAMPLE_RATES: [u32; 13] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
];

pub const fn parse_adts(data: &[u8]) -> Option<AdtsInfo> {
    if data.len() < 7 {
        return None;
    }
    if data[0] != 0xFF || (data[1] & 0xF0) != 0xF0 {
        return None;
    }

    let protection_absent = data[1] & 0x01;
    let header_len = if protection_absent == 1 { 7 } else { 9 };

    let profile = ((data[2] >> 6) & 0x03) + 1;
    let freq_index = ((data[2] >> 2) & 0x0F) as usize;
    let channels = ((data[2] & 0x01) << 2) | ((data[3] >> 6) & 0x03);

    let sample_rate = if freq_index < SAMPLE_RATES.len() {
        SAMPLE_RATES[freq_index]
    } else {
        return None;
    };

    Some(AdtsInfo {
        header_len,
        profile,
        sample_rate,
        channels,
    })
}

pub fn strip_adts(data: &[u8]) -> (&[u8], Option<AdtsInfo>) {
    parse_adts(data).map_or((data, None), |info| {
        let raw = &data[info.header_len..];
        (raw, Some(info))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_adts_header() {
        // ADTS: sync=0xFFF, MPEG-4, Layer=0, protection_absent=1
        // profile=LC(01), freq_index=4(44100Hz), channel=1(mono)
        let header = [0xFF, 0xF1, 0x50, 0x80, 0x02, 0x80, 0x00];
        let info = parse_adts(&header).unwrap();
        assert_eq!(info.header_len, 7);
        assert_eq!(info.profile, 2); // LC
        assert_eq!(info.sample_rate, 44100);
        assert_eq!(info.channels, 2);
    }

    #[test]
    fn strip_removes_header() {
        let mut frame = vec![0xFF, 0xF1, 0x50, 0x80, 0x02, 0x80, 0x00];
        let payload = vec![0xAA; 20];
        frame.extend_from_slice(&payload);

        let (raw, info) = strip_adts(&frame);
        assert!(info.is_some());
        assert_eq!(raw, &payload);
    }

    #[test]
    fn non_adts_passthrough() {
        let data = vec![0x00, 0x01, 0x02, 0x03];
        let (raw, info) = strip_adts(&data);
        assert!(info.is_none());
        assert_eq!(raw, &data);
    }
}
