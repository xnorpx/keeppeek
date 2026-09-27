//! Local HTTPS protocol fixture with bounded requests and no process-wide trust changes.

use rustls::{ServerConfig, ServerConnection, StreamOwned, pki_types::PrivatePkcs8KeyDer};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub(crate) fn json(value: &serde_json::Value) -> Self {
        Self {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(value).unwrap(),
        }
    }
}

pub struct Fixture {
    pub origin: String,
    pub certificate: ureq::tls::Certificate<'static>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Fixture {
    pub(crate) fn new(handler: impl Fn(Request) -> Response + Send + 'static) -> Self {
        Self::bounded(handler, Duration::from_secs(60), 64)
    }

    pub(crate) fn for_browser(handler: impl Fn(Request) -> Response + Send + 'static) -> Self {
        Self::bounded(handler, Duration::from_secs(600), 4_096)
    }

    fn bounded(
        handler: impl Fn(Request) -> Response + Send + 'static,
        lifetime: Duration,
        request_limit: usize,
    ) -> Self {
        let (tls_config, certificate) = certificates();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!(
            "https://127.0.0.1:{}",
            listener.local_addr().unwrap().port()
        );
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + lifetime;
            let mut accepted = 0;
            while !stopped.load(Ordering::Acquire)
                && Instant::now() < deadline
                && accepted < request_limit
            {
                let (socket, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                };
                accepted += 1;
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let connection = ServerConnection::new(tls_config.clone()).unwrap();
                let mut stream = StreamOwned::new(connection, socket);
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                let response = handler(request);
                write_response(&mut stream, response);
                stream.conn.send_close_notify();
                let _ = stream.flush();
            }
        });
        Self {
            origin,
            certificate,
            stop,
            worker: Some(worker),
        }
    }
}

fn write_response(stream: &mut impl Write, response: Response) {
    let mut headers = format!(
        "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.body.len()
    );
    for (name, value) in response.headers {
        headers.push_str(&format!("{name}: {value}\r\n"));
    }
    headers.push_str("\r\n");
    if stream.write_all(headers.as_bytes()).is_ok() {
        let _ = stream.write_all(&response.body);
        let _ = stream.flush();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("fixture thread failed");
        }
    }
}

fn certificates() -> (Arc<ServerConfig>, ureq::tls::Certificate<'static>) {
    let mut ca = rcgen::CertificateParams::new(vec![]).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "KeepPeek OIDC Fixture CA");
    ca.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::DigitalSignature,
    ];
    let ca_key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).unwrap();
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    let mut leaf = rcgen::CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    leaf.distinguished_name
        .push(rcgen::DnType::CommonName, "KeepPeek OIDC Fixture Server");
    leaf.use_authority_key_identifier_extension = true;
    leaf.key_usages = vec![
        rcgen::KeyUsagePurpose::DigitalSignature,
        rcgen::KeyUsagePurpose::KeyEncipherment,
    ];
    leaf.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_RSA_SHA256).unwrap();
    let cert = leaf.signed_by(&key, &issuer).unwrap();
    // Keep fixture keys in memory: native-tls 0.2.18 reuses Windows key containers across processes.
    let config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )
    .unwrap();
    (
        Arc::new(config),
        ureq::tls::Certificate::from_der(ca_cert.der()).to_owned(),
    )
}

fn read_request(stream: &mut impl Read) -> Option<Request> {
    let mut bytes = Vec::new();
    while bytes.len() < 32_768 && !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).ok()?;
        bytes.push(byte[0]);
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let mut lines = text.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: BTreeMap<_, _> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .map_or(Some(0), |value| value.parse().ok())?;
    if length > 65_536 {
        return None;
    }
    let mut body = vec![0; length];
    stream.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}
