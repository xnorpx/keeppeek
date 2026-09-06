use std::collections::BTreeMap;
use std::io::Cursor;

use super::wire::CapturedRequest;
use super::{Reply, State};

pub struct Resource {
    pub body: Vec<u8>,
    pub media: &'static str,
}

pub fn defaults() -> anyhow::Result<BTreeMap<String, Resource>> {
    let mut resources = BTreeMap::new();
    for (path, body) in [
        (
            "/ISAPI/System/deviceInfo",
            "<DeviceInfo><deviceName>Fake Hikvision</deviceName><model>FAKE-HIKVISION</model><serialNumber>FAKE0001</serialNumber><firmwareVersion>test-1.0</firmwareVersion></DeviceInfo>",
        ),
        (
            "/ISAPI/System/time",
            "<Time><timeMode>manual</timeMode><localTime>2026-09-05T00:00:00Z</localTime><timeZone>UTC0</timeZone></Time>",
        ),
        (
            "/ISAPI/System/Video/inputs/channels/1/motionDetection",
            "<MotionDetection><enabled>true</enabled><sensitivityLevel>60</sensitivityLevel><MotionDetectionLayout><regionName>retained</regionName></MotionDetectionLayout></MotionDetection>",
        ),
        (
            "/ISAPI/Smart/LineDetection/1",
            "<LineDetection><enabled>true</enabled><LineItemList><LineItem><id>1</id><enabled>true</enabled></LineItem></LineItemList></LineDetection>",
        ),
        (
            "/ISAPI/Smart/FieldDetection/1",
            "<FieldDetection><enabled>true</enabled><FieldDetectionRegionList/></FieldDetection>",
        ),
        (
            "/ISAPI/PTZCtrl/channels/1/status",
            "<PTZStatus><AbsoluteHigh><azimuth>1800</azimuth><elevation>450</elevation><absoluteZoom>10</absoluteZoom></AbsoluteHigh></PTZStatus>",
        ),
        (
            "/ISAPI/PTZCtrl/channels/1/capabilities",
            "<PTZChanelCap><ContinuousPanTiltSpace><XRange><Min>-100</Min><Max>100</Max></XRange><YRange><Min>-100</Min><Max>100</Max></YRange></ContinuousPanTiltSpace><ContinuousZoomSpace><ZRange><Min>-100</Min><Max>100</Max></ZRange></ContinuousZoomSpace><homePostionSupport>true</homePostionSupport><maxPresetNum>32</maxPresetNum></PTZChanelCap>",
        ),
        (
            "/ISAPI/System/TwoWayAudio/channels/1",
            "<TwoWayAudioChannel><id>1</id><enabled>true</enabled><audioCompressionType>G.711ulaw</audioCompressionType><speakerVolume>50</speakerVolume><lineOutForbidden>false</lineOutForbidden><micInForbidden>false</micInForbidden></TwoWayAudioChannel>",
        ),
        (
            "/ISAPI/System/TwoWayAudio/channels/1/capabilities",
            "<TwoWayAudioChannelCap><id opt=\"1\"/><enabled opt=\"true,false\"/><audioCompressionType opt=\"G.711ulaw,G.711alaw\"/><audioInboundCompressionType opt=\"G.711ulaw,G.711alaw\"/><speakerVolume opt=\"0-100\"/><microphoneVolume opt=\"0-100\"/></TwoWayAudioChannelCap>",
        ),
        (
            "/ISAPI/Event/notification/httpHosts/capabilities",
            "<HttpHostNotificationCap><hostNumber>4</hostNumber><httpAuthenticationMethod opt=\"MD5digest\"/><parameterFormatType opt=\"XML,JSON\"/><protocolType opt=\"HTTP,HTTPS\"/><portNo min=\"1\" max=\"65535\"/></HttpHostNotificationCap>",
        ),
    ] {
        resources.insert(
            path.to_owned(),
            Resource {
                body: body.as_bytes().to_vec(),
                media: "application/xml",
            },
        );
    }
    for stream in [101, 102] {
        resources.insert(format!("/ISAPI/Streaming/channels/{stream}"), Resource { body: format!("<StreamingChannel><id>{stream}</id><Video><videoCodecType>H.264</videoCodecType><videoResolutionWidth>320</videoResolutionWidth><videoResolutionHeight>180</videoResolutionHeight><constantBitRate>512</constantBitRate><maxFrameRate>1000</maxFrameRate></Video></StreamingChannel>").into_bytes(), media: "application/xml" });
        let mut jpeg = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(320, 180).write_to(&mut jpeg, image::ImageFormat::Jpeg)?;
        resources.insert(
            format!("/ISAPI/Streaming/channels/{stream}/picture"),
            Resource {
                body: jpeg.into_inner(),
                media: "image/jpeg",
            },
        );
    }
    Ok(resources)
}

pub fn route(request: &CapturedRequest, state: &mut State) -> anyhow::Result<Reply> {
    let path = request.target().split('?').next().unwrap_or_default();
    if request.method() == "PUT" && request.body().is_empty() {
        if path == "/ISAPI/System/TwoWayAudio/channels/1/open" {
            if state.audio_session.is_some() {
                return Ok(status(409, 2, "deviceBusy"));
            }
            let session = state.next_audio_session;
            state.next_audio_session += 1;
            state.audio_session = Some(session);
            state.audio_output.clear();
            return Ok(Reply::http(
                200,
                "application/xml",
                format!(
                    "<TwoWayAudioSession><sessionId>{session}</sessionId></TwoWayAudioSession>"
                ),
            ));
        }
        if path == "/ISAPI/System/TwoWayAudio/channels/1/close" {
            state.audio_session = None;
            return Ok(status(200, 1, "ok"));
        }
    }
    if request.method() == "GET" {
        if path == "/ISAPI/Event/notification/alertStream" {
            return Ok(state.streams.pop_front().unwrap_or_else(|| Reply::raw(b"HTTP/1.1 200 OK\r\nContent-Type: multipart/mixed; boundary=camera\r\nConnection: close\r\n\r\n".to_vec()).hold_open()));
        }
        if let Some(resource) = state.resources.get(path) {
            return Ok(Reply::http(200, resource.media, &resource.body));
        }
        if path.ends_with("/capabilities") {
            return Ok(Reply::http(
                200,
                "application/xml",
                "<Capabilities><enabled opt=\"true,false\"/></Capabilities>",
            ));
        }
        for (prefix, root) in [
            ("/ISAPI/Streaming/channels", "StreamingChannelList"),
            (
                "/ISAPI/Event/notification/httpHosts",
                "HttpHostNotificationList",
            ),
            ("/ISAPI/PTZCtrl/channels/1/presets", "PTZPresetList"),
            (
                "/ISAPI/System/TwoWayAudio/channels",
                "TwoWayAudioChannelList",
            ),
        ] {
            if path == prefix {
                let mut body =
                    format!("<{root} xmlns=\"http://www.isapi.org/ver20/XMLSchema\">").into_bytes();
                for (key, value) in &state.resources {
                    if key
                        .strip_prefix(&format!("{prefix}/"))
                        .is_some_and(|suffix| suffix.parse::<u32>().is_ok())
                    {
                        append_element(&mut body, &value.body)?;
                    }
                }
                body.extend_from_slice(format!("</{root}>").as_bytes());
                return Ok(Reply::http(200, "application/xml", body));
            }
        }
        return Ok(status(404, 4, "notSupport"));
    }
    if request.method() == "DELETE" {
        return Ok(if state.resources.remove(path).is_some() {
            status(200, 1, "ok")
        } else {
            status(404, 4, "notFound")
        });
    }
    if path.ends_with("/test") || path.ends_with("/goto") {
        return Ok(status(200, 1, "ok"));
    }
    if matches!(request.method(), "PUT" | "POST") {
        if xml_root(request.body()).is_err() {
            return Ok(status(200, 5, "badXmlFormat"));
        }
        let path = if path == "/ISAPI/Event/notification/httpHosts" && request.method() == "POST" {
            format!(
                "{path}/{}",
                xml_field(request.body(), "id")?
                    .ok_or_else(|| anyhow::anyhow!("fake host ID missing"))?
            )
        } else {
            path.to_owned()
        };
        if state.resources.len() >= 128 {
            return Ok(status(503, 2, "deviceBusy"));
        }
        state.resources.insert(
            path,
            Resource {
                body: request.body().to_vec(),
                media: "application/xml",
            },
        );
        return Ok(status(200, 1, "ok"));
    }
    Ok(status(405, 4, "invalidOperation"))
}

pub fn status(http: u16, device: u32, subcode: &str) -> Reply {
    Reply::http(
        http,
        "application/xml",
        format!(
            "<ResponseStatus><statusCode>{device}</statusCode><subStatusCode>{subcode}</subStatusCode></ResponseStatus>"
        ),
    )
}

fn xml_root(bytes: &[u8]) -> anyhow::Result<String> {
    let mut root = None;
    for event in xml::reader::EventReader::new(bytes) {
        if let xml::reader::XmlEvent::StartElement { name, .. } = event? {
            root.get_or_insert(name.local_name);
        }
    }
    root.ok_or_else(|| anyhow::anyhow!("fake received empty XML"))
}

fn append_element(output: &mut Vec<u8>, bytes: &[u8]) -> anyhow::Result<()> {
    let mut writer = xml::writer::EmitterConfig::new()
        .write_document_declaration(false)
        .create_writer(output);
    for event in xml::reader::EventReader::new(bytes) {
        let event = event?;
        if matches!(
            event,
            xml::reader::XmlEvent::StartDocument { .. } | xml::reader::XmlEvent::EndDocument
        ) {
            continue;
        }
        if let Some(event) = event.as_writer_event() {
            writer.write(event)?;
        }
    }
    Ok(())
}

pub fn xml_field(bytes: &[u8], field: &str) -> anyhow::Result<Option<String>> {
    let mut selected = false;
    let mut value = None;
    for event in xml::reader::EventReader::new(bytes) {
        match event? {
            xml::reader::XmlEvent::StartElement { name, .. } => selected = name.local_name == field,
            xml::reader::XmlEvent::Characters(text) if selected => {
                anyhow::ensure!(value.is_none(), "duplicate fake XML field");
                value = Some(text);
            }
            xml::reader::XmlEvent::EndElement { .. } => selected = false,
            _ => {}
        }
    }
    Ok(value)
}
