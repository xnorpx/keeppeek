//! Reads leased stored-media ranges with one caller-owned work budget.

use super::*;
use crate::storage::archive::target::Work;

pub(super) struct StoredReadRange<'a> {
    pub path: &'a Path,
    pub archive: Option<&'a crate::storage::catalog::archive_playback::ArchiveRead>,
    pub offset: u64,
    pub length: u64,
}

pub(super) fn read(
    range: StoredReadRange<'_>,
    work: &Work<'_>,
) -> Result<Vec<u8>, ControlCommandError> {
    work.check().map_err(unavailable)?;
    if range.length == 0 || range.length > MAX_STORED_OBJECT_BYTES {
        return Err(internal("stored media byte range is invalid"));
    }
    if let Some(archive) = range.archive {
        return crate::storage::archive::read_range(archive, range.offset, range.length, work)
            .map_err(|error| {
                tracing::warn!(%error, "archive playback range is unavailable");
                unavailable("Archive media is unavailable or failed integrity verification. Check the archive target and retry.")
            });
    }
    let mut file = File::open(range.path)
        .map_err(|error| internal(format!("unable to open indexed recording: {error}")))?;
    let file_len = file
        .metadata()
        .map_err(|error| internal(format!("unable to inspect indexed recording: {error}")))?
        .len();
    if range
        .offset
        .checked_add(range.length)
        .is_none_or(|end| end > file_len)
    {
        return Err(internal(
            "stored media byte range exceeds its indexed recording",
        ));
    }
    file.seek(SeekFrom::Start(range.offset))
        .map_err(|error| internal(format!("unable to seek indexed recording: {error}")))?;
    let capacity = usize::try_from(range.length)
        .map_err(|_| internal("stored media byte range does not fit memory"))?;
    let mut payload = vec![0; capacity];
    // ponytail: bounded blocking file reads; asynchronous I/O needs a separate storage design.
    for chunk in payload.chunks_mut(64 * 1024) {
        work.check().map_err(unavailable)?;
        file.read_exact(chunk).map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                internal("indexed recording byte range was truncated")
            } else {
                internal(format!("unable to read indexed recording: {error}"))
            }
        })?;
    }
    work.check().map_err(unavailable)?;
    Ok(payload)
}

fn unavailable(error: impl std::fmt::Display) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Unavailable, 503, error.to_string())
}

fn internal(error: impl std::fmt::Display) -> ControlCommandError {
    ControlCommandError::new(proto::ErrorCode::Internal, 500, error.to_string())
}
