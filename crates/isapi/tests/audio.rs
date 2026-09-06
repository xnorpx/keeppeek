use isapi::management::{AudioChannel, AudioCodec};

#[test]
fn audio_channels_distinguish_enabled_microphone_speaker_and_codecs() {
    let query = AudioChannel::list().unwrap();
    assert_eq!(
        query.request().resource(),
        "/ISAPI/System/TwoWayAudio/channels"
    );
    let channels = query.parse("application/xml", br#"<TwoWayAudioChannelList><TwoWayAudioChannel><id>1</id><enabled>false</enabled><audioCompressionType>G.711ulaw</audioCompressionType><audioInboundCompressionType>G.711alaw</audioInboundCompressionType><lineOutForbidden>false</lineOutForbidden><micInForbidden>true</micInForbidden><speakerVolume>50</speakerVolume></TwoWayAudioChannel></TwoWayAudioChannelList>"#).unwrap();
    let channel = &channels[0];
    assert!(!channel.enabled());
    assert!(channel.speaker_supported());
    assert!(!channel.microphone_supported());
    assert_eq!(channel.output_codec(), &AudioCodec::G711Ulaw);
    assert_eq!(channel.input_codec(), &AudioCodec::G711Alaw);
    assert_eq!(channel.speaker_volume(), Some(50));
    assert_eq!(
        channel.open().unwrap().request().method(),
        isapi::Method::Put
    );
}

#[test]
fn audio_sessions_preserve_identity_and_encode_it_only_as_query_data() {
    let channel = AudioChannel::query(2).unwrap().parse("application/xml", br#"<TwoWayAudioChannel><id>2</id><enabled>true</enabled><audioCompressionType>G.711ulaw</audioCompressionType></TwoWayAudioChannel>"#).unwrap();
    let session = channel
        .open()
        .unwrap()
        .parse(
            "application/xml",
            br#"<TwoWayAudioSession><sessionId>session &amp; 1</sessionId></TwoWayAudioSession>"#,
        )
        .unwrap();
    let send = session.send_request(channel.id()).unwrap();
    assert_eq!(send.method(), isapi::Method::Put);
    assert_eq!(send.content_type(), Some("application/octet-stream"));
    assert_eq!(
        send.resource(),
        "/ISAPI/System/TwoWayAudio/channels/2/audioData?sessionId=session+%26+1"
    );
    assert_eq!(
        session.receive_request(2).unwrap().method(),
        isapi::Method::Get
    );
    assert_eq!(
        channel.close().unwrap().resource(),
        "/ISAPI/System/TwoWayAudio/channels/2/close"
    );
    assert!(session.send_request(0).is_err());
}

#[test]
fn audio_capability_parsing_rejects_wrong_roots_duplicates_and_invalid_ranges() {
    for body in [
        "<TwoWayAudioChannel><id>0</id><enabled>true</enabled><audioCompressionType>G.711ulaw</audioCompressionType></TwoWayAudioChannel>",
        "<TwoWayAudioChannel><id>1</id><enabled>maybe</enabled><audioCompressionType>G.711ulaw</audioCompressionType></TwoWayAudioChannel>",
        "<TwoWayAudioChannel><id>1</id><enabled>true</enabled><audioCompressionType>G.711ulaw</audioCompressionType><speakerVolume>101</speakerVolume></TwoWayAudioChannel>",
        "<TwoWayAudioChannel><id>1</id><id>2</id><enabled>true</enabled><audioCompressionType>G.711ulaw</audioCompressionType></TwoWayAudioChannel>",
    ] {
        assert!(
            AudioChannel::query(1)
                .unwrap()
                .parse("application/xml", body.as_bytes())
                .is_err()
        );
    }
    let unknown = AudioChannel::query(1).unwrap().parse("application/xml", b"<TwoWayAudioChannel><id>1</id><enabled>true</enabled><audioCompressionType>newCodec</audioCompressionType><lineOutForbidden>true</lineOutForbidden></TwoWayAudioChannel>").unwrap();
    assert!(!unknown.speaker_supported());
    assert!(matches!(unknown.output_codec(), AudioCodec::Other(_)));
}

#[test]
fn audio_capability_options_do_not_replace_the_configured_codec() {
    use isapi::management::{Capabilities, Endpoint};
    let query = Capabilities::query(Endpoint::Audio(1)).unwrap();
    assert_eq!(
        query.request().resource(),
        "/ISAPI/System/TwoWayAudio/channels/1/capabilities"
    );
    let capabilities = query.parse("application/xml", br#"<TwoWayAudioChannelCap><audioCompressionType opt="G.711ulaw,G.711alaw,G.726"/><audioInboundCompressionType opt="G.711ulaw"/></TwoWayAudioChannelCap>"#).unwrap();
    assert_eq!(
        capabilities
            .field(&["audioCompressionType"])
            .unwrap()
            .unwrap()
            .options(),
        ["G.711ulaw", "G.711alaw", "G.726"]
    );
    assert_eq!(
        capabilities
            .field(&["audioInboundCompressionType"])
            .unwrap()
            .unwrap()
            .options(),
        ["G.711ulaw"]
    );
    assert!(Capabilities::query(Endpoint::Audio(0)).is_err());
}
