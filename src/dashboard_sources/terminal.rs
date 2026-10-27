//! Own the attachment on the remote host. Never export a socket or a lease path.
use super::protocol::{Action, DISPLAY_WINDOW, Request, Response, TERMINAL_VERSION, Wire};
use super::ssh::nonblocking;
use crate::terminal_sessions::{self, Attachment, ControlOutcome, TerminalDescriptor};
use anyhow::{Context, Result, ensure};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::Path;
use std::process::Child;
use std::time::{Duration, Instant};

const PEER_TIMEOUT: Duration = Duration::from_secs(8);

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
        let setup_started = Instant::now();
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
        trace_setup(attachment, "spawning", None, setup_started);
        let child = attachment.spawn_in_pty(&slave)?;
        let mut client = Self { master, child };
        drop(slave);
        trace_setup(
            attachment,
            "spawned",
            Some(client.child.id()),
            setup_started,
        );
        // Admit input only after the terminal client has entered raw mode.
        // Otherwise its startup tcsetattr can flush newly accepted keystrokes.
        let start = Instant::now();
        let mut modes = None;
        let result = (|| -> Result<()> {
            loop {
                attachment.validate_live()?;
                let status = client.child.try_wait()?;
                ensure!(
                    status.is_none(),
                    "remote terminal client exited during setup: {status:?}"
                );
                let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
                ensure!(
                    unsafe { libc::tcgetattr(client.master.as_raw_fd(), attributes.as_mut_ptr()) }
                        == 0,
                    "reading remote PTY modes failed"
                );
                let attributes = unsafe { attributes.assume_init() };
                modes = Some(attributes.c_lflag);
                if attributes.c_lflag & (libc::ICANON | libc::ECHO) == 0 {
                    return Ok(());
                }
                ensure!(
                    start.elapsed() < Duration::from_secs(5),
                    "remote terminal client did not enter raw mode"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        })();
        if let Err(error) = result {
            // Failed clients may explain their setup error on the PTY. Preserve
            // a bounded excerpt before Drop settles them and closes the master.
            // Successful setup leaves every display byte for the normal relay.
            let mut output = [0; 8192];
            let captured = match client.master.read(&mut output) {
                Ok(n) => super::display_diagnostic(&String::from_utf8_lossy(&output[..n])),
                Err(error) => format!("<PTY diagnostic read: {error}>"),
            };
            return Err(error).with_context(|| format!(
                "remote PTY setup pid={} control={} elapsed={:?} raw_wait={:?} lflag={modes:?} output={captured}",
                client.child.id(), attachment.has_control(), setup_started.elapsed(), start.elapsed(),
            ));
        }
        trace_setup(attachment, "raw", Some(client.child.id()), setup_started);
        Ok(client)
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

fn trace_setup(attachment: &Attachment, phase: &str, child: Option<u32>, started: Instant) {
    // The controlled host harness retains helper stderr on failure. Keep phase
    // receipts out of ordinary sessions and production builds.
    #[cfg(feature = "test-support")]
    if std::env::var_os("FLUENT_SSH_PTY_EVIDENCE_DIR").is_some() {
        eprintln!(
            "SSH_PTY_SETUP phase={phase} helper_pid={} child_pid={child:?} generation={} control={} elapsed={:?}",
            std::process::id(),
            attachment.descriptor().execution_generation,
            attachment.has_control(),
            started.elapsed(),
        );
    }
    let _ = (attachment, phase, child, started);
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
        schema_version: TERMINAL_VERSION,
        descriptor: requested,
    })?;
    let mut display_credit = DISPLAY_WINDOW;
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
                return end_attachment(
                    &mut attachment,
                    &mut pty,
                    &mut wire,
                    &mut input,
                    &mut output,
                );
            }
        }
        ensure!(
            last_request.elapsed() < PEER_TIMEOUT,
            "SSH client stopped responding"
        );
        for request in wire.read::<Request>(&mut input)? {
            last_request = Instant::now();
            let result = (|| -> Result<()> {
                attachment.validate_live()?;
                match request.action {
                    Action::Control => {
                        let had_control = attachment.has_control();
                        ensure!(
                            attachment.acquire_control()? == ControlOutcome::Acquired,
                            "Control is occupied"
                        );
                        pty.take();
                        pending.clear();
                        offset = 0;
                        input_requires_control = false;
                        let setup = (|| -> Result<Pty> {
                            attachment.resize(size.0, size.1)?;
                            Pty::open(&attachment, size.0, size.1)
                        })();
                        if setup.is_err() && !had_control {
                            // Failed setup has settled its writable child. Do
                            // not strand a lease acquired by this failed request.
                            // Existing Control survives a failed client reopen.
                            attachment.release_control();
                        }
                        pty = Some(setup?);
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
                    Action::OutputConsumed { bytes } => {
                        ensure!(
                            bytes <= DISPLAY_WINDOW - display_credit,
                            "invalid display credit"
                        );
                        display_credit += bytes;
                    }
                    Action::Ping => {}
                }
                Ok(())
            })();
            let response = request_response(&attachment, request.id, result);
            if matches!(response, Response::Ended) {
                return end_attachment(
                    &mut attachment,
                    &mut pty,
                    &mut wire,
                    &mut input,
                    &mut output,
                );
            }
            wire.queue(&response)?;
        }
        if let Some(client) = &mut pty {
            let mut bytes = [0; 8192];
            // A slow display may stop PTY reads, but never requests, input,
            // ownership validation or the helper's liveness checks.
            let capacity = bytes.len().min(display_credit);
            let detached = if capacity == 0 {
                false
            } else {
                match client.master.read(&mut bytes[..capacity]) {
                    Ok(0) => true,
                    Ok(n) => {
                        display_credit -= n;
                        wire.queue(&Response::Output {
                            bytes: bytes[..n].to_vec(),
                        })?;
                        false
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
                    Err(e) if e.raw_os_error() == Some(libc::EIO) => true,
                    Err(e) => return Err(e.into()),
                }
            };
            if detached || client.child.try_wait()?.is_some() {
                pty.take();
                pending.clear();
                offset = 0;
                wire.queue(&Response::Detached)?;
            } else if !pending.is_empty() {
                let validation = if input_requires_control {
                    attachment.validate_input()
                } else {
                    attachment.validate_live()
                };
                if validation.is_err() && attachment.validate_live().is_err() {
                    return end_attachment(
                        &mut attachment,
                        &mut pty,
                        &mut wire,
                        &mut input,
                        &mut output,
                    );
                }
                validation?;
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

fn request_response(attachment: &Attachment, id: u64, result: Result<()>) -> Response {
    // Settlement can race any request, including pings and display credit.
    // Report it just as the periodic live check does, without hiding errors
    // from a still-live invocation (for example, an occupied Control lease).
    if result.is_err() && attachment.validate_live().is_err() {
        return Response::Ended;
    }
    Response::Ack {
        id,
        control: attachment.has_control(),
        error: result.err().map(|e| format!("{e:#}")),
    }
}

fn end_attachment(
    attachment: &mut Attachment,
    pty: &mut Option<Pty>,
    wire: &mut Wire,
    input: &mut impl Read,
    output: &mut impl Write,
) -> Result<()> {
    pty.take();
    attachment.release_control();
    flush_ended(wire, input, output)
}

fn flush_ended(wire: &mut Wire, input: &mut impl Read, output: &mut impl Write) -> Result<()> {
    wire.queue(&Response::Ended)?;
    let start = Instant::now();
    loop {
        wire.flush(output)?;
        if !wire.has_pending_output() {
            return Ok(());
        }
        ensure!(
            start.elapsed() < PEER_TIMEOUT,
            "SSH output stalled; disconnecting"
        );
        // The PTY and Control lease have already settled. Drain queued requests
        // without applying them while preserving complete output frames.
        wire.read::<Request>(input)?;
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use crate::terminal_sessions::{TerminalIdentity, TerminalSession};

    #[test]
    fn settlement_flushes_complete_frames_after_backpressure() {
        struct SlowWriter {
            stalls: u8,
            bytes: Vec<u8>,
        }
        impl Write for SlowWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.stalls > 0 {
                    self.stalls -= 1;
                    return Err(if self.stalls == 0 {
                        std::io::ErrorKind::Interrupted
                    } else {
                        std::io::ErrorKind::WouldBlock
                    }
                    .into());
                }
                let length = bytes.len().min(7);
                self.bytes.extend_from_slice(&bytes[..length]);
                Ok(length)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut wire = Wire::default();
        wire.queue(&Response::Output {
            bytes: b"final paint".to_vec(),
        })
        .unwrap();
        let mut output = SlowWriter {
            stalls: 2,
            bytes: Vec::new(),
        };
        let (mut input, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        flush_ended(&mut wire, &mut input, &mut output).unwrap();
        let mut bytes = output.bytes.as_slice();
        let responses = Wire::default().read::<Response>(&mut bytes).unwrap();
        assert!(
            matches!(&responses[..], [Response::Output { bytes }, Response::Ended]
            if bytes == b"final paint")
        );
        assert_eq!(output.bytes.last(), Some(&b'\n'));
    }

    #[test]
    fn request_error_from_live_invocation_remains_an_error() {
        let root = tempfile::tempdir().unwrap();
        let mut owner = TerminalSession::start_fixture(
            TerminalIdentity::fixture(root.path(), "work", "attempt-1", "writer"),
            "exec /bin/sleep 60",
        )
        .unwrap();
        let attachment = owner.observe(owner.descriptor()).unwrap();
        let response = request_response(&attachment, 1, attachment.validate_input());
        owner.stop().unwrap();
        assert!(matches!(response, Response::Ack { error: Some(error), .. }
            if error.contains("Control is required")));
    }

    #[test]
    fn settlement_does_not_mask_a_broken_pipe() {
        let (mut output, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(peer);
        let mut input = std::io::empty();
        let error = flush_ended(&mut Wire::default(), &mut input, &mut output).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn request_after_settlement_reports_ended() {
        for finish in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut owner = TerminalSession::start_fixture(
                TerminalIdentity::fixture(root.path(), "work", "attempt-1", "writer"),
                "exec /bin/sleep 60",
            )
            .unwrap();
            let attachment = owner.observe(owner.descriptor()).unwrap();
            assert!(attachment.validate_live().is_ok());
            if finish {
                owner.finish().unwrap();
            } else {
                owner.stop().unwrap();
            }
            // Model a queued request racing the helper's periodic live check.
            let result = attachment.validate_live();
            assert!(result.is_err());
            assert!(matches!(
                request_response(&attachment, 1, result),
                Response::Ended
            ));
        }
    }
}

pub(crate) fn dimensions(columns: u16, rows: u16) -> Result<()> {
    ensure!(
        (1..=500).contains(&columns) && (1..=200).contains(&rows),
        "terminal dimensions must be 1..500 by 1..200"
    );
    Ok(())
}
