use reo_proto::magic::BC_MAGIC;

/// Build raw header bytes for testing. Produces 20 or 24 bytes depending
/// on whether `extension` is provided.
pub fn make_header_bytes(
    msg_id: u32,
    body_len: u32,
    encryption_offset: u32,
    status_class: u32,
    extension: Option<u32>,
) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&BC_MAGIC.to_le_bytes());
    buf.extend_from_slice(&msg_id.to_le_bytes());
    buf.extend_from_slice(&body_len.to_le_bytes());
    buf.extend_from_slice(&encryption_offset.to_le_bytes());
    buf.extend_from_slice(&status_class.to_le_bytes());
    if let Some(ext) = extension {
        buf.extend_from_slice(&ext.to_le_bytes());
    }
    buf
}

/// Build a complete TalkAbility response body for a selected channel.
#[expect(dead_code)]
pub fn talk_ability_response(channel: u8) -> Vec<u8> {
    let extension = format!(
        "<Extension version=\"1.1\"><channelId>{channel}</channelId></Extension>"
    );
    let xml = br#"<body><TalkAbility version="1.1"><duplexList><duplex>fullDuplex</duplex></duplexList><audioStreamModeList><audioStreamMode>speaker</audioStreamMode></audioStreamModeList><audioConfigList><audioConfig><audioType>adpcm</audioType><sampleRate>16000</sampleRate><samplePrecision>16</samplePrecision><lengthPerEncoder>640</lengthPerEncoder><soundTrack>mono</soundTrack></audioConfig></audioConfigList></TalkAbility></body>"#;
    let mut response = extension.into_bytes();
    response.extend_from_slice(xml);
    response
}
