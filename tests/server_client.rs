mod harness;

use harness::TestHarness;

#[test]
fn server_ready() {
    let harness = TestHarness::start();
    let ready = harness.client.ready().unwrap();
    assert!(ready.ready);
}

#[test]
fn server_health() {
    let harness = TestHarness::start();
    let health = harness.client.health().unwrap();
    assert_eq!(health.status, "ok");
}

#[test]
fn server_lists_cameras() {
    let harness = TestHarness::start();
    assert!(harness.client.cameras().unwrap().is_empty());
}
