use anyhow::{Context, Result, bail, ensure};
use keeppeek::{
    client::KeepPeekClient,
    config,
    logging::initialize_global_logging,
    shutdown::{Restart, Shutdown},
    storage::RecordingCatalog,
    tui::DashboardMode,
};
use portable_pty::{Child, CommandBuilder, PtySize, native_pty_system};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{Receiver, sync_channel},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use test_camera::{TestCamera, TestCameraBuilder};

const CHILD_CONFIG_ENV: &str = "KEEPPEEK_TUI_CONTINUITY_CONFIG";
const CHILD_MODE_ENV: &str = "KEEPPEEK_TUI_CONTINUITY_MODE";
const STREAMS: [&str; 2] = ["front/main", "front/sub"];
const TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
// Allow small GOP timestamp rounding without masking a missed fragment.
const MAX_FRAGMENT_GAP_MS: i64 = 120;

struct RecorderProcess {
    child: Box<dyn Child + Send + Sync>,
    directory: PathBuf,
}

impl Drop for RecorderProcess {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

const RECENT_TAIL_BYTES: usize = 1024;

struct TerminalOutput {
    input: Box<dyn Write + Send>,
    chunks: Receiver<Vec<u8>>,
    #[cfg(windows)]
    query: [u8; 4],
    pending: Vec<u8>,
    recent: Vec<u8>,
}

impl TerminalOutput {
    fn new(master: &dyn portable_pty::MasterPty) -> Result<Self> {
        let mut reader = master.try_clone_reader()?;
        let input = master.take_writer()?;
        let (writer, chunks) = sync_channel(32);
        thread::spawn(move || {
            let mut buffer = [0; 2048];
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 || writer.send(buffer[..count].to_vec()).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            input,
            chunks,
            #[cfg(windows)]
            query: [0; 4],
            pending: Vec::new(),
            recent: Vec::new(),
        })
    }

    fn handle_chunk(&mut self, chunk: &[u8]) -> Result<()> {
        #[cfg(windows)]
        for byte in chunk {
            // ConPTY requests the host cursor position on attach and resize.
            self.query.rotate_left(1);
            self.query[3] = *byte;
            if self.query == *b"\x1b[6n" {
                self.input.write_all(b"\x1b[1;1R")?;
                self.input.flush()?;
            }
        }
        self.pending.extend_from_slice(chunk);
        self.recent.extend_from_slice(chunk);
        if self.recent.len() > RECENT_TAIL_BYTES {
            let drop_prefix = self.recent.len() - RECENT_TAIL_BYTES;
            self.recent.drain(..drop_prefix);
        }
        Ok(())
    }

    fn drain_into(&mut self, captured: Option<&mut Vec<u8>>) -> Result<()> {
        let mut captured = captured;
        while let Ok(chunk) = self.chunks.try_recv() {
            self.handle_chunk(&chunk)?;
            if let Some(target) = captured.as_deref_mut() {
                target.extend_from_slice(&chunk);
            }
            self.pending.clear();
        }
        Ok(())
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
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(
                !remaining.is_zero(),
                "terminal marker {:?} deadline elapsed; recent={:?}",
                String::from_utf8_lossy(marker),
                String::from_utf8_lossy(&self.recent)
            );
            let chunk = self.chunks.recv_timeout(remaining).with_context(|| {
                format!(
                    "terminal did not emit {:?}; recent={:?}",
                    String::from_utf8_lossy(marker),
                    String::from_utf8_lossy(&self.recent)
                )
            })?;
            self.handle_chunk(&chunk)?;
        }
    }
}

const fn terminal_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn unix_now_ms() -> Result<i64> {
    let elapsed = SystemTime::now().duration_since(UNIX_EPOCH)?;
    i64::try_from(elapsed.as_millis()).context("unix timestamp overflow")
}

fn fixture_camera() -> Result<TestCamera> {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/cc-4k-640x360-h264.mp4");
    TestCameraBuilder::rtsp(&source, &source)
        .realtime_start_at(Duration::ZERO)
        .start()
}

fn spawn_pty_recorder(
    slave: &dyn portable_pty::SlavePty,
    camera: &TestCamera,
    mode: &str,
    test_name: &str,
) -> Result<(RecorderProcess, u16)> {
    let port = TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port();
    let temp_root = std::env::temp_dir();
    // Resolve symlinked Unix temp roots such as macOS /var -> /private/var before Root::open checks them.
    #[cfg(unix)]
    let temp_root = fs::canonicalize(&temp_root)
        .with_context(|| format!("unable to canonicalize temp root {}", temp_root.display()))?;
    let directory = temp_root.join(format!("keeppeek-tui-rec-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory)
        .with_context(|| format!("unable to create test directory {}", directory.display()))?;
    let config_path = directory.join("config.toml");
    let config_text = format!(
        "host = \"127.0.0.1\"\nport = {port}\n\n[storage]\nshort_term_secs = 0\nmedium_term_secs = 60\nflush_interval_secs = 1\nwrite_buffer_bytes = 8192\n\n{}recording_mode = \"both\"\n",
        camera.connection().toml_entry("front"),
    );
    fs::write(&config_path, config_text)?;

    let mut cmd = CommandBuilder::new(std::env::current_exe()?);
    cmd.cwd(&directory);
    cmd.args(["--exact", test_name, "--nocapture"]);
    cmd.env("APPDATA", &directory);
    cmd.env("HOME", &directory);
    cmd.env("XDG_CONFIG_HOME", &directory);
    cmd.env("TERM", "xterm-256color");
    cmd.env("NO_COLOR", "1");
    cmd.env(CHILD_CONFIG_ENV, &config_path);
    cmd.env(CHILD_MODE_ENV, mode);

    let process = RecorderProcess {
        child: slave.spawn_command(cmd)?,
        directory,
    };
    Ok((process, port))
}

fn maybe_run_child_server() -> Result<bool> {
    let Ok(config_path) = std::env::var(CHILD_CONFIG_ENV) else {
        return Ok(false);
    };
    let mode_text = std::env::var(CHILD_MODE_ENV).context("missing child dashboard mode")?;
    let mode = match mode_text.as_str() {
        "auto" => DashboardMode::Auto,
        "never" => DashboardMode::Never,
        other => bail!("unsupported child dashboard mode: {other}"),
    };
    let source_config = PathBuf::from(config_path);
    let target_config = config::config_path();
    if let Some(parent) = target_config.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "unable to create child configuration directory {}",
                parent.display()
            )
        })?;
    }
    fs::copy(&source_config, &target_config).with_context(|| {
        format!(
            "unable to copy child configuration from {} to {}",
            source_config.display(),
            target_config.display()
        )
    })?;
    let (cfg, config_path) = config::load().with_context(|| {
        format!(
            "unable to load child configuration from {}",
            target_config.display()
        )
    })?;
    let logging = initialize_global_logging(&config_path).with_context(|| {
        format!(
            "unable to initialize child logging for {}",
            config_path.display()
        )
    })?;
    let shutdown = Shutdown::new();
    let restart = Restart::default();
    let signal_shutdown = shutdown.clone();
    #[cfg(windows)]
    enable_windows_ctrl_c();
    ctrlc::set_handler(move || {
        signal_shutdown.cancel();
    })
    .context("unable to install Ctrl+C handler in continuity child")?;
    let restarted =
        keeppeek::app::run_with_dashboard(cfg, &config_path, logging, shutdown, restart, mode)
            .with_context(|| {
                format!(
                    "continuity child server failed for {}",
                    config_path.display()
                )
            })?;
    ensure!(
        !restarted,
        "continuity child requested an unexpected restart"
    );
    Ok(true)
}

#[cfg(windows)]
fn enable_windows_ctrl_c() {
    // SAFETY: Passing None with false clears only the inherited IGNORE_CTRL_C flag.
    let _ = unsafe { windows::Win32::System::Console::SetConsoleCtrlHandler(None, false) };
}

fn find_catalog_db(root: &Path) -> Result<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("db")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("catalog-") || name == "recordings.db")
            {
                return Ok(path);
            }
        }
    }
    bail!(
        "recording catalog database was not created under {}",
        root.display()
    )
}

fn mp4_sizes(root: &Path) -> Result<BTreeMap<PathBuf, u64>> {
    let mut sizes = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if !dir.exists() {
            continue;
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("mp4") {
                sizes.insert(path.clone(), fs::metadata(&path)?.len());
            }
        }
    }
    Ok(sizes)
}

fn wait_for_recording_growth(
    recordings_dir: &Path,
    output: &mut TerminalOutput,
    captured: Option<&mut Vec<u8>>,
    required_increases: usize,
) -> Result<BTreeMap<PathBuf, u64>> {
    let deadline = Instant::now() + TIMEOUT;
    let mut previous: BTreeMap<PathBuf, u64> = BTreeMap::new();
    let mut increases: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut captured = captured;
    loop {
        output.drain_into(captured.as_deref_mut())?;
        let current = mp4_sizes(recordings_dir)?;
        for (path, size) in &current {
            if *size == 0 {
                continue;
            }
            let last = previous.get(path).copied().unwrap_or(0);
            if *size > last {
                previous.insert(path.clone(), *size);
                *increases.entry(path.clone()).or_insert(0) += 1;
            }
        }
        let progressing = increases
            .values()
            .filter(|count| **count >= required_increases)
            .count();
        if progressing >= STREAMS.len() {
            return Ok(current);
        }
        ensure!(
            Instant::now() < deadline,
            "recordings did not advance {required_increases} times across both streams: {increases:?}"
        );
        thread::sleep(POLL_INTERVAL);
    }
}

fn stop_with_ctrl_c(
    recorder: &mut RecorderProcess,
    output: &mut TerminalOutput,
    expect_alt_screen_exit: bool,
) -> Result<()> {
    output.input.write_all(b"\x03")?;
    output.input.flush()?;
    if expect_alt_screen_exit {
        output.through(b"\x1b[?1049l")?;
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        output.drain_into(None)?;
        if let Some(status) = recorder.child.try_wait()? {
            ensure!(status.success(), "recorder exited unsuccessfully: {status}");
            return Ok(());
        }
        ensure!(Instant::now() < deadline, "Ctrl+C did not stop recorder");
        thread::sleep(POLL_INTERVAL);
    }
}

fn decode_mp4_frames(path: &Path) -> Result<usize> {
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-xerror", "-i"])
        .arg(path)
        .args(["-f", "framehash", "-"])
        .output()
        .context("unable to invoke ffmpeg for MP4 verification")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure!(
        output.status.success() && stderr.trim().is_empty(),
        "ffmpeg failed decoding {}: {stderr}",
        path.display()
    );
    let frames = std::str::from_utf8(&output.stdout)?
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .count();
    Ok(frames)
}

fn verify_stream_recordings(
    catalog_path: &Path,
    min_fragments: usize,
    boundary_unix_ms: Option<i64>,
) -> Result<()> {
    let catalog = RecordingCatalog::open(catalog_path)?;
    let handle = catalog.handle();
    for stream in STREAMS {
        let fragments = handle.media_fragments_in_range(stream, 0, i64::MAX)?;
        ensure!(
            fragments.len() >= min_fragments,
            "{stream} recorded only {} fragments, expected at least {min_fragments}",
            fragments.len()
        );
        if let Some(boundary) = boundary_unix_ms {
            let has_before = fragments
                .iter()
                .any(|fragment| fragment.start_ms < boundary);
            let has_after = fragments.iter().any(|fragment| {
                fragment.start_ms + i64::try_from(fragment.duration_ms).unwrap_or(0) > boundary
            });
            ensure!(
                has_before && has_after,
                "{stream} did not record continuously across detach boundary {boundary}"
            );
        }
        verify_stream_fragments(stream, &fragments)?;
    }
    catalog.shutdown();
    Ok(())
}

fn verify_stream_fragments(
    stream: &str,
    fragments: &[keeppeek::storage::catalog::CatalogMediaFragment],
) -> Result<()> {
    let mut files = BTreeMap::<String, u64>::new();
    for pair in fragments.windows(2) {
        let prev = &pair[0];
        let next = &pair[1];
        let prev_end = prev
            .start_ms
            .checked_add(i64::try_from(prev.duration_ms)?)
            .context("fragment end timestamp overflow")?;
        let gap_ms = next.start_ms - prev_end;
        ensure!(
            gap_ms.abs() <= MAX_FRAGMENT_GAP_MS,
            "{stream} has a {gap_ms} ms recording gap between {} and {}",
            prev.start_ms,
            next.start_ms
        );
    }
    for fragment in fragments {
        ensure!(
            fragment.init_len > 0 && fragment.duration_ms > 0 && fragment.byte_len > 0,
            "{stream} has an invalid catalog fragment"
        );
        let size = match files.get(&fragment.path) {
            Some(size) => *size,
            None => {
                let len = fs::metadata(&fragment.path)?.len();
                files.insert(fragment.path.clone(), len);
                len
            }
        };
        let end = fragment
            .byte_offset
            .checked_add(fragment.byte_len)
            .context("fragment byte offset overflow")?;
        ensure!(
            end <= size,
            "{stream} fragment exceeds finalized file length ({end} > {size})"
        );
    }
    for path in files.keys() {
        verify_mp4_file(stream, Path::new(path))?;
    }
    Ok(())
}

fn verify_mp4_file(stream: &str, path: &Path) -> Result<()> {
    let reader = mp4::read_mp4(File::open(path)?)?;
    ensure!(
        reader.is_fragmented(),
        "{stream} recording {} is not fragmented MP4",
        path.display()
    );
    ensure!(
        reader.tracks().len() == 1,
        "{stream} recording has unexpected track count {}",
        reader.tracks().len()
    );
    let sample_count = usize::try_from(reader.sample_count(1)?)?;
    ensure!(
        sample_count >= 25,
        "{stream} recorded only {sample_count} samples in {}",
        path.display()
    );
    let decoded_frames = decode_mp4_frames(path)?;
    ensure!(
        decoded_frames == sample_count,
        "{stream} decoded {decoded_frames} frames but MP4 track reports {sample_count}"
    );
    Ok(())
}

#[test]
fn dashboard_on_detach_and_ctrl_c_preserve_decodable_continuous_recordings() -> Result<()> {
    if maybe_run_child_server()? {
        return Ok(());
    }
    let camera = fixture_camera()?;
    let pair = native_pty_system().openpty(terminal_size(100, 30))?;
    let mut output = TerminalOutput::new(pair.master.as_ref())?;
    let (mut recorder, port) = spawn_pty_recorder(
        pair.slave.as_ref(),
        &camera,
        "auto",
        "dashboard_on_detach_and_ctrl_c_preserve_decodable_continuous_recordings",
    )?;
    drop(pair.slave);

    output.through(b"Uptime")?;
    output.through(b"front")?;
    let _ = wait_for_recording_growth(&recorder.directory, &mut output, None, 3)?;

    output.input.write_all(b"\x1b[B\x1b[A")?;
    output.input.flush()?;
    pair.master.resize(terminal_size(80, 24))?;
    output.through(b"Uptime")?;
    pair.master.resize(terminal_size(100, 30))?;
    output.through(b"Uptime")?;

    let detach_unix_ms = unix_now_ms()?;
    output.input.write_all(b"q")?;
    output.input.flush()?;
    output.through(b"\x1b[?1049l")?;
    ensure!(
        recorder.child.try_wait()?.is_none(),
        "detaching with q stopped the recorder"
    );
    let health = KeepPeekClient::new(&format!("http://127.0.0.1:{port}")).health()?;
    ensure!(
        health.status == "ok",
        "recorder did not remain healthy after q detach"
    );

    let _ = wait_for_recording_growth(&recorder.directory, &mut output, None, 3)?;
    stop_with_ctrl_c(&mut recorder, &mut output, false)?;
    let catalog_path = find_catalog_db(&recorder.directory)?;
    verify_stream_recordings(&catalog_path, 4, Some(detach_unix_ms))
}

#[test]
fn dashboard_off_and_direct_ctrl_c_finalize_decodable_continuous_recordings() -> Result<()> {
    if maybe_run_child_server()? {
        return Ok(());
    }
    let camera = fixture_camera()?;

    for mode in ["never", "auto"] {
        let pair = native_pty_system().openpty(terminal_size(100, 30))?;
        let mut output = TerminalOutput::new(pair.master.as_ref())?;
        let (mut recorder, port) = spawn_pty_recorder(
            pair.slave.as_ref(),
            &camera,
            mode,
            "dashboard_off_and_direct_ctrl_c_finalize_decodable_continuous_recordings",
        )?;
        drop(pair.slave);

        let mut captured = Vec::new();
        if mode == "auto" {
            output.through(b"Uptime")?;
            output.through(b"front")?;
        } else {
            let client = KeepPeekClient::new(&format!("http://127.0.0.1:{port}"));
            let deadline = Instant::now() + TIMEOUT;
            loop {
                output.drain_into(Some(&mut captured))?;
                if client.health().is_ok_and(|health| health.status == "ok") {
                    break;
                }
                if let Some(status) = recorder.child.try_wait()? {
                    output.drain_into(Some(&mut captured))?;
                    bail!(
                        "--tui never child exited early ({status}): {}",
                        String::from_utf8_lossy(&captured)
                    );
                }
                ensure!(
                    Instant::now() < deadline,
                    "--tui never server never became healthy; output={}",
                    String::from_utf8_lossy(&captured)
                );
                thread::sleep(POLL_INTERVAL);
            }
        }
        let capture_target = (mode == "never").then_some(&mut captured);
        let _ = wait_for_recording_growth(&recorder.directory, &mut output, capture_target, 3)?;
        if mode == "never" {
            output.drain_into(Some(&mut captured))?;
            ensure!(
                !captured.windows(8).any(|part| part == b"\x1b[?1049h"),
                "--tui never entered the alternate screen while recording"
            );
        }
        stop_with_ctrl_c(&mut recorder, &mut output, mode == "auto")?;
        let catalog_path = find_catalog_db(&recorder.directory)?;
        verify_stream_recordings(&catalog_path, 3, None)?;
    }
    Ok(())
}
