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

struct Screen {
    _console: ConsolePause,
    modes: u8,
}

impl Screen {
    fn enter() -> io::Result<Self> {
        let mut guard = Self {
            _console: ConsolePause::acquire()?,
            modes: 0,
        };
        PANIC_HOOK.call_once(|| {
            let prior = panic::take_hook();
            panic::set_hook(Box::new(move |info| {
                restore_before_exit();
                prior(info);
            }));
        });
        terminal::enable_raw_mode()?;
        guard.remember(RAW);
        // Record each output command first so a partially written command gets a cleanup attempt.
        guard.remember(ALTERNATE);
        execute!(io::stdout(), EnterAlternateScreen)?;
        guard.remember(HIDDEN);
        execute!(io::stdout(), Hide)?;
        Ok(guard)
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

pub(super) fn present(
    latest: &Latest,
    stop: &Shutdown,
    server: &Shutdown,
    start_collector: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    let (w, h) = terminal::size()?;
    let _screen = Screen::enter()?;
    let mut terminal: Terminal<CrosstermBackend<Stdout>> = Terminal::with_options(
        CrosstermBackend::new(io::stdout()),
        TerminalOptions {
            viewport: Viewport::Fixed(viewport(w, h)),
        },
    )?;
    start_collector()?;
    let mut nav = Navigation::default();
    let mut redraw = true;
    let mut last = Instant::now();
    while !stop.is_cancelled() && !server.is_cancelled() {
        let data = latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if redraw || last.elapsed() >= Duration::from_secs(1) {
            let age = data
                .as_ref()
                .map_or(Duration::ZERO, |snap| snap.collected.elapsed());
            terminal.draw(|frame| render::draw(frame, data.as_deref(), &mut nav, age))?;
            last = Instant::now();
            redraw = false;
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        // Coalesce input bursts without delaying shutdown or the next snapshot indefinitely.
        for _ in 0..32 {
            match event::read()? {
                Event::Resize(w, h) => {
                    terminal.resize(viewport(w, h))?;
                    redraw = true;
                }
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let count = data.as_ref().map_or(0, |snap| snap.rows.len());
                    match key.code {
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            server.cancel();
                            return Ok(());
                        }
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Up => nav.step(-1, count),
                        KeyCode::Down => nav.step(1, count),
                        KeyCode::PageUp => nav.step(-(nav.page as isize), count),
                        KeyCode::PageDown => nav.step(nav.page as isize, count),
                        _ => {}
                    }
                    redraw = true;
                }
                _ => {}
            }
            if !event::poll(Duration::ZERO)? {
                break;
            }
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
}
