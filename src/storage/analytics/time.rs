//! Deterministic IANA timezone bucketization and calendar comparison windows.

use super::{BucketGranularity, ComparisonMode, WeekStart};
use chrono::{Datelike, LocalResult, NaiveDate, Offset, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use std::collections::BTreeMap;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;

/// One deterministic time bucket produced for an activity summary query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketSpec {
    pub start_time_ms: i64,
    pub end_time_ms: i64,
    pub label: String,
    pub utc_offset_seconds: i32,
    pub dst_fold: u32,
    pub partial_current: bool,
    pub hour_aligned_utc: bool,
    pub day_aligned_utc: bool,
}

/// Returns the compiled IANA tzdata release version.
pub const fn tzdb_version() -> &'static str {
    chrono_tz::IANA_TZDB_VERSION
}

/// Parses an explicit IANA timezone identifier.
pub fn parse_timezone(name: &str) -> anyhow::Result<Tz> {
    let trimmed = name.trim();
    anyhow::ensure!(!trimmed.is_empty(), "timezone must not be empty");
    trimmed
        .parse::<Tz>()
        .map_err(|_| anyhow::anyhow!("unknown IANA timezone '{trimmed}'"))
}

/// Returns `(iso_weekday 1..=7, local_hour 0..=23)` for one UTC millisecond timestamp.
pub fn local_weekday_and_hour(timestamp_ms: i64, tz: Tz) -> anyhow::Result<(u8, u8)> {
    let dt = utc_from_ms(timestamp_ms)?.with_timezone(&tz);
    let weekday = u8::try_from(dt.weekday().number_from_monday())?;
    let hour = u8::try_from(dt.hour())?;
    Ok((weekday, hour))
}

/// Returns true only when every UTC hour in `[start_ms, end_ms)` maps to one whole local hour in `tz`.
pub fn utc_hours_align_with_local_hours(
    start_ms: i64,
    end_ms: i64,
    tz: Tz,
) -> anyhow::Result<bool> {
    anyhow::ensure!(start_ms < end_ms, "alignment check requires start < end");
    let mut cursor = start_ms;
    while cursor < end_ms {
        let next = cursor.saturating_add(DAY_MS).min(end_ms);
        let start_off = utc_from_ms(cursor)?
            .with_timezone(&tz)
            .offset()
            .fix()
            .local_minus_utc();
        let end_off = utc_from_ms(next - 1)?
            .with_timezone(&tz)
            .offset()
            .fix()
            .local_minus_utc();
        if start_off.rem_euclid(3_600) != 0 || end_off.rem_euclid(3_600) != 0 {
            return Ok(false);
        }
        if start_off != end_off && !day_hours_align(cursor, next, tz)? {
            return Ok(false);
        }
        cursor = next;
    }
    Ok(true)
}

fn day_hours_align(day_start_ms: i64, day_end_ms: i64, tz: Tz) -> anyhow::Result<bool> {
    let mut hour_cursor = day_start_ms;
    while hour_cursor < day_end_ms {
        let hour_end = hour_cursor.saturating_add(HOUR_MS).min(day_end_ms);
        let h_start = utc_from_ms(hour_cursor)?
            .with_timezone(&tz)
            .offset()
            .fix()
            .local_minus_utc();
        let h_end = utc_from_ms(hour_end - 1)?
            .with_timezone(&tz)
            .offset()
            .fix()
            .local_minus_utc();
        if h_start != h_end || h_start.rem_euclid(3_600) != 0 {
            return Ok(false);
        }
        hour_cursor = hour_end;
    }
    Ok(true)
}

/// Builds deterministic civil-time buckets for `[start_ms, end_ms)` in `tz`.
pub fn build_time_buckets(
    start_ms: i64,
    end_ms: i64,
    tz: Tz,
    granularity: BucketGranularity,
    week_start: WeekStart,
    now_ms: i64,
    max_buckets: usize,
) -> anyhow::Result<Vec<BucketSpec>> {
    anyhow::ensure!(start_ms < end_ms, "bucket start must precede end");
    match granularity {
        BucketGranularity::Hour => build_hour_buckets(start_ms, end_ms, tz, now_ms, max_buckets),
        BucketGranularity::Day => build_day_buckets(start_ms, end_ms, tz, now_ms, max_buckets),
        BucketGranularity::Week => {
            build_week_buckets(start_ms, end_ms, tz, week_start, now_ms, max_buckets)
        }
        BucketGranularity::Month => build_month_buckets(start_ms, end_ms, tz, now_ms, max_buckets),
        BucketGranularity::HourOfDay => build_hour_of_day_buckets(start_ms, end_ms, tz, now_ms),
        BucketGranularity::WeekdayWeekend => {
            build_weekday_weekend_buckets(start_ms, end_ms, tz, now_ms)
        }
    }
}

/// Computes the previous comparable UTC half-open interval.
pub fn previous_comparison_window(
    start_ms: i64,
    end_ms: i64,
    tz: Tz,
    granularity: BucketGranularity,
    week_start: WeekStart,
    mode: ComparisonMode,
) -> anyhow::Result<Option<(i64, i64)>> {
    match mode {
        ComparisonMode::None => Ok(None),
        ComparisonMode::PreviousDuration => {
            let span = end_ms
                .checked_sub(start_ms)
                .ok_or_else(|| anyhow::anyhow!("comparison span overflowed"))?;
            let prev_start = start_ms
                .checked_sub(span)
                .ok_or_else(|| anyhow::anyhow!("comparison start timestamp overflowed"))?;
            Ok(Some((prev_start, start_ms)))
        }
        ComparisonMode::PreviousCalendar => {
            previous_calendar_window(start_ms, end_ms, tz, granularity, week_start)
        }
    }
}

fn build_hour_buckets(
    start_ms: i64,
    end_ms: i64,
    tz: Tz,
    now_ms: i64,
    max_buckets: usize,
) -> anyhow::Result<Vec<BucketSpec>> {
    let mut buckets = Vec::new();
    let mut seen_local_hours: BTreeMap<(i32, u32, u32, u32), u32> = BTreeMap::new();
    let mut cursor_ms = start_ms;
    while cursor_ms < end_ms {
        anyhow::ensure!(
            buckets.len() < max_buckets,
            "requested range exceeds maximum of {max_buckets} hour buckets"
        );
        let local = utc_from_ms(cursor_ms)?.with_timezone(&tz);
        let next_ms = next_civil_hour_boundary_ms(cursor_ms, tz)?.min(end_ms);
        anyhow::ensure!(next_ms > cursor_ms, "hour bucket failed to advance");
        let local_key = (local.year(), local.month(), local.day(), local.hour());
        let fold = *seen_local_hours
            .entry(local_key)
            .and_modify(|count| *count += 1)
            .or_insert(0);
        let offset = local.offset().fix().local_minus_utc();
        let label = format_hour_label(local, offset, fold);
        buckets.push(make_bucket(cursor_ms, next_ms, label, offset, fold, now_ms));
        cursor_ms = next_ms;
    }
    Ok(buckets)
}

fn build_day_buckets(
    start_ms: i64,
    end_ms: i64,
    tz: Tz,
    now_ms: i64,
    max_buckets: usize,
) -> anyhow::Result<Vec<BucketSpec>> {
    let mut buckets = Vec::new();
    let mut cursor_ms = start_ms;
    while cursor_ms < end_ms {
        anyhow::ensure!(
            buckets.len() < max_buckets,
            "requested range exceeds maximum of {max_buckets} day buckets"
        );
        let local = utc_from_ms(cursor_ms)?.with_timezone(&tz);
        let date = local.date_naive();
        let next_date = date
            .succ_opt()
            .ok_or_else(|| anyhow::anyhow!("calendar date overflowed"))?;
        let next_ms = local_midnight_ms(next_date, tz)?.min(end_ms);
        anyhow::ensure!(next_ms > cursor_ms, "day bucket failed to advance");
        let offset = local.offset().fix().local_minus_utc();
        buckets.push(make_bucket(
            cursor_ms,
            next_ms,
            date.format("%Y-%m-%d").to_string(),
            offset,
            0,
            now_ms,
        ));
        cursor_ms = next_ms;
    }
    Ok(buckets)
}

fn build_week_buckets(
    start_ms: i64,
    end_ms: i64,
    tz: Tz,
    week_start: WeekStart,
    now_ms: i64,
    max_buckets: usize,
) -> anyhow::Result<Vec<BucketSpec>> {
    let mut buckets = Vec::new();
    let mut cursor_ms = start_ms;
    while cursor_ms < end_ms {
        anyhow::ensure!(
            buckets.len() < max_buckets,
            "requested range exceeds maximum of {max_buckets} week buckets"
        );
        let local = utc_from_ms(cursor_ms)?.with_timezone(&tz);
        let week_origin = align_week_start(local.date_naive(), week_start)?;
        let next_week = week_origin
            .checked_add_days(chrono::Days::new(7))
            .ok_or_else(|| anyhow::anyhow!("calendar week overflowed"))?;
        let next_ms = local_midnight_ms(next_week, tz)?.min(end_ms);
        anyhow::ensure!(next_ms > cursor_ms, "week bucket failed to advance");
        let offset = local.offset().fix().local_minus_utc();
        let label = format!("Week of {}", week_origin.format("%Y-%m-%d"));
        buckets.push(make_bucket(cursor_ms, next_ms, label, offset, 0, now_ms));
        cursor_ms = next_ms;
    }
    Ok(buckets)
}

fn build_month_buckets(
    start_ms: i64,
    end_ms: i64,
    tz: Tz,
    now_ms: i64,
    max_buckets: usize,
) -> anyhow::Result<Vec<BucketSpec>> {
    let mut buckets = Vec::new();
    let mut cursor_ms = start_ms;
    while cursor_ms < end_ms {
        anyhow::ensure!(
            buckets.len() < max_buckets,
            "requested range exceeds maximum of {max_buckets} month buckets"
        );
        let local = utc_from_ms(cursor_ms)?.with_timezone(&tz);
        let date = local.date_naive();
        let next_month = first_day_of_next_month(date)?;
        let next_ms = local_midnight_ms(next_month, tz)?.min(end_ms);
        anyhow::ensure!(next_ms > cursor_ms, "month bucket failed to advance");
        let offset = local.offset().fix().local_minus_utc();
        let label = format!("{:04}-{:02}", date.year(), date.month());
        buckets.push(make_bucket(cursor_ms, next_ms, label, offset, 0, now_ms));
        cursor_ms = next_ms;
    }
    Ok(buckets)
}

fn build_hour_of_day_buckets(
    start_ms: i64,
    end_ms: i64,
    tz: Tz,
    now_ms: i64,
) -> anyhow::Result<Vec<BucketSpec>> {
    let partial = start_ms < now_ms && now_ms < end_ms;
    let hour_aligned = start_ms.rem_euclid(HOUR_MS) == 0
        && end_ms.rem_euclid(HOUR_MS) == 0
        && utc_hours_align_with_local_hours(start_ms, end_ms, tz)?;
    Ok((0..24u8)
        .map(|hour| BucketSpec {
            start_time_ms: start_ms,
            end_time_ms: end_ms,
            label: format!("{hour:02}:00"),
            utc_offset_seconds: 0,
            dst_fold: 0,
            partial_current: partial,
            hour_aligned_utc: hour_aligned,
            day_aligned_utc: false,
        })
        .collect())
}

fn build_weekday_weekend_buckets(
    start_ms: i64,
    end_ms: i64,
    tz: Tz,
    now_ms: i64,
) -> anyhow::Result<Vec<BucketSpec>> {
    let partial = start_ms < now_ms && now_ms < end_ms;
    let hour_aligned = start_ms.rem_euclid(HOUR_MS) == 0
        && end_ms.rem_euclid(HOUR_MS) == 0
        && utc_hours_align_with_local_hours(start_ms, end_ms, tz)?;
    Ok(["Weekday", "Weekend"]
        .into_iter()
        .map(|label| BucketSpec {
            start_time_ms: start_ms,
            end_time_ms: end_ms,
            label: label.to_owned(),
            utc_offset_seconds: 0,
            dst_fold: 0,
            partial_current: partial,
            hour_aligned_utc: hour_aligned,
            day_aligned_utc: false,
        })
        .collect())
}

fn previous_calendar_window(
    start_ms: i64,
    end_ms: i64,
    tz: Tz,
    granularity: BucketGranularity,
    week_start: WeekStart,
) -> anyhow::Result<Option<(i64, i64)>> {
    let start_local = utc_from_ms(start_ms)?.with_timezone(&tz);
    let end_local = utc_from_ms(end_ms)?.with_timezone(&tz);
    let prev_start_ms = match granularity {
        BucketGranularity::Month => {
            let start_months = start_local.year() * 12 + (start_local.month() as i32 - 1);
            let end_months = end_local.year() * 12 + (end_local.month() as i32 - 1);
            let step = (end_months - start_months).max(1);
            let target = shift_month(start_local.date_naive(), -step)?;
            local_midnight_ms(target, tz)?
        }
        BucketGranularity::Week => {
            let start_week = align_week_start(start_local.date_naive(), week_start)?;
            let days = (end_local.date_naive() - start_local.date_naive()).num_days();
            let weeks = ((days + 6) / 7).max(1);
            let target = start_week
                .checked_sub_days(chrono::Days::new(u64::try_from(weeks * 7)?))
                .ok_or_else(|| anyhow::anyhow!("previous week underflowed"))?;
            local_midnight_ms(target, tz)?
        }
        BucketGranularity::Day
        | BucketGranularity::HourOfDay
        | BucketGranularity::WeekdayWeekend => {
            let days = (end_local.date_naive() - start_local.date_naive())
                .num_days()
                .max(1);
            let target = start_local
                .date_naive()
                .checked_sub_days(chrono::Days::new(u64::try_from(days)?))
                .ok_or_else(|| anyhow::anyhow!("previous day underflowed"))?;
            local_midnight_ms(target, tz)?
        }
        BucketGranularity::Hour => {
            let span = end_ms
                .checked_sub(start_ms)
                .ok_or_else(|| anyhow::anyhow!("comparison span overflowed"))?;
            start_ms
                .checked_sub(span)
                .ok_or_else(|| anyhow::anyhow!("previous hour start underflowed"))?
        }
    };
    anyhow::ensure!(
        prev_start_ms < start_ms,
        "previous calendar window must precede query start"
    );
    Ok(Some((prev_start_ms, start_ms)))
}

fn next_civil_hour_boundary_ms(cursor_ms: i64, tz: Tz) -> anyhow::Result<i64> {
    let local = utc_from_ms(cursor_ms)?.with_timezone(&tz);
    let offset_ms = i64::from(local.offset().fix().local_minus_utc()) * 1_000;
    let local_ms = cursor_ms
        .checked_add(offset_ms)
        .ok_or_else(|| anyhow::anyhow!("local timestamp overflowed"))?;
    let rem = local_ms.rem_euclid(HOUR_MS);
    let to_next = HOUR_MS - rem;
    // Check for sub-hour DST transitions (for example Australia/Lord_Howe 30-minute shift).
    let candidate_ms = cursor_ms
        .checked_add(to_next)
        .ok_or_else(|| anyhow::anyhow!("next hour timestamp overflowed"))?;
    let next_local = utc_from_ms(candidate_ms)?.with_timezone(&tz);
    if next_local.minute() == 0 && next_local.second() == 0 {
        return Ok(candidate_ms);
    }
    let half_step = cursor_ms
        .checked_add(to_next / 2)
        .ok_or_else(|| anyhow::anyhow!("sub-hour transition overflowed"))?;
    let mid_local = utc_from_ms(half_step)?.with_timezone(&tz);
    if mid_local.minute() == 0 && mid_local.second() == 0 {
        return Ok(half_step);
    }
    Ok(candidate_ms)
}

const fn make_bucket(
    start_time_ms: i64,
    end_time_ms: i64,
    label: String,
    utc_offset_seconds: i32,
    dst_fold: u32,
    now_ms: i64,
) -> BucketSpec {
    let partial_current = start_time_ms < now_ms && now_ms < end_time_ms;
    let hour_aligned_utc =
        start_time_ms.rem_euclid(HOUR_MS) == 0 && end_time_ms.rem_euclid(HOUR_MS) == 0;
    let day_aligned_utc =
        start_time_ms.rem_euclid(DAY_MS) == 0 && end_time_ms.rem_euclid(DAY_MS) == 0;
    BucketSpec {
        start_time_ms,
        end_time_ms,
        label,
        utc_offset_seconds,
        dst_fold,
        partial_current,
        hour_aligned_utc,
        day_aligned_utc,
    }
}

fn format_hour_label(local: chrono::DateTime<Tz>, offset_seconds: i32, fold: u32) -> String {
    let sign = if offset_seconds >= 0 { '+' } else { '-' };
    let abs_secs = offset_seconds.unsigned_abs();
    let hours = abs_secs / 3_600;
    let minutes = (abs_secs % 3_600) / 60;
    let base = format!(
        "{:04}-{:02}-{:02} {:02}:00 {sign}{hours:02}:{minutes:02}",
        local.year(),
        local.month(),
        local.day(),
        local.hour()
    );
    if fold > 0 {
        format!("{base} (DST repeat)")
    } else {
        base
    }
}

fn align_week_start(date: NaiveDate, week_start: WeekStart) -> anyhow::Result<NaiveDate> {
    let offset = match week_start {
        WeekStart::Monday => u64::from(date.weekday().num_days_from_monday()),
        WeekStart::Sunday => u64::from(date.weekday().num_days_from_sunday()),
    };
    date.checked_sub_days(chrono::Days::new(offset))
        .ok_or_else(|| anyhow::anyhow!("week start date underflowed"))
}

fn first_day_of_next_month(date: NaiveDate) -> anyhow::Result<NaiveDate> {
    let (year, month) = if date.month() == 12 {
        (
            date.year()
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("calendar year overflowed"))?,
            1,
        )
    } else {
        (date.year(), date.month() + 1)
    };
    NaiveDate::from_ymd_opt(year, month, 1)
        .ok_or_else(|| anyhow::anyhow!("next calendar month is invalid"))
}

fn shift_month(date: NaiveDate, delta_months: i32) -> anyhow::Result<NaiveDate> {
    let total = date.year() * 12 + (date.month() as i32 - 1) + delta_months;
    let year = total.div_euclid(12);
    let month = u32::try_from(total.rem_euclid(12) + 1)?;
    NaiveDate::from_ymd_opt(year, month, 1)
        .ok_or_else(|| anyhow::anyhow!("shifted calendar month is invalid"))
}

fn local_midnight_ms(date: NaiveDate, tz: Tz) -> anyhow::Result<i64> {
    let midnight = date
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("local midnight is invalid"))?;
    match tz.from_local_datetime(&midnight) {
        LocalResult::Single(dt) => Ok(dt.timestamp_millis()),
        LocalResult::Ambiguous(earliest, _) => Ok(earliest.timestamp_millis()),
        LocalResult::None => {
            // Spring-forward midnight gap (for example America/Sao_Paulo or America/Havana):
            // advance in 1-minute steps (bounded to 180 minutes) to the first valid local instant.
            for minutes in 1..=180u32 {
                let candidate = midnight + chrono::Duration::minutes(i64::from(minutes));
                match tz.from_local_datetime(&candidate) {
                    LocalResult::Single(dt) => return Ok(dt.timestamp_millis()),
                    LocalResult::Ambiguous(earliest, _) => return Ok(earliest.timestamp_millis()),
                    LocalResult::None => continue,
                }
            }
            anyhow::bail!(
                "could not resolve local midnight for {date} in {}",
                tz.name()
            )
        }
    }
}

fn utc_from_ms(timestamp_ms: i64) -> anyhow::Result<chrono::DateTime<Utc>> {
    Utc.timestamp_millis_opt(timestamp_ms)
        .single()
        .ok_or_else(|| anyhow::anyhow!("UTC timestamp {timestamp_ms} ms is out of range"))
}
