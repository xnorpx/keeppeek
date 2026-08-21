use keeppeek::{
    client::KeepPeekClient, runtime::Router, server::serve_on_listener, shutdown::Shutdown,
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
            serve_on_listener(listener, server_shutdown, router_tx).unwrap();
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
