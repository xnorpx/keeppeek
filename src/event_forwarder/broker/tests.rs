use super::{
    BrokerOptions, BrokerSession, MQTT_PINGRESP_TIMEOUT_MS, connack_failure, probe,
    protocol::{ClientAction, MqttClientState},
    transport::CA_FILE_BYTES_MAX,
};
use crate::event_forwarder::{BrokerFailureKind, config::MqttForwarderConfig, model::Publication};
use mqtt_protocol_core::mqtt::result_code::ConnectReasonCode;
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

#[test]
fn builds_plain_and_authenticated_tls_options_without_exposing_credentials() {
    let plain = BrokerOptions::from_config(&MqttForwarderConfig::default()).unwrap();
    assert_eq!((plain.host.as_str(), plain.port), ("127.0.0.1", 1883));
    assert!(!plain.use_tls);

    let missing_ca = PathBuf::from("/definitely/missing/keeppeek-mqtt-ca.pem");
    let tls = MqttForwarderConfig {
        broker_url: "mqtts://broker.example".to_owned(),
        username: Some("operator".to_owned()),
        password: Some("super-secret".to_owned()),
        tls_ca_path: Some(missing_ca),
        ..MqttForwarderConfig::default()
    };
    let error = BrokerOptions::from_config(&tls).unwrap_err();
    assert_eq!(error.kind, BrokerFailureKind::Tls);
    assert!(!error.detail.contains("operator"));
    assert!(!error.detail.contains("super-secret"));
    assert!(!error.detail.contains("missing"));

    let oversized_ca = std::env::temp_dir().join(format!(
        "keeppeek-oversized-ca-{}.pem",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&oversized_ca, vec![b'A'; CA_FILE_BYTES_MAX + 1]).unwrap();
    let oversized_config = MqttForwarderConfig {
        broker_url: "mqtts://broker.example".to_owned(),
        tls_ca_path: Some(oversized_ca.clone()),
        ..MqttForwarderConfig::default()
    };
    let oversized_error = BrokerOptions::from_config(&oversized_config).unwrap_err();
    let _ = std::fs::remove_file(&oversized_ca);
    assert_eq!(oversized_error.kind, BrokerFailureKind::Tls);

    let authenticated = MqttForwarderConfig {
        broker_url: "mqtts://broker.example".to_owned(),
        username: Some("operator".to_owned()),
        password: Some("super-secret".to_owned()),
        ..MqttForwarderConfig::default()
    };
    let debug_text = format!("{:?}", BrokerOptions::from_config(&authenticated).unwrap());
    assert!(!debug_text.contains("operator"));
    assert!(!debug_text.contains("super-secret"));
}

#[test]
fn classifies_credentials_and_tls_without_leaking_transport_errors() {
    let authentication = connack_failure(ConnectReasonCode::BadUserNameOrPassword);
    assert_eq!(authentication.kind, BrokerFailureKind::Authentication);
    assert_eq!(
        authentication.detail,
        "MQTT broker rejected the configured credentials."
    );
}

#[test]
fn connection_waits_for_connack_within_operation_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let mut stream = accept_before_deadline(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (_, connect) = read_frame(&mut stream);
        assert_eq!(connect[6], 5);
        std::thread::sleep(Duration::from_millis(250));
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
    });
    let config = MqttForwarderConfig {
        broker_url: format!("mqtt://{address}"),
        ..MqttForwarderConfig::default()
    };
    let mut session = BrokerSession::new(&config).unwrap();
    let result = session.connect(Duration::from_secs(2));
    broker.join().unwrap();
    result.unwrap();
}

#[test]
fn connection_without_connack_stops_at_operation_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let mut stream = accept_before_deadline(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (_, connect) = read_frame(&mut stream);
        assert_eq!(connect[6], 5);
        let mut closed = [0_u8; 1];
        assert_eq!(stream.read(&mut closed).unwrap(), 0);
    });
    let config = MqttForwarderConfig {
        broker_url: format!("mqtt://{address}"),
        ..MqttForwarderConfig::default()
    };
    let mut session = BrokerSession::new(&config).unwrap();
    let result = session.connect(Duration::from_millis(250));
    broker.join().unwrap();
    assert_eq!(result.unwrap_err().kind, BrokerFailureKind::Timeout);
}

#[test]
#[cfg(not(windows))]
fn authenticated_tls_probe_uses_configured_ca_and_credentials() {
    #[cfg(target_os = "macos")]
    // Isolate Secure Transport because native-tls server handshakes can fail in a parallel macOS test process (rust-native-tls/rust-native-tls#34).
    if std::env::var_os("KEEPPEEK_MQTT_TLS_TEST_CHILD").is_none() {
        run_isolated_macos_tls_test();
        return;
    }

    let (acceptor, ca_path) = build_test_tls_fixture();
    let listener = TcpListener::bind("localhost:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut stream = acceptor.accept(stream).unwrap();
        let (_, connect) = read_frame(&mut stream);
        let (username, password) = connect_credentials(&connect);
        assert_eq!(username.as_deref(), Some("operator"));
        assert_eq!(password.as_deref(), Some(b"correct-horse".as_slice()));
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
        let (header, publish) = read_frame(&mut stream);
        let packet_id = publish_packet_id(header, &publish)
            .expect("test publication must request an acknowledgement")
            .to_be_bytes();
        stream
            .write_all(&[0x40, 0x02, packet_id[0], packet_id[1]])
            .unwrap();
        let (disconnect_header, _) = read_frame(&mut stream);
        assert_eq!(disconnect_header >> 4, 14);
    });
    let config = MqttForwarderConfig {
        broker_url: format!("mqtts://localhost:{}", address.port()),
        username: Some("operator".to_owned()),
        password: Some("correct-horse".to_owned()),
        tls_ca_path: Some(ca_path.clone()),
        ..MqttForwarderConfig::default()
    };

    let result = probe(&config, Duration::from_secs(3));
    let broker_result = broker.join();
    let _ = std::fs::remove_file(ca_path);
    assert!(
        result.is_ok() && broker_result.is_ok(),
        "client={result:?}, broker={broker_result:?}"
    );
}

#[test]
fn stalled_tls_handshake_times_out_with_timeout_kind() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let broker = std::thread::spawn(move || {
        let _stream = accept_before_deadline(&listener);
        let _ = done_rx.recv_timeout(Duration::from_secs(3));
    });
    let config = MqttForwarderConfig {
        broker_url: format!("mqtts://{address}"),
        ..MqttForwarderConfig::default()
    };
    let mut session = BrokerSession::new(&config).unwrap();
    let started = Instant::now();
    let error = session.connect(Duration::from_millis(200)).unwrap_err();
    let elapsed = started.elapsed();
    let _ = done_tx.send(());
    broker.join().unwrap();
    assert_eq!(error.kind, BrokerFailureKind::Timeout);
    assert!(
        elapsed < Duration::from_secs(2),
        "stalled TLS handshake exceeded deadline budget: {elapsed:?}"
    );
}

#[test]
fn stalled_disconnect_respects_deadline_without_blocking() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let broker = std::thread::spawn(move || {
        let mut stream = accept_before_deadline(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (_, _) = read_frame(&mut stream);
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
        let _ = done_rx.recv_timeout(Duration::from_secs(3));
    });
    let config = MqttForwarderConfig {
        broker_url: format!("mqtt://{address}"),
        ..MqttForwarderConfig::default()
    };
    let mut session = BrokerSession::new(&config).unwrap();
    session.connect(Duration::from_secs(2)).unwrap();

    let started = Instant::now();
    let error = session.disconnect(Duration::ZERO).unwrap_err();
    let elapsed = started.elapsed();
    let _ = done_tx.send(());
    broker.join().unwrap();
    assert_eq!(error.kind, BrokerFailureKind::Timeout);
    assert!(elapsed < Duration::from_millis(500));
}

#[test]
#[cfg(not(windows))]
fn stalled_tls_disconnect_respects_deadline_without_blocking() {
    #[cfg(target_os = "macos")]
    if std::env::var_os("KEEPPEEK_MQTT_TLS_TEST_CHILD").is_none() {
        run_isolated_macos_tls_test();
        return;
    }

    let (acceptor, ca_path) = build_test_tls_fixture();
    let listener = TcpListener::bind("localhost:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (done_tx, done_rx) = mpsc::channel();
    let broker = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut stream = acceptor.accept(stream).unwrap();
        let (_, _) = read_frame(&mut stream);
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
        let _ = done_rx.recv_timeout(Duration::from_secs(3));
    });
    let config = MqttForwarderConfig {
        broker_url: format!("mqtts://localhost:{}", address.port()),
        tls_ca_path: Some(ca_path.clone()),
        ..MqttForwarderConfig::default()
    };
    let mut session = BrokerSession::new(&config).unwrap();
    session.connect(Duration::from_secs(3)).unwrap();

    let started = Instant::now();
    let expired_error = session.disconnect(Duration::ZERO).unwrap_err();
    let elapsed = started.elapsed();
    let _ = done_tx.send(());
    let broker_result = broker.join();
    let _ = std::fs::remove_file(ca_path);
    assert_eq!(expired_error.kind, BrokerFailureKind::Timeout);
    assert!(elapsed < Duration::from_millis(500));
    assert!(broker_result.is_ok(), "broker={broker_result:?}");
}

#[test]
fn broker_rejected_credentials_are_actionable_and_secret_safe() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (_, connect) = read_frame(&mut stream);
        let (username, password) = connect_credentials(&connect);
        assert_eq!(username.as_deref(), Some("operator"));
        assert_eq!(password.as_deref(), Some(b"wrong-secret".as_slice()));
        stream.write_all(&[0x20, 0x03, 0x00, 0x86, 0x00]).unwrap();
    });
    let config = MqttForwarderConfig {
        broker_url: format!("mqtt://{address}"),
        username: Some("operator".to_owned()),
        password: Some("wrong-secret".to_owned()),
        ..MqttForwarderConfig::default()
    };

    let error = probe(&config, Duration::from_secs(2)).unwrap_err();
    broker.join().unwrap();
    assert_eq!(error.kind, BrokerFailureKind::Authentication);
    assert_eq!(
        error.detail,
        "MQTT broker rejected the configured credentials."
    );
    assert!(!error.detail.contains("wrong-secret"));
}

#[test]
fn invalid_tls_trust_is_actionable_and_does_not_expose_paths() {
    let listener = TcpListener::bind("localhost:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut closed = [0_u8; 1];
        let _ = stream.read(&mut closed);
    });
    let ca_path = std::env::temp_dir().join(format!(
        "keeppeek-invalid-mqtt-ca-{}.pem",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&ca_path, "not a certificate").unwrap();
    let config = MqttForwarderConfig {
        broker_url: format!("mqtts://localhost:{}", address.port()),
        tls_ca_path: Some(ca_path.clone()),
        ..MqttForwarderConfig::default()
    };

    let error = probe(&config, Duration::from_secs(2)).unwrap_err();
    broker.join().unwrap();
    let _ = std::fs::remove_file(&ca_path);
    assert_eq!(error.kind, BrokerFailureKind::Tls);
    assert_eq!(
        error.detail,
        "MQTT TLS validation failed; verify the broker hostname and CA trust."
    );
    assert!(
        !error
            .detail
            .contains(&ca_path.to_string_lossy().into_owned())
    );
}

#[test]
fn probe_uses_only_mqtt_five_with_required_publish_properties() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (connect_header, connect) = read_frame(&mut stream);
        assert_eq!(connect_header, 0x10);
        assert_eq!(&connect[2..6], b"MQTT");
        assert_eq!(connect[6], 5, "client must send MQTT protocol level 5");
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();

        let (publish_header, publish) = read_frame(&mut stream);
        assert_eq!(publish_header >> 4, 3);
        let topic_length = usize::from(u16::from_be_bytes([publish[0], publish[1]]));
        let topic_end = 2 + topic_length;
        assert_eq!(
            std::str::from_utf8(&publish[2..topic_end]).unwrap(),
            "keeppeek/home-nvr/forwarders/mqtt/status"
        );
        let packet_id = u16::from_be_bytes([publish[topic_end], publish[topic_end + 1]]);
        let properties_start = topic_end + 2;
        let (properties_length, properties_prefix_length) =
            variable_integer(&publish[properties_start..]);
        let properties = &publish[properties_start + properties_prefix_length
            ..properties_start + properties_prefix_length + properties_length];
        assert!(contains_bytes(properties, &[0x01, 0x01]));
        assert!(contains_bytes(
            properties,
            &[
                0x03, 0x00, 0x10, b'a', b'p', b'p', b'l', b'i', b'c', b'a', b't', b'i', b'o', b'n',
                b'/', b'j', b's', b'o', b'n'
            ]
        ));
        assert!(contains_bytes(
            properties,
            &[0x09, 0x00, 0x04, b'm', b'q', b't', b't']
        ));
        let packet_id = packet_id.to_be_bytes();
        stream
            .write_all(&[0x40, 0x02, packet_id[0], packet_id[1]])
            .unwrap();
        let (disconnect_header, _) = read_frame(&mut stream);
        assert_eq!(disconnect_header >> 4, 14);
    });

    let config = MqttForwarderConfig {
        broker_url: format!("mqtt://{address}"),
        ..MqttForwarderConfig::default()
    };
    let result = probe(&config, Duration::from_secs(2));
    broker.join().unwrap();
    result.unwrap();
}

#[test]
fn mqtt_five_rejection_does_not_fallback_to_mqtt_three() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (_, connect) = read_frame(&mut stream);
        assert_eq!(connect[6], 5);
        stream.write_all(&[0x20, 0x03, 0x00, 0x84, 0x00]).unwrap();
    });

    let config = MqttForwarderConfig {
        broker_url: format!("mqtt://{address}"),
        ..MqttForwarderConfig::default()
    };
    let error = probe(&config, Duration::from_secs(2)).unwrap_err();
    assert_eq!(error.kind, BrokerFailureKind::Protocol);
    assert!(error.detail.contains("MQTT 5"));
    broker.join().unwrap();
}

#[test]
fn qos_zero_and_qos_two_complete_expected_mqtt_five_handshakes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let mut stream = accept_before_deadline(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (_, connect) = read_frame(&mut stream);
        assert_eq!(connect[6], 5);
        stream.write_all(&[0x20, 0x03]).unwrap();
        std::thread::sleep(Duration::from_millis(10));
        stream.write_all(&[0x00, 0x00, 0x00]).unwrap();

        let (qos0_header, qos0_body) = read_frame(&mut stream);
        assert_eq!(qos0_header >> 4, 3);
        assert_eq!((qos0_header >> 1) & 0x03, 0);
        assert!(publish_packet_id(qos0_header, &qos0_body).is_none());

        let (qos2_header, qos2_body) = read_frame(&mut stream);
        assert_eq!(qos2_header >> 4, 3);
        assert_eq!((qos2_header >> 1) & 0x03, 2);
        let id_bytes = publish_packet_id(qos2_header, &qos2_body)
            .expect("QoS 2 publish must carry a packet identifier")
            .to_be_bytes();
        stream
            .write_all(&[0x50, 0x02, id_bytes[0], id_bytes[1]])
            .unwrap();

        let (pubrel_header, pubrel_body) = read_frame(&mut stream);
        assert_eq!(pubrel_header, 0x62);
        assert_eq!(&pubrel_body[..2], &id_bytes);
        stream
            .write_all(&[0x70, 0x02, id_bytes[0], id_bytes[1]])
            .unwrap();

        let (disconnect_header, _) = read_frame(&mut stream);
        assert_eq!(disconnect_header >> 4, 14);
    });

    let config = MqttForwarderConfig {
        broker_url: format!("mqtt://{address}"),
        ..MqttForwarderConfig::default()
    };
    let mut session = BrokerSession::new(&config).unwrap();
    session
        .publish(&sample_publication("qos0", 0), Duration::from_secs(2))
        .unwrap();
    session
        .publish(&sample_publication("qos2", 2), Duration::from_secs(2))
        .unwrap();
    session.disconnect(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn qos_two_pubrec_no_matching_subscribers_completes_pubrel_and_next_publish() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let mut stream = accept_before_deadline(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (_, _) = read_frame(&mut stream);
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();

        let (first_header, first_body) = read_frame(&mut stream);
        let first_id = publish_packet_id(first_header, &first_body).unwrap();
        let first_bytes = first_id.to_be_bytes();
        // Return PUBREC with NoMatchingSubscribers (0x10), which still requires PUBREL/PUBCOMP.
        stream
            .write_all(&[0x50, 0x03, first_bytes[0], first_bytes[1], 0x10])
            .unwrap();
        let (first_rel_header, first_rel_body) = read_frame(&mut stream);
        assert_eq!(first_rel_header, 0x62);
        assert_eq!(&first_rel_body[..2], &first_bytes);
        stream
            .write_all(&[0x70, 0x02, first_bytes[0], first_bytes[1]])
            .unwrap();

        let (second_header, second_body) = read_frame(&mut stream);
        let second_id = publish_packet_id(second_header, &second_body).unwrap();
        assert_ne!(second_id, first_id);
        let second_bytes = second_id.to_be_bytes();
        stream
            .write_all(&[0x50, 0x02, second_bytes[0], second_bytes[1]])
            .unwrap();
        let (second_rel_header, second_rel_body) = read_frame(&mut stream);
        assert_eq!(second_rel_header, 0x62);
        assert_eq!(&second_rel_body[..2], &second_bytes);
        stream
            .write_all(&[0x70, 0x02, second_bytes[0], second_bytes[1]])
            .unwrap();

        let (disconnect_header, _) = read_frame(&mut stream);
        assert_eq!(disconnect_header >> 4, 14);
    });

    let config = MqttForwarderConfig {
        broker_url: format!("mqtt://{address}"),
        qos: 2,
        ..MqttForwarderConfig::default()
    };
    let mut session = BrokerSession::new(&config).unwrap();
    session
        .publish(&sample_publication("qos2-first", 2), Duration::from_secs(2))
        .unwrap();
    session
        .publish(
            &sample_publication("qos2-second", 2),
            Duration::from_secs(2),
        )
        .unwrap();
    session.disconnect(Duration::from_secs(2)).unwrap();
    broker.join().unwrap();
}

#[test]
fn poll_sends_keepalive_ping_and_detects_broker_disconnect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let mut stream = accept_before_deadline(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (_, connect) = read_frame(&mut stream);
        assert_eq!(connect[6], 5);
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();

        let (ping_header, ping_body) = read_frame(&mut stream);
        assert_eq!(ping_header, 0xC0);
        assert!(ping_body.is_empty());
        stream.write_all(&[0xD0, 0x00]).unwrap();
        stream.write_all(&[0xE0, 0x01, 0x8B]).unwrap();
    });

    let config = MqttForwarderConfig {
        broker_url: format!("mqtt://{address}"),
        ..MqttForwarderConfig::default()
    };
    let mut session = BrokerSession::new(&config).unwrap();
    session.options.pingreq_interval_override_ms = Some(40);
    session.connect(Duration::from_secs(2)).unwrap();
    let error = session.poll(Duration::from_secs(2)).unwrap_err();
    broker.join().unwrap();
    assert_eq!(error.kind, BrokerFailureKind::Network);
    assert!(session.active.is_none());
}

#[test]
fn puback_and_pubrec_rejections_fail_with_protocol_error() {
    let assert_rejection = |qos: u8, ack_header: u8| {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let broker = std::thread::spawn(move || {
            let mut stream = accept_before_deadline(&listener);
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let (_, _) = read_frame(&mut stream);
            stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();
            let (header, publish) = read_frame(&mut stream);
            let packet_id = publish_packet_id(header, &publish).unwrap().to_be_bytes();
            stream
                .write_all(&[ack_header, 0x03, packet_id[0], packet_id[1], 0x87])
                .unwrap();
            // Client must close after a >= 0x80 PUBACK/PUBREC without sending PUBREL.
            let mut trailing = [0_u8; 1];
            assert_eq!(stream.read(&mut trailing).unwrap(), 0);
        });

        let config = MqttForwarderConfig {
            broker_url: format!("mqtt://{address}"),
            qos,
            ..MqttForwarderConfig::default()
        };
        let error = probe(&config, Duration::from_secs(2)).unwrap_err();
        broker.join().unwrap();
        assert_eq!(error.kind, BrokerFailureKind::Protocol);
        assert_eq!(error.detail, "MQTT 5 broker rejected the publication.");
    };

    assert_rejection(1, 0x40);
    assert_rejection(2, 0x50);
}

#[test]
fn repeated_short_polls_complete_keepalive_and_timeout_when_pingresp_missing() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let broker = std::thread::spawn(move || {
        let mut stream = accept_before_deadline(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let (_, connect) = read_frame(&mut stream);
        assert_eq!(connect[6], 5);
        stream.write_all(&[0x20, 0x03, 0x00, 0x00, 0x00]).unwrap();

        let (first_ping, _) = read_frame(&mut stream);
        assert_eq!(first_ping, 0xC0);
        stream.write_all(&[0xD0, 0x00]).unwrap();

        let (second_ping, _) = read_frame(&mut stream);
        assert_eq!(second_ping, 0xC0);

        let (disconnect_header, disconnect_body) = read_frame(&mut stream);
        assert_eq!(disconnect_header >> 4, 14);
        assert_eq!(disconnect_body.first().copied(), Some(0x8D));
    });

    let config = MqttForwarderConfig {
        broker_url: format!("mqtt://{address}"),
        ..MqttForwarderConfig::default()
    };
    let mut session = BrokerSession::new(&config).unwrap();
    session.options.pingreq_interval_override_ms = Some(60);
    session.options.pingresp_timeout_ms = 40;
    session.connect(Duration::from_secs(2)).unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    let error = loop {
        assert!(Instant::now() < deadline, "missing PINGRESP must time out");
        if let Err(error) = session.poll(Duration::from_millis(15)) {
            break error;
        }
    };
    broker.join().unwrap();
    assert_eq!(error.kind, BrokerFailureKind::Timeout);
    assert!(session.active.is_none());
}

#[test]
fn connack_server_keep_alive_clamps_pingresp_timeout_and_enforces_max_packet_size() {
    let now = Instant::now();
    let mut state = MqttClientState::new(Some(15_000), MQTT_PINGRESP_TIMEOUT_MS);
    // CONNACK with ServerKeepAlive = 2s (0x13 0x00 0x02) and MaximumPacketSize = 32 (0x27 0x00 0x00 0x00 0x20).
    let connack = [
        0x20, 0x0B, 0x00, 0x00, 0x08, 0x13, 0x00, 0x02, 0x27, 0x00, 0x00, 0x00, 0x20,
    ];
    let actions = state.feed_bytes(&connack, now).unwrap();
    assert_eq!(actions, vec![ClientAction::Connected]);
    assert_eq!(state.keep_alive_ms(), Some(2_000));
    assert_eq!(state.pingresp_timeout_ms(), 1_000);

    let error = state
        .start_publish(&sample_publication("oversized", 1), now)
        .unwrap_err();
    assert_eq!(error.kind, BrokerFailureKind::Protocol);
}

#[test]
fn frame_decoder_rejects_oversized_packets_and_malformed_vbi() {
    let now = Instant::now();
    let mut oversized = MqttClientState::new(Some(15_000), MQTT_PINGRESP_TIMEOUT_MS);
    // Advertise remaining length = 268,435,455 (> 16 MiB) in 5 header bytes without sending a body.
    let error = oversized
        .feed_bytes(&[0x20, 0xFF, 0xFF, 0xFF, 0x7F], now)
        .unwrap_err();
    assert_eq!(error.kind, BrokerFailureKind::Protocol);

    let mut overlong_vbi = MqttClientState::new(Some(15_000), MQTT_PINGRESP_TIMEOUT_MS);
    let error = overlong_vbi
        .feed_bytes(&[0xD0, 0x80, 0x00], now)
        .unwrap_err();
    assert_eq!(error.kind, BrokerFailureKind::Protocol);
}

fn sample_publication(key: &str, qos: u8) -> Publication {
    Publication {
        dedup_key: key.to_owned(),
        topic: "keeppeek/home-nvr/sources/cam/events/motion".to_owned(),
        payload: b"{}".to_vec(),
        qos,
        retain: false,
        event_timestamp_ms: 1,
        content_type: "application/json".to_owned(),
        payload_format_indicator: Some(1),
        correlation_data: key.as_bytes().to_vec(),
    }
}

#[cfg(target_os = "macos")]
fn run_isolated_macos_tls_test() {
    let current_thread = std::thread::current();
    let test_name = current_thread
        .name()
        .expect("the Rust test harness must name its test threads");
    let status = std::process::Command::new(
        std::env::current_exe().expect("the current test executable must be available"),
    )
    .args([test_name, "--exact", "--test-threads=1"])
    .env("KEEPPEEK_MQTT_TLS_TEST_CHILD", "1")
    .status()
    .expect("the isolated MQTT TLS test process must start");
    assert!(status.success(), "the isolated MQTT TLS test failed");
}

#[cfg(not(windows))]
fn build_test_tls_fixture() -> (native_tls::TlsAcceptor, PathBuf) {
    let mut ca_params = rcgen::CertificateParams::new(Vec::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "KeepPeek MQTT Test CA");
    ca_params.key_usages.extend([
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ]);
    let ca_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).unwrap();
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);

    let mut server_params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    server_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "localhost");
    server_params.use_authority_key_identifier_extension = true;
    server_params.key_usages.extend([
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ]);
    server_params
        .extended_key_usages
        .push(rcgen::ExtendedKeyUsagePurpose::ServerAuth);
    let signing_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).unwrap();
    let cert = server_params.signed_by(&signing_key, &issuer).unwrap();
    let certificate_chain = format!("{}{}", cert.pem(), ca_cert.pem());
    let identity = native_tls::Identity::from_pkcs8(
        certificate_chain.as_bytes(),
        signing_key.serialize_pem().as_bytes(),
    )
    .unwrap();
    let acceptor = native_tls::TlsAcceptor::new(identity).unwrap();
    let ca_path =
        std::env::temp_dir().join(format!("keeppeek-mqtt-ca-{}.pem", uuid::Uuid::new_v4()));
    std::fs::write(&ca_path, ca_cert.pem()).unwrap();
    (acceptor, ca_path)
}

fn accept_before_deadline(listener: &TcpListener) -> TcpStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("MQTT fixture accept failed: {error}"),
        }
    }
    panic!("MQTT client did not connect before the fixture deadline");
}

fn read_frame(stream: &mut impl Read) -> (u8, Vec<u8>) {
    let mut header = [0_u8; 1];
    stream.read_exact(&mut header).unwrap();
    let mut multiplier = 1_usize;
    let mut remaining = 0_usize;
    loop {
        let mut encoded = [0_u8; 1];
        stream.read_exact(&mut encoded).unwrap();
        remaining += usize::from(encoded[0] & 0x7f) * multiplier;
        if encoded[0] & 0x80 == 0 {
            break;
        }
        multiplier *= 128;
    }
    let mut body = vec![0_u8; remaining];
    stream.read_exact(&mut body).unwrap();
    (header[0], body)
}

fn variable_integer(bytes: &[u8]) -> (usize, usize) {
    let mut value = 0_usize;
    let mut multiplier = 1_usize;
    for (index, byte) in bytes.iter().copied().enumerate() {
        value += usize::from(byte & 0x7f) * multiplier;
        if byte & 0x80 == 0 {
            return (value, index + 1);
        }
        multiplier *= 128;
    }
    panic!("MQTT variable integer is incomplete");
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn connect_credentials(body: &[u8]) -> (Option<String>, Option<Vec<u8>>) {
    let protocol_length = usize::from(u16::from_be_bytes([body[0], body[1]]));
    let mut cursor = 2 + protocol_length;
    assert_eq!(body[cursor], 5);
    cursor += 1;
    let flags = body[cursor];
    cursor += 3;
    let (properties_length, properties_prefix) = variable_integer(&body[cursor..]);
    cursor += properties_prefix + properties_length;
    let _client_id = mqtt_binary(body, &mut cursor);
    if flags & 0x04 != 0 {
        let (will_properties_length, will_properties_prefix) = variable_integer(&body[cursor..]);
        cursor += will_properties_prefix + will_properties_length;
        let _will_topic = mqtt_binary(body, &mut cursor);
        let _will_payload = mqtt_binary(body, &mut cursor);
    }
    let username =
        (flags & 0x80 != 0).then(|| String::from_utf8(mqtt_binary(body, &mut cursor)).unwrap());
    let password = (flags & 0x40 != 0).then(|| mqtt_binary(body, &mut cursor));
    (username, password)
}

fn mqtt_binary(body: &[u8], cursor: &mut usize) -> Vec<u8> {
    let length = usize::from(u16::from_be_bytes([body[*cursor], body[*cursor + 1]]));
    *cursor += 2;
    let value = body[*cursor..*cursor + length].to_vec();
    *cursor += length;
    value
}

fn publish_packet_id(header: u8, body: &[u8]) -> Option<u16> {
    if (header >> 1) & 0x03 == 0 {
        return None;
    }
    let topic_length = usize::from(u16::from_be_bytes([body[0], body[1]]));
    let offset = 2 + topic_length;
    Some(u16::from_be_bytes([body[offset], body[offset + 1]]))
}
