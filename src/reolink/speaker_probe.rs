//! Read-only Baichuan discovery. A camera may reject this query when no external stream is open.
//! A short separate TCP connection keeps that rejection isolated from live media and recording.

use reo_proto::{
    EncryptionMode, LoginParams,
    session::{BcSession, BcSessionConfig, Command, Event, Input, Output},
};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::{Duration, Instant},
};

const DEADLINE: Duration = Duration::from_secs(3);
const BUFFER_BYTES: usize = 16 * 1024;

pub(crate) fn compatible(
    address: SocketAddr,
    username: &str,
    password: &str,
    channel: u8,
) -> anyhow::Result<bool> {
    let now = Instant::now();
    let deadline = now + DEADLINE;
    let mut socket = TcpStream::connect_timeout(&address, DEADLINE)?;
    let mut session = BcSession::new(
        BcSessionConfig {
            tcp_recv_buf_size: BUFFER_BYTES,
            tcp_send_buf_size: BUFFER_BYTES,
            keepalive_channel: channel,
            ..BcSessionConfig::default_client()
        },
        now,
    );
    session.handle_input(Input::Command(Command::Login(LoginParams::new(
        username,
        password,
        EncryptionMode::BcEncrypt,
    ))))?;
    let mut buffer = vec![0; BUFFER_BYTES];
    let mut received = 0;
    let mut queried = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        match session.poll_output(&mut buffer)? {
            Output::TcpSend { data } => {
                socket.set_write_timeout(Some(remaining))?;
                socket.write_all(data)?;
            }
            Output::Event(Event::LoggedIn(_)) if !queried => {
                session.handle_input(Input::Command(Command::ReadTalkAbility { channel }))?;
                queried = true;
            }
            Output::Event(Event::ReadTalkAbility(ability)) if queried => {
                return Ok(super::speaker::select_profile(*ability, channel).is_ok());
            }
            Output::Event(
                Event::LoginFailed(_) | Event::CommandFailed { .. } | Event::SessionTimeout,
            ) => {
                return Ok(false);
            }
            Output::Event(_) => {}
            Output::Timeout(_) => {
                // A metadata peer gets one login handshake and one small ability document.
                if received == BUFFER_BYTES * 4 {
                    return Ok(false);
                }
                let limit = BUFFER_BYTES.min(BUFFER_BYTES * 4 - received);
                socket.set_read_timeout(Some(remaining))?;
                let size = socket.read(&mut buffer[..limit])?;
                if size == 0 {
                    return Ok(false);
                }
                received += size;
                session.handle_input(Input::TcpData(Instant::now(), &buffer[..size]))?;
            }
        }
    }
}
