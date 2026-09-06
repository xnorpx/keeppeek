use std::io;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{Builder, CONNECTIONS_MAX, FakeOnvif, IO_TIMEOUT, Shared, State, wire};

pub(super) fn start(mut config: Builder) -> anyhow::Result<FakeOnvif> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    listener.set_nonblocking(true)?;
    let address = listener.local_addr()?;
    let state = State {
        notifications: std::mem::take(&mut config.notifications).into(),
        ..State::default()
    };
    let shared = Arc::new(Shared {
        state: Mutex::new(state),
        changed: Condvar::new(),
        config,
        address,
        started: Instant::now(),
    });
    let worker = Arc::clone(&shared);
    let handle = std::thread::Builder::new()
        .name("fake-onvif".to_owned())
        .spawn(move || serve(&listener, &worker))?;
    Ok(FakeOnvif {
        address,
        shared,
        handle: Some(handle),
    })
}

fn serve(listener: &TcpListener, shared: &Arc<Shared>) {
    let mut workers: Vec<JoinHandle<()>> = Vec::with_capacity(CONNECTIONS_MAX);
    while !shared.stopped() {
        for index in (0..workers.len()).rev() {
            if workers[index].is_finished() {
                workers
                    .remove(index)
                    .join()
                    .expect("fake ONVIF worker panicked");
            }
        }
        match listener.accept() {
            Ok((socket, _)) if workers.len() < CONNECTIONS_MAX => {
                if let Some(worker) = spawn(socket, shared) {
                    workers.push(worker);
                }
            }
            Ok((socket, _)) => drop(socket),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                shared.wait(Duration::from_millis(5));
            }
            Err(_) => {
                shared.state.lock().unwrap().transport_errors += 1;
                shared.stop();
            }
        }
    }
    for worker in workers {
        worker.join().expect("fake ONVIF worker panicked");
    }
}

fn spawn(socket: TcpStream, shared: &Arc<Shared>) -> Option<JoinHandle<()>> {
    let id = match register(shared, &socket) {
        Ok(Some(id)) => id,
        Ok(None) => return None,
        Err(_) => {
            shared.state.lock().unwrap().transport_errors += 1;
            return None;
        }
    };
    let worker = Arc::clone(shared);
    std::thread::Builder::new()
        .name("fake-onvif-http".to_owned())
        .spawn(move || {
            let failed = wire::handle(socket, &worker).is_err();
            let mut state = worker.state.lock().unwrap();
            state.sockets.remove(&id);
            state.transport_errors += u64::from(failed && !state.stopped);
            worker.changed.notify_all();
        })
        .map_or_else(
            |_| {
                let mut state = shared.state.lock().unwrap();
                state.sockets.remove(&id);
                state.transport_errors += 1;
                None
            },
            Some,
        )
}

fn register(shared: &Shared, socket: &TcpStream) -> io::Result<Option<u64>> {
    socket.set_nonblocking(false)?;
    socket.set_read_timeout(Some(IO_TIMEOUT))?;
    socket.set_write_timeout(Some(IO_TIMEOUT))?;
    socket.set_nodelay(true)?;
    let socket = socket.try_clone()?;
    let mut state = shared.state.lock().unwrap();
    if state.stopped {
        return Ok(None);
    }
    assert!(
        state.sockets.len() < CONNECTIONS_MAX,
        "fake ONVIF socket capacity violated"
    );
    let id = state.next_socket;
    state.next_socket = id
        .checked_add(1)
        .expect("fake ONVIF socket identifiers exhausted");
    assert!(
        state.sockets.insert(id, socket).is_none(),
        "fake ONVIF socket identifier reused"
    );
    shared.changed.notify_all();
    Ok(Some(id))
}
