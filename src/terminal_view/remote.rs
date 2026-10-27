use super::{Frame, Screen, releases_focus};
use crate::dashboard_sources::{OpenSsh, RemoteConnection, SshSource};
use crate::terminal_sessions::TerminalDescriptor;
use anyhow::{Context, Result, ensure};
use crossterm::event::Event;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

// Permit a slow display to resume, but never let it own local teardown.
const RESTORE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(crate) struct DisplayRestoreFailed;

impl std::fmt::Display for DisplayRestoreFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("terminal display unavailable; dashboard closed without repaint")
    }
}

impl std::error::Error for DisplayRestoreFailed {}

pub struct SshTerminal {
    ssh: OpenSsh,
    source: SshSource,
    descriptor: TerminalDescriptor,
    connection: Option<RemoteConnection>,
    screen: Screen,
    frame: Frame,
    size: (u16, u16),
    focused: bool,
    ended: bool,
    diagnostic: Option<String>,
}
impl SshTerminal {
    pub fn observe(
        ssh: OpenSsh,
        source: SshSource,
        descriptor: TerminalDescriptor,
        columns: u16,
        rows: u16,
    ) -> Result<Self> {
        let mut screen = Screen::new(columns, rows)?;
        let frame = screen.snapshot()?;
        let mut terminal = Self {
            ssh,
            source,
            descriptor,
            connection: None,
            screen,
            frame,
            size: (columns, rows),
            focused: false,
            ended: false,
            diagnostic: None,
        };
        terminal.reconnect()?;
        Ok(terminal)
    }
    pub fn reconnect(&mut self) -> Result<()> {
        self.connection.take();
        self.focused = false;
        ensure!(
            !self.ended,
            "remote invocation ended; select another worker"
        );
        // Never replace this saved descriptor with a refreshed task's generation.
        let connection = RemoteConnection::open(
            self.ssh.clone(),
            self.source.clone(),
            self.descriptor.clone(),
            self.size.0,
            self.size.1,
        )
        .map_err(|error| {
            self.diagnostic = Some(format!("{error:#}"));
            error
        })?;
        self.screen = Screen::new(self.size.0, self.size.1)?;
        self.connection = Some(connection);
        self.diagnostic = None;
        Ok(())
    }
    fn connection(&mut self) -> Result<&mut RemoteConnection> {
        self.connection
            .as_mut()
            .context("Disconnected · t reconnect")
    }
    pub fn identity(&self) -> String {
        format!(
            "{} ({}) / {}",
            self.source.name,
            self.source.host,
            super::identity(&self.descriptor)
        )
    }
    pub fn descriptor(&self) -> &TerminalDescriptor {
        &self.descriptor
    }
    pub fn frame(&self) -> &Frame {
        &self.frame
    }
    pub fn focused(&self) -> bool {
        self.focused
    }
    pub fn ended(&self) -> bool {
        self.ended
    }
    pub fn has_control(&self) -> bool {
        self.connection.as_ref().is_some_and(|c| c.has_control())
    }
    pub fn release_focus(&mut self) {
        self.focused = false;
    }
    pub fn state_label(&self) -> String {
        if self.ended {
            "Ended · input unavailable".into()
        } else if let Some(reason) = &self.diagnostic {
            format!("Disconnected · {reason} · t reconnect")
        } else if self.connection.is_none() {
            "Disconnected · t reconnect".into()
        } else if self.has_control() && self.focused {
            "Control · Ctrl-] navigate".into()
        } else if self.has_control() {
            "Control · navigation".into()
        } else {
            "Observe · i control".into()
        }
    }
    pub fn acquire_control(&mut self) -> Result<()> {
        self.connection()?.acquire_control()?;
        self.focused = true;
        Ok(())
    }
    pub fn observe_only(&mut self) -> Result<()> {
        self.focused = false;
        self.connection()?.observe_only()
    }
    pub fn send(&mut self, event: Event) -> Result<()> {
        ensure!(
            self.focused && self.has_control(),
            "terminal input requires focused Control"
        );
        if releases_focus(&event) {
            self.release_focus();
            return Ok(());
        }
        if let super::Input::Bytes(bytes) = super::client::tmux_input(&mut self.screen, event)? {
            self.connection()?.input(&bytes)?;
        }
        Ok(())
    }
    pub fn send_literal_escape(&mut self) -> Result<()> {
        self.connection()?.input(&[29])?;
        self.focused = true;
        Ok(())
    }
    pub fn resize(&mut self, columns: u16, rows: u16) -> Result<()> {
        if self.size == (columns, rows) || self.ended {
            return Ok(());
        }
        self.screen.resize(columns, rows)?;
        self.size = (columns, rows);
        self.connection()?.resize(columns, rows)
    }
    pub fn refresh(&mut self) -> bool {
        let Some(connection) = &mut self.connection else {
            return false;
        };
        let mut result = connection.poll();
        let bytes = connection.take_output();
        if !bytes.is_empty() {
            self.screen.write(&bytes);
            let replies = self.screen.replies();
            if result.is_ok() && !replies.is_empty() {
                result = connection.reply(replies);
            }
        }
        let ended = connection.ended();
        let detached = connection.detached();
        let changed = !bytes.is_empty() || result.is_err() || ended || detached;
        if !changed {
            return false;
        }
        if result.is_err() || ended || detached {
            self.focused = false;
            self.ended = ended;
            self.diagnostic = Some(
                result
                    .err()
                    .map(|e| format!("{e:#}"))
                    .unwrap_or_else(|| "display closed".into()),
            );
            self.connection.take();
        }
        match self.screen.snapshot() {
            Ok(frame) => self.frame = frame,
            Err(error) => {
                self.diagnostic = Some(error.to_string());
                self.connection.take();
                self.focused = false;
            }
        }
        true
    }
    pub fn fullscreen(&mut self, columns: u16, rows: u16) -> Result<i32> {
        self.focused = false;
        let result = self.run_fullscreen(columns, rows);
        if let Some(connection) = &mut self.connection {
            if connection.ended() {
                self.ended = true;
            }
            if !self.ended && result.is_ok() {
                let restore = connection
                    .resize(self.size.0, self.size.1)
                    .and_then(|_| connection.repaint());
                if let Err(error) = restore {
                    self.diagnostic = Some(format!("{error:#}"));
                    self.connection.take();
                } else {
                    self.screen = Screen::new(self.size.0, self.size.1)?;
                }
            } else {
                self.connection.take();
            }
        }
        result
    }
    fn run_fullscreen(&mut self, columns: u16, rows: u16) -> Result<i32> {
        // Keep the same remote helper and lease through the handoff. Only its
        // tmux display client repaints; the worker is never restarted.
        let connection = self.connection()?;
        connection.resize(columns, rows)?;
        connection.take_output();
        connection.repaint()?;
        let mut raw = RawTerminal::new()?;
        let mut size = (columns, rows);
        let mut prefix = false;
        let result = (|| {
            loop {
                connection.poll()?;
                connection.write_output(&mut raw.output)?;
                if connection.ended() || connection.detached() {
                    break;
                }
                let current = crossterm::terminal::size()?;
                if current != size {
                    connection.resize(current.0, current.1)?;
                    size = current;
                }
                let mut input = [0; 8192];
                match raw.input.read(&mut input) {
                    Ok(0) => break,
                    Ok(n) => {
                        if connection.has_control() {
                            connection.input(&input[..n])?;
                        } else {
                            // Observe never sends user input. Honor native detach
                            // locally while its remote tmux client remains read-only.
                            for &byte in &input[..n] {
                                if prefix && byte == b'd' {
                                    return Ok(0);
                                }
                                prefix = byte == 2;
                            }
                        }
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(e) => return Err(e.into()),
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(0)
        })();
        // Preserve a display failure even if transport already failed: the
        // dashboard must not repaint an unavailable terminal during cleanup.
        raw.restore_display(connection)?;
        result
    }
}

struct RawTerminal {
    input: File,
    output: File,
    raw: bool,
}
impl RawTerminal {
    fn restore_display(&mut self, connection: &mut RemoteConnection) -> Result<()> {
        // Discard only after leaving full-screen. Cancel any partial terminal
        // sequence and reset display modes before the dashboard paints again.
        let mut reset: &[u8] = b"\x18\x1b[!p\x1b[0m\x1b[?25h";
        let mut transport_error = None;
        let deadline = Instant::now() + RESTORE_TIMEOUT;
        let restored = (|| -> Result<()> {
            while !reset.is_empty() {
                ensure!(
                    Instant::now() < deadline,
                    "full-screen display restoration timed out"
                );
                if transport_error.is_none() {
                    transport_error = connection.poll().err();
                }
                connection.take_output();
                let mut ready = libc::pollfd {
                    fd: self.output.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let polled = unsafe { libc::poll(&mut ready, 1, 0) };
                if polled < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::Interrupted {
                        return Err(error.into());
                    }
                } else if polled > 0 {
                    match self.output.write(reset) {
                        Ok(0) => anyhow::bail!("full-screen terminal output closed"),
                        Ok(n) => reset = &reset[n..],
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                if !reset.is_empty() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(())
        })();
        if let Err(error) = restored {
            // Full-screen has ended. Discard its queued paint and attempt one
            // nonblocking reset; neither emulator progress nor a successful
            // write is required to release raw mode or settle the attachment.
            unsafe { libc::tcflush(self.output.as_raw_fd(), libc::TCOFLUSH) };
            let _ = self.output.write(
                b"\x18\x1b[!p\x1b[0m\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2004l\x1b[?1049l",
            );
            return Err(error.context(DisplayRestoreFailed));
        }
        match transport_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn new() -> Result<Self> {
        // PTY stdin and stdout can share an open file description. Changing
        // stdin's flags would also make repaint writes fail under backpressure.
        // Open the controlling terminal separately to poll only input.
        let input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open("/dev/tty")
            .context("opening full-screen terminal input")?;
        // stdout may share stdin's file description. A separate open keeps
        // both inherited flags intact, including on setup and write failures.
        let output = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open("/dev/tty")
            .context("opening full-screen terminal output")?;
        let mut guard = Self {
            input,
            output,
            raw: false,
        };
        crossterm::terminal::enable_raw_mode()?;
        guard.raw = true;
        Ok(guard)
    }
}
impl Drop for RawTerminal {
    fn drop(&mut self) {
        if self.raw {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}
