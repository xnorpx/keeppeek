use rouille::{Request, Response, Server};
use std::{net::SocketAddr, sync::mpsc::Sender, thread::JoinHandle};

pub struct CameraWebUiServer {
    address: SocketAddr,
    stop: Sender<()>,
    worker: Option<JoinHandle<()>>,
}

impl CameraWebUiServer {
    pub fn start(address: SocketAddr, title: String) -> anyhow::Result<Self> {
        let server = Server::new(address, move |request| handle_request(request, &title))
            .map_err(|error| anyhow::anyhow!("unable to bind fake camera web UI: {error}"))?;
        let address = server.server_addr();
        let (worker, stop) = server.stoppable();
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub const fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for CameraWebUiServer {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn handle_request(request: &Request, title: &str) -> Response {
    if request.method() != "GET" || request.url() != "/" {
        return Response::empty_404();
    }
    Response::html(format!(
        "<!doctype html><html><head><title>{title}</title></head><body><h1>{title}</h1><p>Fake camera built-in UI</p></body></html>"
    ))
}
