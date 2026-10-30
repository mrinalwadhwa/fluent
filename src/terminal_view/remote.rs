use super::fullscreen::RawTerminal;
use super::{Frame, Screen, releases_focus};
use crate::dashboard_sources::{OpenSsh, RemoteConnection, SshSource};
use crate::terminal_sessions::TerminalDescriptor;
use anyhow::{Context, Result, ensure};
use crossterm::event::Event;
use std::time::Duration;

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
        let mut screen = Screen::new(columns, rows)?;
        let result = (|| {
            loop {
                connection.poll()?;
                connection.write_output(&mut FullscreenOutput {
                    output: &mut raw.output,
                    screen: &mut screen,
                })?;
                if connection.ended() || connection.detached() {
                    break;
                }
                let current = crossterm::terminal::size()?;
                if current != size {
                    connection.resize(current.0, current.1)?;
                    screen.resize(current.0, current.1)?;
                    size = current;
                }
                if crossterm::event::poll(Duration::ZERO)? {
                    let event = crossterm::event::read()?;
                    if connection.has_control() {
                        if let super::Input::Bytes(input) =
                            super::input::fullscreen_input(&mut screen, event)?
                        {
                            connection.input(&input)?;
                        }
                    } else if super::input::observe_detach(&event, &mut prefix) {
                        return Ok(0);
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(0)
        })();
        // Preserve a display failure even if transport already failed: the
        // dashboard must not repaint an unavailable terminal during cleanup.
        raw.restore_display(|| {
            let result = connection.poll();
            connection.take_output();
            result
        })?;
        result
    }
}

// Track the native client's keyboard modes only for bytes the display accepted.
struct FullscreenOutput<'a> {
    output: &'a mut std::fs::File,
    screen: &'a mut Screen,
}
impl std::io::Write for FullscreenOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = std::io::Write::write(self.output, bytes)?;
        self.screen.write(&bytes[..written]);
        self.screen.replies();
        Ok(written)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(self.output)
    }
}
