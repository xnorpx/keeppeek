use reo_proto::magic::*;

#[test]
fn test_buffer_constants_are_reasonable() {
    const { assert!(reo_proto::TCP_RECV_BUF_SIZE >= 64 * 1024) };
    const { assert!(reo_proto::TCP_SEND_BUF_SIZE >= 8 * 1024) };
    const { assert!(reo_proto::MAX_XML_BODY >= 4 * 1024) };
    const { assert!(reo_proto::DEFAULT_MEDIA_OUTPUT_BUFFER_SIZE >= 128 * 1024) };
    const { assert!(reo_proto::MAX_MEDIA_FRAME >= 2_292_974) };
}

#[test]
fn test_message_ids_are_distinct() {
    use reo_proto::*;
    let ids = [
        COMMAND_LOGIN,
        COMMAND_LOGOUT,
        COMMAND_STREAM,
        COMMAND_PREVIEW_STOP,
        COMMAND_TALK_CAPABILITIES,
        COMMAND_PTZ,
        COMMAND_PTZ_PRESET,
        COMMAND_REBOOT,
        COMMAND_START_MOTION_ALARM,
        COMMAND_ALARM_EVENT_LIST,
        COMMAND_EMAIL_READ,
        COMMAND_EMAIL_WRITE,
        COMMAND_OSD_READ,
        COMMAND_OSD_WRITE,
        COMMAND_RECORD_CFG_READ,
        COMMAND_ABILITY_SUPPORT,
        COMMAND_FIRMWARE_DETAILS,
        COMMAND_SNAP,
        COMMAND_PUSH_INFO,
        COMMAND_STREAM_CATALOG,
        COMMAND_CAPABILITY_DETAILS,
        COMMAND_TALK_CONFIG,
        COMMAND_TALK,
        COMMAND_LED_READ,
        COMMAND_LED_WRITE,
        COMMAND_BATTERY_LIST,
        COMMAND_BATTERY_INFO,
        COMMAND_RECORDING_SEARCH,
        COMMAND_TIME_CFG,
        COMMAND_FLOODLIGHT,
        COMMAND_AI_CFG_READ,
        COMMAND_AI_ALARM_READ,
        COMMAND_AI_ALARM_WRITE,
        COMMAND_LINK_TYPE,
        COMMAND_UDP_KEEP_ALIVE,
    ];
    for i in 0..ids.len() {
        for j in (i + 1)..ids.len() {
            assert_ne!(
                ids[i], ids[j],
                "duplicate msg id {} at indices {i} and {j}",
                ids[i]
            );
        }
    }
}

#[test]
fn test_magic_bytes_match_magic_u32() {
    let from_u32 = BC_MAGIC.to_le_bytes();
    assert_eq!(from_u32, reo_proto::magic::BC_MAGIC_BYTES);
}

#[test]
fn test_jpeg_magic_is_distinct() {
    assert_ne!(reo_proto::magic::JPEG_MAGIC, BC_MAGIC);
}
