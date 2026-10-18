use super::*;
use std::io::Cursor;

fn video() -> mp4::TrackConfig {
    mp4::TrackConfig {
        track_type: mp4::TrackType::Video,
        timescale: 90_000,
        language: "und".into(),
        media_conf: mp4::MediaConfig::AvcConfig(mp4::AvcConfig {
            width: 640,
            height: 360,
            seq_param_set: vec![0x67, 0x42, 0, 0x1f],
            pic_param_set: vec![0x68, 0xce, 0x3c, 0x80],
        }),
    }
}

fn audio() -> mp4::TrackConfig {
    mp4::TrackConfig {
        track_type: mp4::TrackType::Audio,
        timescale: 48_000,
        language: "und".into(),
        media_conf: mp4::MediaConfig::AacConfig(mp4::AacConfig {
            bitrate: 64_000,
            profile: mp4::AudioObjectType::AacLowComplexity,
            freq_index: mp4::SampleFreqIndex::Freq48000,
            chan_conf: mp4::ChannelConfig::Mono,
        }),
    }
}

fn sample(start_time: u64, duration: u32, rendering_offset: i32) -> mp4::Mp4Sample {
    mp4::Mp4Sample {
        start_time,
        duration,
        rendering_offset,
        is_sync: true,
        bytes: bytes::Bytes::from_static(&[0, 0, 0, 1, 0x65]),
    }
}

fn source(tracks: &[mp4::TrackConfig], samples: Vec<(u32, mp4::Mp4Sample)>) -> PathBuf {
    let path = std::env::temp_dir().join(format!("export-envelope-{}.mp4", uuid::Uuid::new_v4()));
    let config = mp4::Mp4Config {
        major_brand: "iso6".parse().unwrap(),
        minor_version: 1,
        compatible_brands: vec!["iso6".parse().unwrap()],
        timescale: 1_000,
    };
    let mut writer =
        mp4::FragmentedMp4Writer::write_start(File::create(&path).unwrap(), &config, tracks)
            .unwrap();
    for (track, sample) in samples {
        writer.write_sample(track, sample).unwrap();
    }
    writer.write_end().unwrap();
    path
}

fn fragment(path: &Path, id: &str, start_ms: i64) -> CatalogMediaFragment {
    CatalogMediaFragment {
        recording_id: id.into(),
        recording_started_at_ms: start_ms,
        path: path.to_string_lossy().into_owned(),
        init_offset: 0,
        init_len: 0,
        sequence: 1,
        start_ms,
        duration_ms: 10_000,
        byte_offset: 0,
        byte_len: 1,
        archive: None,
    }
}

fn export(fragments: &[CatalogMediaFragment], end_ms: i64) -> (ExportArtifact, Cursor<Vec<u8>>) {
    let mut output = Cursor::new(Vec::new());
    let artifact =
        export_fragment_ranges_to_writer(fragments, end_ms, &mut output, || false, |_| {}).unwrap();
    output.set_position(0);
    (artifact, output)
}

#[test]
fn export_envelope_includes_crossing_end_video_and_fractional_aac() {
    let path = source(
        &[video(), audio()],
        vec![(1, sample(0, 90_000, 0)), (2, sample(47_520, 1_024, 0))],
    );
    let (artifact, output) = export(&[fragment(&path, "one", 10_000)], 10_995);
    assert_eq!(
        artifact.media_bounds,
        Some(ExportMediaBounds {
            start_ms: 10_000,
            end_ms: 11_012
        })
    );
    let mut reader = mp4::Mp4Reader::read_header(output, artifact.bytes).unwrap();
    let last = reader.read_sample(2, 1).unwrap().unwrap();
    assert_eq!(last.start_time, 47_520);
    assert_eq!(last.duration, 1_024);
    assert!(artifact.media_bounds.unwrap().end_ms > artifact.delivered_end_ms);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn export_envelope_includes_signed_composition_offsets_and_preroll() {
    let path = source(
        &[video()],
        vec![(1, sample(0, 3_600, -1)), (1, sample(3_600, 3_600, 4_501))],
    );
    let (artifact, output) = export(&[fragment(&path, "one", 10_000)], 10_060);
    assert_eq!(
        artifact.media_bounds,
        Some(ExportMediaBounds {
            start_ms: 9_999,
            end_ms: 10_131
        })
    );
    let mut reader = mp4::Mp4Reader::read_header(output, artifact.bytes).unwrap();
    assert_eq!(
        reader.read_sample(1, 1).unwrap().unwrap().rendering_offset,
        -1
    );
    assert_eq!(
        reader.read_sample(1, 2).unwrap().unwrap().rendering_offset,
        4_501
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn export_envelope_ignores_duplicate_samples_that_are_not_emitted() {
    let first = source(&[video()], vec![(1, sample(900, 3_600, 0))]);
    let duplicate = source(&[video()], vec![(1, sample(0, 3_600, 90_000))]);
    let (artifact, output) = export(
        &[
            fragment(&first, "one", 9_990),
            fragment(&duplicate, "two", 10_000),
        ],
        10_040,
    );
    assert_eq!(
        artifact.media_bounds,
        Some(ExportMediaBounds {
            start_ms: 10_000,
            end_ms: 10_040
        })
    );
    let reader = mp4::Mp4Reader::read_header(output, artifact.bytes).unwrap();
    assert_eq!(reader.sample_count(1).unwrap(), 1);
    std::fs::remove_file(first).unwrap();
    std::fs::remove_file(duplicate).unwrap();
}

#[test]
fn export_envelope_tracks_writer_rewritten_audio_duration_and_preserves_gaps() {
    let path = source(
        &[video(), audio()],
        vec![
            (1, sample(0, 90_000, 0)),
            (2, sample(0, 480, 48_000)),
            (2, sample(96_000, 480, 0)),
        ],
    );
    let later = source(&[video(), audio()], vec![(1, sample(0, 90_000, 0))]);
    let (artifact, output) = export(
        &[
            fragment(&path, "one", 10_000),
            fragment(&later, "two", 15_000),
        ],
        16_000,
    );
    assert_eq!(
        artifact.media_bounds,
        Some(ExportMediaBounds {
            start_ms: 10_000,
            end_ms: 16_000
        })
    );
    let mut reader = mp4::Mp4Reader::read_header(output, artifact.bytes).unwrap();
    assert_eq!(reader.read_sample(2, 1).unwrap().unwrap().duration, 96_000);
    assert_eq!(
        reader.read_sample(1, 2).unwrap().unwrap().start_time,
        450_000
    );
    std::fs::remove_file(path).unwrap();
    std::fs::remove_file(later).unwrap();
}

#[test]
fn export_envelope_rejects_invalid_timing_permanently_and_empty_output() {
    let mut envelope = ExportEnvelope::new(10_000);
    assert_eq!(envelope.bounds(), None);
    envelope.sample(1, 1_000, &sample(0, 1, 0));
    envelope.flush();
    assert!(envelope.bounds().is_some());
    envelope.sample(1, 0, &sample(0, 1, 0));
    envelope.flush();
    envelope.sample(1, 1_000, &sample(1, 1, 0));
    envelope.flush();
    assert_eq!(envelope.bounds(), None);
    let mut overflow = ExportEnvelope::new(i64::MAX);
    overflow.sample(1, 1, &sample(u64::MAX, 1, i32::MAX));
    overflow.flush();
    assert_eq!(overflow.bounds(), None);
}

#[test]
fn export_envelope_observes_duration_rewriting_before_composition_offset() {
    let path = source(
        &[video(), audio()],
        vec![
            (1, sample(0, 90_000, 0)),
            (2, sample(0, 480, 48_000)),
            (2, sample(48_000, 480, 0)),
            (2, sample(96_000, 480, 0)),
        ],
    );
    let mut first = fragment(&path, "one", 10_000);
    first.duration_ms = 10;
    let mut last = fragment(&path, "one", 12_000);
    last.recording_started_at_ms = 10_000;
    last.duration_ms = 10;
    let (artifact, output) = export(&[first, last], 12_010);
    let mut reader = mp4::Mp4Reader::read_header(output, artifact.bytes).unwrap();
    let first = reader.read_sample(2, 1).unwrap().unwrap();
    assert_eq!(first.duration, 96_000);
    assert_eq!(first.rendering_offset, 48_000);
    assert_eq!(reader.sample_count(2).unwrap(), 2);
    assert_eq!(
        artifact.media_bounds,
        Some(ExportMediaBounds {
            start_ms: 10_000,
            end_ms: 13_000
        })
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn export_envelope_can_exceed_two_minutes_despite_bounded_request() {
    let path = source(&[video()], vec![(1, sample(0, 10_800_090, 0))]);
    let (artifact, output) = export(&[fragment(&path, "one", 10_000)], 130_000);
    let mut reader = mp4::Mp4Reader::read_header(output, artifact.bytes).unwrap();
    assert_eq!(
        reader.read_sample(1, 1).unwrap().unwrap().duration,
        10_800_090
    );
    assert_eq!(
        artifact.media_bounds,
        Some(ExportMediaBounds {
            start_ms: 10_000,
            end_ms: 130_001
        })
    );
    std::fs::remove_file(path).unwrap();
}

fn archived_source(bytes: &[u8]) -> anyhow::Result<ArchiveRead> {
    use crate::storage::{
        archive::{
            filesystem::FilesystemArchiveTarget,
            target::{ArchiveTarget, ObjectId},
        },
        long_term::inspection::removal::create_private_directory,
        volumes::root::Root,
    };
    use sha2::{Digest, Sha256};
    let base = std::env::temp_dir();
    #[cfg(unix)]
    let base = std::fs::canonicalize(base)?;
    let parent = cap_std::fs::Dir::open_ambient_dir(&base, cap_std::ambient_authority())?;
    let name = format!("export-envelope-archive-{}", uuid::Uuid::new_v4());
    drop(create_private_directory(
        &parent,
        std::ffi::OsStr::new(&name),
    )?);
    let root = base.join(name);
    let identity = Root::open(&root)?.identity().clone();
    let target = FilesystemArchiveTarget::open(&root, &identity)?;
    let cancelled = || false;
    let work = Work {
        deadline: Instant::now() + Duration::from_secs(30),
        cancelled: &cancelled,
    };
    let object = ObjectId::random();
    let version = target.publish(
        object,
        &mut Cursor::new(bytes),
        u64::try_from(bytes.len())?,
        None,
        &mut |_| Ok(()),
        &work,
    )?;
    assert!(u64::try_from(bytes.len())? <= crate::storage::archive::CHUNK_BYTES);
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    Ok(ArchiveRead {
        target_id: "envelope-fixture".into(),
        root,
        filesystem: identity.filesystem,
        root_identity: identity.directory,
        object_id: object.to_string(),
        version: version.token,
        bytes: version.bytes,
        digest,
        chunk_hashes: digest.to_vec().into(),
    })
}

#[test]
fn archive_export_envelope_matches_local_after_primary_is_removed() -> anyhow::Result<()> {
    let path = source(
        &[video(), audio()],
        vec![(1, sample(0, 90_000, -1)), (2, sample(47_520, 1_024, 0))],
    );
    let mut selected = fragment(&path, "one", 10_000);
    let (local, local_output) = export(&[selected.clone()], 10_995);
    let archive = archived_source(&std::fs::read(&path)?)?;
    selected.archive = Some(archive.clone());
    std::fs::remove_file(&path)?;
    assert!(!path.exists());
    let (archived, archived_output) = export(&[selected], 10_995);
    assert_eq!(archived.media_bounds, local.media_bounds);
    assert_eq!(
        archived.media_bounds,
        Some(ExportMediaBounds {
            start_ms: 9_999,
            end_ms: 11_012
        })
    );
    assert_eq!(archived_output.get_ref(), local_output.get_ref());
    let mut reader = mp4::Mp4Reader::read_header(archived_output, archived.bytes)?;
    assert_eq!(reader.read_sample(1, 1)?.unwrap().rendering_offset, -1);
    assert_eq!(reader.read_sample(2, 1)?.unwrap().duration, 1_024);
    std::fs::remove_dir_all(archive.root)?;
    Ok(())
}
