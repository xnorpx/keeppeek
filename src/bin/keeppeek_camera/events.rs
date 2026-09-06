use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    time::{Duration, Instant},
};

use clap::{Args, ValueEnum};
use keeppeek::{
    cameras::CameraConfig,
    config,
    logging::{LogHub, LogLevel},
    shutdown::Shutdown,
};
use onvif::{
    event::{Notification, PropertyOperation, parse_notifications},
    soap::client::{AuthType, Client, ClientBuilder, Credentials},
};
use schema::{
    b_2, devicemgmt, event,
    transport::{self, Transport},
};
use serde_json::{Value, json};
use url::Url;

mod isapi_probe;

const TOPICS_NAMESPACE: &str = "http://www.onvif.org/ver10/topics";
const EVENTS_NAMESPACE: &str = "http://www.onvif.org/ver10/events/wsdl";
const RESPONSE_BYTES_MAX: u64 = 256 * 1024;
const PULL_REQUEST_COUNT_MAX: usize = 2048;
const PULL: &str = r#"<e:PullMessages xmlns:e="http://www.onvif.org/ver10/events/wsdl"><e:Timeout>PT2S</e:Timeout><e:MessageLimit>32</e:MessageLimit></e:PullMessages>"#;

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum Protocol {
    #[default]
    Onvif,
    Isapi,
}

#[derive(Debug, Args)]
pub struct Cli {
    /// Select the camera event protocol; neither choice changes camera settings.
    #[arg(long, value_enum, default_value_t = Protocol::Onvif)]
    protocol: Protocol,

    /// Load the selected camera's existing credentials from this configuration.
    #[arg(long, default_value_os_t = config::config_path())]
    credentials_from: PathBuf,

    /// Select one configured camera by its name or IP address.
    #[arg(long)]
    camera: String,

    /// Listen for notifications for this many seconds.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
    duration: u64,
}

pub fn run(cli: Cli) -> anyhow::Result<()> {
    match cli.protocol {
        Protocol::Onvif => run_onvif(cli),
        Protocol::Isapi => isapi_probe::run(cli),
    }
}

fn run_onvif(cli: Cli) -> anyhow::Result<()> {
    let configured = config::load_cameras(&cli.credentials_from)
        .map_err(|_| anyhow::anyhow!("could not load camera credentials from the configuration"))?;
    let camera = super::stream_test::select_camera_config(&configured, &cli.camera)?;
    if camera.username.is_empty() {
        anyhow::bail!("selected camera has no configured username");
    }
    let report = LogHub::default();
    report.set_sensitive_values(
        configured
            .values()
            .flatten()
            .flat_map(|camera| [camera.username.clone(), camera.password.clone()]),
    );
    let shutdown = Shutdown::new();
    let signal = shutdown.clone();
    ctrlc::set_handler(move || signal.cancel())?;
    let authority = SocketAddr::new(camera.ip, camera.onvif_port.unwrap_or(8000));
    let endpoint = Url::parse(&format!("http://{authority}/onvif/device_service"))?;
    let device = probe_client(&endpoint, &camera);
    let identity =
        devicemgmt::get_device_information(&device, &devicemgmt::GetDeviceInformation {})
            .map_err(|error| probe_error("GetDeviceInformation", error))?;
    emit(
        &report,
        "camera",
        json!({
            "camera": camera.display_name(), "ip": camera.ip,
            "manufacturer": identity.manufacturer, "model": identity.model,
            "firmware": identity.firmware_version,
        }),
    );
    let services = devicemgmt::get_services(
        &device,
        &devicemgmt::GetServices {
            include_capability: true,
        },
    )
    .map_err(|error| probe_error("GetServices", error))?;
    let service = services
        .service
        .iter()
        .find(|service| service.namespace == EVENTS_NAMESPACE)
        .ok_or_else(|| anyhow::anyhow!("camera does not advertise an ONVIF Events service"))?;
    let endpoint = validated_subscription_endpoint(&service.x_addr, camera.ip)?;
    let events = probe_client(&endpoint, &camera);
    report_capabilities(&events, &report);
    let subscription = event::create_pull_point_subscription(
        &events,
        &event::CreatePullPointSubscription {
            initial_termination_time: Some(b_2::AbsoluteOrRelativeTimeType::Duration(
                "PT90S"
                    .parse()
                    .expect("fixed subscription duration is valid"),
            )),
            ..Default::default()
        },
    )
    .map_err(|error| probe_error("CreatePullPointSubscription", error))?;
    let endpoint =
        validated_subscription_endpoint(&subscription.subscription_reference.address, camera.ip)?;
    let subscriber = probe_client(&endpoint, &camera);
    let result = observe_subscription(&subscriber, &subscription, &shutdown, &report, cli.duration);
    let cleanup: Result<b_2::UnsubscribeResponse, _> =
        transport::request(&subscriber, &b_2::Unsubscribe {});
    emit(
        &report,
        "unsubscribe",
        json!({"succeeded": cleanup.is_ok()}),
    );
    result?;
    cleanup.map_err(|error| probe_error("Unsubscribe", error))?;
    Ok(())
}

fn probe_client(endpoint: &Url, camera: &CameraConfig) -> Client {
    ClientBuilder::new(endpoint)
        .credentials(Some(Credentials {
            username: camera.username.clone(),
            password: camera.password.clone(),
        }))
        .auth_type(AuthType::Any)
        .timeout(Duration::from_secs(5))
        .follow_redirects(false)
        .response_size_limit(RESPONSE_BYTES_MAX)
        .build()
}

fn report_capabilities(client: &Client, report: &LogHub) {
    match event::get_service_capabilities(client, &event::GetServiceCapabilities {}) {
        Ok(response) => emit(
            report,
            "event_capabilities",
            json!({
                "pullpoint": response.capabilities.ws_pull_point_support,
                "max_pullpoints": response.capabilities.max_pull_points,
                "persistent_storage": response.capabilities.persistent_notification_storage,
            }),
        ),
        Err(error) => emit(
            report,
            "event_capabilities",
            json!({
                "error": probe_error("GetServiceCapabilities", error).to_string(),
            }),
        ),
    }
    match event::get_event_properties(client, &event::GetEventProperties {}) {
        Ok(response) => emit(
            report,
            "event_properties",
            json!({
                "received": true, "topic_namespaces": response.topic_namespace_location,
            }),
        ),
        Err(error) => emit(
            report,
            "event_properties",
            json!({
                "error": probe_error("GetEventProperties", error).to_string(),
            }),
        ),
    }
}

fn observe_subscription(
    client: &Client,
    subscription: &event::CreatePullPointSubscriptionResponse,
    shutdown: &Shutdown,
    report: &LogHub,
    duration_secs: u64,
) -> anyhow::Result<()> {
    if subscription
        .subscription_reference
        .reference_parameters
        .is_some()
    {
        anyhow::bail!(
            "subscription requires WS-Addressing reference parameters; this probe does not yet support them"
        );
    }
    let lease = subscription
        .termination_time
        .to_chrono_datetime()
        .signed_duration_since(subscription.current_time.to_chrono_datetime())
        .num_seconds();
    let renew_every = lease_renewal_interval(lease)?;
    emit(
        report,
        "subscribed",
        json!({"lease_seconds": lease, "duration_seconds": duration_secs}),
    );
    let synchronized = event::set_synchronization_point(client, &event::SetSynchronizationPoint {});
    emit(
        report,
        "synchronize",
        json!({"succeeded": synchronized.is_ok()}),
    );
    let started = Instant::now();
    let deadline = started + Duration::from_secs(duration_secs);
    let mut renew_at = started + renew_every;
    let mut counts = ObservationCounts::default();
    for _ in 0..PULL_REQUEST_COUNT_MAX {
        if Instant::now() >= deadline || shutdown.is_cancelled() {
            emit(
                report,
                "summary",
                json!({
                    "notifications": counts.notifications, "motion_states": counts.motion_states,
                    "motion_changes": counts.motion_changes, "polls": counts.polls,
                    "elapsed_ms": started.elapsed().as_millis(),
                }),
            );
            return Ok(());
        }
        if Instant::now() >= renew_at {
            renew_at = Instant::now() + renew_subscription(client)?;
            emit(report, "renew", json!({"succeeded": true}));
        }
        let response = client
            .request(PULL)
            .map_err(|error| probe_error("PullMessages", error))?;
        let notifications = parse_notifications(response.as_bytes()).map_err(|_| {
            anyhow::anyhow!("PullMessages returned unsupported or malformed notification XML")
        })?;
        counts.polls += 1;
        for notification in &notifications {
            report_notification(report, notification, &mut counts);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        shutdown.wait_timeout(remaining.min(Duration::from_millis(200)));
    }
    anyhow::bail!("event probe reached its bounded pull request limit")
}

#[derive(Default)]
struct ObservationCounts {
    polls: u32,
    notifications: u32,
    motion_states: u32,
    motion_changes: u32,
}

fn report_notification(
    report: &LogHub,
    notification: &Notification,
    counts: &mut ObservationCounts,
) {
    let motion = motion_state(notification);
    counts.notifications += 1;
    counts.motion_states += u32::from(motion.is_some());
    let operation = match &notification.property_operation {
        Some(PropertyOperation::Initialized) => "Initialized",
        Some(PropertyOperation::Changed) => "Changed",
        Some(PropertyOperation::Deleted) => "Deleted",
        None => "point",
        _ => "unknown",
    };
    counts.motion_changes += u32::from(motion.is_some() && operation == "Changed");
    let topic: Vec<_> = notification
        .topic
        .path
        .iter()
        .map(|segment| {
            json!({
                "namespace": segment.namespace_uri, "name": segment.local_name,
            })
        })
        .collect();
    let source: Vec<_> = notification
        .source
        .simple
        .iter()
        .filter(|item| {
            matches!(
                item.name.as_str(),
                "VideoSourceConfigurationToken" | "VideoSourceToken" | "InputToken"
            )
        })
        .map(|item| json!({"name": item.name, "value": item.value}))
        .collect();
    emit(
        report,
        "notification",
        json!({
            "utc_time": notification.utc_time.to_rfc3339(), "operation": operation,
            "topic": topic, "source": source, "motion_active": motion,
            "data_fields": notification.data.simple.iter().map(|item| &item.name).collect::<Vec<_>>(),
        }),
    );
}

fn renew_subscription(client: &Client) -> anyhow::Result<Duration> {
    let response: b_2::RenewResponse = transport::request(
        client,
        &b_2::Renew {
            termination_time: b_2::AbsoluteOrRelativeTimeType::Duration(
                "PT90S"
                    .parse()
                    .expect("fixed subscription duration is valid"),
            ),
        },
    )
    .map_err(|error| probe_error("Renew", error))?;
    let lease = response
        .termination_time
        .to_chrono_datetime()
        .signed_duration_since(response.current_time.to_chrono_datetime())
        .num_seconds();
    lease_renewal_interval(lease)
}

fn lease_renewal_interval(lease_secs: i64) -> anyhow::Result<Duration> {
    if lease_secs < 2 {
        anyhow::bail!("camera returned an expired or unusably short subscription lease");
    }
    Ok(Duration::from_secs(u64::try_from(lease_secs / 2)?.min(30)))
}

fn emit(report: &LogHub, stage: &'static str, fields: Value) {
    let Value::Object(fields) = fields else {
        unreachable!("probe reports always contain object fields");
    };
    let entry = report.record(
        LogLevel::Info,
        "camera_events",
        stage.to_owned(),
        fields.into_iter().collect(),
        None,
        None,
    );
    println!("{}", json!({"stage": stage, "evidence": entry.fields}));
}

fn probe_error(operation: &'static str, error: transport::Error) -> anyhow::Error {
    let kind = match error {
        transport::Error::Authorization(_) => "authentication failed",
        transport::Error::Timeout(_) => "request timed out",
        transport::Error::Connection(_) => "connection failed",
        transport::Error::Redirection(_) => "redirect refused",
        transport::Error::Serialization(_) => "request serialization failed",
        transport::Error::Deserialization(_) => "response deserialization failed",
        transport::Error::Protocol(_) => "invalid or oversized response",
        transport::Error::Other(_) => "camera rejected the request",
    };
    anyhow::anyhow!("{operation}: {kind}")
}

fn validated_subscription_endpoint(raw: &str, camera_ip: IpAddr) -> anyhow::Result<Url> {
    let mut endpoint = Url::parse(raw)?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        anyhow::bail!("subscription endpoint must use HTTP or HTTPS");
    }
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        anyhow::bail!("subscription endpoint must not contain credentials");
    }
    if endpoint.fragment().is_some() {
        anyhow::bail!("subscription endpoint must not contain a fragment");
    }

    let endpoint_ip = endpoint
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("subscription endpoint has no host"))?
        .parse::<IpAddr>()?;
    if endpoint_ip.is_unspecified() {
        endpoint.set_host(Some(&camera_ip.to_string()))?;
    } else if endpoint_ip != camera_ip {
        anyhow::bail!("subscription endpoint does not use the selected camera address");
    }
    Ok(endpoint)
}

fn motion_state(notification: &Notification) -> Option<bool> {
    let segments = &notification.topic.path;
    if segments.first()?.namespace_uri.as_deref() != Some(TOPICS_NAMESPACE)
        || segments.iter().skip(1).any(|segment| {
            segment
                .namespace_uri
                .as_deref()
                .is_some_and(|namespace| namespace != TOPICS_NAMESPACE)
        })
    {
        return None;
    }
    let names = segments.iter().map(|segment| segment.local_name.as_str());
    let field = if names
        .clone()
        .eq(["RuleEngine", "CellMotionDetector", "Motion"])
    {
        "IsMotion"
    } else if names.eq(["VideoSource", "MotionAlarm"]) {
        "State"
    } else {
        return None;
    };
    match notification
        .data
        .simple
        .iter()
        .find(|item| item.name == field)?
        .value
        .as_str()
    {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::net::Ipv4Addr;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        command: Cli,
    }

    #[test]
    fn event_probe_accepts_explicit_isapi_and_rejects_unknown_protocols() {
        assert!(
            TestCli::try_parse_from(["events", "--camera", "front", "--protocol", "isapi",])
                .is_ok()
        );
        assert!(
            TestCli::try_parse_from(["events", "--camera", "front", "--protocol", "sdk",]).is_err()
        );
    }

    #[test]
    fn event_probe_loads_credentials_from_config_and_bounds_duration() {
        let command = TestCli::try_parse_from([
            "events",
            "--credentials-from",
            "/private/config.toml",
            "--camera",
            "192.168.1.20",
            "--duration",
            "10",
        ])
        .unwrap()
        .command;
        assert_eq!(
            command.credentials_from,
            std::path::PathBuf::from("/private/config.toml")
        );
        assert_eq!(command.duration, 10);
        for duration in ["0", "301"] {
            assert!(
                TestCli::try_parse_from(["events", "--camera", "front", "--duration", duration,])
                    .is_err()
            );
        }
        assert!(
            TestCli::try_parse_from(["events", "--camera", "front", "--password", "not-accepted",])
                .is_err()
        );
    }

    #[test]
    fn accepts_only_subscription_endpoints_on_the_selected_camera() {
        let camera_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));

        let endpoint = validated_subscription_endpoint(
            "http://192.168.1.20/onvif/Subscription?Idx=5",
            camera_ip,
        )
        .unwrap();
        assert_eq!(
            endpoint.as_str(),
            "http://192.168.1.20/onvif/Subscription?Idx=5"
        );

        assert!(validated_subscription_endpoint("http://192.168.1.21/private", camera_ip).is_err());
        assert!(validated_subscription_endpoint("file:///etc/passwd", camera_ip).is_err());
    }

    #[test]
    fn repairs_unspecified_subscription_host_without_changing_resource() {
        let camera_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));

        let endpoint = validated_subscription_endpoint(
            "http://0.0.0.0:8080/onvif/Subscription?Idx=5",
            camera_ip,
        )
        .unwrap();

        assert_eq!(
            endpoint.as_str(),
            "http://192.168.1.20:8080/onvif/Subscription?Idx=5"
        );
    }

    #[test]
    fn identifies_motion_only_from_the_standard_topic_and_boolean_field() {
        let xml = r#"<n:NotificationMessage xmlns:n="http://docs.oasis-open.org/wsn/b-2"
            xmlns:t="http://www.onvif.org/ver10/schema"
            xmlns:topics="http://www.onvif.org/ver10/topics">
            <n:Topic Dialect="http://www.onvif.org/ver10/tev/topicExpression/ConcreteSet">topics:RuleEngine/CellMotionDetector/Motion</n:Topic>
            <n:Message><t:Message UtcTime="2026-09-04T12:34:56Z" PropertyOperation="Changed">
                <t:Data><t:SimpleItem Name="IsMotion" Value="true"/></t:Data>
            </t:Message></n:Message></n:NotificationMessage>"#;
        for (document, expected) in [
            (xml.to_owned(), Some(true)),
            (
                xml.replace("Value=\"true\"", "Value=\"false\""),
                Some(false),
            ),
            (xml.replace("Value=\"true\"", "Value=\"uncertain\""), None),
            (
                xml.replace("http://www.onvif.org/ver10/topics", "urn:vendor:topics"),
                None,
            ),
        ] {
            let notifications = onvif::event::parse_notifications(document.as_bytes()).unwrap();
            assert_eq!(motion_state(&notifications[0]), expected);
        }
    }
}
