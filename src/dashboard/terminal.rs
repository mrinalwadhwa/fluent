//! Restore the host terminal around an inherited-terminal attachment.
use anyhow::Result;
use crossterm::{event, terminal};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;

pub struct DashboardTerminal {
    pub(super) terminal: Terminal<CrosstermBackend<io::Stdout>>,
    cleanup: Cleanup,
}

#[derive(Default)]
struct Cleanup {
    raw: bool,
    alternate: bool,
    mouse: bool,
    paste: bool,
}
impl Cleanup {
    fn restore(&mut self) {
        let mut stdout = io::stdout();
        if self.mouse {
            let _ = crossterm::execute!(stdout, event::DisableMouseCapture);
            self.mouse = false;
        }
        if self.paste {
            let _ = crossterm::execute!(stdout, event::DisableBracketedPaste);
            self.paste = false;
        }
        if self.alternate {
            let _ = crossterm::execute!(stdout, terminal::LeaveAlternateScreen);
            self.alternate = false;
        }
        if self.raw {
            let _ = terminal::disable_raw_mode();
            self.raw = false;
        }
    }
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.restore();
    }
}

impl DashboardTerminal {
    pub fn open() -> Result<Self> {
        let mut cleanup = Cleanup::default();
        terminal::enable_raw_mode()?;
        cleanup.raw = true;
        // Arm cleanup before writes, including partial setup failures.
        cleanup.alternate = true;
        cleanup.mouse = true;
        cleanup.paste = true;
        let mut stdout = io::stdout();
        crossterm::execute!(
            stdout,
            terminal::EnterAlternateScreen,
            event::EnableMouseCapture,
            event::EnableBracketedPaste
        )?;
        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(stdout))?,
            cleanup,
        })
    }
    pub fn set_mouse_enabled(&mut self, enabled: bool) -> Result<()> {
        if self.cleanup.mouse != enabled {
            if enabled {
                crossterm::execute!(self.terminal.backend_mut(), event::EnableMouseCapture)?;
            } else {
                crossterm::execute!(self.terminal.backend_mut(), event::DisableMouseCapture)?;
            }
            self.cleanup.mouse = enabled;
        }
        Ok(())
    }
    pub fn mouse_enabled(&self) -> bool {
        self.cleanup.mouse
    }
    pub fn size(&self) -> Result<(u16, u16)> {
        Ok(terminal::size()?)
    }
    /// Run synchronously while the dashboard neither reads nor draws. Restore
    /// modes and invalidate Ratatui's buffer even when the attachment fails.
    pub fn handoff<T>(&mut self, attach: impl FnOnce() -> Result<T>) -> Result<T> {
        let mouse = self.mouse_enabled();
        self.terminal.show_cursor()?;
        self.cleanup.restore();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(attach));
        let restored = Self::open().and_then(|mut terminal| {
            terminal.set_mouse_enabled(mouse)?;
            terminal.terminal.clear()?;
            Ok(terminal)
        });
        match restored {
            Ok(terminal) => *self = terminal,
            Err(error) => return Err(error.context("restore dashboard terminal")),
        }
        match result {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }
}
impl Drop for DashboardTerminal {
    fn drop(&mut self) {
        let _ = self.terminal.show_cursor();
    }
}
