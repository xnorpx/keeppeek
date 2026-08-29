use crate::storage::catalog::{
    backfill_recording_coverage, catalog_coverage, initialize_schema, media_fragments_in_range,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use turso::transaction::TransactionBehavior;

const DAY_MS: i64 = 86_400_000;
const STREAMS: [&str; 2] = ["main", "sub"];
pub const BENCHMARK_FRAGMENTS_PER_RECORDING: u32 = 128;
const FRAGMENT_DURATION_MS: i64 = DAY_MS / BENCHMARK_FRAGMENTS_PER_RECORDING as i64;

#[derive(Debug, Clone)]
pub struct BenchmarkRecordingSeed {
    pub recording_id: String,
    pub stream_key: String,
    pub source_id: String,
    pub stream_id: String,
    pub path: PathBuf,
    pub started_at_ms: i64,
    pub file_len: u64,
    pub keyframe_offset: u64,
    pub keyframe_len: u64,
}

#[derive(Debug, Clone)]
pub struct EventKeyframeCorpusConfig {
    pub catalog_path: PathBuf,
    pub recordings: Vec<BenchmarkRecordingSeed>,
    pub target_bytes: u64,
    pub camera_count: u32,
    pub day_count: u32,
    pub start_time_ms: i64,
    pub batch_events: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventKeyframeCorpusSummary {
    pub logical_database_bytes: u64,
    pub recording_count: u64,
    pub fragment_count: u64,
    pub keyframe_count: u64,
    pub event_count: u64,
    pub event_keyframe_link_count: u64,
}

#[derive(Debug, Clone)]
pub struct RecordingCoverageCorpusConfig {
    pub catalog_path: PathBuf,
    pub camera_count: u32,
    pub day_count: u32,
    pub start_time_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordingCoverageBenchmarkReport {
    pub camera_count: u32,
    pub stream_count: usize,
    pub day_count: u32,
    pub fragment_count: u64,
    pub samples: usize,
    pub baseline_median_ms: f64,
    pub baseline_p95_ms: f64,
    pub snapshot_median_ms: f64,
    pub snapshot_p95_ms: f64,
    pub median_delta_percent: f64,
    pub retained_ranges: usize,
    pub retained_range_limit: usize,
    pub snapshot_owned_bytes: usize,
}

pub fn build_recording_coverage_corpus(
    config: &RecordingCoverageCorpusConfig,
) -> anyhow::Result<u64> {
    if config.camera_count == 0 || config.day_count == 0 {
        anyhow::bail!("recording coverage corpus requires at least one camera and day");
    }
    let mut recordings = Vec::with_capacity(
        usize::try_from(config.camera_count)?
            .saturating_mul(usize::try_from(config.day_count)?)
            .saturating_mul(STREAMS.len()),
    );
    for camera_index in 0..config.camera_count {
        for day_index in 0..config.day_count {
            for stream_id in STREAMS {
                let recording_id = benchmark_recording_id(camera_index, day_index, stream_id);
                recordings.push(BenchmarkRecordingSeed {
                    path: config
                        .catalog_path
                        .with_file_name(format!("{recording_id}.mp4")),
                    recording_id,
                    stream_key: format!("{}/{stream_id}", benchmark_camera_id(camera_index)),
                    source_id: benchmark_camera_id(camera_index),
                    stream_id: stream_id.to_owned(),
                    started_at_ms: config
                        .start_time_ms
                        .saturating_add(i64::from(day_index).saturating_mul(DAY_MS)),
                    file_len: 16 * 1_048_576,
                    keyframe_offset: 1_024,
                    keyframe_len: 32 * 1_024,
                });
            }
        }
    }
    if let Some(parent) = config.catalog_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if config.catalog_path.exists() {
        anyhow::bail!(
            "benchmark catalog already exists at {}",
            config.catalog_path.display()
        );
    }
    pollster::block_on(async {
        let path = path_text(&config.catalog_path)?;
        let database = turso::Builder::new_local(&path).build().await?;
        let mut connection = database.connect()?;
        initialize_schema(&connection).await?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")
            .await?;
        insert_recordings(&mut connection, &recordings).await?;
        backfill_recording_coverage(&connection).await
    })?;
    Ok(u64::try_from(recordings.len())?
        .saturating_mul(u64::from(BENCHMARK_FRAGMENTS_PER_RECORDING)))
}

pub fn measure_recording_coverage(
    config: &RecordingCoverageCorpusConfig,
    samples: usize,
) -> anyhow::Result<RecordingCoverageBenchmarkReport> {
    if samples == 0 {
        anyhow::bail!("recording coverage benchmark requires at least one sample");
    }
    let end_ms = config
        .start_time_ms
        .saturating_add(i64::from(config.day_count).saturating_mul(DAY_MS));
    let window = config.start_time_ms..end_ms;
    pollster::block_on(async {
        let path = path_text(&config.catalog_path)?;
        let database = turso::Builder::new_local(&path).build().await?;
        let connection = database.connect()?;
        let _ = catalog_coverage(&connection, window.clone()).await?;
        let _ = materialized_coverage(&connection, config.camera_count, window.clone()).await?;
        let mut snapshot_samples = Vec::with_capacity(samples);
        let mut baseline_samples = Vec::with_capacity(samples);
        let mut retained_ranges = 0;
        let mut stream_count = 0;
        let mut fragment_count = 0;
        let mut snapshot_owned_bytes = 0;
        for _ in 0..samples {
            let started = std::time::Instant::now();
            let snapshot = catalog_coverage(&connection, window.clone()).await?;
            snapshot_samples.push(started.elapsed().as_nanos());
            retained_ranges = snapshot
                .streams
                .iter()
                .map(|stream| stream.ranges.len())
                .sum();
            stream_count = snapshot.streams.len();
            snapshot_owned_bytes = coverage_snapshot_owned_bytes(&snapshot);

            let started = std::time::Instant::now();
            fragment_count =
                materialized_coverage(&connection, config.camera_count, window.clone()).await?;
            baseline_samples.push(started.elapsed().as_nanos());
        }
        snapshot_samples.sort_unstable();
        baseline_samples.sort_unstable();
        let snapshot_median = percentile(&snapshot_samples, 50);
        let baseline_median = percentile(&baseline_samples, 50);
        Ok(RecordingCoverageBenchmarkReport {
            camera_count: config.camera_count,
            stream_count,
            day_count: config.day_count,
            fragment_count,
            samples,
            baseline_median_ms: nanos_ms(baseline_median),
            baseline_p95_ms: nanos_ms(percentile(&baseline_samples, 95)),
            snapshot_median_ms: nanos_ms(snapshot_median),
            snapshot_p95_ms: nanos_ms(percentile(&snapshot_samples, 95)),
            median_delta_percent: if baseline_median == 0 {
                0.0
            } else {
                (snapshot_median as f64 - baseline_median as f64) * 100.0 / baseline_median as f64
            },
            retained_ranges,
            retained_range_limit: usize::try_from(config.camera_count)?
                .saturating_mul(STREAMS.len())
                .saturating_mul(256),
            snapshot_owned_bytes,
        })
    })
}

fn coverage_snapshot_owned_bytes(
    snapshot: &crate::storage::catalog::CatalogCoverageSnapshot,
) -> usize {
    std::mem::size_of_val(snapshot)
        .saturating_add(
            snapshot
                .streams
                .capacity()
                .saturating_mul(std::mem::size_of::<
                    crate::storage::catalog::CatalogStreamCoverage,
                >()),
        )
        .saturating_add(snapshot.streams.iter().fold(0usize, |total, stream| {
            total
                .saturating_add(stream.stream_id.capacity())
                .saturating_add(stream.source_id.as_ref().map_or(0, String::capacity))
                .saturating_add(
                    stream
                        .logical_stream_id
                        .as_ref()
                        .map_or(0, String::capacity),
                )
                .saturating_add(
                    stream
                        .ranges
                        .capacity()
                        .saturating_mul(std::mem::size_of::<(i64, i64)>()),
                )
                .saturating_add(
                    stream
                        .buckets
                        .capacity()
                        .saturating_mul(std::mem::size_of::<
                            crate::storage::catalog::CatalogCoverageBucket,
                        >()),
                )
                .saturating_add(
                    stream
                        .deletions
                        .capacity()
                        .saturating_mul(std::mem::size_of::<
                            crate::storage::catalog::CatalogDeletionRange,
                        >()),
                )
        }))
}

async fn materialized_coverage(
    connection: &turso::Connection,
    camera_count: u32,
    window: std::ops::Range<i64>,
) -> anyhow::Result<u64> {
    let mut fragments = 0u64;
    for camera_index in 0..camera_count {
        for stream_id in STREAMS {
            let stream_key = format!("{}/{stream_id}", benchmark_camera_id(camera_index));
            fragments = fragments.saturating_add(u64::try_from(
                media_fragments_in_range(connection, &stream_key, window.start, window.end)
                    .await?
                    .len(),
            )?);
        }
    }
    Ok(fragments)
}

fn percentile(sorted: &[u128], percentile: usize) -> u128 {
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
    sorted[rank.saturating_sub(1)]
}

fn nanos_ms(value: u128) -> f64 {
    value as f64 / 1_000_000.0
}

pub fn build_event_keyframe_corpus(
    config: &EventKeyframeCorpusConfig,
) -> anyhow::Result<EventKeyframeCorpusSummary> {
    pollster::block_on(build_event_keyframe_corpus_async(config))
}

pub fn benchmark_event_id(index: u64) -> String {
    format!("event-{index:016x}")
}

async fn build_event_keyframe_corpus_async(
    config: &EventKeyframeCorpusConfig,
) -> anyhow::Result<EventKeyframeCorpusSummary> {
    validate_config(config)?;
    if let Some(parent) = config.catalog_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if config.catalog_path.exists() {
        anyhow::bail!(
            "benchmark catalog already exists at {}",
            config.catalog_path.display()
        );
    }

    let path = path_text(&config.catalog_path)?;
    let database = turso::Builder::new_local(&path).build().await?;
    let mut connection = database.connect()?;
    initialize_schema(&connection).await?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = NORMAL;")
        .await?;

    insert_recordings(&mut connection, &config.recordings).await?;

    let mut event_count = 0u64;
    let target_bytes = config.target_bytes.max(1);
    loop {
        insert_event_batch(&mut connection, config, event_count, config.batch_events).await?;
        event_count = event_count.saturating_add(config.batch_events);
        let logical_bytes = logical_database_bytes(&connection).await?;
        if event_count.is_multiple_of(config.batch_events.saturating_mul(10))
            || logical_bytes >= target_bytes
        {
            eprintln!(
                "seeded {event_count} events ({:.1} MiB logical database)",
                logical_bytes as f64 / 1_048_576.0
            );
        }
        if logical_bytes >= target_bytes {
            quick_check(&connection).await?;
            return Ok(EventKeyframeCorpusSummary {
                logical_database_bytes: logical_bytes,
                recording_count: config.recordings.len() as u64,
                fragment_count: (config.recordings.len() as u64)
                    .saturating_mul(u64::from(BENCHMARK_FRAGMENTS_PER_RECORDING)),
                keyframe_count: (config.recordings.len() as u64)
                    .saturating_mul(u64::from(BENCHMARK_FRAGMENTS_PER_RECORDING)),
                event_count,
                event_keyframe_link_count: event_count.saturating_mul(STREAMS.len() as u64),
            });
        }
    }
}

fn validate_config(config: &EventKeyframeCorpusConfig) -> anyhow::Result<()> {
    if config.camera_count == 0 || config.day_count == 0 {
        anyhow::bail!("benchmark corpus requires at least one camera and day");
    }
    if config.batch_events == 0 {
        anyhow::bail!("benchmark corpus event batch size must be positive");
    }
    let expected_recordings = usize::try_from(config.camera_count)?
        .checked_mul(usize::try_from(config.day_count)?)
        .and_then(|count| count.checked_mul(STREAMS.len()))
        .ok_or_else(|| anyhow::anyhow!("benchmark recording count overflows"))?;
    if config.recordings.len() != expected_recordings {
        anyhow::bail!(
            "benchmark corpus expected {expected_recordings} recordings, got {}",
            config.recordings.len()
        );
    }
    Ok(())
}

async fn insert_recordings(
    connection: &mut turso::Connection,
    recordings: &[BenchmarkRecordingSeed],
) -> anyhow::Result<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut recording_statement = transaction
        .prepare(
            "INSERT INTO recording_files (
                 id, stream_id, source_id, logical_stream_id,
                 started_at_ms, ended_at_ms, path,
                 init_offset, init_len, finalized, finalized_at_ms, file_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0, 1, ?6, ?8)",
        )
        .await?;
    let mut fragment_statement = transaction
        .prepare(
            "INSERT INTO recording_fragments (
                 recording_id, sequence, start_ms, duration_ms,
                 byte_offset, byte_len, random_access
             ) VALUES (?1, ?2, ?3, ?4, 0, ?5, 1)",
        )
        .await?;
    let mut keyframe_statement = transaction
        .prepare(
            "INSERT INTO recording_keyframes (
                 recording_id, fragment_sequence, byte_offset, byte_len
             ) VALUES (?1, ?2, ?3, ?4)",
        )
        .await?;
    for recording in recordings {
        let end_ms = recording.started_at_ms.saturating_add(DAY_MS);
        recording_statement
            .execute(turso::params![
                recording.recording_id.clone(),
                recording.stream_key.clone(),
                recording.source_id.clone(),
                recording.stream_id.clone(),
                recording.started_at_ms,
                end_ms,
                path_text(&recording.path)?,
                to_i64(recording.file_len, "benchmark recording length")?,
            ])
            .await?;
        for sequence in 1..=BENCHMARK_FRAGMENTS_PER_RECORDING {
            let fragment_start_ms = recording.started_at_ms.saturating_add(
                i64::from(sequence.saturating_sub(1)).saturating_mul(FRAGMENT_DURATION_MS),
            );
            fragment_statement
                .execute(turso::params![
                    recording.recording_id.clone(),
                    i64::from(sequence),
                    fragment_start_ms,
                    FRAGMENT_DURATION_MS,
                    to_i64(recording.file_len, "benchmark recording length")?,
                ])
                .await?;
            keyframe_statement
                .execute(turso::params![
                    recording.recording_id.clone(),
                    i64::from(sequence),
                    to_i64(recording.keyframe_offset, "benchmark keyframe offset")?,
                    to_i64(recording.keyframe_len, "benchmark keyframe length")?,
                ])
                .await?;
        }
    }
    drop(recording_statement);
    drop(fragment_statement);
    drop(keyframe_statement);
    transaction.commit().await?;
    Ok(())
}

async fn insert_event_batch(
    connection: &mut turso::Connection,
    config: &EventKeyframeCorpusConfig,
    first_event: u64,
    event_count: u64,
) -> anyhow::Result<()> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut event_statement = transaction
        .prepare(
            "INSERT INTO recording_events (
                 id, camera_id, stream, source, kind, start_time_ms,
                 end_time_ms, confidence, bbox_json, zone, thumbnail_filename
             ) VALUES (?1, ?2, NULL, 'camera', ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .await?;
    let mut link_statement = transaction
        .prepare(
            "INSERT INTO recording_event_keyframes (
                 event_id, stream_id, recording_id, fragment_sequence
             ) VALUES (?1, ?2, ?3, ?4)",
        )
        .await?;
    let bucket_count = u64::from(config.camera_count) * u64::from(config.day_count);
    for index in first_event..first_event.saturating_add(event_count) {
        let bucket = index % bucket_count;
        let camera_index = u32::try_from(bucket % u64::from(config.camera_count))?;
        let day_index = u32::try_from(bucket / u64::from(config.camera_count))?;
        let event_id = benchmark_event_id(index);
        let camera_id = benchmark_camera_id(camera_index);
        let day_start_ms = config
            .start_time_ms
            .saturating_add(i64::from(day_index).saturating_mul(DAY_MS));
        let offset_ms = i64::try_from(splitmix64(index) % u64::try_from(DAY_MS - 60_000)?)?;
        let start_ms = day_start_ms.saturating_add(offset_ms);
        let end_ms = start_ms.saturating_add(30_000 + i64::try_from(index % 30_000)?);
        let event_type = match index % 5 {
            0 => "person",
            1 => "vehicle",
            _ => "motion",
        };
        let zone = benchmark_zone(index, camera_index, day_index);
        let thumbnail = format!("{event_id}.jpg");
        event_statement
            .execute(turso::params![
                event_id.clone(),
                camera_id,
                event_type,
                start_ms,
                end_ms,
                (index % 1_000) as f64 / 1_000.0,
                "[0.125,0.25,0.5,0.5]",
                zone,
                thumbnail,
            ])
            .await?;
        for stream_id in STREAMS {
            let fragment_sequence = offset_ms
                .checked_div(FRAGMENT_DURATION_MS)
                .and_then(|index| index.checked_add(1))
                .ok_or_else(|| anyhow::anyhow!("benchmark fragment sequence overflowed"))?;
            link_statement
                .execute(turso::params![
                    event_id.clone(),
                    stream_id,
                    benchmark_recording_id(camera_index, day_index, stream_id),
                    fragment_sequence,
                ])
                .await?;
        }
    }
    drop(event_statement);
    drop(link_statement);
    transaction.commit().await?;
    Ok(())
}

pub fn benchmark_recording_id(camera_index: u32, day_index: u32, stream_id: &str) -> String {
    format!("recording-{camera_index:03}-{day_index:02}-{stream_id}")
}

pub fn benchmark_camera_id(camera_index: u32) -> String {
    format!("camera-{camera_index:03}")
}

fn benchmark_zone(index: u64, camera_index: u32, day_index: u32) -> String {
    format!(
        "zone-{camera_index:03}-{day_index:02}-{index:016x}-{:016x}-{:016x}-{:016x}",
        splitmix64(index),
        splitmix64(index.wrapping_add(1)),
        splitmix64(index.wrapping_add(2)),
    )
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

async fn logical_database_bytes(connection: &turso::Connection) -> anyhow::Result<u64> {
    let page_count = pragma_u64(connection, "PRAGMA page_count").await?;
    let page_size = pragma_u64(connection, "PRAGMA page_size").await?;
    page_count
        .checked_mul(page_size)
        .ok_or_else(|| anyhow::anyhow!("benchmark database size overflows"))
}

async fn pragma_u64(connection: &turso::Connection, sql: &str) -> anyhow::Result<u64> {
    let mut rows = connection.query(sql, ()).await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("{sql} returned no row"))?;
    let value = row.get::<i64>(0)?;
    u64::try_from(value).map_err(|_| anyhow::anyhow!("{sql} returned a negative value"))
}

async fn quick_check(connection: &turso::Connection) -> anyhow::Result<()> {
    let mut rows = connection.query("PRAGMA quick_check", ()).await?;
    let result = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("database quick check returned no row"))?
        .get::<String>(0)?;
    if result != "ok" {
        anyhow::bail!("database quick check failed: {result}");
    }
    Ok(())
}

fn path_text(path: &Path) -> anyhow::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("benchmark path is not valid UTF-8: {}", path.display()))
}

fn to_i64(value: u64, name: &str) -> anyhow::Result<i64> {
    i64::try_from(value).map_err(|_| anyhow::anyhow!("{name} exceeds Turso INTEGER range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{EventKeyframeLookup, RecordingCatalog};

    #[test]
    fn builds_a_small_production_schema_corpus() {
        let root = std::env::temp_dir().join(format!(
            "keeppeek-event-keyframe-corpus-{}",
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let recordings = ["main", "sub"]
            .into_iter()
            .map(|stream_id| {
                let path = root.join(format!("{stream_id}.mp4"));
                std::fs::write(&path, b"header-encoded-keyframe-trailer").unwrap();
                BenchmarkRecordingSeed {
                    recording_id: benchmark_recording_id(0, 0, stream_id),
                    stream_key: format!("camera-000/{stream_id}"),
                    source_id: "camera-000".to_owned(),
                    stream_id: stream_id.to_owned(),
                    path,
                    started_at_ms: 1_000,
                    file_len: 31,
                    keyframe_offset: 7,
                    keyframe_len: 16,
                }
            })
            .collect();
        let catalog_path = root.join("catalog.db");
        let summary = build_event_keyframe_corpus(&EventKeyframeCorpusConfig {
            catalog_path: catalog_path.clone(),
            recordings,
            target_bytes: 1,
            camera_count: 1,
            day_count: 1,
            start_time_ms: 1_000,
            batch_events: 10,
        })
        .unwrap();
        assert!(summary.logical_database_bytes > 0);
        assert_eq!(summary.recording_count, 2);
        assert_eq!(summary.fragment_count, 256);
        assert_eq!(summary.keyframe_count, 256);
        assert_eq!(summary.event_count, 10);
        assert_eq!(summary.event_keyframe_link_count, 20);

        let catalog = RecordingCatalog::open(&catalog_path).unwrap();
        let lookup = EventKeyframeLookup::new(catalog.handle());
        let keyframe = lookup
            .read(&benchmark_event_id(0), "main")
            .unwrap()
            .unwrap();
        assert_eq!(keyframe.bytes, b"encoded-keyframe");
        drop(lookup);
        catalog.shutdown();
        std::fs::remove_dir_all(root).unwrap();
    }
}
