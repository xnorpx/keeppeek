//! Enforces a per-attempt byte ceiling at the output sink before publication.

use super::{ExportArtifact, ExportSources};
use std::io::{self, Seek, SeekFrom, Write};

pub(super) fn read_payload<R: io::Read + Seek>(
    reader: &mut mp4::Mp4Reader<R>,
    track_id: u32,
    sample_id: u32,
    remaining: Option<u64>,
) -> anyhow::Result<bytes::Bytes> {
    use anyhow::Context;
    if let Some(remaining) = remaining {
        let location = reader
            .sample_location(track_id, sample_id)?
            .context("export sample has no byte location")?;
        anyhow::ensure!(
            u64::from(location.size) <= remaining,
            "export exceeds the guest preparation byte limit"
        );
    }
    Ok(reader
        .read_sample(track_id, sample_id)?
        .context("selected export sample is missing")?
        .bytes)
}

struct BoundedWriter<'a, W> {
    inner: &'a mut W,
    position: u64,
    maximum: u64,
}

impl<W: Write> Write for BoundedWriter<'_, W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if u64::try_from(input.len()).unwrap_or(u64::MAX)
            > self.maximum.saturating_sub(self.position)
        {
            return Err(io::Error::other(
                "export exceeds the guest preparation byte limit",
            ));
        }
        let written = self.inner.write(input)?;
        self.position += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> Seek for BoundedWriter<'_, W> {
    fn seek(&mut self, offset: SeekFrom) -> io::Result<u64> {
        self.position = self.inner.seek(offset)?;
        Ok(self.position)
    }
}

pub(crate) fn write<W: Write + Seek>(
    sources: ExportSources<'_>,
    output: &mut W,
    offset_ms: i32,
    maximum: Option<u64>,
    cancelled: impl Fn() -> bool,
    progress: impl Fn(u64),
) -> anyhow::Result<ExportArtifact> {
    if let Some(maximum) = maximum {
        let position = output.stream_position()?;
        let mut bounded = BoundedWriter {
            inner: output,
            position,
            maximum,
        };
        super::export_sources_to_writer(
            sources,
            &mut bounded,
            offset_ms,
            Some(maximum),
            cancelled,
            progress,
        )
    } else {
        super::export_sources_to_writer(sources, output, offset_ms, None, cancelled, progress)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn overwrites_do_not_grow_the_output_past_the_guest_budget() {
        let mut bytes = Cursor::new(Vec::new());
        {
            let mut output = BoundedWriter {
                inner: &mut bytes,
                position: 0,
                maximum: 8,
            };
            output.write_all(b"12345678").unwrap();
            output.seek(SeekFrom::Start(2)).unwrap();
            output.write_all(b"abc").unwrap();
            assert_eq!(
                output.write_all(b"long").unwrap_err().to_string(),
                crate::test_support::guest_range_recovery_messages()["byteLimit"]
            );
            output.seek(SeekFrom::End(1)).unwrap();
            assert!(output.write_all(b"x").is_err());
        }
        assert_eq!(bytes.into_inner(), b"12abc678");
    }
}
