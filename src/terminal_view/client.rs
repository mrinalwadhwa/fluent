use super::{Frame, Input, Screen, input::MAX_PASTE_BYTES, releases_focus, screen::dimensions};
use crate::terminal_sessions::{Attachment, ControlOutcome, TerminalDescriptor};
use anyhow::{Context, Result, ensure};
use crossterm::event::Event;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::Child;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, SyncSender},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct Update {
    frame: Option<Frame>,
    error: Option<String>,
    closed: bool,
}
enum Command {
    Input(Event),
    LiteralEscape,
    Resize(u16, u16),
}

struct Client {
    child: Child,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    commands: SyncSender<Command>,
}
impl Drop for Client {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// Own only the display client. Execution ownership and Control admission stay
/// with the runtime; input is revalidated again immediately before each write.
pub struct LocalTerminal {
    attachment: Arc<Mutex<Attachment>>,
    descriptor: TerminalDescriptor,
    client: Option<Client>,
    latest: Arc<Mutex<Update>>,
    frame: Frame,
    size: (u16, u16),
    focused: bool,
    ended: bool,
    next_liveness: Instant,
    pub diagnostic: Option<String>,
}
impl LocalTerminal {
    pub fn observe(attachment: Attachment, columns: u16, rows: u16) -> Result<Self> {
        dimensions(columns, rows)?;
        ensure!(
            !attachment.has_control(),
            "observation cannot inherit Control"
        );
        let descriptor = attachment.descriptor().clone();
        let mut terminal = Self {
            attachment: Arc::new(Mutex::new(attachment)),
            descriptor,
            client: None,
            latest: Arc::new(Mutex::new(Update {
                frame: None,
                error: None,
                closed: false,
            })),
            frame: Screen::new(columns, rows)?.snapshot()?,
            size: (columns, rows),
            focused: false,
            ended: false,
            next_liveness: Instant::now(),
            diagnostic: None,
        };
        terminal.connect()?;
        Ok(terminal)
    }
    fn connect(&mut self) -> Result<()> {
        ensure!(!self.ended, "invocation ended; select another worker");
        self.client.take();
        *self.latest.lock().unwrap() = Update {
            frame: None,
            error: None,
            closed: false,
        };
        let (master, slave) = open_pty(self.size.0, self.size.1)?;
        let mut child = self.attachment.lock().unwrap().spawn_in_pty(&slave)?;
        drop(slave);
        let (tx, rx) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_reader = Arc::clone(&stop);
        let latest = Arc::clone(&self.latest);
        let attachment = Arc::clone(&self.attachment);
        let (columns, rows) = self.size;
        let reader = match thread::Builder::new()
            .name("terminal-display".into())
            .spawn(move || {
                let result = drain(
                    master,
                    columns,
                    rows,
                    &stop_reader,
                    &latest,
                    &rx,
                    &attachment,
                );
                let mut latest = latest.lock().unwrap();
                latest.closed = true;
                if let Err(error) = result {
                    latest.error = Some(format!("{error:#}; press t to reconnect"));
                }
            }) {
            Ok(reader) => reader,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.into());
            }
        };
        self.client = Some(Client {
            child,
            stop,
            reader: Some(reader),
            commands: tx,
        });
        self.diagnostic = None;
        Ok(())
    }
    pub fn reconnect(&mut self) -> Result<()> {
        self.connect()
    }
    /// Transfer the existing authority after settling the embedded client.
    /// The inherited client retains the same Control lease, if one was held.
    pub fn fullscreen(&mut self, columns: u16, rows: u16) -> Result<i32> {
        ensure!(!self.ended, "invocation ended");
        self.client.take();
        self.focused = false;
        let result = {
            let attachment = self.attachment.lock().unwrap();
            (|| {
                if attachment.has_control() {
                    attachment.resize(columns, rows)?;
                }
                attachment.run()
            })()
        };
        if self.attachment.lock().unwrap().validate_live().is_err() {
            self.ended = true;
            self.attachment.lock().unwrap().release_control();
        } else {
            let restore = (|| {
                if self.has_control() {
                    self.attachment
                        .lock()
                        .unwrap()
                        .resize(self.size.0, self.size.1)?;
                }
                self.connect()
            })();
            if let Err(error) = restore {
                self.diagnostic = Some(format!("{error:#}; press t to reconnect"));
            }
        }
        result
    }
    pub fn frame(&self) -> &Frame {
        &self.frame
    }
    pub fn descriptor(&self) -> &TerminalDescriptor {
        &self.descriptor
    }
    pub fn focused(&self) -> bool {
        self.focused
    }
    pub fn release_focus(&mut self) {
        self.focused = false;
    }
    pub fn ended(&self) -> bool {
        self.ended
    }
    pub fn has_control(&self) -> bool {
        self.attachment.lock().unwrap().has_control()
    }
    pub fn state_label(&self) -> String {
        if self.ended {
            return "Ended · input unavailable".into();
        }
        if let Some(reason) = &self.diagnostic {
            return reason.clone();
        }
        if self.has_control() {
            if self.focused {
                "Control · Ctrl-] navigate"
            } else {
                "Control · navigation"
            }
        } else {
            "Observe · i control"
        }
        .into()
    }
    pub fn acquire_control(&mut self) -> Result<()> {
        ensure!(!self.ended, "invocation ended");
        self.client.take();
        let result = (|| {
            let mut attachment = self.attachment.lock().unwrap();
            ensure!(
                attachment.acquire_control()? == ControlOutcome::Acquired,
                "Control is occupied"
            );
            attachment.resize(self.size.0, self.size.1)
        })();
        if let Err(error) = result {
            self.attachment.lock().unwrap().release_control();
            let _ = self.connect();
            return Err(error);
        }
        if let Err(error) = self.connect() {
            self.attachment.lock().unwrap().release_control();
            return Err(error);
        }
        self.focused = true;
        Ok(())
    }
    pub fn observe_only(&mut self) -> Result<()> {
        self.client.take();
        self.focused = false;
        self.attachment.lock().unwrap().release_control();
        self.connect()
    }
    pub fn send(&mut self, event: Event) -> Result<()> {
        ensure!(
            self.focused && !self.ended,
            "terminal input requires focused Control"
        );
        self.attachment.lock().unwrap().validate_input()?;
        if releases_focus(&event) {
            self.release_focus();
            return Ok(());
        }
        if let Event::Paste(text) = &event {
            ensure!(text.len() <= MAX_PASTE_BYTES, "paste exceeds 64 KiB");
        }
        self.enqueue(Command::Input(event))
    }
    pub fn send_literal_escape(&mut self) -> Result<()> {
        ensure!(!self.ended, "invocation ended");
        self.attachment.lock().unwrap().validate_input()?;
        self.enqueue(Command::LiteralEscape)?;
        self.focused = true;
        Ok(())
    }
    fn enqueue(&self, command: Command) -> Result<()> {
        self.client
            .as_ref()
            .context("display disconnected; press t to reconnect")?
            .commands
            .try_send(command)
            .map_err(|_| {
                anyhow::anyhow!("terminal input queue unavailable; input was not accepted")
            })
    }
    pub fn resize(&mut self, columns: u16, rows: u16) -> Result<()> {
        dimensions(columns, rows)?;
        if self.size == (columns, rows) || self.ended {
            return Ok(());
        }
        if self.has_control() {
            self.attachment.lock().unwrap().resize(columns, rows)?;
        }
        self.enqueue(Command::Resize(columns, rows))?;
        self.size = (columns, rows);
        Ok(())
    }
    pub fn refresh(&mut self) -> bool {
        let mut changed = false;
        {
            let mut latest = self.latest.lock().unwrap();
            if let Some(frame) = latest.frame.take() {
                self.frame = frame;
                changed = true;
            }
            if let Some(error) = latest.error.take() {
                self.diagnostic = Some(error);
                self.focused = false;
                changed = true;
            }
            if latest.closed && self.diagnostic.is_none() && !self.ended {
                self.diagnostic = Some("Disconnected · t reconnect".into());
                self.focused = false;
                changed = true;
            }
        }
        if !self.ended && Instant::now() >= self.next_liveness {
            self.next_liveness = Instant::now() + Duration::from_millis(200);
            let unavailable = self.attachment.lock().unwrap().validate_live().is_err();
            if unavailable {
                self.client.take();
                // Settling the reader can publish one last complete frame.
                if let Some(frame) = self.latest.lock().unwrap().frame.take() {
                    self.frame = frame;
                }
                self.focused = false;
                self.ended = true;
                self.attachment.lock().unwrap().release_control();
                changed = true;
            }
        }
        changed
    }
}

fn open_pty(columns: u16, rows: u16) -> Result<(File, File)> {
    let (mut master, mut slave) = (-1, -1);
    let mut size = libc::winsize {
        ws_col: columns,
        ws_row: rows,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    ensure!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        } == 0,
        "opening terminal PTY: {}",
        std::io::Error::last_os_error()
    );
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        ensure!(
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
            "protecting terminal descriptor"
        );
    }
    ensure!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == 0,
        "setting terminal nonblocking mode"
    );
    Ok((master, slave))
}

pub(super) fn tmux_input(screen: &mut Screen, event: Event) -> Result<Input> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    let prefix = matches!(&event, Event::Key(key)
        if key.kind != KeyEventKind::Release
            && key.code == KeyCode::Char('b')
            && key.modifiers == KeyModifiers::CONTROL);
    let key_event = matches!(event, Event::Key(_));
    let paste = matches!(event, Event::Paste(_));
    let mut input = screen.input(event)?;
    if let Input::Bytes(bytes) = &mut input {
        if key_event && !bytes.is_empty() && (prefix || bytes.as_slice() == b"\x02") {
            // The managed server keeps tmux's default C-b C-b send-prefix
            // binding. The inherited full-screen client keeps normal detach.
            bytes.insert(0, 2);
        } else if paste && !bytes.starts_with(b"\x1b[200~") && bytes.contains(&2) {
            // tmux bypasses key bindings inside bracketed paste. Before that
            // mode arrives, quote prefix bytes in plain paste as native keys.
            let mut quoted = Vec::with_capacity(bytes.len() * 2);
            for &byte in bytes.iter() {
                quoted.push(byte);
                if byte == 2 {
                    quoted.push(byte);
                }
            }
            *bytes = quoted;
        }
    }
    Ok(input)
}

fn drain(
    mut master: File,
    columns: u16,
    rows: u16,
    stop: &AtomicBool,
    latest: &Mutex<Update>,
    commands: &Receiver<Command>,
    attachment: &Mutex<Attachment>,
) -> Result<()> {
    let mut screen = Screen::new(columns, rows)?;
    let mut bytes = [0; 8192];
    let mut next_frame = Instant::now();
    let mut dirty = false;
    // Keep at most one input pending. Revalidate every partial write, including
    // when a queued event outlives the invocation that originally admitted it.
    let mut pending: Vec<u8> = Vec::new();
    let mut offset = 0;
    while !stop.load(Ordering::Relaxed) {
        let mut active = false;
        match master.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                screen.write(&bytes[..n]);
                dirty = true;
                active = true;
                let replies = screen.replies();
                if !replies.is_empty() {
                    master
                        .write_all(&replies)
                        .context("terminal reply lost; reconnect to repaint")?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
            Err(e) => return Err(e.into()),
        }
        if pending.is_empty() {
            if let Ok(command) = commands.try_recv() {
                active = true;
                match command {
                    Command::Input(event) => {
                        if let Input::Bytes(bytes) = tmux_input(&mut screen, event)? {
                            pending = bytes;
                        }
                    }
                    Command::LiteralEscape => pending = vec![29],
                    Command::Resize(c, r) => {
                        let size = libc::winsize {
                            ws_col: c,
                            ws_row: r,
                            ws_xpixel: 0,
                            ws_ypixel: 0,
                        };
                        ensure!(
                            unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCSWINSZ, &size) }
                                == 0,
                            "resizing client PTY failed"
                        );
                        screen.resize(c, r)?;
                        dirty = true;
                    }
                }
            }
        }
        if !pending.is_empty() {
            let authority = attachment.lock().unwrap();
            authority.validate_input()?;
            match master.write(&pending[offset..]) {
                Ok(0) => anyhow::bail!("terminal input closed"),
                Ok(n) => {
                    offset += n;
                    active = true;
                    if offset == pending.len() {
                        pending.clear();
                        offset = 0;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
        if dirty && Instant::now() >= next_frame {
            let frame = screen.snapshot()?;
            latest.lock().unwrap().frame = Some(frame);
            dirty = false;
            next_frame = Instant::now() + Duration::from_millis(33);
        }
        if !active {
            thread::sleep(Duration::from_millis(5));
        }
    }
    if dirty {
        latest.lock().unwrap().frame = Some(screen.snapshot()?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn embedded_input_quotes_tmux_prefix_without_changing_paste() {
        let mut screen = Screen::new(40, 8).unwrap();
        let prefix = Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
        assert_eq!(
            tmux_input(&mut screen, prefix.clone()).unwrap(),
            Input::Bytes(b"\x02\x02".to_vec())
        );
        let following = Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        assert_eq!(
            tmux_input(&mut screen, following).unwrap(),
            Input::Bytes(b"d".to_vec())
        );
        assert_eq!(
            tmux_input(&mut screen, Event::Paste("\x02d界".into())).unwrap(),
            Input::Bytes("\x02\x02d界".as_bytes().to_vec())
        );
        screen.write(b"\x1b[?2004h");
        assert_eq!(
            tmux_input(&mut screen, Event::Paste("\x02d界".into())).unwrap(),
            Input::Bytes("\x1b[200~\x02d界\x1b[201~".as_bytes().to_vec())
        );
        screen.write(b"\x1b[>1u");
        assert_eq!(
            tmux_input(&mut screen, prefix).unwrap(),
            Input::Bytes(b"\x02\x1b[98;5u".to_vec())
        );
        let alt = Event::Key(KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ));
        let expected = screen.input(alt.clone()).unwrap();
        assert_eq!(tmux_input(&mut screen, alt).unwrap(), expected);
    }
}
