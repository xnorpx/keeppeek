use super::*;
use native_tls::{TlsConnector, TlsStream};
use std::net::Shutdown;

fn server<F>(handler: F) -> Server<F>
where
    F: Send + Sync + 'static + Fn(&crate::Request) -> Response,
{
    // This certificate and private key are public test fixtures.
    Server::new_ssl(
        "127.0.0.1:0",
        handler,
        include_bytes!("test_certificate.pem").to_vec(),
        include_bytes!("test_private_key.pem").to_vec(),
    )
    .unwrap()
}

fn client<F>(server: &Server<F>) -> TlsStream<TcpStream>
where
    F: Send + Sync + 'static + Fn(&crate::Request) -> Response,
{
    let socket = TcpStream::connect(server.server_addr()).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    TlsConnector::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap()
        .connect("localhost", socket)
        .unwrap()
}

fn terminal(client: &mut TlsStream<TcpStream>, deadline: Instant) -> bool {
    let mut buffer = [0; 64 * 1024];
    let mut bytes = 0;
    while bytes <= 64 * 1024 * 1024 + 128 * 1024 && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || client.get_ref().set_read_timeout(Some(remaining)).is_err() {
            return false;
        }
        match client.read(&mut buffer) {
            Ok(0) => return true,
            Ok(count) => bytes += count,
            Err(error) => {
                return !matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::Interrupted
                );
            }
        }
    }
    false
}

#[test]
fn native_tls_request_reports_secure_transport() {
    let server = server(|request| Response::text(request.is_secure().to_string()));
    let mut client = client(&server);
    client
        .write_all(b"GET /secure HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    server.poll_timeout(Duration::from_millis(10));
    let body = response_body(&mut client);
    let finished = server.join_timeout(Duration::from_secs(1));
    let _ = client.get_ref().shutdown(Shutdown::Both);
    drop(client);
    assert!(server.join_timeout(Duration::from_secs(2)));
    assert!(finished);
    assert_eq!(body, b"true");
}

#[test]
fn native_tls_abort_releases_slow_reader_and_worker_within_one_deadline() {
    let (handle_tx, handle_rx) = mpsc::sync_channel(1);
    let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
    let server = server(move |request| {
        let handle = request.response_abort_handle().unwrap();
        handle.enable_bounded_writes().unwrap();
        handle_tx.send(handle).unwrap();
        tracked_response(64 * 1024 * 1024, dropped_tx.clone())
    });
    let mut client = client(&server);
    client
        .write_all(b"GET /large HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    server.poll_timeout(Duration::from_millis(10));
    let handle = handle_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(!server.join_timeout(Duration::from_millis(100)));
    let started = Instant::now();
    let deadline = started + Duration::from_secs(1);
    let aborted = handle.abort().unwrap();
    let released = dropped_rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .is_ok();
    let body_elapsed = started.elapsed();
    let finished = server.join_timeout(deadline.saturating_duration_since(Instant::now()));
    let worker_elapsed = started.elapsed();
    let terminated = terminal(&mut client, deadline);
    let socket_elapsed = started.elapsed();
    let _ = client.get_ref().shutdown(Shutdown::Both);
    drop(client);
    assert!(server.join_timeout(Duration::from_secs(2)));
    assert!(aborted);
    assert!(
        released && body_elapsed < Duration::from_secs(1),
        "TLS body release took {body_elapsed:?}"
    );
    assert!(
        finished && worker_elapsed < Duration::from_secs(1),
        "TLS worker release took {worker_elapsed:?}"
    );
    assert!(
        terminated && socket_elapsed < Duration::from_secs(1),
        "TLS socket termination took {socket_elapsed:?}"
    );
    assert!(handle.is_finished().unwrap());
    assert!(!handle.abort().unwrap());
    eprintln!(
        "native TLS cancellation: body={body_elapsed:?}, worker={worker_elapsed:?}, socket={socket_elapsed:?}"
    );
}

#[test]
fn native_tls_completed_handle_preserves_the_next_normal_keepalive_response() {
    let (handle_tx, handle_rx) = mpsc::sync_channel(1);
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    let server = server(move |request| {
        if request.url() == "/first" {
            let handle = request.response_abort_handle().unwrap();
            handle.enable_bounded_writes().unwrap();
            handle_tx.send(handle).unwrap();
            Response::text("first")
        } else {
            started_tx.send(()).unwrap();
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            Response::text("second")
        }
    });
    let mut client = client(&server);
    client
        .write_all(b"GET /first HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    server.poll_timeout(Duration::from_millis(10));
    let obsolete = handle_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(response_body(&mut client), b"first");
    assert!(server.join_timeout(Duration::from_secs(1)));
    client
        .write_all(b"GET /second HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    server.poll_timeout(Duration::from_millis(10));
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let finished = obsolete.is_finished().unwrap();
    let aborted = obsolete.abort().unwrap();
    release_tx.send(()).unwrap();
    let body = response_body(&mut client);
    let joined = server.join_timeout(Duration::from_secs(1));
    let _ = client.get_ref().shutdown(Shutdown::Both);
    drop(client);
    assert!(server.join_timeout(Duration::from_secs(2)));
    assert!(finished && !aborted);
    assert_eq!(body, b"second");
    assert!(joined);
}
