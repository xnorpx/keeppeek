use super::*;

struct Packet {
    track: u32,
    timescale: u32,
    sample: mp4::Mp4Sample,
}

pub(super) fn write(path: &Path) -> Vec<CatalogMediaFragment> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("crates/test-camera/testdata/flash-tone-lead-200ms.mp4");
    let mut reader = mp4::read_mp4(File::open(fixture).unwrap()).unwrap();
    let tracks = configurations(&reader);
    let mut packets = Vec::new();
    for &(id, ref track) in &tracks {
        assert!(reader.sample_count(id).unwrap() <= 256);
        for index in 1..=reader.sample_count(id).unwrap() {
            packets.push(Packet {
                track: id,
                timescale: track.timescale,
                sample: reader.read_sample(id, index).unwrap().unwrap(),
            });
        }
    }
    packets.sort_by(|left, right| {
        (u128::from(left.sample.start_time) * u128::from(right.timescale))
            .cmp(&(u128::from(right.sample.start_time) * u128::from(left.timescale)))
            .then(left.track.cmp(&right.track))
    });
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec!["iso6".parse().unwrap()],
        timescale: 1_000,
    };
    let mut writer = mp4::FragmentedMp4Writer::write_start(
        File::create(path).unwrap(),
        &config,
        &tracks
            .into_iter()
            .map(|(_, config)| config)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let initialization = writer.initialization();
    let fragments = append(&mut writer, packets);
    drop(writer);
    fragments
        .into_iter()
        .map(|(range, start, end)| CatalogMediaFragment {
            archive: None,
            recording_id: "calibration".into(),
            recording_started_at_ms: 0,
            path: path.to_string_lossy().into_owned(),
            init_offset: initialization.offset,
            init_len: initialization.size,
            sequence: u64::from(range.sequence_number),
            start_ms: i64::try_from(start * 1_000 / 90_000).unwrap(),
            duration_ms: (end - start) * 1_000 / 90_000,
            byte_offset: range.range.offset,
            byte_len: range.range.size,
        })
        .collect()
}

fn configurations<R: Read + Seek>(reader: &mp4::Mp4Reader<R>) -> Vec<(u32, mp4::TrackConfig)> {
    let mut tracks = reader
        .tracks()
        .iter()
        .map(|(&id, track)| {
            (
                id,
                mp4::TrackConfig {
                    track_type: track.track_type().unwrap(),
                    timescale: track.timescale(),
                    language: track.language().into(),
                    media_conf: track.media_config_for_description(1).unwrap(),
                },
            )
        })
        .collect::<Vec<_>>();
    tracks.sort_unstable_by_key(|(id, _)| *id);
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].1.track_type, mp4::TrackType::Video);
    assert_eq!(tracks[1].1.track_type, mp4::TrackType::Audio);
    tracks
}

fn append(
    writer: &mut mp4::FragmentedMp4Writer<File>,
    packets: Vec<Packet>,
) -> Vec<(mp4::Mp4FragmentInfo, u64, u64)> {
    let mut fragments = Vec::new();
    let mut start = 0;
    let mut end = 0;
    for packet in packets {
        if packet.track == 1 {
            if packet.sample.is_sync && writer.has_pending_samples() {
                let fragment = writer
                    .flush_fragment_before_sample(1, packet.sample.start_time)
                    .unwrap()
                    .unwrap();
                fragments.push((fragment, start, packet.sample.start_time));
                start = packet.sample.start_time;
            }
            end = packet.sample.start_time + u64::from(packet.sample.duration);
        }
        writer.write_sample(packet.track, packet.sample).unwrap();
    }
    fragments.push((writer.write_end().unwrap().unwrap(), start, end));
    fragments
}

pub(super) fn stored(
    fragments: &[CatalogMediaFragment],
    offset: i32,
    first: usize,
    origin_ms: i64,
    path: &Path,
) {
    let bytes = std::fs::read(&fragments[0].path).unwrap();
    let periods = fragments
        .iter()
        .map(|fragment| recorded::Period {
            initialization: range(&bytes, fragment.init_offset, fragment.init_len),
            fragment: range(&bytes, fragment.byte_offset, fragment.byte_len),
            recording_started_at_ms: fragment.recording_started_at_ms,
        })
        .collect::<Vec<_>>();
    let mut writer = File::create(path).unwrap();
    let mut initialization = None;
    for (index, fragment) in fragments.iter().enumerate().skip(first) {
        if offset == 0 {
            if index == first {
                writer.write_all(periods[index].initialization).unwrap();
            }
            writer.write_all(periods[index].fragment).unwrap();
            continue;
        }
        let output = recorded::remux(
            periods[index],
            &periods,
            recorded::Options {
                audio_offset_ms: offset,
                playback_origin_ms: origin_ms,
                window_start_ms: fragment.start_ms,
                window_end_ms: fragment.start_ms + i64::try_from(fragment.duration_ms).unwrap(),
            },
            &recorded::Work {
                deadline: Instant::now() + Duration::from_secs(5),
                cancelled: &|| false,
            },
        )
        .unwrap();
        if let Some(previous) = &initialization {
            assert_eq!(previous, &output.initialization);
        } else {
            writer.write_all(&output.initialization).unwrap();
            initialization = Some(output.initialization);
        }
        writer.write_all(&output.fragment).unwrap();
    }
}

fn range(bytes: &[u8], offset: u64, length: u64) -> &[u8] {
    &bytes[usize::try_from(offset).unwrap()..usize::try_from(offset + length).unwrap()]
}
