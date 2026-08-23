use keeppeek::{
    client::KeepPeekClient,
    runtime::Router,
    server::{ServerState, serve_with_state_on_listener},
    shutdown::Shutdown,
    test_support::TestCameraCatalog,
};
use std::{net::TcpListener, thread::JoinHandle};

pub struct TestHarness {
    pub client: KeepPeekClient,
    shutdown: Shutdown,
    server: Option<JoinHandle<()>>,
    router: Option<JoinHandle<()>>,
}

impl TestHarness {
    pub fn start() -> Self {
        Self::start_with_state(ServerState::for_test())
    }

    pub fn start_with_test_camera_catalog(catalog: TestCameraCatalog) -> Self {
        Self::start_with_state(ServerState::for_test().with_test_camera_catalog(catalog))
    }

    fn start_with_state(state: ServerState) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let shutdown = Shutdown::new();

        let (mut router, router_tx) = Router::new().unwrap();
        let router_shutdown = shutdown.clone();
        let router_thread = std::thread::spawn(move || {
            while !router_shutdown.is_cancelled() && !router.is_shutting_down() {
                router
                    .wait_and_drain(Some(std::time::Duration::from_millis(100)))
                    .unwrap();
            }
        });

        let server_shutdown = shutdown.clone();
        let server = std::thread::spawn(move || {
            serve_with_state_on_listener(listener, server_shutdown, router_tx, state).unwrap();
        });

        let client = KeepPeekClient::new(&format!("http://{addr}"));

        let health = client.health().unwrap();
        assert_eq!(health.status, "ok");

        Self {
            client,
            shutdown,
            server: Some(server),
            router: Some(router_thread),
        }
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.server
            .take()
            .expect("test server handle is present")
            .join()
            .expect("test server thread did not panic");
        self.router
            .take()
            .expect("test router handle is present")
            .join()
            .expect("test router thread did not panic");
    }
}
