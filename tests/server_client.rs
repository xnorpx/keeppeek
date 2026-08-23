mod harness;

use harness::TestHarness;
use keeppeek::test_support::TestCameraCatalog;

#[test]
fn server_health() {
    let harness = TestHarness::start();
    let health = harness.client.health().unwrap();
    assert_eq!(health.status, "ok");
}

#[test]
fn server_health_with_test_camera_catalog() {
    let harness = TestHarness::start_with_test_camera_catalog(TestCameraCatalog::standard());
    let health = harness.client.health().unwrap();
    assert_eq!(health.status, "ok");
}
