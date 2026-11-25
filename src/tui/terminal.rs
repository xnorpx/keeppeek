use crossterm::{
    cursor::{Hide, Show},
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{self, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{Terminal, TerminalOptions, Viewport, backend::CrosstermBackend, layout::Rect};
use std::{
    io::{self, Stdout},
    panic,
    sync::{
        Once,
        atomic::{AtomicU8, Ordering},
    },
    time::{Duration, Instant},
};

use super::{
    Latest,
    render::{self, Navigation},
};
use crate::{logging::ConsolePause, shutdown::Shutdown};

const RAW: u8 = 1;
const ALTERNATE: u8 = 2;
const HIDDEN: u8 = 4;
static ACQUIRED: AtomicU8 = AtomicU8::new(0);
static PANIC_HOOK: Once = Once::new();

struct ConsoleLease(std::sync::Mutex<Option<ConsolePause>>);

impl ConsoleLease {
    fn acquire() -> io::Result<Self> {
        Ok(Self(std::sync::Mutex::new(Some(ConsolePause::acquire()?))))
    }

    fn release(&self) {
        drop(
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
    }
}

struct Screen {
    modes: u8,
}

impl Screen {
    fn prepare() -> io::Result<Self> {
        let mut guard = Self { modes: 0 };
        PANIC_HOOK.call_once(|| {
            let prior = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                restore_before_exit();
                prior(info);
            }));
        });
        terminal::enable_raw_mode()?;
        guard.remember(RAW);
        Ok(guard)
    }

    fn show(&mut self) -> io::Result<()> {
        // Remember a mode before emitting it so cleanup retries a partially written command.
        self.remember(ALTERNATE);
        execute!(io::stdout(), EnterAlternateScreen)?;
        self.remember(HIDDEN);
        execute!(io::stdout(), Hide)
    }

    fn remember(&mut self, mode: u8) {
        self.modes |= mode;
        ACQUIRED.fetch_or(mode, Ordering::Release);
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if self.modes & RAW != 0 {
            let _ = terminal::disable_raw_mode();
        }
        // Retry even after emergency cleanup if a previously blocked screen write later completed.
        restore_output(io::stdout(), self.modes);
        ACQUIRED.fetch_and(!self.modes, Ordering::AcqRel);
    }
}

fn restore_output(mut output: impl io::Write, modes: u8) {
    if modes & HIDDEN != 0 {
        let _ = execute!(output, Show);
    }
    if modes & ALTERNATE != 0 {
        let _ = execute!(output, LeaveAlternateScreen);
    }
}

pub(super) fn restore_before_exit() {
    let modes = ACQUIRED.load(Ordering::Acquire);
    if modes & RAW != 0 {
        let _ = terminal::disable_raw_mode();
    }
    if modes & (HIDDEN | ALTERNATE) == 0 {
        return;
    }
    #[cfg(windows)]
    const CONSOLE: &str = "CONOUT$";
    #[cfg(not(windows))]
    const CONSOLE: &str = "/dev/tty";
    // Direct access avoids the renderer's stdout lock. Flow control can still block the device.
    let (restored, result) = std::sync::mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("terminal-restore".into())
        .spawn(move || {
            if let Ok(console) = std::fs::OpenOptions::new().write(true).open(CONSOLE) {
                restore_output(console, modes);
            }
            let _ = restored.send(());
        })
        .is_ok()
    {
        let _ = result.recv_timeout(Duration::from_millis(100));
    }
}

fn viewport(columns: u16, rows: u16) -> Rect {
    Rect::new(0, 0, columns.min(500), rows.min(200))
}

enum Input {
    Resize,
    Rows(isize),
    Pages(isize),
}

fn read_input(
    sender: &std::sync::mpsc::SyncSender<Input>,
    stop: &Shutdown,
    server: &Shutdown,
) -> io::Result<()> {
    while !stop.is_cancelled() && !server.is_cancelled() {
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let input = match event::read()? {
            Event::Resize(_, _) => Input::Resize,
            Event::Key(key)
                if key.kind == KeyEventKind::Press || key.kind == KeyEventKind::Repeat =>
            {
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        server.cancel();
                        break;
                    }
                    KeyCode::Char('q') => break,
                    KeyCode::Up => Input::Rows(-1),
                    KeyCode::Down => Input::Rows(1),
                    KeyCode::PageUp => Input::Pages(-1),
                    KeyCode::PageDown => Input::Pages(1),
                    _ => continue,
                }
            }
            _ => continue,
        };
        // Input must still reach q or Ctrl+C when output is stalled and navigation is backed up.
        if matches!(
            sender.try_send(input),
            Err(std::sync::mpsc::TrySendError::Disconnected(_))
        ) {
            break;
        }
    }
    Ok(())
}

fn detach_when_output_stalled(
    wait: impl FnOnce() -> bool,
    resume_console: bool,
    restore: impl FnOnce(),
    release_console: impl FnOnce(),
) -> bool {
    if wait() {
        return false;
    }
    restore();
    if resume_console {
        // Keep console logs paused during server shutdown on stalled output so stderr writes cannot wedge exit.
        release_console();
    }
    true
}

pub(super) fn present(
    latest: &Latest,
    stop: &Shutdown,
    server: &Shutdown,
    start_collector: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let restored = Shutdown::new();
    let console = ConsoleLease::acquire()?;
    std::thread::scope(|scope| {
        let _restoration = super::CancelOnDrop(restored.clone());
        let mut screen = Screen::prepare()?;
        let (sender, receiver) = std::sync::mpsc::sync_channel(32);
        let restoration = &restored;
        let console_lease = &console;
        let input = std::thread::Builder::new()
            .name("terminal-input".into())
            .spawn_scoped(scope, move || {
                let result = read_input(&sender, stop, server);
                drop(sender);
                stop.cancel();
                detach_when_output_stalled(
                    || restoration.wait_timeout(Duration::from_millis(150)),
                    !server.is_cancelled(),
                    restore_before_exit,
                    || console_lease.release(),
                );
                result
            })?;
        let _input_lifetime = super::CancelOnDrop(stop.clone());
        let result = screen.show().and_then(|()| {
            start_collector()?;
            render_until_closed(latest, stop, server, &receiver)
        });
        stop.cancel();
        drop(screen);
        console.release();
        restored.cancel();
        result.and(
            input
                .join()
                .map_err(|_| io::Error::other("terminal input worker panicked"))?,
        )
    })
}

fn render_until_closed(
    latest: &Latest,
    stop: &Shutdown,
    server: &Shutdown,
    receiver: &std::sync::mpsc::Receiver<Input>,
) -> io::Result<()> {
    let (w, h) = terminal::size()?;
    let mut area = viewport(w, h);
    let mut terminal: Terminal<CrosstermBackend<Stdout>> = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(area),
        },
    )?;
    let mut nav = Navigation::default();
    let mut redraw = true;
    let mut last = Instant::now();
    while !stop.is_cancelled() && !server.is_cancelled() {
        let will_draw = redraw || last.elapsed() >= Duration::from_secs(1);
        let data = {
            let mut observation = latest
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if will_draw {
                observation.read_for_render()
            } else {
                observation.clone()
            }
        };
        if will_draw {
            let (w, h) = terminal::size()?;
            let actual = viewport(w, h);
            if area != actual {
                terminal.resize(actual)?;
                area = actual;
            }
            let age = data
                .snapshot
                .as_ref()
                .map_or(Duration::ZERO, |snap| snap.collected.elapsed());
            terminal.draw(|frame| {
                render::draw(
                    frame,
                    data.snapshot.as_deref(),
                    &mut nav,
                    age,
                    data.failed_at.map(|when| when.elapsed()),
                );
            })?;
            last = Instant::now();
            redraw = false;
        }
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(first) => {
                let rows = data.snapshot.as_ref().map_or(0, |snap| snap.rows.len());
                for command in std::iter::once(first).chain(receiver.try_iter().take(31)) {
                    match command {
                        Input::Rows(n) => nav.step(n, rows),
                        Input::Pages(n) => {
                            let step = isize::try_from(nav.page.max(1)).unwrap_or(isize::MAX);
                            nav.step(n.saturating_mul(step), rows);
                        }
                        Input::Resize => {}
                    }
                }
                redraw = true;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversized_terminal_allocation_is_clipped_before_creation() {
        assert_eq!(viewport(u16::MAX, u16::MAX), Rect::new(0, 0, 500, 200));
        assert_eq!(viewport(80, 24), Rect::new(0, 0, 80, 24));
    }

    #[test]
    fn blocked_output_releases_console_pause_only_when_detaching() {
        for (resume_console, expected_events) in
            [(true, vec!["restore", "release"]), (false, vec!["restore"])]
        {
            let console = ConsoleLease::acquire().unwrap();
            assert!(ConsolePause::acquire().is_err());

            let events = std::cell::RefCell::new(Vec::new());
            let detached = detach_when_output_stalled(
                || false,
                resume_console,
                || events.borrow_mut().push("restore"),
                || {
                    events.borrow_mut().push("release");
                    console.release();
                },
            );
            assert!(detached);
            assert_eq!(*events.borrow(), expected_events);
            assert_eq!(ConsolePause::acquire().is_ok(), resume_console);
            console.release();
            assert!(ConsolePause::acquire().is_ok());
        }
    }
}
