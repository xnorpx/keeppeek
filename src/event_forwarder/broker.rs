use super::{
    BrokerFailure, BrokerFailureKind,
    config::MqttForwarderConfig,
    model::{Publication, status_topic},
};
use bytes::Bytes;
use rumqttc::v5::{
    Client, Connection, ConnectionError, Event, Incoming, MqttOptions, RecvTimeoutError,
    mqttbytes::{
        QoS,
        v5::{
            ConnectReturnCode, LastWill, LastWillProperties, PubAckReason, PubCompReason,
            PublishProperties,
        },
    },
};
use rumqttc::{Outgoing, TlsConfiguration, Transport};
use std::time::{Duration, Instant};
use url::Url;

const MQTT_CHANNEL_CAPACITY: usize = 8;
const MQTT_KEEP_ALIVE: Duration = Duration::from_secs(15);
const MQTT_MAX_PACKET_BYTES: u32 = 16 * 1_024 * 1_024;
const MQTT_SESSION_EXPIRY_SECS: u32 = 24 * 60 * 60;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct BrokerSession {
    client: Client,
    connection: Connection,
    connected: bool,
}

impl BrokerSession {
    pub(super) fn new(config: &MqttForwarderConfig) -> Result<Self, BrokerFailure> {
        let options = mqtt_options(config)?;
        let (client, connection) = Client::new(options, MQTT_CHANNEL_CAPACITY);
        Ok(Self {
            client,
            connection,
            connected: false,
        })
    }

    pub(super) fn connect(&mut self, timeout: Duration) -> Result<(), BrokerFailure> {
        if self.connected {
            return Ok(());
        }
        let deadline = Instant::now() + timeout;
        loop {
            match self.next_event(deadline)? {
                Event::Incoming(Incoming::ConnAck(ack))
                    if ack.code == ConnectReturnCode::Success =>
                {
                    self.connected = true;
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    pub(super) fn publish(
        &mut self,
        publication: &Publication,
        timeout: Duration,
    ) -> Result<(), BrokerFailure> {
        let started_at = Instant::now();
        self.connect(timeout)?;
        let elapsed = started_at.elapsed();
        let remaining = timeout.checked_sub(elapsed).ok_or_else(timeout_failure)?;
        self.client
            .publish_with_properties(
                publication.topic.clone(),
                qos(publication.qos),
                publication.retain,
                publication.payload.clone(),
                publish_properties(publication),
            )
            .map_err(|_| BrokerFailure {
                kind: BrokerFailureKind::Network,
                detail: "MQTT publication could not enter the bounded client queue.".to_owned(),
            })?;

        let deadline = Instant::now() + remaining;
        let mut packet_id = None;
        loop {
            match self.next_event(deadline)? {
                Event::Outgoing(Outgoing::Publish(id)) => {
                    if publication.qos == 0 {
                        return Ok(());
                    }
                    packet_id = Some(id);
                }
                Event::Incoming(Incoming::PubAck(ack))
                    if publication.qos == 1 && packet_id == Some(ack.pkid) =>
                {
                    return match ack.reason {
                        PubAckReason::Success | PubAckReason::NoMatchingSubscribers => Ok(()),
                        _ => Err(publication_rejected()),
                    };
                }
                Event::Incoming(Incoming::PubComp(ack))
                    if publication.qos == 2 && packet_id == Some(ack.pkid) =>
                {
                    return match ack.reason {
                        PubCompReason::Success => Ok(()),
                        PubCompReason::PacketIdentifierNotFound => Err(publication_rejected()),
                    };
                }
                _ => {}
            }
        }
    }

    pub(super) fn poll(&mut self, timeout: Duration) -> Result<(), BrokerFailure> {
        match self.connection.recv_timeout(timeout) {
            Ok(Ok(Event::Incoming(Incoming::ConnAck(ack))))
                if ack.code == ConnectReturnCode::Success =>
            {
                self.connected = true;
                Ok(())
            }
            Ok(Ok(_)) | Err(RecvTimeoutError::Timeout) => Ok(()),
            Ok(Err(error)) => {
                self.connected = false;
                Err(connection_failure(&error))
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.connected = false;
                Err(BrokerFailure {
                    kind: BrokerFailureKind::Network,
                    detail: "MQTT client stopped before the broker operation completed.".to_owned(),
                })
            }
        }
    }

    pub(super) fn disconnect(&mut self, timeout: Duration) -> Result<(), BrokerFailure> {
        if !self.connected {
            return Ok(());
        }
        self.client.try_disconnect().map_err(|_| BrokerFailure {
            kind: BrokerFailureKind::Network,
            detail: "MQTT disconnect could not enter the bounded client queue.".to_owned(),
        })?;
        let deadline = Instant::now() + timeout;
        loop {
            if self.next_event(deadline)? == Event::Outgoing(Outgoing::Disconnect) {
                self.connected = false;
                return Ok(());
            }
        }
    }

    fn next_event(&mut self, deadline: Instant) -> Result<Event, BrokerFailure> {
        let now = Instant::now();
        let remaining = deadline
            .checked_duration_since(now)
            .ok_or_else(timeout_failure)?;
        match self.connection.recv_timeout(remaining.min(POLL_INTERVAL)) {
            Ok(Ok(event)) => Ok(event),
            Ok(Err(error)) => {
                self.connected = false;
                Err(connection_failure(&error))
            }
            Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {
                self.next_event(deadline)
            }
            Err(RecvTimeoutError::Timeout) => Err(timeout_failure()),
            Err(RecvTimeoutError::Disconnected) => Err(BrokerFailure {
                kind: BrokerFailureKind::Network,
                detail: "MQTT client stopped before the broker operation completed.".to_owned(),
            }),
        }
    }
}

pub(super) fn probe(config: &MqttForwarderConfig, timeout: Duration) -> Result<(), BrokerFailure> {
    let mut probe_config = config.clone();
    probe_config.client_id = probe_client_id(&config.client_id);
    let mut session = BrokerSession::new(&probe_config)?;
    let publication = Publication {
        dedup_key: "probe".to_owned(),
        topic: status_topic(config),
        payload: serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "state": "test",
            "forwarder_id": config.forwarder_id,
        }))
        .map_err(|_| BrokerFailure {
            kind: BrokerFailureKind::Protocol,
            detail: "MQTT test status could not be encoded.".to_owned(),
        })?,
        qos: config.qos,
        retain: false,
        event_timestamp_ms: 0,
        content_type: "application/json".to_owned(),
        payload_format_indicator: Some(1),
        correlation_data: config.forwarder_id.as_bytes().to_vec(),
    };
    session.publish(&publication, timeout)?;
    session.disconnect(timeout)
}

fn probe_client_id(client_id: &str) -> String {
    let suffix = format!("-test-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let maximum_prefix_bytes = 128_usize.saturating_sub(suffix.len());
    let prefix = client_id
        .char_indices()
        .take_while(|(index, character)| index + character.len_utf8() <= maximum_prefix_bytes)
        .map(|(_, character)| character)
        .collect::<String>();
    format!("{prefix}{suffix}")
}

fn mqtt_options(config: &MqttForwarderConfig) -> Result<MqttOptions, BrokerFailure> {
    config.validate().map_err(|error| BrokerFailure {
        kind: BrokerFailureKind::Protocol,
        detail: error.to_string(),
    })?;
    let url = Url::parse(&config.broker_url).map_err(|_| BrokerFailure {
        kind: BrokerFailureKind::Protocol,
        detail: "MQTT broker URL is invalid.".to_owned(),
    })?;
    let host = url.host_str().ok_or_else(|| BrokerFailure {
        kind: BrokerFailureKind::Protocol,
        detail: "MQTT broker URL has no host.".to_owned(),
    })?;
    let default_port = if url.scheme() == "mqtts" { 8883 } else { 1883 };
    let mut options = MqttOptions::new(
        config.client_id.clone(),
        host,
        url.port().unwrap_or(default_port),
    );
    options
        .set_keep_alive(MQTT_KEEP_ALIVE)
        .set_clean_start(false)
        .set_session_expiry_interval(Some(MQTT_SESSION_EXPIRY_SECS))
        .set_outgoing_inflight_upper_limit(1)
        .set_max_packet_size(Some(MQTT_MAX_PACKET_BYTES));
    if let Some(username) = &config.username {
        options.set_credentials(
            username.clone(),
            config.password.clone().unwrap_or_default(),
        );
    }
    if url.scheme() == "mqtts" {
        let tls = if let Some(path) = &config.tls_ca_path {
            let ca = std::fs::read(path).map_err(|_| BrokerFailure {
                kind: BrokerFailureKind::Tls,
                detail: "Configured MQTT CA certificate could not be read.".to_owned(),
            })?;
            TlsConfiguration::SimpleNative {
                ca,
                client_auth: None,
            }
        } else {
            TlsConfiguration::Native
        };
        options.set_transport(Transport::tls_with_config(tls));
    }
    let will = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "state": "disconnected",
        "forwarder_id": config.forwarder_id,
    }))
    .map_err(|_| BrokerFailure {
        kind: BrokerFailureKind::Protocol,
        detail: "MQTT Last Will status could not be encoded.".to_owned(),
    })?;
    options.set_last_will(LastWill::new(
        status_topic(config),
        will,
        qos(config.qos),
        config.retain_health,
        Some(LastWillProperties {
            delay_interval: None,
            payload_format_indicator: Some(1),
            message_expiry_interval: None,
            content_type: Some("application/json".to_owned()),
            response_topic: None,
            correlation_data: Some(Bytes::copy_from_slice(config.forwarder_id.as_bytes())),
            user_properties: vec![("schema-version".to_owned(), "1".to_owned())],
        }),
    ));
    Ok(options)
}

fn publish_properties(publication: &Publication) -> PublishProperties {
    PublishProperties {
        payload_format_indicator: publication.payload_format_indicator,
        correlation_data: Some(Bytes::copy_from_slice(&publication.correlation_data)),
        content_type: Some(publication.content_type.clone()),
        user_properties: vec![("schema-version".to_owned(), "1".to_owned())],
        ..PublishProperties::default()
    }
}

const fn qos(value: u8) -> QoS {
    match value {
        0 => QoS::AtMostOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtLeastOnce,
    }
}

fn timeout_failure() -> BrokerFailure {
    BrokerFailure {
        kind: BrokerFailureKind::Timeout,
        detail: "MQTT broker did not acknowledge the operation before its deadline.".to_owned(),
    }
}

fn publication_rejected() -> BrokerFailure {
    BrokerFailure {
        kind: BrokerFailureKind::Protocol,
        detail: "MQTT 5 broker rejected the publication.".to_owned(),
    }
}

fn connection_failure(error: &ConnectionError) -> BrokerFailure {
    match error {
        ConnectionError::ConnectionRefused(
            ConnectReturnCode::BadUserNamePassword
            | ConnectReturnCode::NotAuthorized
            | ConnectReturnCode::BadAuthenticationMethod,
        ) => BrokerFailure {
            kind: BrokerFailureKind::Authentication,
            detail: "MQTT broker rejected the configured credentials.".to_owned(),
        },
        ConnectionError::Tls(_) => BrokerFailure {
            kind: BrokerFailureKind::Tls,
            detail: "MQTT TLS validation failed; verify the broker hostname and CA trust."
                .to_owned(),
        },
        ConnectionError::MqttState(_) | ConnectionError::NotConnAck(_) => BrokerFailure {
            kind: BrokerFailureKind::Protocol,
            detail: "MQTT 5 broker rejected the client protocol settings.".to_owned(),
        },
        ConnectionError::Timeout(_) => timeout_failure(),
        ConnectionError::ConnectionRefused(
            ConnectReturnCode::ServiceUnavailable
            | ConnectReturnCode::ServerUnavailable
            | ConnectReturnCode::ServerBusy
            | ConnectReturnCode::UseAnotherServer
            | ConnectReturnCode::ServerMoved
            | ConnectReturnCode::ConnectionRateExceeded,
        )
        | ConnectionError::Io(_)
        | ConnectionError::RequestsDone => BrokerFailure {
            kind: BrokerFailureKind::Network,
            detail: "MQTT 5 broker is unavailable.".to_owned(),
        },
        ConnectionError::ConnectionRefused(_) => BrokerFailure {
            kind: BrokerFailureKind::Protocol,
            detail: "MQTT 5 broker rejected the connection settings.".to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        path::PathBuf,
    };

    #[test]
    fn builds_plain_and_authenticated_tls_options_without_exposing_credentials() {
        let plain = mqtt_options(&MqttForwarderConfig::default()).unwrap();
        assert_eq!(plain.broker_address(), ("127.0.0.1".to_owned(), 1883));

        let missing_ca = PathBuf::from("/definitely/missing/keeppeek-mqtt-ca.pem");
        let tls = MqttForwarderConfig {
            broker_url: "mqtts://broker.example".to_owned(),
            username: Some("operator".to_owned()),
            password: Some("super-secret".to_owned()),
            tls_ca_path: Some(missing_ca),
            ..MqttForwarderConfig::default()
        };
        let error = mqtt_options(&tls).unwrap_err();
        assert_eq!(error.kind, BrokerFailureKind::Tls);
        assert!(!error.detail.contains("operator"));
        assert!(!error.detail.contains("super-secret"));
        assert!(!error.detail.contains("missing"));
    }

    #[test]
    fn classifies_credentials_and_tls_without_leaking_transport_errors() {
        let authentication = connection_failure(&ConnectionError::ConnectionRefused(
            ConnectReturnCode::BadUserNamePassword,
        ));
        assert_eq!(authentication.kind, BrokerFailureKind::Authentication);
        assert_eq!(
            authentication.detail,
            "MQTT broker rejected the configured credentials."
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn authenticated_tls_probe_uses_configured_ca_and_credentials() {
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
        let mut server_params =
            rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
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
        let identity = rumqttc::tokio_native_tls::native_tls::Identity::from_pkcs8(
            certificate_chain.as_bytes(),
            signing_key.serialize_pem().as_bytes(),
        )
        .unwrap();
        let acceptor = rumqttc::tokio_native_tls::native_tls::TlsAcceptor::new(identity).unwrap();
        let ca_path =
            std::env::temp_dir().join(format!("keeppeek-mqtt-ca-{}.pem", uuid::Uuid::new_v4()));
        std::fs::write(&ca_path, ca_cert.pem()).unwrap();
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
            let mut closed = [0_u8; 1];
            let _ = stream.read(&mut closed);
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
                    0x03, 0x00, 0x10, b'a', b'p', b'p', b'l', b'i', b'c', b'a', b't', b'i', b'o',
                    b'n', b'/', b'j', b's', b'o', b'n'
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
            let mut closed = [0_u8; 1];
            let _ = stream.read(&mut closed);
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
            let (will_properties_length, will_properties_prefix) =
                variable_integer(&body[cursor..]);
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

    #[cfg(not(windows))]
    fn publish_packet_id(header: u8, body: &[u8]) -> Option<u16> {
        if (header >> 1) & 0x03 == 0 {
            return None;
        }
        let topic_length = usize::from(u16::from_be_bytes([body[0], body[1]]));
        let offset = 2 + topic_length;
        Some(u16::from_be_bytes([body[offset], body[offset + 1]]))
    }
}
