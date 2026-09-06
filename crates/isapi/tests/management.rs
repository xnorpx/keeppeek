use isapi::management::{CallbackHost, DeviceInfo, Motion, ResponseStatus, Stream, Time};
use isapi::{Method, Ptz};

#[test]
fn typed_queries_validate_the_resource_root_and_device_status() {
    let query = DeviceInfo::query().unwrap();
    assert_eq!(query.request().resource(), "/ISAPI/System/deviceInfo");
    let info = query.parse("application/xml", b"<DeviceInfo><deviceName>Gate</deviceName><model>I91ET</model><serialNumber>TEST</serialNumber><firmwareVersion>V5.8.10</firmwareVersion></DeviceInfo>").unwrap();
    assert_eq!(info.model(), "I91ET");
    assert!(
        query
            .parse("application/xml", b"<Other><model>I91ET</model></Other>")
            .is_err()
    );
    let rejected = b"<ResponseStatus xmlns=\"http://www.std-cgi.org/ver20/XMLSchema\"><statusCode>4</statusCode><subStatusCode>notSupport</subStatusCode></ResponseStatus>";
    assert_eq!(
        query
            .parse("application/xml", rejected)
            .unwrap_err()
            .device_status(),
        Some(4)
    );
    let reboot = ResponseStatus::parse(
        "application/json",
        br#"{"statusCode":7,"subStatusCode":"rebootRequired"}"#,
    )
    .unwrap();
    assert!(reboot.success());
    assert!(reboot.reboot_required());
}

#[test]
fn configuration_updates_preserve_unknown_fields_and_validate_numeric_ranges() {
    let query = Motion::query(1).unwrap();
    let mut motion = query.parse("application/xml", b"<MotionDetection xmlns=\"http://www.hikvision.com/ver20/XMLSchema\"><enabled>true</enabled><sensitivityLevel>60</sensitivityLevel><MotionDetectionLayout><regionName>unchanged</regionName></MotionDetectionLayout><Extension mode=\"keep\">value</Extension></MotionDetection>").unwrap();
    motion.set_enabled(false).unwrap();
    motion.set_sensitivity(55).unwrap();
    assert!(motion.set_sensitivity(101).is_err());
    let request = motion.update(1).unwrap();
    assert_eq!(request.method(), Method::Put);
    assert_eq!(
        request.resource(),
        "/ISAPI/System/Video/inputs/channels/1/motionDetection"
    );
    let text = std::str::from_utf8(request.body()).unwrap();
    assert!(text.contains("<enabled>false</enabled>"));
    assert!(text.contains("mode=\"keep\""));
    assert!(text.contains("regionName"));
    let roundtrip = query.parse("application/xml", request.body()).unwrap();
    assert!(!roundtrip.enabled());
    assert_eq!(roundtrip.sensitivity(), Some(55));
    assert!(Stream::query(0).is_err());
    assert!(
        Time::query()
            .unwrap()
            .parse(
                "application/xml",
                b"<Time><timeMode>invalid</timeMode></Time>"
            )
            .is_err()
    );
}

#[test]
fn callback_management_serializes_credentials_without_debug_disclosure() {
    let host = CallbackHost::new(
        2,
        "https://192.0.2.10:9443/camera",
        "camera-receiver",
        "test-only-secret",
    )
    .unwrap();
    let create = host.create().unwrap();
    assert_eq!(create.method(), Method::Post);
    assert_eq!(create.resource(), "/ISAPI/Event/notification/httpHosts");
    assert!(
        std::str::from_utf8(create.body())
            .unwrap()
            .contains("MD5digest")
    );
    assert!(!format!("{host:?}").contains("test-only-secret"));
    assert_eq!(
        host.update().unwrap().resource(),
        "/ISAPI/Event/notification/httpHosts/2"
    );
    assert_eq!(CallbackHost::delete(2).unwrap().method(), Method::Delete);
    assert_eq!(CallbackHost::test(2).unwrap().method(), Method::Post);
    assert!(
        CallbackHost::new(
            1,
            "https://user:password@example.test/callback",
            "receiver",
            "test-only-secret"
        )
        .is_err()
    );
}

#[test]
fn ptz_operations_are_explicit_and_bound_axes_channels_and_presets() {
    let velocity = Ptz::new(10, -20, 0).unwrap();
    assert_eq!(
        velocity.continuous(1).unwrap().resource(),
        "/ISAPI/PTZCtrl/channels/1/continuous"
    );
    assert_eq!(
        Ptz::absolute(1, 900, 1800, 20).unwrap().method(),
        Method::Put
    );
    assert!(Ptz::absolute(1, 901, 3601, 0).is_err());
    assert_eq!(
        Ptz::goto_preset(1, 3).unwrap().resource(),
        "/ISAPI/PTZCtrl/channels/1/presets/3/goto"
    );
    assert!(Ptz::goto_preset(0, 3).is_err());
}

#[test]
fn capability_queries_expose_device_limits_and_list_queries_preserve_channel_identity() {
    use isapi::management::{Capabilities, Endpoint, Preset, PtzStatus, Rule, RuleKind};
    let query = Capabilities::query(Endpoint::CallbackHosts).unwrap();
    assert_eq!(
        query.request().resource(),
        "/ISAPI/Event/notification/httpHosts/capabilities"
    );
    let capabilities = query.parse("application/xml", b"<HttpHostNotificationCap><hostNumber>3</hostNumber><httpAuthenticationMethod opt=\"MD5digest,none\"/><portNo min=\"1\" max=\"65535\"/></HttpHostNotificationCap>").unwrap();
    let auth = capabilities
        .field(&["httpAuthenticationMethod"])
        .unwrap()
        .unwrap();
    assert!(auth.options().iter().any(|value| value == "MD5digest"));
    assert_eq!(
        capabilities.field(&["portNo"]).unwrap().unwrap().maximum(),
        Some(65535)
    );
    let hosts = CallbackHost::list().unwrap().parse("application/xml", b"<HttpHostNotificationList><HttpHostNotification><id>1</id><url>http://192.0.2.10/camera</url><httpAuthenticationMethod>MD5digest</httpAuthenticationMethod></HttpHostNotification></HttpHostNotificationList>").unwrap();
    assert_eq!(hosts[0].id(), 1);
    let streams = Stream::list().unwrap().parse("application/xml", b"<StreamingChannelList><StreamingChannel><id>101</id></StreamingChannel><StreamingChannel><id>102</id></StreamingChannel></StreamingChannelList>").unwrap();
    assert_eq!(
        streams.iter().map(Stream::id).collect::<Vec<_>>(),
        [101, 102]
    );
    let status = PtzStatus::query(1).unwrap().parse("application/xml", b"<PTZStatus><AbsoluteHigh><elevation>900</elevation><azimuth>1800</azimuth><absoluteZoom>20</absoluteZoom></AbsoluteHigh></PTZStatus>").unwrap();
    assert_eq!(status.azimuth(), Some(1800));
    let preset = Preset::new(3, "Gate & drive").unwrap();
    assert!(
        std::str::from_utf8(preset.store(1).unwrap().body())
            .unwrap()
            .contains("&amp;")
    );
    let mut rule = Rule::query(RuleKind::Line, 1).unwrap().parse("application/xml", b"<LineDetection><enabled>true</enabled><LineItemList><LineItem><id>4</id></LineItem></LineItemList></LineDetection>").unwrap();
    rule.set_enabled(false).unwrap();
    assert_eq!(
        rule.update(1).unwrap().resource(),
        "/ISAPI/Smart/LineDetection/1"
    );
}

#[test]
fn ptz_capabilities_use_reported_spaces_and_preset_capacity() {
    use isapi::management::PtzCapabilities;
    let query = PtzCapabilities::query(1).unwrap();
    let capabilities = query.parse("application/xml", b"<PTZChanelCap><ContinuousZoomSpace><ZRange><Min>-100</Min><Max>100</Max></ZRange></ContinuousZoomSpace><homePostionSupport>false</homePostionSupport><maxPresetNum>0</maxPresetNum></PTZChanelCap>").unwrap();
    assert!(!capabilities.continuous_pan_tilt());
    assert!(capabilities.continuous_zoom());
    assert_eq!(capabilities.max_presets(), 0);
    assert!(
        query
            .parse(
                "application/xml",
                b"<Capabilities><enabled>true</enabled></Capabilities>"
            )
            .is_err()
    );
    assert!(query.parse("application/xml", b"<PTZChanelCap><ContinuousPanTiltSpace/><maxPresetNum>-1</maxPresetNum></PTZChanelCap>").is_err());
}
