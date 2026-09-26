#[path = "../build_support/camera_database.rs"]
mod camera_database;

use camera_database::download_camera_database_archive;
use test_hikvision::{FakeHikvision, Reply};

#[test]
fn retries_server_errors_and_returns_success_bytes() {
    let server = FakeHikvision::builder()
        .digest(false)
        .replies([
            Reply::http(500, "text/plain", "unavailable"),
            Reply::http(200, "application/zip", b"archive bytes"),
        ])
        .start()
        .unwrap();

    let bytes = download_camera_database_archive(&server.origin()).unwrap();

    assert_eq!(bytes, b"archive bytes");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn does_not_retry_client_errors() {
    let server = FakeHikvision::builder()
        .digest(false)
        .replies([
            Reply::http(404, "text/plain", "missing"),
            Reply::http(200, "application/zip", b"archive bytes"),
        ])
        .start()
        .unwrap();

    let error = download_camera_database_archive(&server.origin()).unwrap_err();

    assert!(error.to_string().contains("404"));
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn server_errors_exhaust_the_existing_three_attempt_limit() {
    let server = FakeHikvision::builder()
        .digest(false)
        .replies([
            Reply::http(503, "text/plain", "unavailable"),
            Reply::http(503, "text/plain", "unavailable"),
            Reply::http(503, "text/plain", "unavailable"),
            Reply::http(200, "application/zip", b"archive bytes"),
        ])
        .start()
        .unwrap();

    let error = download_camera_database_archive(&server.origin()).unwrap_err();

    assert!(error.to_string().contains("503"));
    assert_eq!(server.requests().len(), 3);
}
