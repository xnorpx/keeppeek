mod harness;

use harness::TestHarness;

#[test]
fn server_health() {
    let harness = TestHarness::start();
    let health = harness.client.health().unwrap();
    assert_eq!(health.status, "ok");
}
