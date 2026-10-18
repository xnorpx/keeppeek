use crate::{Response, ResponseBody, Server};
use std::{
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{Mutex, mpsc},
    time::{Duration, Instant},
};

struct TrackedBody {
    remaining: usize,
    dropped: mpsc::SyncSender<()>,
}

impl Read for TrackedBody {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let length = buffer.len().min(self.remaining).min(64 * 1024);
        buffer[..length].fill(0);
        self.remaining -= length;
        Ok(length)
    }
}

impl Drop for TrackedBody {
    fn drop(&mut self) {
        let _ = self.dropped.try_send(());
    }
}

fn tracked_response(bytes: usize, dropped: mpsc::SyncSender<()>) -> Response {
    Response {
        status_code: 200,
        headers: Vec::new(),
        data: ResponseBody::from_reader_and_size(
            TrackedBody {
                remaining: bytes,
                dropped,
            },
            bytes,
        ),
        upgrade: None,
    }
}
#[test]
fn abort_response_releases_a_body_while_the_client_does_not_read() {
    const BYTES: usize = 64 * 1024 * 1024;
    let (handle_tx, handle_rx) = mpsc::sync_channel(1);
    let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
    let server = Server::new("127.0.0.1:0", move |request| {
        let handle = request.response_abort_handle().unwrap();
        handle.enable_bounded_writes().unwrap();
        handle_tx.send(handle).unwrap();
        tracked_response(BYTES, dropped_tx.clone())
    })
    .unwrap();
    let mut client = TcpStream::connect(server.server_addr()).unwrap();
    client
        .write_all(b"GET /large HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    server.poll_timeout(Duration::from_millis(10));
    let handle = handle_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(!server.join_timeout(Duration::from_millis(100)));

    let started = Instant::now();
    let deadline = started + Duration::from_secs(1);
    assert!(!handle.is_finished().unwrap());
    let aborted = handle.abort().unwrap();
    let released = dropped_rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .is_ok();
    let elapsed = started.elapsed();
    let finished = server.join_timeout(deadline.saturating_duration_since(Instant::now()));
    let worker_elapsed = started.elapsed();
    // Observe socket termination only after the reader and worker release without client progress.
    let terminated = connection_terminated(&mut client, deadline);
    drop(client);
    assert!(server.join_timeout(Duration::from_secs(2)));
    assert!(
        aborted,
        "unfinished response must retain an active abort handle"
    );
    assert!(
        released,
        "abort must release the response reader without client progress"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "body release took {elapsed:?}"
    );
    assert!(
        finished && worker_elapsed < Duration::from_secs(1),
        "worker release took {worker_elapsed:?}"
    );
    assert!(
        terminated,
        "socket delivery must end within the same one-second deadline"
    );
    assert!(handle.is_finished().unwrap());
    assert!(!handle.abort().unwrap());
}

fn connection_terminated(client: &mut TcpStream, deadline: Instant) -> bool {
    let mut buffer = [0; 64 * 1024];
    let mut bytes = 0;
    while bytes <= 64 * 1024 * 1024 + 128 * 1024 && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || client.set_read_timeout(Some(remaining)).is_err() {
            return false;
        }
        match client.read(&mut buffer) {
            Ok(0) => return true,
            Ok(count) => bytes += count,
            Err(error) => {
                return matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                );
            }
        }
    }
    false
}

fn response_body(client: &mut impl Read) -> Vec<u8> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        assert!(header.len() < 8192);
        let mut byte = [0];
        client.read_exact(&mut byte).unwrap();
        header.push(byte[0]);
    }
    let header = std::str::from_utf8(&header).unwrap();
    let length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("Content-Length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    assert!(length <= 64);
    let mut body = vec![0; length];
    client.read_exact(&mut body).unwrap();
    body
}

#[test]
fn completed_response_handle_cannot_abort_a_later_keep_alive_response() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (handle_tx, handle_rx) = mpsc::sync_channel(1);
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    let server = Server::from_tcp_listener(listener, move |request| {
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
    })
    .unwrap();
    let mut client = TcpStream::connect(address).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .write_all(b"GET /first HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    server.poll_timeout(Duration::from_millis(100));
    let obsolete = handle_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(response_body(&mut client), b"first");
    assert!(server.join_timeout(Duration::from_secs(1)));
    client
        .write_all(b"GET /second HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .unwrap();
    server.poll_timeout(Duration::from_millis(100));
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(obsolete.is_finished().unwrap());
    let aborted = obsolete.abort().unwrap();
    release_tx.send(()).unwrap();
    assert!(
        !aborted,
        "completed request must not close its reused connection"
    );
    assert_eq!(response_body(&mut client), b"second");
    assert!(server.join_timeout(Duration::from_secs(1)));
}

fn wait_for_request_workers<F>(server: &Server<F>, expected: usize) {
    let crate::Executor::Threaded { count } = &server.executor else {
        panic!("test uses the default request executor");
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    while count.load(std::sync::atomic::Ordering::Acquire) != expected && Instant::now() < deadline
    {
        std::thread::yield_now();
    }
    assert_eq!(count.load(std::sync::atomic::Ordering::Acquire), expected);
}

#[test]
fn completed_response_handle_cannot_abort_a_pipelined_response() {
    let (handle_tx, handle_rx) = mpsc::sync_channel(1);
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (first_tx, first_rx) = mpsc::sync_channel(1);
    let (second_tx, second_rx) = mpsc::sync_channel(1);
    let first_rx = Mutex::new(first_rx);
    let second_rx = Mutex::new(second_rx);
    let server = Server::new("127.0.0.1:0", move |request| {
        if request.url() == "/first" {
            let handle = request.response_abort_handle().unwrap();
            handle.enable_bounded_writes().unwrap();
            handle_tx.send(handle).unwrap();
            first_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            Response::text("first")
        } else {
            started_tx.send(()).unwrap();
            second_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            Response::text("second")
        }
    })
    .unwrap();
    let mut client = TcpStream::connect(server.server_addr()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    client
        .write_all(
            concat!(
                "GET /first HTTP/1.1\r\nHost: localhost\r\n\r\n",
                "GET /second HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .as_bytes(),
        )
        .unwrap();
    server.poll_timeout(Duration::from_millis(100));
    let obsolete = handle_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    first_tx.send(()).unwrap();
    assert_eq!(response_body(&mut client), b"first");
    wait_for_request_workers(&server, 1);
    assert!(obsolete.is_finished().unwrap());
    let aborted = obsolete.abort().unwrap();
    second_tx.send(()).unwrap();
    assert!(!aborted);
    assert_eq!(response_body(&mut client), b"second");
    assert!(server.join_timeout(Duration::from_secs(1)));
}

#[test]
fn queued_response_abort_stops_delivery_before_prior_handler_releases() {
    const BYTES: usize = 64 * 1024 * 1024;
    let (handle_tx, handle_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (dropped_tx, dropped_rx) = mpsc::sync_channel(1);
    let release_rx = Mutex::new(release_rx);
    let server = Server::new("127.0.0.1:0", move |request| {
        if request.url() == "/first" {
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
            return Response::text("first");
        }
        let handle = request.response_abort_handle().unwrap();
        handle.enable_bounded_writes().unwrap();
        handle_tx.send(handle).unwrap();
        tracked_response(BYTES, dropped_tx.clone())
    })
    .unwrap();
    let mut client = TcpStream::connect(server.server_addr()).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    client
        .write_all(
            concat!(
                "GET /first HTTP/1.1\r\nHost: localhost\r\n\r\n",
                "GET /second HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
            )
            .as_bytes(),
        )
        .unwrap();
    server.poll_timeout(Duration::from_millis(100));
    let handle = handle_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    let started = Instant::now();
    assert!(!handle.is_finished().unwrap());
    let aborted = handle.abort().unwrap();
    let terminated = match client.read(&mut [0]) {
        Ok(0) => true,
        Err(error) => matches!(
            error.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
        ),
        _ => false,
    };
    let elapsed = started.elapsed();
    let still_queued = matches!(dropped_rx.try_recv(), Err(mpsc::TryRecvError::Empty));
    release_tx.send(()).unwrap();
    let released = dropped_rx.recv_timeout(Duration::from_secs(1)).is_ok();
    let finished = server.join_timeout(Duration::from_secs(1));
    drop(client);
    assert!(server.join_timeout(Duration::from_secs(2)));
    assert!(aborted && terminated);
    assert!(elapsed < Duration::from_secs(1));
    assert!(
        still_queued,
        "socket cancellation does not bypass response sequencing"
    );
    assert!(
        released && finished,
        "release follows completion of the earlier handler"
    );
    assert!(handle.is_finished().unwrap());
}

mod tls;
