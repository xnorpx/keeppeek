use super::*;
use crate::storage::{
    MediaFrame, RecordingFrame, VideoCodec, VideoFrame,
    archive::{CHUNK_BYTES, publish_fixture, take_verified_reads},
    catalog::RecordingCatalog,
    medium_term::MediumTermWriter,
};

fn recorded_fragments(directory: &Path, count: u32) -> anyhow::Result<Vec<CatalogMediaFragment>> {
    let catalog = RecordingCatalog::open(&directory.join("catalog.db"))?;
    let start = Instant::now() - Duration::from_secs(u64::from(count));
    let mut writer = MediumTermWriter::create_with_catalog(
        directory,
        "front-door/sub",
        start,
        8192,
        catalog.handle(),
    )?;
    let keyframe =
        super::super::tests::fixture_video_keyframe("cc-4k-640x360-h264.mp4", mp4::MediaType::H264);
    for second in 0..count {
        let elapsed = Duration::from_secs(u64::from(second));
        writer.append_one(RecordingFrame {
            source_rtp: None,
            received_at: start + elapsed,
            timestamp: Some(elapsed),
            frame: MediaFrame::Video(VideoFrame {
                codec: VideoCodec::H264,
                is_keyframe: true,
                width: 640,
                height: 368,
                data: keyframe.clone(),
            }),
        })?;
    }
    writer.finalize()?;
    let fragments = catalog
        .handle()
        .media_fragments_in_range("front-door/sub", 0, i64::MAX)?;
    catalog.shutdown();
    assert_eq!(fragments.len(), count as usize);
    Ok(fragments)
}

fn encode_test_batch(
    fragments: Vec<CatalogMediaFragment>,
) -> Result<StoredMediaBatch, ControlCommandError> {
    encode(
        "archive-budget",
        1,
        fragments[0].start_ms,
        DataChannelTarget::Reliable,
        fragments,
        StoredMediaIdentifiers::default(),
    )
}

fn compare_verified_ranges(count: u32, start: usize, expected_chunks: usize) -> anyhow::Result<()> {
    let directory = std::env::temp_dir().join(format!(
        "keeppeek-verified-playback-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir(&directory)?;
    let fragments = recorded_fragments(&directory, count)?;
    let bytes = std::fs::read(&fragments[0].path)?;
    let (archive_dir, target, descriptor) = publish_fixture(&bytes)?;
    let local = fragments[start..start + 10].to_vec();
    let mut archived = local.clone();
    for fragment in &mut archived {
        fragment.archive = Some(descriptor.clone());
    }
    let mut touched = std::collections::BTreeSet::new();
    for fragment in &archived {
        for (offset, length) in [
            (fragment.init_offset, fragment.init_len),
            (fragment.byte_offset, fragment.byte_len),
        ] {
            touched.extend(offset / CHUNK_BYTES..(offset + length).div_ceil(CHUNK_BYTES));
        }
    }
    assert_eq!(touched.len(), expected_chunks);
    let expected = touched
        .iter()
        .map(|index| (descriptor.bytes - index * CHUNK_BYTES).min(CHUNK_BYTES))
        .sum::<u64>();
    let baseline = encode_test_batch(local).unwrap();
    take_verified_reads();
    let start = Instant::now();
    let observed = encode_test_batch(archived).unwrap();
    let elapsed = start.elapsed();
    let (calls, total) = take_verified_reads();
    assert!(observed.archive);
    let messages = |batch: StoredMediaBatch| {
        batch
            .messages
            .into_iter()
            .map(|m| m.message)
            .collect::<Vec<_>>()
    };
    assert_eq!(messages(baseline), messages(observed));
    println!(
        "archive-encode fragments=10 file_bytes={} distinct_chunks={} verified_reads={calls} verified_bytes={total} elapsed_us={}",
        descriptor.bytes,
        touched.len(),
        elapsed.as_micros()
    );
    drop(target);
    std::fs::remove_dir_all(archive_dir)?;
    std::fs::remove_dir_all(directory)?;
    assert_eq!((calls, total as u64), (touched.len(), expected));
    Ok(())
}

#[test]
fn archive_playback_verifies_one_chunk_once_for_ten_real_fragments() -> anyhow::Result<()> {
    compare_verified_ranges(122, 5, 1)
}

#[test]
fn archive_playback_reuses_initialization_when_ten_real_fragments_cross_chunks()
-> anyhow::Result<()> {
    compare_verified_ranges(400, 350, 2)
}

#[test]
fn archive_batch_fences_changed_verification_evidence_and_fresh_requests() -> anyhow::Result<()> {
    let directory =
        std::env::temp_dir().join(format!("keeppeek-archive-fence-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    let mut fragments = recorded_fragments(&directory, 2)?;
    let bytes = std::fs::read(&fragments[0].path)?;
    let (root, target, descriptor) = publish_fixture(&bytes)?;
    for fragment in &mut fragments {
        fragment.archive = Some(descriptor.clone());
    }
    assert!(encode_test_batch(fragments.clone()).is_ok());
    let mut changed = fragments.clone();
    changed[1].archive.as_mut().unwrap().chunk_hashes = vec![0; 32].into();
    let unavailable = |batch: Result<StoredMediaBatch, ControlCommandError>| {
        let error = batch.err().expect("no partial batch may be returned");
        assert_eq!(error._http_status, 503);
        assert_eq!(
            error.message,
            "Archive media is unavailable or failed integrity verification. Check the archive target and retry."
        );
    };
    unavailable(encode_test_batch(changed));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(descriptor.path())?;
    std::io::Write::write_all(&mut file, &[bytes[0] ^ 1])?;
    file.sync_all()?;
    drop(file);
    unavailable(encode_test_batch(fragments));
    drop(target);
    std::fs::remove_dir_all(root)?;
    std::fs::remove_dir_all(directory)?;
    Ok(())
}
