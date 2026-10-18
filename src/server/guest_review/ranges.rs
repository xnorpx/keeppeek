#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ByteRange {
    pub start: u64,
    pub end: u64,
    pub partial: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InvalidRange;

pub(super) fn parse(header: Option<&str>, bytes: u64) -> Result<ByteRange, InvalidRange> {
    if bytes == 0 {
        return Err(InvalidRange);
    }
    let Some(header) = header else {
        return Ok(ByteRange {
            start: 0,
            end: bytes,
            partial: false,
        });
    };
    if header.len() > 128 {
        return Err(InvalidRange);
    }
    let body = header.strip_prefix("bytes=").ok_or(InvalidRange)?;
    let (start, end) = body.split_once('-').ok_or(InvalidRange)?;
    let (start, end) = if start.is_empty() {
        let suffix = number(end)?;
        if suffix == 0 {
            return Err(InvalidRange);
        }
        (bytes.saturating_sub(suffix), bytes)
    } else {
        let start = number(start)?;
        let end = if end.is_empty() {
            bytes
        } else {
            number(end)?.checked_add(1).ok_or(InvalidRange)?.min(bytes)
        };
        (start, end)
    };
    if start >= end || end > bytes {
        return Err(InvalidRange);
    }
    Ok(ByteRange {
        start,
        end,
        partial: true,
    })
}

fn number(value: &str) -> Result<u64, InvalidRange> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(InvalidRange);
    }
    value.parse().map_err(|_| InvalidRange)
}

#[cfg(test)]
#[path = "range_tests.rs"]
mod tests;
