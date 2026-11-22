use std::io::Read;
use std::io::Result as IoResult;
use std::sync::mpsc::channel;
use std::sync::mpsc::{Receiver, Sender};

/// A `Reader` that reads exactly the number of bytes from a sub-reader.
///
/// If the limit is reached, it returns EOF. If the limit is not reached
/// when the destructor is called, the remaining bytes will be read and
/// thrown away.
pub struct EqualReader<R>
where
    R: Read,
{
    reader: R,
    size: usize,
    last_read_signal: Sender<IoResult<()>>,
}

impl<R> EqualReader<R>
where
    R: Read,
{
    pub fn new(reader: R, size: usize) -> (Self, Receiver<IoResult<()>>) {
        let (tx, rx) = channel();

        let r = Self {
            reader,
            size,
            last_read_signal: tx,
        };

        (r, rx)
    }
}

impl<R> Read for EqualReader<R>
where
    R: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        if self.size == 0 {
            return Ok(0);
        }

        let buf = if buf.len() < self.size {
            buf
        } else {
            &mut buf[..self.size]
        };

        match self.reader.read(buf) {
            Ok(len) => {
                self.size -= len;
                Ok(len)
            }
            err @ Err(_) => err,
        }
    }
}

impl<R> Drop for EqualReader<R>
where
    R: Read,
{
    fn drop(&mut self) {
        let mut remaining_to_read = self.size;

        let mut buf = [0; 8 * 1024];
        while remaining_to_read > 0 {
            let count = remaining_to_read.min(buf.len());
            match self.reader.read(&mut buf[..count]) {
                Err(e) => {
                    self.last_read_signal.send(Err(e)).ok();
                    break;
                }
                Ok(0) => {
                    self.last_read_signal.send(Ok(())).ok();
                    break;
                }
                Ok(other) => {
                    remaining_to_read -= other;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EqualReader;
    use std::io::Read;

    #[test]
    fn test_limit() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (mut equal_reader, _) = EqualReader::new(org_reader.by_ref(), 5);

            let mut string = String::new();
            equal_reader.read_to_string(&mut string).unwrap();
            assert_eq!(string, "hello");
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, " world");
    }

    #[test]
    fn test_not_enough() {
        use std::io::Cursor;

        let mut org_reader = Cursor::new("hello world".to_string().into_bytes());

        {
            let (mut equal_reader, _) = EqualReader::new(org_reader.by_ref(), 5);

            let mut vec = [0];
            equal_reader.read_exact(&mut vec).unwrap();
            assert_eq!(vec[0], b'h');
        }

        let mut string = String::new();
        org_reader.read_to_string(&mut string).unwrap();
        assert_eq!(string, " world");
    }

    struct TrackingReader {
        remaining: usize,
        max_buffer: usize,
        calls: usize,
        fail_on: Option<usize>,
    }

    impl TrackingReader {
        const fn new(remaining: usize) -> Self {
            Self {
                remaining,
                max_buffer: 0,
                calls: 0,
                fail_on: None,
            }
        }
    }

    impl Read for TrackingReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.max_buffer = self.max_buffer.max(buffer.len());
            self.calls += 1;
            if self.fail_on == Some(self.calls) {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            let count = self.remaining.min(buffer.len());
            buffer[..count].fill(42);
            self.remaining -= count;
            Ok(count)
        }
    }

    #[test]
    fn drop_drains_declared_body_with_at_most_eight_kibibyte_reads() {
        const BYTES: usize = 128 * 1024;
        let mut source = TrackingReader::new(BYTES);
        let (reader, _) = EqualReader::new(&mut source, BYTES);
        drop(reader);
        assert_eq!(source.remaining, 0);
        assert!(
            source.max_buffer <= 8 * 1024,
            "requested {} bytes",
            source.max_buffer
        );
        assert!(source.calls >= 16);
    }

    #[test]
    fn partial_body_drop_preserves_the_next_message_boundary() {
        const BYTES: usize = 128 * 1024;
        let mut bytes = vec![42; BYTES];
        bytes.extend_from_slice(b"next");
        let mut source = std::io::Cursor::new(bytes);
        let (mut reader, _) = EqualReader::new(&mut source, BYTES);
        let mut prefix = [0; 13];
        reader.read_exact(&mut prefix).unwrap();
        assert_eq!(prefix, [42; 13]);
        drop(reader);
        assert_eq!(source.position(), u64::try_from(BYTES).unwrap());
        let mut next = [0; 4];
        source.read_exact(&mut next).unwrap();
        assert_eq!(next, *b"next");
    }

    #[test]
    fn truncated_body_drop_stops_at_eof_and_reports_completion() {
        let mut source = TrackingReader::new(16 * 1024);
        let (reader, completion) = EqualReader::new(&mut source, 128 * 1024);
        drop(reader);
        completion
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(source.remaining, 0);
        assert_eq!(source.calls, 3);
        assert!(source.max_buffer <= 8 * 1024);
    }

    #[test]
    fn body_drop_reports_read_error_without_retrying() {
        const BYTES: usize = 128 * 1024;
        let mut source = TrackingReader::new(BYTES);
        source.fail_on = Some(2);
        let (reader, completion) = EqualReader::new(&mut source, BYTES);
        drop(reader);
        let error = completion
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        assert_eq!(source.calls, 2);
        assert_eq!(source.remaining, BYTES - 8 * 1024);
        assert!(source.max_buffer <= 8 * 1024);
    }
}
