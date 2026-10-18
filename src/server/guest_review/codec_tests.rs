use super::*;
use mp4::WriteBox;

fn boxed(kind: &[u8; 4], body: Vec<u8>) -> Vec<u8> {
    let mut result = u32::try_from(body.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    result.extend(kind);
    result.extend(body);
    result
}

fn video_entry() -> Vec<u8> {
    let mut body = vec![0; 78];
    body[6..8].copy_from_slice(&1u16.to_be_bytes());
    body[24..26].copy_from_slice(&640u16.to_be_bytes());
    body[26..28].copy_from_slice(&480u16.to_be_bytes());
    let config = mp4::avc1::AvcCBox::new(&[0x67, 66, 0, 30], &[0x68, 0]);
    config.write_box(&mut body).unwrap();
    boxed(b"avc1", body)
}

fn audio_entry(profile: mp4::AudioObjectType) -> Vec<u8> {
    let mut body = vec![0; 28];
    body[6..8].copy_from_slice(&1u16.to_be_bytes());
    body[16..18].copy_from_slice(&2u16.to_be_bytes());
    body[18..20].copy_from_slice(&16u16.to_be_bytes());
    body[24..28].copy_from_slice(&0xbb800000u32.to_be_bytes());
    let config = mp4::mp4a::EsdsBox::new(&mp4::AacConfig {
        profile,
        ..Default::default()
    });
    config.write_box(&mut body).unwrap();
    boxed(b"mp4a", body)
}

fn track(kind: &[u8; 4], entries: Vec<Vec<u8>>, sample_table: Vec<u8>) -> Vec<u8> {
    let mut descriptions = vec![0; 4];
    descriptions.extend(u32::try_from(entries.len()).unwrap().to_be_bytes());
    for entry in entries {
        descriptions.extend(entry);
    }
    let mut table = boxed(b"stsd", descriptions);
    table.extend(sample_table);
    let mut handler = vec![0; 8];
    handler.extend(kind);
    let mut media = boxed(b"hdlr", handler);
    media.extend(boxed(b"minf", boxed(b"stbl", table)));
    boxed(b"trak", boxed(b"mdia", media))
}

fn artifact(tracks: Vec<Vec<u8>>) -> Vec<u8> {
    let mut output = boxed(b"ftyp", b"isom\0\0\0\0isom".to_vec());
    output.extend(boxed(b"moov", tracks.into_iter().flatten().collect()));
    output.extend(boxed(b"mdat", Vec::new()));
    output
}

fn validate(bytes: Vec<u8>) -> io::Result<()> {
    inspect(&mut Cursor::new(&bytes), bytes.len() as u64, || Ok(()))
}

#[test]
fn actual_h264_and_optional_aac_lc_descriptions_define_admission() {
    let video = track(b"vide", vec![video_entry()], Vec::new());
    assert!(validate(artifact(vec![video.clone()])).is_ok());
    let audio = track(
        b"soun",
        vec![audio_entry(mp4::AudioObjectType::AacLowComplexity)],
        Vec::new(),
    );
    assert!(validate(artifact(vec![video.clone(), audio])).is_ok());
    let unsupported = track(
        b"soun",
        vec![audio_entry(mp4::AudioObjectType::AacMain)],
        Vec::new(),
    );
    assert!(validate(artifact(vec![video, unsupported])).is_err());
}

#[test]
fn unsupported_mixed_duplicate_or_excessive_tracks_are_rejected() {
    let video = track(b"vide", vec![video_entry()], Vec::new());
    assert!(validate(artifact(vec![video.clone(), video.clone()])).is_err());
    assert!(validate(artifact(vec![video.clone(), video.clone(), video])).is_err());
    assert!(
        validate(artifact(vec![track(
            b"vide",
            vec![video_entry(), video_entry()],
            Vec::new()
        )]))
        .is_err()
    );
    let mut unknown = video_entry();
    unknown[4..8].copy_from_slice(b"hvc1");
    assert!(validate(artifact(vec![track(b"vide", vec![unknown], Vec::new())])).is_err());
    assert!(validate(artifact(Vec::new())).is_err());
}

#[test]
fn sample_table_counts_never_allocate_or_affect_codec_inspection() {
    let mut table = vec![0; 8];
    table.extend(u32::MAX.to_be_bytes());
    let video = track(b"vide", vec![video_entry()], boxed(b"stsz", table));
    assert!(validate(artifact(vec![video])).is_ok());
}

#[test]
fn malformed_duplicate_and_oversized_boxes_fail_before_unbounded_parsing() {
    assert!(validate(vec![0; 8]).is_err());
    let video = track(b"vide", vec![video_entry()], Vec::new());
    let mut duplicate = artifact(vec![video.clone()]);
    duplicate.extend(boxed(b"moov", video.clone()));
    assert!(validate(duplicate).is_err());
    let mut oversized = boxed(b"ftyp", b"isom\0\0\0\0".to_vec());
    oversized.extend(u32::try_from(MAX_MOOV_BYTES + 9).unwrap().to_be_bytes());
    oversized.extend(b"moov");
    oversized.resize(usize::try_from(MAX_MOOV_BYTES + 25).unwrap(), 0);
    assert!(validate(oversized).is_err());
    let many = (0..33).flat_map(|_| boxed(b"free", Vec::new())).collect();
    assert!(validate(artifact(vec![video, many])).is_err());
}

#[test]
fn metadata_inspection_obeys_cooperative_cancellation() {
    let bytes = artifact(vec![track(b"vide", vec![video_entry()], Vec::new())]);
    assert!(
        inspect(&mut Cursor::new(&bytes), bytes.len() as u64, || Err(
            unavailable()
        ))
        .is_err()
    );
}
