use anyhow::{Context, Result, ensure};
use portable_pty::{Child, CommandBuilder, PtySize, native_pty_system};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::PathBuf,
    sync::mpsc::{Receiver, sync_channel},
    thread,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(20);

struct Recorder {
    child: Box<dyn Child + Send + Sync>,
    directory: PathBuf,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

struct TerminalOutput {
    input: Box<dyn Write + Send>,
    chunks: Receiver<Vec<u8>>,
    #[cfg(windows)]
    query: [u8; 4],
    pending: Vec<u8>,
}

impl TerminalOutput {
    fn new(master: &dyn portable_pty::MasterPty) -> Result<Self> {
        let mut input = master.try_clone_reader()?;
        let terminal_input = master.take_writer()?;
        let (writer, chunks) = sync_channel(16);
        thread::spawn(move || {
            let mut buffer = [0; 2048];
            while let Ok(count) = input.read(&mut buffer) {
                if count == 0 || writer.send(buffer[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            input: terminal_input,
            chunks,
            #[cfg(windows)]
            query: [0; 4],
            pending: Vec::new(),
        })
    }

    fn through(&mut self, marker: &[u8]) -> Result<()> {
        ensure!(!marker.is_empty(), "terminal marker must be nonempty");
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(index) = self
                .pending
                .windows(marker.len())
                .position(|part| part == marker)
            {
                self.pending.drain(..index + marker.len());
                return Ok(());
            }
            let keep_from = self.pending.len().saturating_sub(marker.len() - 1);
            self.pending.drain(..keep_from);
            let chunk = self
                .chunks
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .with_context(|| {
                    format!(
                        "terminal did not emit {:?}",
                        String::from_utf8_lossy(marker)
                    )
                })?;
            #[cfg(windows)]
            for byte in &chunk {
                // ConPTY requests the host terminal's cursor position at attach and resize.
                self.query.rotate_left(1);
                self.query[3] = *byte;
                if self.query == *b"\x1b[6n" {
                    self.input.write_all(b"\x1b[1;1R")?;
                    self.input.flush()?;
                }
            }
            self.pending.extend_from_slice(&chunk);
            ensure!(
                Instant::now() <= deadline,
                "terminal marker deadline elapsed"
            );
        }
    }
}

fn terminal_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn start_recorder(slave: &dyn portable_pty::SlavePty) -> Result<(Recorder, u16)> {
    let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
    let directory = std::env::temp_dir().join(format!("keeppeek-tui-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&directory)?;
    fs::write(
        directory.join("config.toml"),
        format!("host = \"127.0.0.1\"\nport = {port}\n"),
    )?;
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_keeppeek"));
    cmd.cwd(&directory);
    cmd.args(["--config", "config.toml"]);
    // Storage and credentials have OS-specific defaults even with an explicit --config.
    cmd.env("APPDATA", &directory);
    cmd.env("HOME", &directory);
    cmd.env("XDG_CONFIG_HOME", &directory);
    cmd.env("TERM", "xterm-256color");
    cmd.env("NO_COLOR", "1");
    let recorder = Recorder {
        child: slave.spawn_command(cmd)?,
        directory,
    };
    Ok((recorder, port))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Exit {
    Detach,
    Raw,
    #[cfg(unix)]
    BlockedOutput,
}

#[cfg(unix)]
struct PausedOutput(fs::File);

#[cfg(unix)]
impl Drop for PausedOutput {
    fn drop(&mut self) {
        let _ = rustix::termios::tcflow(&self.0, rustix::termios::Action::OOn);
    }
}

fn foreground_lifecycle(exit: Exit) -> Result<()> {
    let pair = native_pty_system().openpty(terminal_size(100, 30))?;
    #[cfg(unix)]
    let initial_flags = pair
        .master
        .get_termios()
        .context("initial terminal modes")?
        .local_flags;
    let mut output = TerminalOutput::new(pair.master.as_ref())?;
    let (mut recorder, port) = start_recorder(pair.slave.as_ref())?;
    drop(pair.slave);

    output.through(b"Uptime")?;
    pair.master.resize(terminal_size(36, 7))?;
    // A differential render can position the cursor between words.
    output.through(b"Enlarge")?;
    output.through(b"details")?;
    pair.master.resize(terminal_size(80, 24))?;
    output.through(b"Uptime")?;
    if exit == Exit::Detach {
        output.input.write_all(b"q")?;
        output.input.flush()?;
        output.through(b"\x1b[?1049l")?;
        #[cfg(unix)]
        assert_eq!(
            pair.master
                .get_termios()
                .context("restored terminal modes")?
                .local_flags,
            initial_flags
        );
        ensure!(
            recorder.child.try_wait()?.is_none(),
            "detaching stopped the recorder"
        );
        let health =
            keeppeek::client::KeepPeekClient::new(&format!("http://127.0.0.1:{port}")).health()?;
        ensure!(
            health.status == "ok",
            "recorder did not remain healthy after detachment"
        );
    }
    #[cfg(unix)]
    let _pause = if exit == Exit::BlockedOutput {
        let device = pair.master.tty_name().context("slave terminal path")?;
        let pause = PausedOutput(fs::OpenOptions::new().write(true).open(device)?);
        rustix::termios::tcflow(&pause.0, rustix::termios::Action::OOff)?;
        pair.master.resize(terminal_size(38, 8))?;
        // Allow a full refresh interval for the renderer to enter its blocked write.
        thread::sleep(Duration::from_millis(1_100));
        Some(pause)
    } else {
        None
    };
    output.input.write_all(b"\x03")?;
    output.input.flush()?;
    if exit == Exit::Raw {
        output.through(b"\x1b[?1049l")?;
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = recorder.child.try_wait()? {
            ensure!(
                status.success(),
                "recorder did not finish cleanly: {status}"
            );
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "Ctrl+C did not stop the recorder"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn detaching_restores_the_terminal_and_leaves_the_recorder_serving() -> Result<()> {
    foreground_lifecycle(Exit::Detach)
}

#[test]
fn raw_terminal_ctrl_c_restores_the_screen_and_stops_the_server() -> Result<()> {
    foreground_lifecycle(Exit::Raw)
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_the_recorder_while_the_pty_cannot_transmit_output() -> Result<()> {
    foreground_lifecycle(Exit::BlockedOutput)
}
