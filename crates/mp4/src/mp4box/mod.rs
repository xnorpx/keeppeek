//! All ISO-MP4 boxes (atoms) and operations.
//!
//! * [ISO/IEC 14496-12](https://en.wikipedia.org/wiki/MPEG-4_Part_14) - ISO Base Media File Format (QuickTime, MPEG-4, etc)
//! * [ISO/IEC 14496-14](https://en.wikipedia.org/wiki/MPEG-4_Part_14) - MP4 file format
//! * ISO/IEC 14496-17 - Streaming text format
//! * [ISO 23009-1](https://www.iso.org/standard/79329.html) -Dynamic adaptive streaming over HTTP (DASH)
//!
//! http://developer.apple.com/documentation/QuickTime/QTFF/index.html
//! http://www.adobe.com/devnet/video/articles/mp4_movie_atom.html
//! http://mp4ra.org/#/atoms
//!
//! Supported Atoms:
//! ftyp
//! moov
//!     mvhd
//!     udta
//!         meta
//!             ilst
//!                 data
//!     trak
//!         tkhd
//!         mdia
//!             mdhd
//!             hdlr
//!             minf
//!                 stbl
//!                     stsd
//!                         avc1
//!                         hev1
//!                         hvc1
//!                         mp4a
//!                         tx3g
//!                     stts
//!                     stsc
//!                     stsz
//!                     stss
//!                     stco
//!                     co64
//!                     ctts
//!                 dinf
//!                     dref
//!                 smhd
//!                 vmhd
//!         edts
//!             elst
//!     mvex
//!         mehd
//!         trex
//! emsg
//! moof
//!     mfhd
//!     traf
//!         tfhd
//!         tfdt
//!         trun
//! mdat
//! free
//!

use crate::*;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::io::{Read, Seek, SeekFrom, Write};

pub mod avc1;
pub mod co64;
pub mod ctts;
pub mod data;
pub mod dinf;
pub mod edts;
pub mod elst;
pub mod emsg;
pub mod ftyp;
pub mod hdlr;
pub mod hev1;
pub mod ilst;
pub mod mdhd;
pub mod mdia;
pub mod mehd;
pub mod meta;
pub mod mfhd;
pub mod minf;
pub mod moof;
pub mod moov;
pub mod mp4a;
pub mod mvex;
pub mod mvhd;
pub mod smhd;
pub mod stbl;
pub mod stco;
pub mod stsc;
pub mod stsd;
pub mod stss;
pub mod stsz;
pub mod stts;
pub mod tfdt;
pub mod tfhd;
pub mod tkhd;
pub mod traf;
pub mod trak;
pub mod trex;
pub mod trun;
pub mod tx3g;
pub mod udta;
pub mod vmhd;
pub mod vp09;
pub mod vpcc;

pub use emsg::EmsgBox;
pub use ftyp::FtypBox;
pub use moof::MoofBox;
pub use moov::MoovBox;

pub const HEADER_SIZE: u64 = 8;
// const HEADER_LARGE_SIZE: u64 = 16;
pub const HEADER_EXT_SIZE: u64 = 4;

macro_rules! boxtype {
    ($( $name:ident => $value:expr ),*) => {
        #[derive(Clone, Copy, PartialEq, Eq)]
        pub enum BoxType {
            $( $name, )*
            UnknownBox(u32),
        }

        impl From<u32> for BoxType {
            fn from(t: u32) -> BoxType {
                match t {
                    $( $value => BoxType::$name, )*
                    _ => BoxType::UnknownBox(t),
                }
            }
        }

        impl From<BoxType> for u32 {
            fn from(b: BoxType) -> u32 {
                match b {
                    $( BoxType::$name => $value, )*
                    BoxType::UnknownBox(t) => t,
                }
            }
        }
    }
}

boxtype! {
    FtypBox => 0x66747970,
    MvhdBox => 0x6d766864,
    MfhdBox => 0x6d666864,
    FreeBox => 0x66726565,
    MdatBox => 0x6d646174,
    MoovBox => 0x6d6f6f76,
    MvexBox => 0x6d766578,
    MehdBox => 0x6d656864,
    TrexBox => 0x74726578,
    EmsgBox => 0x656d7367,
    MoofBox => 0x6d6f6f66,
    TkhdBox => 0x746b6864,
    TfhdBox => 0x74666864,
    TfdtBox => 0x74666474,
    EdtsBox => 0x65647473,
    MdiaBox => 0x6d646961,
    ElstBox => 0x656c7374,
    MdhdBox => 0x6d646864,
    HdlrBox => 0x68646c72,
    MinfBox => 0x6d696e66,
    VmhdBox => 0x766d6864,
    StblBox => 0x7374626c,
    StsdBox => 0x73747364,
    SttsBox => 0x73747473,
    CttsBox => 0x63747473,
    StssBox => 0x73747373,
    StscBox => 0x73747363,
    StszBox => 0x7374737A,
    StcoBox => 0x7374636F,
    Co64Box => 0x636F3634,
    TrakBox => 0x7472616b,
    TrafBox => 0x74726166,
    TrunBox => 0x7472756E,
    UdtaBox => 0x75647461,
    MetaBox => 0x6d657461,
    DinfBox => 0x64696e66,
    DrefBox => 0x64726566,
    UrlBox  => 0x75726C20,
    SmhdBox => 0x736d6864,
    Avc1Box => 0x61766331,
    AvcCBox => 0x61766343,
    Hev1Box => 0x68657631,
    Hvc1Box => 0x68766331,
    HvcCBox => 0x68766343,
    Mp4aBox => 0x6d703461,
    EsdsBox => 0x65736473,
    Tx3gBox => 0x74783367,
    VpccBox => 0x76706343,
    Vp09Box => 0x76703039,
    DataBox => 0x64617461,
    IlstBox => 0x696c7374,
    NameBox => 0xa96e616d,
    DayBox => 0xa9646179,
    CovrBox => 0x636f7672,
    DescBox => 0x64657363,
    WideBox => 0x77696465
}

pub trait Mp4Box: Sized {
    fn box_type(&self) -> BoxType;
    fn box_size(&self) -> u64;
    #[cfg(feature = "serde")]
    fn to_json(&self) -> Result<String>;
    fn summary(&self) -> Result<String>;
}

pub trait ReadBox<T>: Sized {
    fn read_box(_: T, size: u64) -> Result<Self>;
}

pub trait WriteBox<T>: Sized {
    fn write_box(&self, _: T) -> Result<u64>;
}

#[derive(Debug, Clone, Copy)]
pub struct BoxHeader {
    pub name: BoxType,
    pub size: u64,
}

impl BoxHeader {
    pub const fn new(name: BoxType, size: u64) -> Self {
        Self { name, size }
    }

    // TODO: if size is 0, then this box is the last one in the file
    pub fn read<R: Read>(reader: &mut R) -> Result<Self> {
        // Create and read to buf.
        let mut buf = [0u8; 8]; // 8 bytes for box header.
        reader.read_exact(&mut buf)?;

        // Get size.
        let s = buf[0..4].try_into().unwrap();
        let size = u32::from_be_bytes(s);

        // Get box type string.
        let t = buf[4..8].try_into().unwrap();
        let typ = u32::from_be_bytes(t);

        // Get largesize if size is 1
        if size == 1 {
            reader.read_exact(&mut buf)?;
            let largesize = u64::from_be_bytes(buf);

            Ok(Self {
                name: BoxType::from(typ),

                // Subtract the length of the serialized largesize, as callers assume `size - HEADER_SIZE` is the length
                // of the box data. Disallow `largesize < 16`, or else a largesize of 8 will result in a BoxHeader::size
                // of 0, incorrectly indicating that the box data extends to the end of the stream.
                size: match largesize {
                    0 => 0,
                    1..=15 => return Err(Error::InvalidData("64-bit box size too small")),
                    16..=u64::MAX => largesize - 8,
                },
            })
        } else {
            Ok(Self {
                name: BoxType::from(typ),
                size: size as u64,
            })
        }
    }

    pub fn write<W: Write>(&self, writer: &mut W) -> Result<u64> {
        if self.size > u32::MAX as u64 {
            writer.write_u32::<BigEndian>(1)?;
            writer.write_u32::<BigEndian>(self.name.into())?;
            writer.write_u64::<BigEndian>(self.size)?;
            Ok(16)
        } else {
            writer.write_u32::<BigEndian>(self.size as u32)?;
            writer.write_u32::<BigEndian>(self.name.into())?;
            Ok(8)
        }
    }
}

pub fn read_box_header_ext<R: Read>(reader: &mut R) -> Result<(u8, u32)> {
    let version = reader.read_u8()?;
    let flags = reader.read_u24::<BigEndian>()?;
    Ok((version, flags))
}

pub fn write_box_header_ext<W: Write>(w: &mut W, v: u8, f: u32) -> Result<u64> {
    w.write_u8(v)?;
    w.write_u24::<BigEndian>(f)?;
    Ok(4)
}

pub fn box_start<R: Seek>(seeker: &mut R) -> Result<u64> {
    Ok(seeker.stream_position()? - HEADER_SIZE)
}

pub fn skip_bytes<S: Seek>(seeker: &mut S, size: u64) -> Result<()> {
    seeker.seek(SeekFrom::Current(size as i64))?;
    Ok(())
}

pub fn skip_bytes_to<S: Seek>(seeker: &mut S, pos: u64) -> Result<()> {
    seeker.seek(SeekFrom::Start(pos))?;
    Ok(())
}

pub fn skip_box<S: Seek>(seeker: &mut S, size: u64) -> Result<()> {
    let start = box_start(seeker)?;
    skip_bytes_to(seeker, start + size)?;
    Ok(())
}

pub fn write_zeros<W: Write>(writer: &mut W, size: u64) -> Result<()> {
    for _ in 0..size {
        writer.write_u8(0)?;
    }
    Ok(())
}

#[cfg(feature = "serde")]
mod value_u32 {
    use crate::types::FixedPointU16;
    use serde::Serializer;

    pub fn serialize<S>(fixed: &FixedPointU16, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u16(fixed.value())
    }
}

#[cfg(feature = "serde")]
mod value_i16 {
    use crate::types::FixedPointI8;
    use serde::Serializer;

    pub fn serialize<S>(fixed: &FixedPointI8, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i8(fixed.value())
    }
}

#[cfg(feature = "serde")]
mod value_u8 {
    use crate::types::FixedPointU8;
    use serde::Serializer;

    pub fn serialize<S>(fixed: &FixedPointU8, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(fixed.value())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fourcc() {
        let ftyp_fcc = 0x66747970;
        let ftyp_value = FourCC::from(ftyp_fcc);
        assert_eq!(&ftyp_value.value[..], b"ftyp");
        let ftyp_fcc2: u32 = ftyp_value.into();
        assert_eq!(ftyp_fcc, ftyp_fcc2);
    }

    #[test]
    fn test_largesize_too_small() {
        let error = BoxHeader::read(&mut &[0, 0, 0, 1, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 7][..]);
        assert!(matches!(error, Err(Error::InvalidData(_))));
    }

    #[test]
    fn test_zero_largesize() {
        let error = BoxHeader::read(&mut &[0, 0, 0, 1, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 8][..]);
        assert!(matches!(error, Err(Error::InvalidData(_))));
    }

    #[test]
    fn test_nonzero_largesize_too_small() {
        let error = BoxHeader::read(&mut &[0, 0, 0, 1, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 15][..]);
        assert!(matches!(error, Err(Error::InvalidData(_))));
    }

    #[test]
    fn test_valid_largesize() {
        let header = BoxHeader::read(&mut &[0, 0, 0, 1, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 16][..]);
        assert!(matches!(header, Ok(BoxHeader { size: 8, .. })));
    }
}
