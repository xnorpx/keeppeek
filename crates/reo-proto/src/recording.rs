//! Recording, storage & playback commands.
//!
//! Provides builders and parsers for recording search, file open/read/close,
//! recording config, schedule, HDD info, thumbnails, and calendar queries.

use crate::{error::BcError, header::PacketHeader, magic::*, xml};
use arrayvec::ArrayString;

const FILENAME_CAP: usize = 128;
const TYPE_CAP: usize = 32;

/// Recording date/time (no timezone).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecDateTime {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// A single recording entry from a search result.
#[derive(Debug, Clone)]
pub struct RecordingEntry {
    pub start: RecDateTime,
    pub end: RecDateTime,
    pub filename: ArrayString<FILENAME_CAP>,
    pub record_type: ArrayString<TYPE_CAP>,
}

/// Recording calendar (which days in a month have recordings).
#[derive(Debug, Clone, Copy)]
pub struct RecordingCalendar {
    pub channel: u8,
    pub year: u32,
    pub month: u32,
    /// Bitmask: bit 0 = day 1, bit 30 = day 31.
    pub day_mask: u32,
}

/// Recording configuration.
#[derive(Debug, Clone, Copy)]
pub struct RecordCfgData {
    pub channel: u8,
    pub pre_record: bool,
    pub post_record_time: u32,
}

/// Recording schedule configuration.
#[derive(Debug, Clone, Copy)]
pub struct RecordScheduleData {
    pub channel: u8,
    pub enabled: bool,
}

/// HDD information.
#[derive(Debug, Clone, Copy)]
pub struct HddInfoData {
    pub id: u32,
    pub capacity: u64,
    pub free_space: u64,
}

/// Recording command (client → camera).
#[derive(Debug, Clone)]
pub enum RecordingCommand {
    /// Search recordings by date range (ID 272).
    SearchRecording {
        channel: u8,
        start: RecDateTime,
        end: RecDateTime,
    },
    /// Search recordings by month (ID 273).
    SearchRecordingMonth { channel: u8, year: u32, month: u32 },
    /// Get recording calendar (ID 274).
    GetRecordingCalendar { channel: u8, year: u32, month: u32 },
    /// Open a file for playback (ID 5).
    FileOpen {
        channel: u8,
        filename: ArrayString<FILENAME_CAP>,
    },
    /// Read file data (ID 6).
    FileRead { handle: u32 },
    /// Close file (ID 7).
    FileClose { handle: u32 },
    /// Read recording config (ID 54).
    GetRecordCfg { channel: u8 },
    /// Write recording config (ID 55).
    SetRecordCfg(RecordCfgData),
    /// Read recording schedule (ID 81).
    GetRecordSchedule { channel: u8 },
    /// Write recording schedule (ID 82).
    SetRecordSchedule(RecordScheduleData),
    /// Get HDD info list (ID 102).
    GetHddInfoList,
    /// Get thumbnail for a recording (ID 298).
    GetThumbnail {
        channel: u8,
        filename: ArrayString<FILENAME_CAP>,
    },
}

/// Recording event (camera → client).
#[derive(Debug, Clone)]
pub enum RecordingEvent {
    /// Search result (ID 272).
    SearchResult(Vec<RecordingEntry>),
    /// Month search result (ID 273).
    MonthSearchResult(Vec<RecordingEntry>),
    /// Calendar day bitmask (ID 274).
    Calendar(RecordingCalendar),
    /// File opened with handle and size (ID 5).
    FileOpened { handle: u32, size: u64 },
    /// File closed (ID 7).
    FileClosed,
    /// Recording config response (ID 54).
    RecordCfg(RecordCfgData),
    /// Recording config written ack (ID 55).
    RecordCfgAck,
    /// Recording schedule response (ID 81).
    RecordSchedule(RecordScheduleData),
    /// Recording schedule written ack (ID 82).
    RecordScheduleAck,
    /// HDD info list (ID 102).
    HddInfoList(Vec<HddInfoData>),
}

#[derive(Debug, Clone, Copy)]
pub enum RecordingResponseKind {
    SearchRecording,
    SearchRecordingMonth,
    Calendar,
    FileOpen,
    FileClose,
    RecordCfgRead,
    RecordCfgWrite,
    RecordScheduleRead,
    RecordScheduleWrite,
    HddInfoList,
}

/// Classify an incoming msg_id as a recording response.
pub const fn classify_response(msg_id: u32) -> Option<RecordingResponseKind> {
    match msg_id {
        crate::COMMAND_RECORDING_SEARCH => Some(RecordingResponseKind::SearchRecording),
        crate::COMMAND_RECORDING_SEARCH_MONTH => Some(RecordingResponseKind::SearchRecordingMonth),
        crate::COMMAND_RECORDING_CALENDAR => Some(RecordingResponseKind::Calendar),
        crate::COMMAND_FILE_OPEN | crate::COMMAND_COVER_FILE_OPEN => {
            Some(RecordingResponseKind::FileOpen)
        }
        crate::COMMAND_FILE_CLOSE | crate::COMMAND_COVER_FILE_CLOSE => {
            Some(RecordingResponseKind::FileClose)
        }
        crate::COMMAND_RECORD_CFG_READ => Some(RecordingResponseKind::RecordCfgRead),
        crate::COMMAND_RECORD_CFG_WRITE => Some(RecordingResponseKind::RecordCfgWrite),
        crate::COMMAND_RECORD_SCHEDULE_READ => Some(RecordingResponseKind::RecordScheduleRead),
        crate::COMMAND_RECORD_SCHEDULE_WRITE => Some(RecordingResponseKind::RecordScheduleWrite),
        crate::COMMAND_HDD_INFO_LIST => Some(RecordingResponseKind::HddInfoList),
        _ => None,
    }
}

pub fn build_request(
    cmd: &RecordingCommand,
    buf: &mut [u8],
) -> Result<(PacketHeader, usize), BcError> {
    match cmd {
        RecordingCommand::SearchRecording {
            channel,
            start,
            end,
        } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("RecordList", "1.1");
                b.u8_element("channelId", *channel);
                b.start("startTime");
                write_datetime(b, start);
                b.end();
                b.start("endTime");
                write_datetime(b, end);
                b.end();
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_RECORDING_SEARCH, len), len))
        }
        RecordingCommand::SearchRecordingMonth {
            channel,
            year,
            month,
        } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("RecordList", "1.1");
                b.u8_element("channelId", *channel);
                b.u32_element("year", *year);
                b.u32_element("month", *month);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_RECORDING_SEARCH_MONTH, len), len))
        }
        RecordingCommand::GetRecordingCalendar {
            channel,
            year,
            month,
        } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("RecordingCalendar", "1.1");
                b.u8_element("channelId", *channel);
                b.u32_element("year", *year);
                b.u32_element("month", *month);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_RECORDING_CALENDAR, len), len))
        }
        RecordingCommand::FileOpen { channel, filename } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("FileInfo", "1.1");
                b.u8_element("channelId", *channel);
                b.text_element("fileName", filename.as_str());
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_FILE_OPEN, len), len))
        }
        RecordingCommand::FileRead { handle } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("FileData", "1.1");
                b.u32_element("handle", *handle);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_FILE_READ, len), len))
        }
        RecordingCommand::FileClose { handle } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("FileInfo", "1.1");
                b.u32_element("handle", *handle);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_FILE_CLOSE, len), len))
        }
        RecordingCommand::GetRecordCfg { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("RecordCfg", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_RECORD_CFG_READ, len), len))
        }
        RecordingCommand::SetRecordCfg(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("RecordCfg", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("preRecord", if cfg.pre_record { "1" } else { "0" });
                b.u32_element("postRecordTime", cfg.post_record_time);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_RECORD_CFG_WRITE, len), len))
        }
        RecordingCommand::GetRecordSchedule { channel } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("RecordSchedule", "1.1");
                b.u8_element("channelId", *channel);
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_RECORD_SCHEDULE_READ, len), len))
        }
        RecordingCommand::SetRecordSchedule(cfg) => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("RecordSchedule", "1.1");
                b.u8_element("channelId", cfg.channel);
                b.text_element("enable", if cfg.enabled { "1" } else { "0" });
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_RECORD_SCHEDULE_WRITE, len), len))
        }
        RecordingCommand::GetHddInfoList => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("HddInfoList", "1.1");
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_HDD_INFO_LIST, len), len))
        }
        RecordingCommand::GetThumbnail { channel, filename } => {
            let len = xml::build_xml(buf, |b| {
                b.start_versioned("RecordThumbnail", "1.1");
                b.u8_element("channelId", *channel);
                b.text_element("fileName", filename.as_str());
                b.end();
            })?;
            Ok((make_header(crate::COMMAND_RECORD_THUMBNAIL, len), len))
        }
    }
}

pub fn parse_response(kind: RecordingResponseKind, body: &[u8]) -> Result<RecordingEvent, BcError> {
    match kind {
        RecordingResponseKind::SearchRecording => {
            let entries = parse_recording_entries(body)?;
            Ok(RecordingEvent::SearchResult(entries))
        }
        RecordingResponseKind::SearchRecordingMonth => {
            let entries = parse_recording_entries(body)?;
            Ok(RecordingEvent::MonthSearchResult(entries))
        }
        RecordingResponseKind::Calendar => {
            let mut cal = RecordingCalendar {
                channel: 0,
                year: 0,
                month: 0,
                day_mask: 0,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        cal.channel = v;
                    }
                }
                "year" => {
                    if let Ok(v) = text.parse::<u32>() {
                        cal.year = v;
                    }
                }
                "month" => {
                    if let Ok(v) = text.parse::<u32>() {
                        cal.month = v;
                    }
                }
                "dayMask" | "mask" => {
                    if let Ok(v) = text.parse::<u32>() {
                        cal.day_mask = v;
                    }
                }
                _ => {}
            })?;
            Ok(RecordingEvent::Calendar(cal))
        }
        RecordingResponseKind::FileOpen => {
            let mut handle: u32 = 0;
            let mut size: u64 = 0;
            xml::parse_xml(body, |name, text| match name {
                "handle" => {
                    if let Ok(v) = text.parse::<u32>() {
                        handle = v;
                    }
                }
                "size" | "fileSize" => {
                    if let Ok(v) = text.parse::<u64>() {
                        size = v;
                    }
                }
                _ => {}
            })?;
            Ok(RecordingEvent::FileOpened { handle, size })
        }
        RecordingResponseKind::FileClose => Ok(RecordingEvent::FileClosed),
        RecordingResponseKind::RecordCfgRead => {
            let mut cfg = RecordCfgData {
                channel: 0,
                pre_record: false,
                post_record_time: 0,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        cfg.channel = v;
                    }
                }
                "preRecord" => cfg.pre_record = text == "1" || text.eq_ignore_ascii_case("true"),
                "postRecordTime" => {
                    if let Ok(v) = text.parse::<u32>() {
                        cfg.post_record_time = v;
                    }
                }
                _ => {}
            })?;
            Ok(RecordingEvent::RecordCfg(cfg))
        }
        RecordingResponseKind::RecordCfgWrite => Ok(RecordingEvent::RecordCfgAck),
        RecordingResponseKind::RecordScheduleRead => {
            let mut cfg = RecordScheduleData {
                channel: 0,
                enabled: false,
            };
            xml::parse_xml(body, |name, text| match name {
                "channelId" => {
                    if let Ok(v) = text.parse::<u8>() {
                        cfg.channel = v;
                    }
                }
                "enable" => cfg.enabled = text == "1" || text.eq_ignore_ascii_case("true"),
                _ => {}
            })?;
            Ok(RecordingEvent::RecordSchedule(cfg))
        }
        RecordingResponseKind::RecordScheduleWrite => Ok(RecordingEvent::RecordScheduleAck),
        RecordingResponseKind::HddInfoList => {
            let entries = parse_hdd_entries(body)?;
            Ok(RecordingEvent::HddInfoList(entries))
        }
    }
}

fn write_datetime(b: &mut xml::XmlBuilder<'_>, dt: &RecDateTime) {
    b.u32_element("year", dt.year);
    b.u32_element("month", dt.month);
    b.u32_element("day", dt.day);
    b.u32_element("hour", dt.hour);
    b.u32_element("minute", dt.minute);
    b.u32_element("second", dt.second);
}

fn parse_recording_entries(body: &[u8]) -> Result<Vec<RecordingEntry>, BcError> {
    use ::xml::reader::{EventReader, XmlEvent};
    use arrayvec::ArrayString as AS;

    let mut entries = Vec::new();
    let mut current = RecordingEntry {
        start: RecDateTime::default(),
        end: RecDateTime::default(),
        filename: ArrayString::new(),
        record_type: ArrayString::new(),
    };
    let mut seen_filename = false;

    // Track nesting to distinguish startTime/endTime child elements
    #[derive(PartialEq)]
    enum Section {
        None,
        Start,
        End,
    }
    let mut section = Section::None;
    let mut current_element: Option<AS<64>> = None;

    let reader = EventReader::new(body);
    for event in reader {
        match event {
            Ok(XmlEvent::StartElement { name, .. }) => {
                let local = name.local_name.as_str();
                match local {
                    "startTime" => {
                        section = Section::Start;
                        current_element = None;
                    }
                    "endTime" => {
                        section = Section::End;
                        current_element = None;
                    }
                    _ => {
                        current_element = AS::try_from(local).ok();
                    }
                }
            }
            Ok(XmlEvent::Characters(text)) | Ok(XmlEvent::CData(text)) => {
                if let Some(ref elem) = current_element {
                    match elem.as_str() {
                        "year" => {
                            if let Ok(v) = text.parse::<u32>() {
                                match section {
                                    Section::Start => current.start.year = v,
                                    Section::End => current.end.year = v,
                                    Section::None => {}
                                }
                            }
                        }
                        "month" => {
                            if let Ok(v) = text.parse::<u32>() {
                                match section {
                                    Section::Start => current.start.month = v,
                                    Section::End => current.end.month = v,
                                    Section::None => {}
                                }
                            }
                        }
                        "day" => {
                            if let Ok(v) = text.parse::<u32>() {
                                match section {
                                    Section::Start => current.start.day = v,
                                    Section::End => current.end.day = v,
                                    Section::None => {}
                                }
                            }
                        }
                        "hour" => {
                            if let Ok(v) = text.parse::<u32>() {
                                match section {
                                    Section::Start => current.start.hour = v,
                                    Section::End => current.end.hour = v,
                                    Section::None => {}
                                }
                            }
                        }
                        "minute" => {
                            if let Ok(v) = text.parse::<u32>() {
                                match section {
                                    Section::Start => current.start.minute = v,
                                    Section::End => current.end.minute = v,
                                    Section::None => {}
                                }
                            }
                        }
                        "second" => {
                            if let Ok(v) = text.parse::<u32>() {
                                match section {
                                    Section::Start => current.start.second = v,
                                    Section::End => current.end.second = v,
                                    Section::None => {}
                                }
                            }
                        }
                        "fileName" | "filename" => {
                            if seen_filename {
                                entries.push(current.clone());
                                current = RecordingEntry {
                                    start: RecDateTime::default(),
                                    end: RecDateTime::default(),
                                    filename: ArrayString::new(),
                                    record_type: ArrayString::new(),
                                };
                            }
                            let _ =
                                ArrayString::try_from(text.as_str()).map(|s| current.filename = s);
                            seen_filename = true;
                        }
                        "recordType" | "type" => {
                            let _ = ArrayString::try_from(text.as_str())
                                .map(|s| current.record_type = s);
                        }
                        _ => {}
                    }
                }
            }
            Ok(XmlEvent::EndElement { name, .. }) => {
                let local = name.local_name.as_str();
                if local == "startTime" || local == "endTime" {
                    section = Section::None;
                }
                current_element = None;
            }
            Ok(XmlEvent::EndDocument) => break,
            Err(_) => return Err(BcError::XmlParse("malformed XML")),
            _ => {}
        }
    }

    if seen_filename {
        entries.push(current);
    }

    Ok(entries)
}

fn parse_hdd_entries(body: &[u8]) -> Result<Vec<HddInfoData>, BcError> {
    let mut entries = Vec::new();
    let mut current = HddInfoData {
        id: 0,
        capacity: 0,
        free_space: 0,
    };
    let mut seen_id = false;

    xml::parse_xml(body, |name, text| match name {
        "id" | "hddId" => {
            if seen_id {
                entries.push(current);
                current = HddInfoData {
                    id: 0,
                    capacity: 0,
                    free_space: 0,
                };
            }
            if let Ok(v) = text.parse::<u32>() {
                current.id = v;
            }
            seen_id = true;
        }
        "capacity" | "totalSize" => {
            if let Ok(v) = text.parse::<u64>() {
                current.capacity = v;
            }
        }
        "freeSpace" | "freeSize" => {
            if let Ok(v) = text.parse::<u64>() {
                current.free_space = v;
            }
        }
        _ => {}
    })?;

    if seen_id {
        entries.push(current);
    }

    Ok(entries)
}

const fn make_header(msg_id: u32, body_len: usize) -> PacketHeader {
    PacketHeader {
        msg_id,
        body_len: body_len as u32,
        encryption_offset: 0,
        status_class: make_status(BC_CLASS_MODERN_EXT, 0),
        extension: Some(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_recording_ids() {
        assert!(matches!(
            classify_response(crate::COMMAND_RECORDING_SEARCH),
            Some(RecordingResponseKind::SearchRecording)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_RECORDING_SEARCH_MONTH),
            Some(RecordingResponseKind::SearchRecordingMonth)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_RECORDING_CALENDAR),
            Some(RecordingResponseKind::Calendar)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_FILE_OPEN),
            Some(RecordingResponseKind::FileOpen)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_COVER_FILE_OPEN),
            Some(RecordingResponseKind::FileOpen)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_FILE_CLOSE),
            Some(RecordingResponseKind::FileClose)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_COVER_FILE_CLOSE),
            Some(RecordingResponseKind::FileClose)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_RECORD_CFG_READ),
            Some(RecordingResponseKind::RecordCfgRead)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_RECORD_CFG_WRITE),
            Some(RecordingResponseKind::RecordCfgWrite)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_RECORD_SCHEDULE_READ),
            Some(RecordingResponseKind::RecordScheduleRead)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_RECORD_SCHEDULE_WRITE),
            Some(RecordingResponseKind::RecordScheduleWrite)
        ));
        assert!(matches!(
            classify_response(crate::COMMAND_HDD_INFO_LIST),
            Some(RecordingResponseKind::HddInfoList)
        ));
        assert!(classify_response(999).is_none());
    }

    #[test]
    fn build_search_recording() {
        let mut buf = [0u8; 1024];
        let start = RecDateTime {
            year: 2024,
            month: 1,
            day: 15,
            hour: 0,
            minute: 0,
            second: 0,
        };
        let end = RecDateTime {
            year: 2024,
            month: 1,
            day: 15,
            hour: 23,
            minute: 59,
            second: 59,
        };
        let (hdr, len) = build_request(
            &RecordingCommand::SearchRecording {
                channel: 0,
                start,
                end,
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_RECORDING_SEARCH);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<RecordList"));
        assert!(xml.contains("<channelId>0</channelId>"));
        assert!(xml.contains("<year>2024</year>"));
        assert!(xml.contains("<hour>23</hour>"));
    }

    #[test]
    fn build_search_month() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(
            &RecordingCommand::SearchRecordingMonth {
                channel: 0,
                year: 2024,
                month: 6,
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_RECORDING_SEARCH_MONTH);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<month>6</month>"));
    }

    #[test]
    fn build_get_calendar() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(
            &RecordingCommand::GetRecordingCalendar {
                channel: 0,
                year: 2024,
                month: 3,
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_RECORDING_CALENDAR);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<RecordingCalendar"));
    }

    #[test]
    fn build_file_open() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(
            &RecordingCommand::FileOpen {
                channel: 0,
                filename: ArrayString::try_from("/mnt/sd/rec/2024.mp4").unwrap(),
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_FILE_OPEN);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<fileName>/mnt/sd/rec/2024.mp4</fileName>"));
    }

    #[test]
    fn build_file_read() {
        let mut buf = [0u8; 512];
        let (hdr, len) =
            build_request(&RecordingCommand::FileRead { handle: 42 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_FILE_READ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<handle>42</handle>"));
    }

    #[test]
    fn build_file_close() {
        let mut buf = [0u8; 512];
        let (hdr, len) =
            build_request(&RecordingCommand::FileClose { handle: 42 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_FILE_CLOSE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<handle>42</handle>"));
    }

    #[test]
    fn build_get_record_cfg() {
        let mut buf = [0u8; 512];
        let (hdr, len) =
            build_request(&RecordingCommand::GetRecordCfg { channel: 0 }, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_RECORD_CFG_READ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<RecordCfg"));
    }

    #[test]
    fn build_set_record_cfg() {
        let cfg = RecordCfgData {
            channel: 0,
            pre_record: true,
            post_record_time: 30,
        };
        let mut buf = [0u8; 1024];
        let (hdr, len) = build_request(&RecordingCommand::SetRecordCfg(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_RECORD_CFG_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<preRecord>1</preRecord>"));
        assert!(xml.contains("<postRecordTime>30</postRecordTime>"));
    }

    #[test]
    fn build_get_record_schedule() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(
            &RecordingCommand::GetRecordSchedule { channel: 0 },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_RECORD_SCHEDULE_READ);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<RecordSchedule"));
    }

    #[test]
    fn build_set_record_schedule() {
        let cfg = RecordScheduleData {
            channel: 0,
            enabled: true,
        };
        let mut buf = [0u8; 512];
        let (hdr, len) =
            build_request(&RecordingCommand::SetRecordSchedule(cfg), &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_RECORD_SCHEDULE_WRITE);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<enable>1</enable>"));
    }

    #[test]
    fn build_get_hdd_info_list() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(&RecordingCommand::GetHddInfoList, &mut buf).unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_HDD_INFO_LIST);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<HddInfoList"));
    }

    #[test]
    fn build_get_thumbnail() {
        let mut buf = [0u8; 512];
        let (hdr, len) = build_request(
            &RecordingCommand::GetThumbnail {
                channel: 0,
                filename: ArrayString::try_from("/mnt/sd/thumb.jpg").unwrap(),
            },
            &mut buf,
        )
        .unwrap();
        assert_eq!(hdr.msg_id, crate::COMMAND_RECORD_THUMBNAIL);
        let xml = std::str::from_utf8(&buf[..len]).unwrap();
        assert!(xml.contains("<fileName>/mnt/sd/thumb.jpg</fileName>"));
    }

    #[test]
    fn parse_search_result() {
        let xml = b"<body>\
            <RecordList version=\"1.1\">\
                <fileName>/rec/clip1.mp4</fileName>\
                <recordType>motion</recordType>\
                <startTime>\
                    <year>2024</year>\
                    <month>1</month>\
                    <day>15</day>\
                    <hour>10</hour>\
                    <minute>30</minute>\
                    <second>0</second>\
                </startTime>\
                <endTime>\
                    <year>2024</year>\
                    <month>1</month>\
                    <day>15</day>\
                    <hour>10</hour>\
                    <minute>35</minute>\
                    <second>0</second>\
                </endTime>\
            </RecordList>\
        </body>";
        let event = parse_response(RecordingResponseKind::SearchRecording, xml).unwrap();
        match event {
            RecordingEvent::SearchResult(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].filename.as_str(), "/rec/clip1.mp4");
                assert_eq!(entries[0].record_type.as_str(), "motion");
                assert_eq!(entries[0].start.hour, 10);
                assert_eq!(entries[0].end.minute, 35);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_calendar_response() {
        let xml = b"<body>\
            <RecordingCalendar version=\"1.1\">\
                <channelId>0</channelId>\
                <year>2024</year>\
                <month>3</month>\
                <dayMask>2147483647</dayMask>\
            </RecordingCalendar>\
        </body>";
        let event = parse_response(RecordingResponseKind::Calendar, xml).unwrap();
        match event {
            RecordingEvent::Calendar(cal) => {
                assert_eq!(cal.channel, 0);
                assert_eq!(cal.year, 2024);
                assert_eq!(cal.month, 3);
                // bit 0 = day 1 should be set
                assert!(cal.day_mask & 1 != 0);
                // day 31 = bit 30
                assert!(cal.day_mask & (1 << 30) != 0);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_file_opened() {
        let xml = b"<body>\
            <FileInfo version=\"1.1\">\
                <handle>7</handle>\
                <size>1048576</size>\
            </FileInfo>\
        </body>";
        let event = parse_response(RecordingResponseKind::FileOpen, xml).unwrap();
        match event {
            RecordingEvent::FileOpened { handle, size } => {
                assert_eq!(handle, 7);
                assert_eq!(size, 1048576);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_file_closed() {
        let xml = b"<body><FileInfo version=\"1.1\"></FileInfo></body>";
        let event = parse_response(RecordingResponseKind::FileClose, xml).unwrap();
        assert!(matches!(event, RecordingEvent::FileClosed));
    }

    #[test]
    fn parse_record_cfg() {
        let xml = b"<body>\
            <RecordCfg version=\"1.1\">\
                <channelId>0</channelId>\
                <preRecord>1</preRecord>\
                <postRecordTime>30</postRecordTime>\
            </RecordCfg>\
        </body>";
        let event = parse_response(RecordingResponseKind::RecordCfgRead, xml).unwrap();
        match event {
            RecordingEvent::RecordCfg(cfg) => {
                assert!(cfg.pre_record);
                assert_eq!(cfg.post_record_time, 30);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_record_cfg_ack() {
        let xml = b"<body><RecordCfg version=\"1.1\"></RecordCfg></body>";
        let event = parse_response(RecordingResponseKind::RecordCfgWrite, xml).unwrap();
        assert!(matches!(event, RecordingEvent::RecordCfgAck));
    }

    #[test]
    fn parse_record_schedule() {
        let xml = b"<body>\
            <RecordSchedule version=\"1.1\">\
                <channelId>0</channelId>\
                <enable>1</enable>\
            </RecordSchedule>\
        </body>";
        let event = parse_response(RecordingResponseKind::RecordScheduleRead, xml).unwrap();
        match event {
            RecordingEvent::RecordSchedule(cfg) => {
                assert!(cfg.enabled);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_record_schedule_ack() {
        let xml = b"<body><RecordSchedule version=\"1.1\"></RecordSchedule></body>";
        let event = parse_response(RecordingResponseKind::RecordScheduleWrite, xml).unwrap();
        assert!(matches!(event, RecordingEvent::RecordScheduleAck));
    }

    #[test]
    fn parse_hdd_info_list() {
        let xml = b"<body>\
            <HddInfoList version=\"1.1\">\
                <id>0</id>\
                <capacity>500000000000</capacity>\
                <freeSpace>250000000000</freeSpace>\
            </HddInfoList>\
        </body>";
        let event = parse_response(RecordingResponseKind::HddInfoList, xml).unwrap();
        match event {
            RecordingEvent::HddInfoList(list) => {
                assert_eq!(list.len(), 1);
                assert_eq!(list[0].id, 0);
                assert_eq!(list[0].capacity, 500_000_000_000);
                assert_eq!(list[0].free_space, 250_000_000_000);
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn parse_month_search_result() {
        let xml = b"<body>\
            <RecordList version=\"1.1\">\
                <fileName>/rec/clip1.mp4</fileName>\
                <recordType>manual</recordType>\
            </RecordList>\
        </body>";
        let event = parse_response(RecordingResponseKind::SearchRecordingMonth, xml).unwrap();
        match event {
            RecordingEvent::MonthSearchResult(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].record_type.as_str(), "manual");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn calendar_day_bitmask_specific_days() {
        // Verify specific day bits: days 1, 5, 15, 31
        // day 1 = bit 0 = 1
        // day 5 = bit 4 = 16
        // day 15 = bit 14 = 16384
        // day 31 = bit 30 = 1073741824
        let mask: u32 = (1 << 0) | (1 << 4) | (1 << 14) | (1 << 30);
        let xml_str = format!(
            "<body>\
                <RecordingCalendar version=\"1.1\">\
                    <channelId>0</channelId>\
                    <year>2024</year>\
                    <month>1</month>\
                    <dayMask>{mask}</dayMask>\
                </RecordingCalendar>\
            </body>"
        );
        let event = parse_response(RecordingResponseKind::Calendar, xml_str.as_bytes()).unwrap();
        match event {
            RecordingEvent::Calendar(cal) => {
                assert!(cal.day_mask & (1 << 0) != 0, "day 1 should be set");
                assert!(cal.day_mask & (1 << 4) != 0, "day 5 should be set");
                assert!(cal.day_mask & (1 << 14) != 0, "day 15 should be set");
                assert!(cal.day_mask & (1 << 30) != 0, "day 31 should be set");
                assert!(cal.day_mask & (1 << 1) == 0, "day 2 should not be set");
            }
            _ => panic!("wrong event"),
        }
    }
}
