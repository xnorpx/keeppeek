use std::time::Duration;

pub(super) fn timestamp(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let (year, month, day) = date(seconds / 86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:09}Z",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60,
        elapsed.subsec_nanos()
    )
}

fn date(mut days: u64) -> (u64, usize, u64) {
    let mut year = 2000 + (days / 146_097) * 400;
    days %= 146_097;
    for _ in 0..400 {
        let count = if leap_year(year) { 366 } else { 365 };
        if days < count {
            break;
        }
        days -= count;
        year += 1;
    }
    let months = [
        31,
        28 + u64::from(leap_year(year)),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for (month, count) in months.into_iter().enumerate() {
        if days < count {
            return (year, month + 1, days + 1);
        }
        days -= count;
    }
    unreachable!("the Gregorian cycle must produce a day within one year")
}

const fn leap_year(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

pub(super) fn duration_text(duration: Duration) -> String {
    if duration.subsec_nanos() == 0 {
        format!("PT{}S", duration.as_secs())
    } else {
        format!("PT{}.{:09}S", duration.as_secs(), duration.subsec_nanos())
    }
}

pub(super) fn parse_duration(value: &str) -> Option<Duration> {
    if value.len() > 64 {
        return None;
    }
    let value = value.strip_prefix('P')?;
    let (days, mut time) = value.split_once('T').unwrap_or((value, ""));
    let mut seconds = if days.is_empty() {
        0
    } else {
        integer(days.strip_suffix('D')?)?.checked_mul(86_400)?
    };
    let mut nanos = 0;
    if time.is_empty() && (days.is_empty() || value.contains('T')) {
        return None;
    }
    for (unit, multiplier) in [('H', 3600), ('M', 60), ('S', 1)] {
        if let Some(offset) = time.find(unit) {
            let number = &time[..offset];
            let whole = if unit == 'S' {
                if let Some((whole, fraction)) = number.split_once('.') {
                    if fraction.is_empty() || fraction.len() > 9 {
                        return None;
                    }
                    let padding = 9 - u32::try_from(fraction.len()).ok()?;
                    nanos = u32::try_from(integer(fraction)?)
                        .ok()?
                        .checked_mul(10_u32.pow(padding))?;
                    whole
                } else {
                    number
                }
            } else {
                number
            };
            seconds = seconds.checked_add(integer(whole)?.checked_mul(multiplier)?)?;
            time = &time[offset + 1..];
        }
    }
    time.is_empty().then(|| Duration::new(seconds, nanos))
}

fn integer(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}
