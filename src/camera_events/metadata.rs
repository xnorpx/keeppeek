use std::io::{Cursor, Read};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use retina::codec::CompressionType;

pub(super) fn decode(
    bytes: &[u8],
    compression: CompressionType,
    received_time: DateTime<Utc>,
) -> anyhow::Result<onvif::event::Metadata> {
    anyhow::ensure!(bytes.len() <= 256 * 1024, "metadata input exceeds limit");
    match compression {
        CompressionType::Uncompressed => {
            Ok(onvif::event::Metadata::parse_at(bytes, received_time)?)
        }
        CompressionType::GzipCompressed => {
            let deadline = Instant::now() + Duration::from_millis(100);
            let limit = (bytes.len().saturating_mul(128)).clamp(1024, 1024 * 1024);
            let mut decoder = flate2::bufread::GzDecoder::new(Cursor::new(bytes));
            let mut decoded = Vec::with_capacity(bytes.len().min(limit));
            let mut buffer = [0; 8192];
            loop {
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "metadata decompression deadline exceeded"
                );
                let count = decoder.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                anyhow::ensure!(
                    decoded.len() + count <= limit,
                    "metadata expansion exceeds limit"
                );
                decoded.extend_from_slice(&buffer[..count]);
            }
            anyhow::ensure!(
                decoder.into_inner().position() == bytes.len() as u64,
                "multiple or trailing gzip metadata members are unsupported"
            );
            Ok(onvif::event::Metadata::parse_at(&decoded, received_time)?)
        }
        _ => anyhow::bail!("EXI metadata decoding is unsupported"),
    }
}
