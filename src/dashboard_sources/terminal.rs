//! Own the attachment on the remote host. Never export a socket or a lease path.
use super::protocol::{Action, Request, Response, Wire};
use super::ssh::nonblocking;
use crate::terminal_sessions::{self, Attachment, ControlOutcome, TerminalDescriptor};
use anyhow::{Result, ensure};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

struct Pty {
    master: File,
    child: Child,
}
impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Pty {
    fn open(attachment: &Attachment, columns: u16, rows: u16) -> Result<Self> {
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
            "opening remote attachment PTY failed"
        );
        let master = unsafe { File::from_raw_fd(master) };
        let slave = unsafe { File::from_raw_fd(slave) };
        for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
            ensure!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == 0,
                "protecting remote PTY failed"
            );
        }
        nonblocking(&master)?;
        let child = attachment.spawn_in_pty(&slave)?;
        Ok(Self { master, child })
    }
    fn resize(&self, columns: u16, rows: u16) -> Result<()> {
        let size = libc::winsize {
            ws_col: columns,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        ensure!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) } == 0,
            "remote PTY resize failed"
        );
        Ok(())
    }
}

struct RawMode(bool);
impl Drop for RawMode {
    fn drop(&mut self) {
        if self.0 {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}

pub fn helper_terminal(root: &Path, identity: &str, columns: u16, rows: u16) -> Result<()> {
    ensure!(identity.len() <= 8192, "terminal identity exceeds bound");
    dimensions(columns, rows)?;
    let requested: TerminalDescriptor = serde_json::from_str(identity)?;
    let source = super::SshSource {
        name: "helper".into(),
        host: "remote".into(),
        project_root: root.to_owned(),
    };
    super::validate_descriptor(&source, &requested)?;
    let mut attachment = terminal_sessions::resolve(root, &requested.session_id)?;
    ensure!(
        attachment.descriptor() == &requested,
        "remote invocation ended or changed"
    );
    let tty = unsafe { libc::isatty(0) } == 1;
    let _raw = RawMode(tty);
    if tty {
        crossterm::terminal::enable_raw_mode()?;
    }
    // The service owns these descriptors, and restoring the SSH PTY is handled
    // by sshd. Nonblocking IO keeps owner revocation independent of a slow peer.
    let mut input = std::io::stdin();
    let mut output = std::io::stdout();
    nonblocking(&input)?;
    nonblocking(&output)?;
    let mut wire = Wire::default();
    let mut size = (columns, rows);
    let mut pty = Some(Pty::open(&attachment, columns, rows)?);
    wire.queue(&Response::Ready {
        schema_version: 1,
        descriptor: requested,
    })?;
    let mut pending = Vec::new();
    let mut offset = 0;
    let mut input_requires_control = false;
    let mut last_request = Instant::now();
    let mut next_liveness = Instant::now();
    loop {
        wire.flush(&mut output)?;
        if Instant::now() >= next_liveness {
            next_liveness = Instant::now() + Duration::from_millis(200);
            if attachment.validate_live().is_err() {
                pty.take();
                attachment.release_control();
                wire.queue(&Response::Ended)?;
                wire.flush(&mut output)?;
                return Ok(());
            }
        }
        ensure!(
            last_request.elapsed() < Duration::from_secs(8),
            "SSH client stopped responding"
        );
        for request in wire.read::<Request>(&mut input)? {
            last_request = Instant::now();
            let result = (|| -> Result<()> {
                attachment.validate_live()?;
                match request.action {
                    Action::Control => {
                        ensure!(
                            attachment.acquire_control()? == ControlOutcome::Acquired,
                            "Control is occupied"
                        );
                        pty.take();
                        pending.clear();
                        offset = 0;
                        input_requires_control = false;
                        attachment.resize(size.0, size.1)?;
                        pty = Some(Pty::open(&attachment, size.0, size.1)?);
                    }
                    Action::Observe => {
                        pty.take();
                        pending.clear();
                        offset = 0;
                        input_requires_control = false;
                        attachment.release_control();
                        pty = Some(Pty::open(&attachment, size.0, size.1)?);
                    }
                    Action::Repaint => {
                        pty.take();
                        pending.clear();
                        offset = 0;
                        input_requires_control = false;
                        pty = Some(Pty::open(&attachment, size.0, size.1)?);
                    }
                    Action::Resize { columns, rows } => {
                        dimensions(columns, rows)?;
                        if attachment.has_control() {
                            attachment.resize(columns, rows)?;
                        }
                        if let Some(pty) = &pty {
                            pty.resize(columns, rows)?;
                        }
                        size = (columns, rows);
                    }
                    Action::Input { bytes } => {
                        attachment.validate_input()?;
                        ensure!(pty.is_some(), "terminal detached");
                        ensure!(
                            bytes.len() <= 64 * 1024 && pending.len() + bytes.len() <= 128 * 1024,
                            "remote input queue is full"
                        );
                        pending.extend(bytes);
                        input_requires_control = true;
                    }
                    Action::Reply { bytes } => {
                        // tmux enforces read-only input for an Observe client.
                        // Parser replies must still reach that client.
                        ensure!(
                            bytes.len() <= 8192 && pending.len() + bytes.len() <= 128 * 1024,
                            "terminal reply queue is full"
                        );
                        pending.extend(bytes);
                    }
                    Action::Ping => {}
                }
                Ok(())
            })();
            wire.queue(&Response::Ack {
                id: request.id,
                control: attachment.has_control(),
                error: result.err().map(|e| format!("{e:#}")),
            })?;
        }
        if let Some(client) = &mut pty {
            let mut bytes = [0; 8192];
            let detached = match client.master.read(&mut bytes) {
                Ok(0) => true,
                Ok(n) => {
                    wire.queue(&Response::Output {
                        bytes: bytes[..n].to_vec(),
                    })?;
                    false
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => true,
                Err(e) => return Err(e.into()),
            };
            if detached || client.child.try_wait()?.is_some() {
                pty.take();
                pending.clear();
                offset = 0;
                wire.queue(&Response::Detached)?;
            } else if !pending.is_empty() {
                if input_requires_control {
                    attachment.validate_input()?;
                } else {
                    attachment.validate_live()?;
                }
                match client.master.write(&pending[offset..]) {
                    Ok(0) => anyhow::bail!("terminal input closed"),
                    Ok(n) => {
                        offset += n;
                        if offset == pending.len() {
                            pending.clear();
                            offset = 0;
                            input_requires_control = false;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub(crate) fn dimensions(columns: u16, rows: u16) -> Result<()> {
    ensure!(
        (1..=500).contains(&columns) && (1..=200).contains(&rows),
        "terminal dimensions must be 1..500 by 1..200"
    );
    Ok(())
}
