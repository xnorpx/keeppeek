use super::*;
use crate::storage::{archive::CHUNK_BYTES, catalog::archive_playback::ArchiveRead};
use std::io::{Read, Seek, SeekFrom};

#[derive(Debug, serde::Serialize)]
struct Timings {
    median_us: u64,
    p95_us: u64,
}

struct SeekBaseline {
    track: u32,
    samples: Vec<mp4::Mp4Sample>,
    timings: Timings,
}

fn measure(mut operation: impl FnMut() -> anyhow::Result<()>) -> anyhow::Result<Timings> {
    let mut durations = [0_u64; 30];
    for duration in &mut durations {
        let started = Instant::now();
        operation()?;
        *duration = u64::try_from(started.elapsed().as_micros())?;
    }
    durations.sort_unstable();
    Ok(Timings {
        median_us: durations[15],
        p95_us: durations[28],
    })
}

fn read_at(reader: &mut (impl Read + Seek), offset: u64, expected: &[u8]) -> anyhow::Result<()> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut observed = vec![0; expected.len()];
    reader.read_exact(&mut observed)?;
    assert_eq!(observed, expected);
    Ok(())
}

fn indexed_ranges(
    reader: &mut (impl Read + Seek),
    fragment: &CatalogMediaFragment,
    expected: &[u8],
) -> anyhow::Result<()> {
    for (offset, bytes) in [
        (fragment.init_offset, fragment.init_len),
        (fragment.byte_offset, fragment.byte_len),
    ] {
        let start = usize::try_from(offset)?;
        let end = usize::try_from(offset.checked_add(bytes).expect("fixture range"))?;
        read_at(reader, offset, &expected[start..end])?;
    }
    Ok(())
}

fn seek_samples(
    reader: &mut mp4::Mp4Reader<impl Read + Seek>,
    track: u32,
    expected: &[mp4::Mp4Sample],
) -> anyhow::Result<()> {
    assert_eq!(expected.len(), 3);
    for (id, expected) in [2, 1, 2].into_iter().zip(expected) {
        let observed = reader.read_sample(track, id)?.expect("fixture sample");
        assert_eq!(observed.bytes, expected.bytes);
        assert_eq!(observed.start_time, expected.start_time);
    }
    Ok(())
}

fn copy_ready(context: &Context, recording: &str) -> anyhow::Result<Job> {
    context.bind()?;
    let job = admit(
        &context.settings,
        &context.catalog,
        recording,
        "recording",
        &ObjectId::random().to_string(),
    )?;
    context.process(job.clone(), work().deadline)?;
    context.catalog.archive_job(&job.id)
}

fn local_samples(path: &std::path::Path) -> anyhow::Result<SeekBaseline> {
    let mut reader = mp4::read_mp4(std::fs::File::open(path)?)?;
    let track = reader
        .tracks()
        .iter()
        .find_map(|(id, track)| {
            (track.track_type().ok() == Some(mp4::TrackType::Video)).then_some(*id)
        })
        .expect("video track");
    assert_eq!(reader.tracks()[&track].sample_count(), 2);
    let samples = [2, 1, 2]
        .into_iter()
        .map(|id| Ok(reader.read_sample(track, id)?.expect("source sample")))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let timings = measure(|| seek_samples(&mut reader, track, &samples))?;
    Ok(SeekBaseline {
        track,
        samples,
        timings,
    })
}

#[test]
fn archive_playback_records_indexed_and_sample_seek_timings() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let (recording, path) = fixture.recording("camera")?;
    let original = std::fs::read(&path)?;
    let context = fixture.context();
    let ready = copy_ready(&context, &recording)?;
    let indexed = fragments(&context.catalog)?;
    let fragment = &indexed[0];
    let local = measure(|| indexed_ranges(&mut std::fs::File::open(&path)?, fragment, &original))?;
    let baseline = local_samples(&path)?;
    context.catalog.archive(Request::Action {
        id: ready.id.clone(),
        action: "evict".into(),
    })?;
    context.process(context.catalog.archive_job(&ready.id)?, work().deadline)?;
    let archived = fragments(&context.catalog)?[0]
        .archive
        .clone()
        .expect("archive-backed timeline");
    let first = measure(|| {
        indexed_ranges(
            &mut VerifiedReader::new(archived.clone(), work())?,
            fragment,
            &original,
        )
    })?;
    let mut cached = VerifiedReader::new(archived.clone(), work())?;
    indexed_ranges(&mut cached, fragment, &original)?;
    let repeat = measure(|| indexed_ranges(&mut cached, fragment, &original))?;
    let mut archive_mp4 = mp4::Mp4Reader::read_header(
        VerifiedReader::new(archived.clone(), work())?,
        archived.bytes,
    )?;
    let seek = measure(|| seek_samples(&mut archive_mp4, baseline.track, &baseline.samples))?;
    eprintln!(
        "{}",
        serde_json::json!({
            "measurement": "archive_playback", "samples": 30, "object_bytes": archived.bytes,
            "debug_assertions": cfg!(debug_assertions),
            "chunk_bytes": CHUNK_BYTES, "fs_cache": "warm_or_unknown", "seek_samples_per_iteration": 3,
            "video_samples": 2,
            "local_indexed": local, "archive_fresh_reader": first, "archive_cached_reader": repeat,
            "local_sample_seek": baseline.timings, "archive_sample_seek": seek,
        })
    );
    drop(archive_mp4);
    drop(cached);
    drop(context);
    fixture.close()
}

fn synthetic_archive(fixture: &Fixture, payload: &[u8]) -> anyhow::Result<ArchiveRead> {
    let identity = Root::open(&fixture.settings.root)?.identity().clone();
    let target = FilesystemArchiveTarget::open(&fixture.settings.root, &identity)?;
    let object = ObjectId::random();
    let version = target.publish(
        object,
        &mut Cursor::new(payload),
        payload.len() as u64,
        None,
        &mut |_| Ok(()),
        &work(),
    )?;
    let mut sink = transfer::VerificationSink::new(payload.len() as u64)?;
    target.read(&version, &mut sink, &work())?;
    let verified = sink.finish()?;
    assert_eq!(
        verified.digest,
        <[u8; 32]>::from(sha2::Sha256::digest(payload))
    );
    Ok(ArchiveRead {
        target_id: fixture.settings.target_id.clone(),
        root: fixture.settings.root.clone(),
        filesystem: identity.filesystem,
        root_identity: identity.directory,
        object_id: object.to_string(),
        version: version.token,
        bytes: version.bytes,
        digest: verified.digest,
        chunk_hashes: Arc::from(verified.chunk_hashes),
    })
}

#[test]
fn archive_playback_records_one_touched_chunk_in_a_larger_object() -> anyhow::Result<()> {
    let fixture = Fixture::new()?;
    let chunk = CHUNK_BYTES;
    let payload = vec![37; usize::try_from(3 * chunk + 64)?];
    let archive = synthetic_archive(&fixture, &payload)?;
    let corrupt_offset = 2 * chunk + 9;
    let mut mutation = std::fs::OpenOptions::new()
        .write(true)
        .open(archive.path())?;
    mutation.seek(SeekFrom::Start(corrupt_offset))?;
    mutation.write_all(&[38])?;
    mutation.sync_all()?;
    drop(mutation);
    assert!(read_range(&archive, corrupt_offset, 1, &work()).is_err());
    let offset = chunk + 127;
    let expected = &payload[usize::try_from(offset)?..usize::try_from(offset + 64)?];
    let local = measure(|| read_at(&mut std::fs::File::open(archive.path())?, offset, expected))?;
    let first = measure(|| {
        read_at(
            &mut VerifiedReader::new(archive.clone(), work())?,
            offset,
            expected,
        )
    })?;
    let mut cached = VerifiedReader::new(archive.clone(), work())?;
    read_at(&mut cached, offset, expected)?;
    let repeat = measure(|| read_at(&mut cached, offset, expected))?;
    eprintln!(
        "{}",
        serde_json::json!({
            "measurement": "archive_chunk", "samples": 30, "object_bytes": archive.bytes,
            "debug_assertions": cfg!(debug_assertions),
        "touched_verified_bytes": chunk, "delivered_bytes": 64, "fs_cache": "warm_or_unknown",
        "untouched_chunk_corrupt": true,
            "local_file_range": local, "archive_fresh_reader": first, "archive_cached_reader": repeat,
        })
    );
    drop(cached);
    fixture.close()
}
