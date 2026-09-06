#![cfg(feature = "ureq")]

use isapi::{
    Credentials,
    blocking::Client,
    management::{DeviceInfo, Motion},
};
use test_hikvision::FakeHikvision;

#[test]
fn fake_hikvision_verifies_digest_and_retains_management_writes() {
    let camera = FakeHikvision::builder()
        .credentials("operator", "test-secret")
        .start()
        .unwrap();
    let mut client =
        Client::new(camera.origin(), Credentials::new("operator", "test-secret")).unwrap();
    assert_eq!(
        client.query(&DeviceInfo::query().unwrap()).unwrap().model(),
        "FAKE-HIKVISION"
    );
    let query = Motion::query(1).unwrap();
    let mut motion = client.query(&query).unwrap();
    motion.set_enabled(false).unwrap();
    motion.set_sensitivity(41).unwrap();
    assert!(
        client
            .command(&motion.update(1).unwrap())
            .unwrap()
            .success()
    );
    let actual = client.query(&query).unwrap();
    assert!(!actual.enabled());
    assert_eq!(actual.sensitivity(), Some(41));
    assert!(client.snapshot(101).unwrap().starts_with(&[0xff, 0xd8]));
    let mut wrong = Client::new(
        camera.origin(),
        Credentials::new("operator", "wrong-secret"),
    )
    .unwrap();
    assert!(
        wrong
            .query(&DeviceInfo::query().unwrap())
            .unwrap_err()
            .is_authentication()
    );
    let requests = camera.requests();
    assert!(
        requests
            .iter()
            .any(|request| request.method() == "PUT" && request.authenticated())
    );
    assert!(
        requests
            .iter()
            .all(|request| !format!("{request:?}").contains("test-secret"))
    );
}

#[test]
fn fake_hikvision_covers_management_methods_capabilities_ptz_and_callback_hosts() {
    use isapi::management::{
        CallbackHost, Capabilities, Endpoint, Preset, PtzStatus, Rule, RuleKind, Stream, Time,
        TimeMode,
    };
    let camera = FakeHikvision::builder().start().unwrap();
    let mut client = Client::new(camera.origin(), Credentials::new("test", "test")).unwrap();
    let capabilities = client
        .query(&Capabilities::query(Endpoint::CallbackHosts).unwrap())
        .unwrap();
    assert!(
        capabilities
            .field(&["httpAuthenticationMethod"])
            .unwrap()
            .unwrap()
            .options()
            .iter()
            .any(|option| option == "MD5digest")
    );
    let mut clock = client.query(&Time::query().unwrap()).unwrap();
    clock
        .set(TimeMode::Ntp, "2026-09-05T12:34:56Z", "UTC0")
        .unwrap();
    client.command(&clock.update().unwrap()).unwrap();
    assert_eq!(
        client.query(&Time::query().unwrap()).unwrap().mode(),
        TimeMode::Ntp
    );
    let mut streams = client.query(&Stream::list().unwrap()).unwrap();
    assert_eq!(
        streams.iter().map(Stream::id).collect::<Vec<_>>(),
        [101, 102]
    );
    streams[0].set_video(640, 360, 1024, 1500).unwrap();
    client.command(&streams[0].update().unwrap()).unwrap();
    assert!(
        String::from_utf8(camera.resource("/ISAPI/Streaming/channels/101").unwrap())
            .unwrap()
            .contains("<maxFrameRate>1500</maxFrameRate>")
    );
    let mut rule = client
        .query(&Rule::query(RuleKind::Line, 1).unwrap())
        .unwrap();
    rule.set_enabled(false).unwrap();
    client.command(&rule.update(1).unwrap()).unwrap();
    assert!(
        !client
            .query(&Rule::query(RuleKind::Line, 1).unwrap())
            .unwrap()
            .enabled()
    );
    client
        .command(&isapi::Ptz::new(10, 0, 0).unwrap().continuous(1).unwrap())
        .unwrap();
    client
        .command(&isapi::Ptz::new(0, 0, 0).unwrap().continuous(1).unwrap())
        .unwrap();
    client
        .command(&isapi::Ptz::absolute(1, 450, 1800, 10).unwrap())
        .unwrap();
    assert_eq!(
        client
            .query(&PtzStatus::query(1).unwrap())
            .unwrap()
            .azimuth(),
        Some(1800)
    );
    let preset = Preset::new(2, "Gate & driveway").unwrap();
    client.command(&preset.store(1).unwrap()).unwrap();
    assert_eq!(
        client.query(&Preset::list(1).unwrap()).unwrap()[0].name(),
        "Gate & driveway"
    );
    client
        .command(&isapi::Ptz::goto_preset(1, 2).unwrap())
        .unwrap();
    client.command(&Preset::delete(1, 2).unwrap()).unwrap();
    let host = CallbackHost::new(
        1,
        "http://127.0.0.1:9876/receiver",
        "receiver",
        "test-only-secret",
    )
    .unwrap();
    client.command(&host.create().unwrap()).unwrap();
    client.command(&host.update().unwrap()).unwrap();
    assert_eq!(
        client.query(&CallbackHost::list().unwrap()).unwrap()[0]
            .destination()
            .unwrap(),
        "http://127.0.0.1:9876/receiver"
    );
    client.command(&CallbackHost::test(1).unwrap()).unwrap();
    client.command(&CallbackHost::delete(1).unwrap()).unwrap();
    assert!(
        client
            .query(&CallbackHost::list().unwrap())
            .unwrap()
            .is_empty()
    );
    for method in ["GET", "POST", "PUT", "DELETE"] {
        assert!(
            camera
                .requests()
                .iter()
                .any(|request| request.method() == method && request.authenticated())
        );
    }
}

#[test]
fn fake_hikvision_streams_legacy_text_across_fragmented_http_reads() {
    use test_hikvision::{EventPart, Reply};
    let xml = "<?xml version=\"1.0\" encoding=\"GB2312\"?><EventNotificationAlert><eventType>VMD</eventType><eventState>active</eventState><channelID>1</channelID><channelName>\u{4eba}</channelName><detectionTarget>human</detectionTarget></EventNotificationAlert>";
    let (encoded, _, errors) = encoding_rs::GBK.encode(xml);
    assert!(!errors);
    let camera = FakeHikvision::builder()
        .alert_streams([Reply::alert(
            [EventPart::new(
                "application/xml; charset=gb2312",
                encoded.into_owned(),
            )],
            true,
        )
        .fragmented(13)
        .unwrap()])
        .start()
        .unwrap();
    let mut client = Client::new(camera.origin(), Credentials::new("test", "test")).unwrap();
    let mut stream = client.subscribe().unwrap();
    let event = stream
        .next_part()
        .unwrap()
        .unwrap()
        .event()
        .unwrap()
        .unwrap();
    assert_eq!(event.channel_name(), Some("\u{4eba}"));
    assert_eq!(event.objects()[0].target(), Some(isapi::Target::Person));
    assert!(stream.next_part().unwrap().is_none());
}

#[test]
fn fake_hikvision_failure_scripts_do_not_apply_writes_and_all_connections_are_isolated() {
    use test_hikvision::Reply;
    let first = FakeHikvision::builder().start().unwrap();
    let second = FakeHikvision::builder().start().unwrap();
    assert_ne!(first.address(), second.address());
    let mut client = Client::new(first.origin(), Credentials::new("test", "test")).unwrap();
    let query = Motion::query(1).unwrap();
    let mut motion = client.query(&query).unwrap();
    motion.set_enabled(false).unwrap();
    first
        .enqueue(Reply::http(
            200,
            "application/json",
            br#"{"statusCode":4,"subStatusCode":"notSupport"}"#,
        ))
        .unwrap();
    assert_eq!(
        client
            .command(&motion.update(1).unwrap())
            .unwrap_err()
            .device_status(),
        Some(4)
    );
    assert!(client.query(&query).unwrap().enabled());
    assert!(second.requests().is_empty());
}
