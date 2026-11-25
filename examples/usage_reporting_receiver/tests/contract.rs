use rouille::Request;
use serde_json::Value;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use usage_reporting_receiver::{
    aggregate::{MIN_INSTALLATIONS_PER_CELL, PublicAggregateDataset, compute_public_aggregates},
    http::{ReceiverConfig, ReceiverService},
    retention::MAX_RETENTION_SECS,
    schema::{
        ALLOWED_MANUFACTURERS, ALLOWED_MODELS, Architecture, BasicSystemData,
        CURRENT_DISCLOSURE_REVISION, CameraBackend, CameraCapabilityId, CameraCompatibilityData,
        CameraCompatibilityGroup, CameraTransport, CpuModelFamily, CpuVendor, DeploymentType,
        EventSourceTypeId, FeatureAdoptionData, IntegrationId, MAX_PAYLOAD_BYTES, MemoryBucket,
        OsFamily, ProjectedRetentionBucket, RecordingMode, RecordingModeCount, ReleaseChannel,
        ResolutionBucket, SCHEMA_VERSION_V1, StorageCapacityBucket, UsageReportV1,
        VideoAcceleration, VideoCodec,
    },
    store::ReceiverStore,
};
use uuid::Uuid;

const ADMIN_TOKEN: &str = "admin-secret-token-with-sufficient-entropy-0123456789";

fn make_token(seed: usize) -> String {
    format!("install-proof-token-{seed:020}-abcdef0123456789")
}

struct ReportSpec<'a> {
    os_family: OsFamily,
    manufacturer: &'a str,
    model: &'a str,
    backend: CameraBackend,
    codec: VideoCodec,
    resolution: ResolutionBucket,
    include_optional: bool,
}

fn make_report(id: &str, sequence: u64, spec: ReportSpec<'_>) -> UsageReportV1 {
    let os_family = spec.os_family;
    UsageReportV1 {
        schema_version: SCHEMA_VERSION_V1,
        disclosure_revision: CURRENT_DISCLOSURE_REVISION,
        report_sequence: sequence,
        installation_id: id.to_owned(),
        basic: BasicSystemData {
            keeppeek_version: "0.1.0-alpha.1".to_owned(),
            release_channel: ReleaseChannel::Alpha,
            deployment_type: if os_family == OsFamily::Linux {
                DeploymentType::Container
            } else {
                DeploymentType::Native
            },
            os_family,
            os_version: if os_family == OsFamily::Linux {
                "24.04".to_owned()
            } else {
                "11".to_owned()
            },
            architecture: Architecture::X86_64,
            cpu_vendor: if os_family == OsFamily::Linux {
                CpuVendor::Intel
            } else {
                CpuVendor::Amd
            },
            cpu_model_family: if os_family == OsFamily::Linux {
                CpuModelFamily::IntelNSeries
            } else {
                CpuModelFamily::AmdRyzen
            },
            physical_cores: Some(4),
            logical_cores: 8,
            memory_bucket: if os_family == OsFamily::Linux {
                MemoryBucket::From8To16Gb
            } else {
                MemoryBucket::From16To32Gb
            },
            video_acceleration: vec![if os_family == OsFamily::Linux {
                VideoAcceleration::Vaapi
            } else {
                VideoAcceleration::D3d11va
            }],
        },
        camera_compatibility: spec.include_optional.then(|| CameraCompatibilityData {
            configured_cameras: 2,
            connected_cameras: 2,
            decodable_cameras: 2,
            recording_cameras: 2,
            groups: vec![CameraCompatibilityGroup {
                manufacturer: spec.manufacturer.to_owned(),
                model: spec.model.to_owned(),
                backend: spec.backend,
                transport: CameraTransport::Tcp,
                video_codec: spec.codec,
                resolution_bucket: spec.resolution,
                capabilities: vec![
                    CameraCapabilityId::Audio,
                    CameraCapabilityId::Events,
                    CameraCapabilityId::Recording,
                ],
                count: 2,
            }],
        }),
        feature_adoption: spec.include_optional.then(|| FeatureAdoptionData {
            recording_modes: vec![RecordingModeCount {
                mode: RecordingMode::Continuous,
                count: 2,
            }],
            storage_capacity_bucket: if os_family == OsFamily::Linux {
                StorageCapacityBucket::From1TbTo4Tb
            } else {
                StorageCapacityBucket::From4TbTo16Tb
            },
            projected_retention_bucket: if os_family == OsFamily::Linux {
                ProjectedRetentionBucket::From7To30Days
            } else {
                ProjectedRetentionBucket::From30To90Days
            },
            enabled_integrations: if os_family == OsFamily::Linux {
                vec![IntegrationId::MqttForwarder]
            } else {
                Vec::new()
            },
            event_source_types: vec![
                if os_family == OsFamily::Linux {
                    EventSourceTypeId::ReolinkEvents
                } else {
                    EventSourceTypeId::HikvisionIsapi
                },
                EventSourceTypeId::OperationalEvents,
            ],
        }),
    }
}

const fn linux_reolink(include_optional: bool) -> ReportSpec<'static> {
    ReportSpec {
        os_family: OsFamily::Linux,
        manufacturer: "reolink",
        model: "reolink_rlc_810a",
        backend: CameraBackend::ReoProto,
        codec: VideoCodec::H265,
        resolution: ResolutionBucket::Uhd4k,
        include_optional,
    }
}

const fn windows_hikvision(include_optional: bool) -> ReportSpec<'static> {
    ReportSpec {
        os_family: OsFamily::Windows,
        manufacturer: "hikvision",
        model: "hikvision_ds_2cd2347g2",
        backend: CameraBackend::Retina,
        codec: VideoCodec::H264,
        resolution: ResolutionBucket::Qhd1440p,
        include_optional,
    }
}

const fn macos_axis(include_optional: bool) -> ReportSpec<'static> {
    ReportSpec {
        os_family: OsFamily::Macos,
        manufacturer: "axis",
        model: "axis_p3245_lv",
        backend: CameraBackend::Retina,
        codec: VideoCodec::H264,
        resolution: ResolutionBucket::Fhd1080p,
        include_optional,
    }
}

fn read_response_text(response: rouille::Response) -> String {
    let (mut reader, _) = response.data.into_reader_and_size();
    let mut output = String::new();
    std::io::Read::read_to_string(&mut reader, &mut output).unwrap();
    output
}

fn make_service(now: Arc<AtomicI64>) -> (Arc<Mutex<ReceiverStore>>, ReceiverService) {
    let store = Arc::new(Mutex::new(ReceiverStore::open_in_memory().unwrap()));
    let clock_ref = Arc::clone(&now);
    let service = ReceiverService::with_clock(
        Arc::clone(&store),
        ReceiverConfig::default().with_admin_token(ADMIN_TOKEN),
        Arc::new(move || clock_ref.load(Ordering::Acquire)),
    );
    (store, service)
}

fn put_report(service: &ReceiverService, report: &UsageReportV1, token: &str) -> rouille::Response {
    let body = serde_json::to_vec(report).unwrap();
    let request = Request::fake_http(
        "PUT",
        format!("/v1/installations/{}", report.installation_id),
        vec![
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("Authorization".to_owned(), format!("Bearer {token}")),
            ("X-Forwarded-For".to_owned(), "203.0.113.42".to_owned()),
        ],
        body,
    );
    service.handle(&request)
}

fn delete_report(service: &ReceiverService, id: &str, token: &str) -> rouille::Response {
    let request = Request::fake_http(
        "DELETE",
        format!("/v1/installations/{id}"),
        vec![("Authorization".to_owned(), format!("Bearer {token}"))],
        Vec::new(),
    );
    service.handle(&request)
}

#[test]
fn receiver_schema_matches_published_json_schema() {
    let schema_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas/usage-reporting/v1.schema.json");
    let raw = std::fs::read_to_string(schema_path).unwrap();
    let schema: Value = serde_json::from_str(&raw).unwrap();

    let brands: Vec<&str> = schema["$defs"]["cameraManufacturer"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    assert_eq!(brands, ALLOWED_MANUFACTURERS);

    let models: Vec<&str> = schema["$defs"]["cameraModel"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    let mut expected: Vec<&str> = ALLOWED_MODELS.iter().map(|(_, model)| *model).collect();
    expected.push("unknown");
    assert_eq!(models, expected);
}

#[test]
fn contract_enforces_schema_size_auth_ordering_tombstones_and_no_ip_storage() {
    let now = Arc::new(AtomicI64::new(1_790_000_000));
    let (store, service) = make_service(Arc::clone(&now));
    let id = Uuid::new_v4().to_string();
    let token = make_token(1);
    let wrong_token = make_token(2);

    // Raw GET is always forbidden/method-not-allowed.
    let raw_get = service.handle(&Request::fake_http(
        "GET",
        format!("/v1/installations/{id}"),
        vec![("Authorization".to_owned(), format!("Bearer {token}"))],
        Vec::new(),
    ));
    assert_eq!(raw_get.status_code, 405);

    // Missing bearer token -> 401.
    let report = make_report(&id, 1, linux_reolink(true));
    let no_auth = service.handle(&Request::fake_http(
        "PUT",
        format!("/v1/installations/{id}"),
        vec![("Content-Type".to_owned(), "application/json".to_owned())],
        serde_json::to_vec(&report).unwrap(),
    ));
    assert_eq!(no_auth.status_code, 401);

    // Wrong media type -> 415.
    let wrong_ct = service.handle(&Request::fake_http(
        "PUT",
        format!("/v1/installations/{id}"),
        vec![
            ("Content-Type".to_owned(), "text/plain".to_owned()),
            ("Authorization".to_owned(), format!("Bearer {token}")),
        ],
        serde_json::to_vec(&report).unwrap(),
    ));
    assert_eq!(wrong_ct.status_code, 415);

    // Oversized body (65,537 bytes) -> 413.
    let mut oversized = serde_json::to_vec(&report).unwrap();
    oversized.resize(MAX_PAYLOAD_BYTES + 1, b' ');
    let too_big = service.handle(&Request::fake_http(
        "PUT",
        format!("/v1/installations/{id}"),
        vec![
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("Authorization".to_owned(), format!("Bearer {token}")),
        ],
        oversized,
    ));
    assert_eq!(too_big.status_code, 413);

    // Truncated JSON -> 400.
    let truncated = service.handle(&Request::fake_http(
        "PUT",
        format!("/v1/installations/{id}"),
        vec![
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("Authorization".to_owned(), format!("Bearer {token}")),
        ],
        b"{\"schema_version\":1".to_vec(),
    ));
    assert_eq!(truncated.status_code, 400);

    // Valid PUT -> 200 replaced; identical retry -> 200 idempotent.
    let first = put_report(&service, &report, &token);
    assert_eq!(first.status_code, 200);
    assert!(read_response_text(first).contains("replaced"));

    let retry = put_report(&service, &report, &token);
    assert_eq!(retry.status_code, 200);
    assert!(read_response_text(retry).contains("idempotent"));

    // Wrong installation token cannot replace or delete -> 403.
    let mut seq2 = report.clone();
    seq2.report_sequence = 2;
    assert_eq!(put_report(&service, &seq2, &wrong_token).status_code, 403);
    assert_eq!(delete_report(&service, &id, &wrong_token).status_code, 403);

    // Sequence 2 replaces sequence 1; stale sequence 1 is rejected with 409.
    assert_eq!(put_report(&service, &seq2, &token).status_code, 200);
    assert_eq!(put_report(&service, &report, &token).status_code, 409);

    // Delete removes the row and creates a tombstone; duplicate delete is idempotent 204.
    assert_eq!(delete_report(&service, &id, &token).status_code, 204);
    assert_eq!(delete_report(&service, &id, &token).status_code, 204);

    // Advance past the 60s per-install rate-limit window so we verify tombstone fencing (410 Gone).
    now.fetch_add(61, Ordering::AcqRel);
    let mut seq3 = report;
    seq3.report_sequence = 3;
    assert_eq!(put_report(&service, &seq3, &token).status_code, 410);

    // Verify durable tables contain zero source IP, header, or location columns.
    let guard = store.lock().unwrap();
    let install_cols = guard.table_columns("installations").unwrap();
    let tombstone_cols = guard.table_columns("tombstones").unwrap();
    for col in install_cols.iter().chain(tombstone_cols.iter()) {
        let lower = col.to_ascii_lowercase();
        assert!(!lower.contains("ip"));
        assert!(!lower.contains("geo"));
        assert!(!lower.contains("location"));
        assert!(!lower.contains("forwarded"));
        assert!(!lower.contains("agent"));
    }
}

#[test]
fn admin_endpoints_require_token_and_rate_limiting_enforces_429() {
    let now = Arc::new(AtomicI64::new(1_790_000_000));
    let store = Arc::new(Mutex::new(ReceiverStore::open_in_memory().unwrap()));
    let clock_ref = Arc::clone(&now);
    let config = ReceiverConfig {
        per_install_limit: 2,
        ..ReceiverConfig::default().with_admin_token(ADMIN_TOKEN)
    };
    let service = ReceiverService::with_clock(
        store,
        config,
        Arc::new(move || clock_ref.load(Ordering::Acquire)),
    );

    // Admin route without token -> 401; wrong token -> 403; right token -> 200.
    assert_eq!(
        service
            .handle(&Request::fake_http(
                "GET",
                "/v1/admin/status",
                Vec::new(),
                Vec::new()
            ))
            .status_code,
        401
    );
    assert_eq!(
        service
            .handle(&Request::fake_http(
                "GET",
                "/v1/admin/status",
                vec![(
                    "Authorization".to_owned(),
                    format!("Bearer {}", make_token(9))
                )],
                Vec::new(),
            ))
            .status_code,
        403
    );
    assert_eq!(
        service
            .handle(&Request::fake_http(
                "GET",
                "/v1/admin/status",
                vec![("Authorization".to_owned(), format!("Bearer {ADMIN_TOKEN}"))],
                Vec::new(),
            ))
            .status_code,
        200
    );

    // Rate limit allows 2 requests per window and returns 429 on the 3rd.
    let id = Uuid::new_v4().to_string();
    let token = make_token(10);
    let report = make_report(&id, 1, linux_reolink(false));
    assert_eq!(put_report(&service, &report, &token).status_code, 200);
    assert_eq!(put_report(&service, &report, &token).status_code, 200);
    assert_eq!(put_report(&service, &report, &token).status_code, 429);
}

#[test]
fn aggregation_enforces_k10_distinct_counts_expiry_deletion_and_complementary_suppression() {
    let now = Arc::new(AtomicI64::new(1_790_000_000));
    let (_, service) = make_service(Arc::clone(&now));

    // Insert 9 distinct installations -> public aggregate must be completely suppressed.
    let mut ids = Vec::new();
    for index in 0..9 {
        let id = Uuid::new_v4().to_string();
        let token = make_token(100 + index);
        let report = make_report(&id, 1, linux_reolink(true));
        assert_eq!(put_report(&service, &report, &token).status_code, 200);
        // Updating the same UUID does not inflate distinct installation counts.
        let mut updated = report;
        updated.report_sequence = 2;
        assert_eq!(put_report(&service, &updated, &token).status_code, 200);
        ids.push((id, token));
    }

    let nine_res = service.handle(&Request::fake_http(
        "GET",
        "/v1/aggregates",
        Vec::new(),
        Vec::new(),
    ));
    assert_eq!(nine_res.status_code, 200);
    let nine_body = read_response_text(nine_res);
    let nine_data: PublicAggregateDataset = serde_json::from_str(&nine_body).unwrap();
    assert!(nine_data.suppressed);
    assert_eq!(nine_data.active_installations, None);
    assert!(nine_data.dimensions.os_family.is_empty());
    for (id, _) in &ids {
        assert!(!nine_body.contains(id));
    }

    // Add 10th distinct installation -> cells meet k=10 and publish cleanly.
    let tenth_id = Uuid::new_v4().to_string();
    let tenth_token = make_token(109);
    let tenth = make_report(&tenth_id, 1, linux_reolink(true));
    assert_eq!(put_report(&service, &tenth, &tenth_token).status_code, 200);

    let ten_body = read_response_text(service.handle(&Request::fake_http(
        "GET",
        "/v1/aggregates",
        Vec::new(),
        Vec::new(),
    )));
    let ten_data: PublicAggregateDataset = serde_json::from_str(&ten_body).unwrap();
    assert!(!ten_data.suppressed);
    assert_eq!(
        ten_data.active_installations,
        Some(MIN_INSTALLATIONS_PER_CELL)
    );
    assert_eq!(ten_data.dimensions.os_family.get("linux"), Some(&10));
    assert_eq!(
        ten_data
            .combined_dimensions
            .os_x_architecture
            .get("linux:x86_64"),
        Some(&10)
    );
    assert!(!ten_body.contains(&tenth_id));

    // Delete 1 installation -> drops back to 9 and suppresses immediately.
    assert_eq!(
        delete_report(&service, &tenth_id, &tenth_token).status_code,
        204
    );
    let after_delete: PublicAggregateDataset =
        serde_json::from_str(&read_response_text(service.handle(&Request::fake_http(
            "GET",
            "/v1/aggregates",
            Vec::new(),
            Vec::new(),
        ))))
        .unwrap();
    assert!(after_delete.suppressed);

    // Re-add 5 Linux, 12 Windows, and 3 macOS (total 29: 14 Linux, 12 Windows, 3 macOS).
    // Complementary suppression must suppress macOS (3) AND Windows (12) so residual 29 - 14 = 15 >= 10.
    for index in 0..5 {
        let id = Uuid::new_v4().to_string();
        let token = make_token(200 + index);
        let report = make_report(&id, 1, linux_reolink(true));
        assert_eq!(put_report(&service, &report, &token).status_code, 200);
    }
    for index in 0..12 {
        let id = Uuid::new_v4().to_string();
        let token = make_token(300 + index);
        let report = make_report(&id, 1, windows_hikvision(true));
        assert_eq!(put_report(&service, &report, &token).status_code, 200);
    }
    for index in 0..3 {
        let id = Uuid::new_v4().to_string();
        let token = make_token(400 + index);
        let report = make_report(&id, 1, macos_axis(true));
        assert_eq!(put_report(&service, &report, &token).status_code, 200);
    }

    let mixed: PublicAggregateDataset = serde_json::from_str(&read_response_text(service.handle(
        &Request::fake_http("GET", "/v1/aggregates", Vec::new(), Vec::new()),
    )))
    .unwrap();
    assert_eq!(mixed.active_installations, Some(29));
    assert_eq!(mixed.dimensions.os_family.get("linux"), Some(&14));
    assert_eq!(mixed.dimensions.os_family.get("macos"), None);
    assert_eq!(mixed.dimensions.os_family.get("windows"), None);
    let published_os_sum: u64 = mixed.dimensions.os_family.values().sum();
    let os_residual = mixed.active_installations.unwrap() - published_os_sum;
    assert!(os_residual >= MIN_INSTALLATIONS_PER_CELL);

    // Advance fake clock by 60 days minus 1 second -> still active.
    now.store(1_790_000_000 + MAX_RETENTION_SECS - 1, Ordering::Release);
    let just_before: PublicAggregateDataset =
        serde_json::from_str(&read_response_text(service.handle(&Request::fake_http(
            "GET",
            "/v1/aggregates",
            Vec::new(),
            Vec::new(),
        ))))
        .unwrap();
    assert_eq!(just_before.active_installations, Some(29));

    // Advance fake clock to exact 60-day boundary -> all inactive rows expire and purge.
    now.store(1_790_000_000 + MAX_RETENTION_SECS, Ordering::Release);
    let expired: PublicAggregateDataset =
        serde_json::from_str(&read_response_text(service.handle(&Request::fake_http(
            "GET",
            "/v1/aggregates",
            Vec::new(),
            Vec::new(),
        ))))
        .unwrap();
    assert!(expired.suppressed);
    assert_eq!(expired.active_installations, None);
}

#[test]
fn published_aggregate_example_matches_aggregator_output() {
    let mut reports = Vec::new();
    for _ in 0..14 {
        reports.push(make_report(
            &Uuid::new_v4().to_string(),
            1,
            linux_reolink(true),
        ));
    }
    for _ in 0..10 {
        reports.push(make_report(
            &Uuid::new_v4().to_string(),
            1,
            windows_hikvision(true),
        ));
    }
    let expected = compute_public_aggregates(&reports, 1_790_000_000);
    let example_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("public/aggregate-example.json");
    let raw = std::fs::read_to_string(example_path).unwrap();
    let parsed: PublicAggregateDataset = serde_json::from_str(&raw).unwrap();
    assert_eq!(parsed, expected);
}
