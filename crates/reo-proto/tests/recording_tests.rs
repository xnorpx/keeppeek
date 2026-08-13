//! Phase 8 integration tests: Recording, storage & playback.
//!
//! Tests cover command → TcpSend wire format and response → Event round-trips
//! through the full BcSession state machine.

use arrayvec::ArrayString;
use reo_proto::{magic::*, recording::*, *};
use std::time::Instant;

mod common;
use common::make_header_bytes;

// ── Wire message helpers ─────────────────────────────────────────────

fn make_wire_message(
    msg_id: u32,
    body: &[u8],
    status_class: u32,
    extension: Option<u32>,
) -> Vec<u8> {
    let mut wire = make_header_bytes(
        msg_id,
        body.len() as u32,
        body.len() as u32,
        status_class,
        extension,
    );
    wire.extend_from_slice(body);
    wire
}

fn drain_tcp_sends(session: &mut BcSession) -> Vec<u8> {
    let mut result = Vec::new();
    let mut buf = [0u8; 8192];
    while let Output::TcpSend { data } = session.poll_output(&mut buf).unwrap() {
        result.extend_from_slice(data);
    }
    result
}

// ── Test: SearchRecording command ────────────────────────────────────

#[test]
fn test_recording_search_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

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

    session
        .handle_input(Input::Command(Command::Recording(
            RecordingCommand::SearchRecording {
                channel: 0,
                start,
                end,
            },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_RECORDING_SEARCH);
    assert!(header.is_modern());

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<RecordList"));
    assert!(body_str.contains("<year>2024</year>"));
}

// ── Test: SearchRecording response ──────────────────────────────────

#[test]
fn test_recording_search_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

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

    let wire = make_wire_message(
        COMMAND_RECORDING_SEARCH,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Recording(RecordingEvent::SearchResult(entries))) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].filename.as_str(), "/rec/clip1.mp4");
            assert_eq!(entries[0].record_type.as_str(), "motion");
            assert_eq!(entries[0].start.hour, 10);
            assert_eq!(entries[0].end.minute, 35);
        }
        other => panic!("expected Recording(SearchResult), got {other:?}"),
    }
}

// ── Test: Calendar round-trip ───────────────────────────────────────

#[test]
fn test_recording_calendar_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Recording(
            RecordingCommand::GetRecordingCalendar {
                channel: 0,
                year: 2024,
                month: 3,
            },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_RECORDING_CALENDAR);

    // Feed response with specific day bits
    let mask: u32 = (1 << 0) | (1 << 4) | (1 << 14) | (1 << 30);
    let xml = format!(
        "<body>\
            <RecordingCalendar version=\"1.1\">\
                <channelId>0</channelId>\
                <year>2024</year>\
                <month>3</month>\
                <dayMask>{mask}</dayMask>\
            </RecordingCalendar>\
        </body>"
    );

    let resp = make_wire_message(
        COMMAND_RECORDING_CALENDAR,
        xml.as_bytes(),
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Recording(RecordingEvent::Calendar(cal))) => {
            assert_eq!(cal.year, 2024);
            assert_eq!(cal.month, 3);
            assert!(cal.day_mask & (1 << 0) != 0, "day 1");
            assert!(cal.day_mask & (1 << 4) != 0, "day 5");
            assert!(cal.day_mask & (1 << 14) != 0, "day 15");
            assert!(cal.day_mask & (1 << 30) != 0, "day 31");
            assert!(cal.day_mask & (1 << 1) == 0, "day 2 not set");
        }
        other => panic!("expected Recording(Calendar), got {other:?}"),
    }
}

// ── Test: FileOpen command and response ─────────────────────────────

#[test]
fn test_recording_file_open_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Recording(
            RecordingCommand::FileOpen {
                channel: 0,
                filename: ArrayString::try_from("/mnt/sd/rec/clip.mp4").unwrap(),
            },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_FILE_OPEN);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<fileName>/mnt/sd/rec/clip.mp4</fileName>"));

    // Feed response
    let xml = b"<body>\
        <FileInfo version=\"1.1\">\
            <handle>7</handle>\
            <size>1048576</size>\
        </FileInfo>\
    </body>";

    let resp = make_wire_message(
        COMMAND_FILE_OPEN,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Recording(RecordingEvent::FileOpened { handle, size })) => {
            assert_eq!(handle, 7);
            assert_eq!(size, 1048576);
        }
        other => panic!("expected Recording(FileOpened), got {other:?}"),
    }
}

// ── Test: FileClose command and response ────────────────────────────

#[test]
fn test_recording_file_close_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Recording(
            RecordingCommand::FileClose { handle: 7 },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_FILE_CLOSE);

    // Feed response
    let xml = b"<body><FileInfo version=\"1.1\"></FileInfo></body>";
    let resp = make_wire_message(
        COMMAND_FILE_CLOSE,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Recording(RecordingEvent::FileClosed)) => {}
        other => panic!("expected Recording(FileClosed), got {other:?}"),
    }
}

// ── Test: RecordCfg round-trip ──────────────────────────────────────

#[test]
fn test_recording_cfg_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let cfg = RecordCfgData {
        channel: 0,
        pre_record: true,
        post_record_time: 30,
    };

    session
        .handle_input(Input::Command(Command::Recording(
            RecordingCommand::SetRecordCfg(cfg),
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_RECORD_CFG_WRITE);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<preRecord>1</preRecord>"));
    assert!(body_str.contains("<postRecordTime>30</postRecordTime>"));

    // Feed ack
    let ack = make_wire_message(
        COMMAND_RECORD_CFG_WRITE,
        b"<body><RecordCfg version=\"1.1\"></RecordCfg></body>",
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &ack)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Recording(RecordingEvent::RecordCfgAck)) => {}
        other => panic!("expected Recording(RecordCfgAck), got {other:?}"),
    }
}

// ── Test: RecordSchedule round-trip ─────────────────────────────────

#[test]
fn test_recording_schedule_round_trip() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Recording(
            RecordingCommand::GetRecordSchedule { channel: 0 },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, _) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_RECORD_SCHEDULE_READ);

    // Feed response
    let xml = b"<body>\
        <RecordSchedule version=\"1.1\">\
            <channelId>0</channelId>\
            <enable>1</enable>\
        </RecordSchedule>\
    </body>";

    let resp = make_wire_message(
        COMMAND_RECORD_SCHEDULE_READ,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &resp)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Recording(RecordingEvent::RecordSchedule(cfg))) => {
            assert!(cfg.enabled);
        }
        other => panic!("expected Recording(RecordSchedule), got {other:?}"),
    }
}

// ── Test: HddInfoList response ──────────────────────────────────────

#[test]
fn test_recording_hdd_info_list() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <HddInfoList version=\"1.1\">\
            <id>0</id>\
            <capacity>500000000000</capacity>\
            <freeSpace>250000000000</freeSpace>\
        </HddInfoList>\
    </body>";

    let wire = make_wire_message(
        COMMAND_HDD_INFO_LIST,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Recording(RecordingEvent::HddInfoList(list))) => {
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].id, 0);
            assert_eq!(list[0].capacity, 500_000_000_000);
            assert_eq!(list[0].free_space, 250_000_000_000);
        }
        other => panic!("expected Recording(HddInfoList), got {other:?}"),
    }
}

// ── Test: FileData binary event ─────────────────────────────────────

#[test]
fn test_recording_file_data_binary() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Simulate binary file data response (ID 6 with binary class)
    let file_data = b"\x00\x01\x02\x03\x04\x05\x06\x07";
    let wire = make_wire_message(
        COMMAND_FILE_READ,
        file_data,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::FileData { data }) => {
            assert_eq!(data.len(), 8);
            assert_eq!(data[0], 0x00);
            assert_eq!(data[7], 0x07);
        }
        other => panic!("expected FileData, got {other:?}"),
    }
}

// ── Test: ThumbnailData binary event ────────────────────────────────

#[test]
fn test_recording_thumbnail_data_binary() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    // Simulate binary thumbnail response (ID 298 with binary class)
    let thumb_data = b"\xFF\xD8\xFF\xE0JFIF";
    let wire = make_wire_message(
        COMMAND_RECORD_THUMBNAIL,
        thumb_data,
        make_status(BC_CLASS_LEGACY, 0),
        None,
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::ThumbnailData { data }) => {
            assert_eq!(data.len(), thumb_data.len());
            assert_eq!(&data[..4], &[0xFF, 0xD8, 0xFF, 0xE0]);
        }
        other => panic!("expected ThumbnailData, got {other:?}"),
    }
}

// ── Test: Recording commands wrong role ─────────────────────────────

#[test]
fn test_recording_commands_wrong_role() {
    let now = Instant::now();
    let mut session = BcSession::new(BcSessionConfig::default_camera(), now);

    let result = session.handle_input(Input::Command(Command::Recording(
        RecordingCommand::GetHddInfoList,
    )));
    assert!(matches!(result, Err(BcError::WrongRole)));
}

// ── Test: GetThumbnail command ──────────────────────────────────────

#[test]
fn test_recording_get_thumbnail_command() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    session
        .handle_input(Input::Command(Command::Recording(
            RecordingCommand::GetThumbnail {
                channel: 0,
                filename: ArrayString::try_from("/mnt/sd/thumb.jpg").unwrap(),
            },
        )))
        .unwrap();

    let wire = drain_tcp_sends(&mut session);
    let (header, hdr_len) = PacketHeader::parse(&wire).unwrap();
    assert_eq!(header.msg_id, COMMAND_RECORD_THUMBNAIL);

    let body = &wire[hdr_len..hdr_len + header.body_len as usize];
    let body_str = std::str::from_utf8(body).unwrap();
    assert!(body_str.contains("<RecordThumbnail"));
    assert!(body_str.contains("<fileName>/mnt/sd/thumb.jpg</fileName>"));
}

// ── Test: MonthSearch response ──────────────────────────────────────

#[test]
fn test_recording_month_search_response() {
    let now = Instant::now();
    let mut session = BcSession::default_client(now);
    session.set_state(SessionState::Connected);

    let xml = b"<body>\
        <RecordList version=\"1.1\">\
            <fileName>/rec/clip1.mp4</fileName>\
            <recordType>manual</recordType>\
        </RecordList>\
    </body>";

    let wire = make_wire_message(
        COMMAND_RECORDING_SEARCH_MONTH,
        xml,
        make_status(BC_CLASS_MODERN_EXT, 0),
        Some(0),
    );
    session.handle_input(Input::TcpData(now, &wire)).unwrap();

    let mut buf = [0u8; 4096];
    match session.poll_output(&mut buf).unwrap() {
        Output::Event(Event::Recording(RecordingEvent::MonthSearchResult(entries))) => {
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].record_type.as_str(), "manual");
        }
        other => panic!("expected Recording(MonthSearchResult), got {other:?}"),
    }
}
