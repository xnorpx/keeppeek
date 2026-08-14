use std::error::Error;
use std::fmt;
use std::io::Error as IoError;
use std::io::ErrorKind;
use std::io::Read;
use std::io::Result as IoResult;

pub struct Decoder<R> {
    source: R,
    remaining_chunks_size: Option<usize>,
}

impl<R> Decoder<R>
where
    R: Read,
{
    pub fn new(source: R) -> Decoder<R> {
        Decoder {
            source,
            remaining_chunks_size: None,
        }
    }

    pub fn remaining_chunks_size(&self) -> Option<usize> {
        self.remaining_chunks_size
    }

    pub fn into_inner(self) -> R {
        self.source
    }

    pub fn get_ref(&self) -> &R {
        &self.source
    }

    pub fn get_mut(&mut self) -> &mut R {
        &mut self.source
    }

    fn read_byte(&mut self) -> IoResult<u8> {
        let mut byte = [0_u8; 1];
        match self.source.read_exact(&mut byte) {
            Ok(()) => Ok(byte[0]),
            Err(error) if error.kind() == ErrorKind::UnexpectedEof => {
                Err(IoError::new(ErrorKind::InvalidInput, DecoderError))
            }
            Err(error) => Err(error),
        }
    }

    fn read_chunk_size(&mut self) -> IoResult<usize> {
        let mut chunk_size_bytes = Vec::new();
        let mut has_ext = false;

        loop {
            let byte = self.read_byte()?;

            if byte == b'\r' {
                break;
            }
            if byte == b';' {
                has_ext = true;
                break;
            }
            chunk_size_bytes.push(byte);
        }

        if has_ext {
            loop {
                let byte = self.read_byte()?;
                if byte == b'\r' {
                    break;
                }
            }
        }

        self.read_line_feed()?;
        String::from_utf8(chunk_size_bytes)
            .ok()
            .and_then(|chunk| usize::from_str_radix(chunk.trim(), 16).ok())
            .ok_or_else(|| IoError::new(ErrorKind::InvalidInput, DecoderError))
    }

    fn read_carriage_return(&mut self) -> IoResult<()> {
        match self.read_byte() {
            Ok(b'\r') => Ok(()),
            _ => Err(IoError::new(ErrorKind::InvalidInput, DecoderError)),
        }
    }

    fn read_line_feed(&mut self) -> IoResult<()> {
        match self.read_byte() {
            Ok(b'\n') => Ok(()),
            _ => Err(IoError::new(ErrorKind::InvalidInput, DecoderError)),
        }
    }
}

impl<R> Read for Decoder<R>
where
    R: Read,
{
    fn read(&mut self, buffer: &mut [u8]) -> IoResult<usize> {
        let remaining_chunks_size = match self.remaining_chunks_size {
            Some(size) => size,
            None => {
                let chunk_size = self.read_chunk_size()?;
                if chunk_size == 0 {
                    self.read_carriage_return()?;
                    self.read_line_feed()?;
                    return Ok(0);
                }
                chunk_size
            }
        };

        if buffer.len() < remaining_chunks_size {
            let read = self.source.read(buffer)?;
            self.remaining_chunks_size = Some(remaining_chunks_size - read);
            return Ok(read);
        }

        let buffer = &mut buffer[..remaining_chunks_size];
        let read = self.source.read(buffer)?;
        self.remaining_chunks_size = if read == remaining_chunks_size {
            self.read_carriage_return()?;
            self.read_line_feed()?;
            None
        } else {
            Some(remaining_chunks_size - read)
        };
        Ok(read)
    }
}

#[derive(Debug, Copy, Clone)]
struct DecoderError;

impl fmt::Display for DecoderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> Result<(), fmt::Error> {
        write!(formatter, "Error while decoding chunks")
    }
}

impl Error for DecoderError {}
