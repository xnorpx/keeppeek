use std::io::Result as IoResult;
use std::io::Write;

pub struct Encoder<W>
where
    W: Write,
{
    output: W,
    chunks_size: usize,
    buffer: Vec<u8>,
    flush_after_write: bool,
}

const MAX_CHUNK_SIZE: usize = u32::MAX as usize;
const MAX_HEADER_SIZE: usize = 6;

impl<W> Encoder<W>
where
    W: Write,
{
    pub fn new(output: W) -> Encoder<W> {
        Self::with_flush_after_write(output)
    }

    pub fn get_ref(&self) -> &W {
        &self.output
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.output
    }

    pub fn with_chunks_size(output: W, chunks: usize) -> Encoder<W> {
        let chunks_size = chunks.min(MAX_CHUNK_SIZE);
        let mut encoder = Encoder {
            output,
            chunks_size,
            buffer: vec![0; MAX_HEADER_SIZE],
            flush_after_write: false,
        };
        encoder.reset_buffer();
        encoder
    }

    pub fn with_flush_after_write(output: W) -> Encoder<W> {
        let mut encoder = Encoder {
            output,
            chunks_size: 8192,
            buffer: vec![0; MAX_HEADER_SIZE],
            flush_after_write: true,
        };
        encoder.reset_buffer();
        encoder
    }

    fn reset_buffer(&mut self) {
        self.buffer.truncate(MAX_HEADER_SIZE);
    }

    fn is_buffer_empty(&self) -> bool {
        self.buffer.len() == MAX_HEADER_SIZE
    }

    fn buffer_len(&self) -> usize {
        self.buffer.len() - MAX_HEADER_SIZE
    }

    fn send(&mut self) -> IoResult<()> {
        if self.is_buffer_empty() {
            return Ok(());
        }

        let prelude = format!("{:x}\r\n", self.buffer_len());
        let prelude = prelude.as_bytes();
        assert!(
            prelude.len() <= MAX_HEADER_SIZE,
            "invariant failed: prelude longer than MAX_HEADER_SIZE"
        );
        let offset = MAX_HEADER_SIZE - prelude.len();
        self.buffer[offset..MAX_HEADER_SIZE].clone_from_slice(prelude);
        self.buffer.write_all(b"\r\n")?;
        self.output.write_all(&self.buffer[offset..])?;
        self.reset_buffer();
        Ok(())
    }
}

impl<W> Write for Encoder<W>
where
    W: Write,
{
    fn write(&mut self, data: &[u8]) -> IoResult<usize> {
        let remaining_buffer_space = self.chunks_size - self.buffer_len();
        let bytes_to_buffer = remaining_buffer_space.min(data.len());
        self.buffer.extend_from_slice(&data[..bytes_to_buffer]);
        let more_to_write = bytes_to_buffer < data.len();
        if self.flush_after_write || more_to_write {
            self.send()?;
            if self.flush_after_write {
                self.output.flush()?;
            }
        }

        if more_to_write {
            self.write_all(&data[bytes_to_buffer..])?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> IoResult<()> {
        self.send()?;
        self.output.flush()
    }
}

impl<W> Drop for Encoder<W>
where
    W: Write,
{
    fn drop(&mut self) {
        self.flush().ok();
        write!(self.output, "0\r\n\r\n").ok();
        self.output.flush().ok();
    }
}
