use crate::storage::{
    catalog::{CoverageAccumulator, maintenance::check_deadline},
    long_term::inspection::container::Index,
};
use sha2::{Digest, Sha256};
use std::time::Instant;

pub(super) async fn read(
    connection: &turso::Connection,
    id: &str,
    deadline: Instant,
) -> anyhow::Result<[u8; 32]> {
    let mut digest = Sha256::new();
    for (statement, columns) in [
        (
            "SELECT sequence, start_ms, duration_ms, byte_offset, byte_len, random_access FROM recording_fragments WHERE recording_id = ?1 ORDER BY sequence LIMIT 4097",
            6,
        ),
        (
            "SELECT fragment_sequence, byte_offset, byte_len FROM recording_keyframes WHERE recording_id = ?1 ORDER BY fragment_sequence LIMIT 4097",
            3,
        ),
        (
            "SELECT start_ms, end_ms FROM recording_coverage_ranges WHERE recording_id = ?1 ORDER BY start_ms, end_ms LIMIT 4097",
            2,
        ),
        (
            "SELECT fragment_count, fragment_bytes, coverage_ms FROM recording_coverage_files WHERE recording_id = ?1 LIMIT 2",
            3,
        ),
    ] {
        digest.update([0]);
        let mut rows = connection.query(statement, turso::params![id]).await?;
        let mut count = 0;
        while let Some(row) = rows.next().await? {
            check_deadline(deadline)?;
            count += 1;
            anyhow::ensure!(count <= 4096, "recording index exceeds inspection limit");
            digest.update([1]);
            for column in 0..columns {
                digest.update(row.get::<i64>(column)?.to_be_bytes());
            }
        }
    }
    Ok(digest.finalize().into())
}

pub(super) fn expected(index: &Index, started_ms: i64) -> anyhow::Result<[u8; 32]> {
    let mut digest = Sha256::new();
    let mut fragments: Vec<_> = index.fragments.iter().collect();
    fragments.sort_unstable_by_key(|fragment| fragment.first_sample.sequence_number);
    let mut coverage = Vec::with_capacity(fragments.len());
    let mut total_bytes = 0_u64;
    digest.update([0]);
    for fragment in &fragments {
        let start = started_ms
            .checked_add(i64::try_from(fragment.start_ms)?)
            .ok_or_else(|| anyhow::anyhow!("recording timestamp overflow"))?;
        let end = start
            .checked_add(i64::try_from(fragment.duration_ms)?)
            .ok_or_else(|| anyhow::anyhow!("recording timestamp overflow"))?;
        row(
            &mut digest,
            &[
                i64::from(fragment.first_sample.sequence_number),
                start,
                i64::try_from(fragment.duration_ms)?,
                i64::try_from(fragment.range.offset)?,
                i64::try_from(fragment.range.size)?,
                1,
            ],
        );
        coverage.push((start, end));
        total_bytes = total_bytes
            .checked_add(fragment.range.size)
            .ok_or_else(|| anyhow::anyhow!("recording byte count overflow"))?;
    }
    digest.update([0]);
    for fragment in &fragments {
        row(
            &mut digest,
            &[
                i64::from(fragment.first_sample.sequence_number),
                i64::try_from(fragment.first_sample.location.offset)?,
                i64::from(fragment.first_sample.location.size),
            ],
        );
    }
    coverage.sort_unstable();
    let mut accumulator = CoverageAccumulator::unbounded();
    for range in coverage {
        accumulator.push(range);
    }
    let summary = accumulator.finish();
    digest.update([0]);
    for (start, end) in summary.ranges {
        row(&mut digest, &[start, end]);
    }
    digest.update([0]);
    row(
        &mut digest,
        &[
            i64::try_from(fragments.len())?,
            i64::try_from(total_bytes)?,
            i64::try_from(summary.duration_ms)?,
        ],
    );
    Ok(digest.finalize().into())
}

fn row(digest: &mut Sha256, values: &[i64]) {
    digest.update([1]);
    for value in values {
        digest.update(value.to_be_bytes());
    }
}
