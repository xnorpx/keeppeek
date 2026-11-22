use super::verified::CHUNK_BYTES;
use mp4::ReadBox;
use std::io::{self, Cursor, Read, Seek, SeekFrom};

const MAX_MOOV_BYTES: u64 = 1_024 * 1_024;
const MAX_TOP_METADATA_BOXES: usize = 4_096;
const MAX_CHILD_BOXES: usize = 32;

#[derive(Clone, Copy)]
struct BoxView<'a> {
    kind: [u8; 4],
    encoded: &'a [u8],
    body: &'a [u8],
}

#[derive(Debug)]
struct UnsupportedFormat;

impl std::fmt::Display for UnsupportedFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("guest artifact is unavailable")
    }
}

impl std::error::Error for UnsupportedFormat {}

fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, UnsupportedFormat)
}

/// Only a rejection made by the container inspector permits another stream choice.
pub(super) fn compatible(
    reader: &mut (impl Read + Seek),
    bytes: u64,
    check: impl FnMut() -> io::Result<()>,
) -> io::Result<bool> {
    match inspect(reader, bytes, check) {
        Ok(()) => Ok(true),
        Err(error)
            if error
                .get_ref()
                .is_some_and(|cause| cause.is::<UnsupportedFormat>()) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// Checks only bounded initialization paths; sample tables and fragment caches are never parsed.
pub(super) fn inspect(
    reader: &mut (impl Read + Seek),
    bytes: u64,
    mut check: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let (start, length) = initialization(reader, bytes, &mut check)?;
    let mut moov = vec![0; usize::try_from(length).map_err(|_| unsupported())?];
    reader.seek(SeekFrom::Start(start))?;
    for chunk in moov.chunks_mut(CHUNK_BYTES) {
        check()?;
        reader.read_exact(chunk)?;
    }
    let tracks = boxes(&moov)?;
    if !(1..=2).contains(&tracks.iter().filter(|entry| entry.kind == *b"trak").count()) {
        return Err(unsupported());
    }
    let mut video = 0;
    let mut audio = 0;
    for track in tracks.iter().filter(|entry| entry.kind == *b"trak") {
        check()?;
        match track_codec(track.body)? {
            true => video += 1,
            false => audio += 1,
        }
        if video > 1 || audio > 1 {
            return Err(unsupported());
        }
    }
    if video != 1 {
        return Err(unsupported());
    }
    check()
}

fn initialization(
    reader: &mut (impl Read + Seek),
    bytes: u64,
    check: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<(u64, u64)> {
    let mut offset = 0u64;
    let mut ftyp = false;
    let mut moov = None;
    let mut metadata_boxes = 0;
    if bytes > crate::shares::MAX_MEDIA_BYTES {
        return Err(unsupported());
    }
    // Every header consumes at least eight bytes and checks the caller's deadline.
    // A valid exporter may produce a moof/mdat pair for every sync frame.
    while offset < bytes {
        check()?;
        let (kind, header, length) = top_header(reader, offset, bytes)?;
        if kind != *b"moof" && kind != *b"mdat" {
            metadata_boxes += 1;
            if metadata_boxes > MAX_TOP_METADATA_BOXES {
                return Err(unsupported());
            }
        }
        if kind == *b"ftyp" {
            if ftyp || !(16..=4_096).contains(&length) {
                return Err(unsupported());
            }
            ftyp = true;
        }
        if kind == *b"moov" {
            let body = length.checked_sub(header).ok_or_else(unsupported)?;
            if moov.is_some() || body == 0 || body > MAX_MOOV_BYTES {
                return Err(unsupported());
            }
            moov = Some((offset + header, body));
        }
        offset = offset.checked_add(length).ok_or_else(unsupported)?;
    }
    if ftyp {
        moov.ok_or_else(unsupported)
    } else {
        Err(unsupported())
    }
}

fn top_header(
    reader: &mut (impl Read + Seek),
    start: u64,
    bytes: u64,
) -> io::Result<([u8; 4], u64, u64)> {
    if bytes
        .checked_sub(start)
        .is_none_or(|remaining| remaining < 8)
    {
        return Err(unsupported());
    }
    reader.seek(SeekFrom::Start(start))?;
    let mut header = [0; 8];
    reader.read_exact(&mut header)?;
    let mut length = u64::from(u32::from_be_bytes(
        header[..4].try_into().map_err(|_| unsupported())?,
    ));
    let header_bytes = if length == 1 {
        if bytes - start < 16 {
            return Err(unsupported());
        }
        let mut extended = [0; 8];
        reader.read_exact(&mut extended)?;
        length = u64::from_be_bytes(extended);
        16
    } else {
        8
    };
    if length < header_bytes || start.checked_add(length).is_none_or(|end| end > bytes) {
        return Err(unsupported());
    }
    Ok((
        header[4..].try_into().map_err(|_| unsupported())?,
        header_bytes,
        length,
    ))
}

fn boxes(mut input: &[u8]) -> io::Result<Vec<BoxView<'_>>> {
    let mut result = Vec::new();
    for _ in 0..MAX_CHILD_BOXES {
        if input.is_empty() {
            return Ok(result);
        }
        let header = input.get(..8).ok_or_else(unsupported)?;
        let length = usize::try_from(u32::from_be_bytes(
            header[..4].try_into().map_err(|_| unsupported())?,
        ))
        .map_err(|_| unsupported())?;
        if length < 8 || length > input.len() {
            return Err(unsupported());
        }
        let encoded = &input[..length];
        result.push(BoxView {
            kind: header[4..].try_into().map_err(|_| unsupported())?,
            encoded,
            body: &encoded[8..],
        });
        input = &input[length..];
    }
    if input.is_empty() {
        Ok(result)
    } else {
        Err(unsupported())
    }
}

fn one<'a>(input: &'a [u8], kind: &[u8; 4]) -> io::Result<BoxView<'a>> {
    let mut selected = None;
    for entry in boxes(input)? {
        if entry.kind == *kind {
            if selected.is_some() {
                return Err(unsupported());
            }
            selected = Some(entry);
        }
    }
    selected.ok_or_else(unsupported)
}

fn track_codec(input: &[u8]) -> io::Result<bool> {
    let media = one(input, b"mdia")?;
    let handler = one(media.body, b"hdlr")?;
    let kind = handler.body.get(8..12).ok_or_else(unsupported)?;
    let information = one(media.body, b"minf")?;
    let table = one(information.body, b"stbl")?;
    let description = one(table.body, b"stsd")?;
    let prefix = description.body.get(..8).ok_or_else(unsupported)?;
    if prefix[..4] != [0; 4] || prefix[4..] != 1u32.to_be_bytes() {
        return Err(unsupported());
    }
    let entries = boxes(&description.body[8..])?;
    if entries.len() != 1 {
        return Err(unsupported());
    }
    match (kind, entries[0].kind) {
        (b"vide", [b'a', b'v', b'c', b'1']) => {
            video(entries[0].body)?;
            Ok(true)
        }
        (b"soun", [b'm', b'p', b'4', b'a']) => {
            audio(entries[0].body)?;
            Ok(false)
        }
        _ => Err(unsupported()),
    }
}

fn video(body: &[u8]) -> io::Result<()> {
    if body.len() < 78 || body.get(6..8) != Some(&1u16.to_be_bytes()) {
        return Err(unsupported());
    }
    let width = u16::from_be_bytes(body[24..26].try_into().map_err(|_| unsupported())?);
    let height = u16::from_be_bytes(body[26..28].try_into().map_err(|_| unsupported())?);
    if width == 0 || height == 0 || width > 4_096 || height > 2_160 {
        return Err(unsupported());
    }
    let configuration = one(&body[78..], b"avcC")?;
    if configuration.encoded.len() > 8_192 {
        return Err(unsupported());
    }
    let mut cursor = Cursor::new(configuration.encoded);
    cursor.set_position(8);
    let avc = mp4::avc1::AvcCBox::read_box(&mut cursor, configuration.encoded.len() as u64)
        .map_err(|_| unsupported())?;
    if avc.configuration_version != 1
        || avc.length_size_minus_one != 3
        || !matches!(avc.avc_profile_indication, 66 | 77 | 100)
        || avc.avc_level_indication == 0
        || avc.avc_level_indication > 52
        || avc.sequence_parameter_sets.len() != 1
        || avc.picture_parameter_sets.len() != 1
    {
        return Err(unsupported());
    }
    let sps = &avc.sequence_parameter_sets[0].bytes;
    let pps = &avc.picture_parameter_sets[0].bytes;
    if sps.len() < 4
        || sps[0] & 31 != 7
        || sps[1] != avc.avc_profile_indication
        || sps[3] != avc.avc_level_indication
        || pps.first().is_none_or(|nal| nal & 31 != 8)
    {
        return Err(unsupported());
    }
    Ok(())
}

fn audio(body: &[u8]) -> io::Result<()> {
    if body.len() < 28 || body.get(6..8) != Some(&1u16.to_be_bytes()) {
        return Err(unsupported());
    }
    let channels = u16::from_be_bytes(body[16..18].try_into().map_err(|_| unsupported())?);
    let rate = u32::from_be_bytes(body[24..28].try_into().map_err(|_| unsupported())?);
    if !matches!(channels, 1 | 2)
        || body[18..20] != 16u16.to_be_bytes()
        || !matches!(rate, 0xac440000 | 0xbb800000)
    {
        return Err(unsupported());
    }
    let configuration = one(&body[28..], b"esds")?;
    if configuration.encoded.len() > 512 {
        return Err(unsupported());
    }
    let mut cursor = Cursor::new(configuration.encoded);
    cursor.set_position(8);
    let esds = mp4::mp4a::EsdsBox::read_box(&mut cursor, configuration.encoded.len() as u64)
        .map_err(|_| unsupported())?;
    let decoder = esds.es_desc.dec_config;
    let audio = decoder.dec_specific;
    if decoder.object_type_indication != 0x40
        || decoder.stream_type != 5
        || audio.profile != 2
        || u16::from(audio.chan_conf) != channels
        || !matches!((audio.freq_index, rate), (3, 0xbb800000) | (4, 0xac440000))
    {
        return Err(unsupported());
    }
    Ok(())
}

#[cfg(test)]
#[path = "codec_tests.rs"]
mod tests;
