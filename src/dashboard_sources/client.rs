use super::protocol::{Action, DISPLAY_WINDOW, Request, Response, TERMINAL_VERSION, Wire};
use super::ssh::{nonblocking, remedy};
use super::{OpenSsh, SshSource, validate_descriptor};
use crate::terminal_sessions::TerminalDescriptor;
use anyhow::{Context, Result, bail, ensure};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio};
use std::time::{Duration, Instant};

/// One connection owns one remote helper and its lease. Drop abandons all queued
/// bytes; a new connection must resolve the original descriptor in Observe.
pub struct RemoteConnection {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
    stderr: ChildStderr,
    errors: Vec<u8>,
    wire: Wire,
    descriptor: TerminalDescriptor,
    control: bool,
    ready: bool,
    ended: bool,
    detached: bool,
    serial: u64,
    outstanding: VecDeque<(u64, Option<bool>, &'static str)>,
    waiting: Option<(u64, Option<String>)>,
    bytes: Vec<u8>,
    consumed: usize,
    heartbeat: Instant,
    last_response: Instant,
    started: Instant,
}
impl Drop for RemoteConnection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl RemoteConnection {
    pub fn open(
        ssh: OpenSsh,
        source: SshSource,
        descriptor: TerminalDescriptor,
        columns: u16,
        rows: u16,
    ) -> Result<Self> {
        super::terminal::dimensions(columns, rows)?;
        validate_descriptor(&source, &descriptor)?;
        let identity = serde_json::to_string(&descriptor)?;
        let mut command = ssh.command(
            &source,
            "terminal",
            &[
                "--identity",
                &identity,
                "--columns",
                &columns.to_string(),
                "--rows",
                &rows.to_string(),
            ],
            true,
        )?;
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| remedy(&source))?;
        let input = child.stdin.take().unwrap();
        let output = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut client = Self {
            child,
            input,
            output,
            stderr,
            errors: Vec::new(),
            wire: Wire::default(),
            descriptor,
            control: false,
            ready: false,
            ended: false,
            detached: false,
            serial: 0,
            outstanding: VecDeque::new(),
            waiting: None,
            bytes: Vec::new(),
            consumed: 0,
            heartbeat: Instant::now(),
            last_response: Instant::now(),
            started: Instant::now(),
        };
        nonblocking(&client.input)?;
        nonblocking(&client.output)?;
        nonblocking(&client.stderr)?;
        let start = Instant::now();
        while !client.ready {
            client.poll().with_context(|| remedy(&source))?;
            ensure!(
                start.elapsed() < Duration::from_secs(8),
                "{}: terminal helper timed out",
                remedy(&source)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(client)
    }
    pub fn descriptor(&self) -> &TerminalDescriptor {
        &self.descriptor
    }
    pub fn has_control(&self) -> bool {
        self.control && !self.ended
    }
    pub fn ended(&self) -> bool {
        self.ended
    }
    pub fn detached(&self) -> bool {
        self.detached
    }
    pub fn take_output(&mut self) -> Vec<u8> {
        self.consumed += self.bytes.len();
        std::mem::take(&mut self.bytes)
    }
    /// Attempt one partial display write. The caller must supply a nonblocking
    /// writer; bytes retain their exact order until the terminal accepts them.
    pub(crate) fn write_output(&mut self, writer: &mut impl Write) -> Result<()> {
        self.consumed += write_display(&mut self.bytes, writer)?;
        Ok(())
    }

    pub fn poll(&mut self) -> Result<()> {
        let result = self.poll_inner().map_err(|error| {
            // A helper can finish writing stderr between our first read and
            // stdout closing. Drain the remaining available diagnostics before
            // reporting the failure and settling the helper.
            let error = match self.read_errors() {
                Ok(()) => error,
                Err(stderr_error) => error.context(format!(
                    "reading remaining SSH diagnostics: {stderr_error:#}"
                )),
            };
            // Snapshot state before failure revokes local authority. In
            // particular, startup timeouts used to discard retained stderr.
            anyhow::anyhow!(
                "{error:#}; SSH state ready={} control={} elapsed={:?} pending={:?}; helper stderr: {}",
                self.ready, self.control, self.started.elapsed(), self.outstanding.front(),
                String::from_utf8_lossy(&self.errors).trim(),
            )
        });
        if result.is_err() {
            self.control = false;
            self.ready = false;
            self.wire = Wire::default();
            self.outstanding.clear();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        result.map_err(|error| {
            anyhow::anyhow!("{}", super::display_diagnostic(&format!("{error:#}")))
        })
    }
    fn read_errors(&mut self) -> Result<()> {
        let mut errors = [0; 1024];
        loop {
            match self.stderr.read(&mut errors) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    ensure!(
                        self.errors.len() + n <= 16 * 1024,
                        "SSH error output exceeds bound"
                    );
                    self.errors.extend_from_slice(&errors[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
    fn poll_inner(&mut self) -> Result<()> {
        self.read_errors()?;
        if self.ended {
            return Ok(());
        }
        // poll adds the collected stderr once, after the final bounded drain.
        let responses = self.wire.read::<Response>(&mut self.output)?;
        for response in responses {
            self.last_response = Instant::now();
            ensure!(
                self.ready || matches!(response, Response::Ready { .. }),
                "SSH response precedes protocol admission"
            );
            match response {
                Response::Ready {
                    schema_version,
                    descriptor,
                } => {
                    ensure!(
                        !self.ready
                            && schema_version == TERMINAL_VERSION
                            && descriptor == self.descriptor,
                        "remote attachment identity or protocol changed"
                    );
                    self.ready = true;
                }
                Response::Ack { id, control, error } => {
                    let (expected_id, requested_control, _) = self
                        .outstanding
                        .pop_front()
                        .context("unsolicited SSH acknowledgement")?;
                    ensure!(id == expected_id, "unexpected SSH acknowledgement identity");
                    // A requested lease transition can precede a fallible PTY
                    // setup. On error accept only the previous or requested
                    // state, never a transition authorized by another action.
                    let expected_control = requested_control.unwrap_or(self.control);
                    ensure!(
                        control == expected_control || (error.is_some() && control == self.control),
                        "unsolicited SSH Control change for request {id}: previous={} requested={requested_control:?} reported={control} error={error:?}",
                        self.control
                    );
                    let error = error.map(|error| super::display_diagnostic(&error));
                    self.control = control;
                    if self
                        .waiting
                        .as_ref()
                        .is_some_and(|(expected, _)| *expected == id)
                    {
                        self.waiting = Some((0, error));
                    } else if let Some(error) = error {
                        bail!("remote attachment rejected input: {error}");
                    }
                }
                Response::Output { bytes } => {
                    ensure!(
                        self.ready
                            && bytes.len() <= 8192
                            && self.bytes.len() + bytes.len() <= DISPLAY_WINDOW,
                        "SSH display output exceeds bound"
                    );
                    self.bytes.extend(bytes);
                }
                Response::Detached => {
                    self.detached = true;
                }
                Response::Ended => {
                    self.ended = true;
                    self.control = false;
                    self.wire = Wire::default();
                }
            }
        }
        ensure!(
            self.last_response.elapsed() < Duration::from_secs(8),
            "SSH attachment timed out"
        );
        if self.ready && !self.ended && self.heartbeat.elapsed() >= Duration::from_secs(1) {
            self.enqueue(Action::Ping)?;
            self.heartbeat = Instant::now();
        }
        // Batch credits so partial local writes cannot fill the request queue
        // ahead of acknowledgements. Half a window leaves room to make progress.
        if self.ready && !self.ended && self.consumed >= DISPLAY_WINDOW / 2 {
            self.enqueue(Action::OutputConsumed {
                bytes: self.consumed,
            })?;
            self.consumed = 0;
        }
        self.wire.flush(&mut self.input)?;
        Ok(())
    }
    fn enqueue(&mut self, action: Action) -> Result<u64> {
        ensure!(self.ready && !self.ended, "remote invocation unavailable");
        ensure!(self.outstanding.len() < 64, "SSH request queue is full");
        let requested_control = match action {
            Action::Control => Some(true),
            Action::Observe => Some(false),
            _ => None,
        };
        self.serial += 1;
        let name = action.name();
        self.wire.queue(&Request {
            id: self.serial,
            action,
        })?;
        self.outstanding
            .push_back((self.serial, requested_control, name));
        Ok(self.serial)
    }
    fn request(&mut self, action: Action) -> Result<()> {
        let name = action.name();
        let previous_control = self.control;
        let id = self.enqueue(action)?;
        self.waiting = Some((id, None));
        let start = Instant::now();
        loop {
            self.poll()?;
            ensure!(!self.ended, "remote invocation ended");
            if matches!(self.waiting, Some((0, _))) {
                let (_, error) = self.waiting.take().unwrap();
                if let Some(error) = error {
                    bail!(
                        "SSH {name} request {id} failed (previous_control={previous_control} reported_control={} elapsed={:?}): {error}",
                        self.control,
                        start.elapsed()
                    );
                }
                return Ok(());
            }
            ensure!(
                start.elapsed() < Duration::from_secs(8),
                "SSH {name} request {id} timed out; control={} elapsed={:?}; helper stderr: {}",
                self.control,
                start.elapsed(),
                super::display_diagnostic(String::from_utf8_lossy(&self.errors).trim())
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn acquire_control(&mut self) -> Result<()> {
        self.request(Action::Control)
    }
    pub fn observe_only(&mut self) -> Result<()> {
        self.request(Action::Observe)
    }
    pub fn repaint(&mut self) -> Result<()> {
        self.request(Action::Repaint)?;
        self.detached = false;
        Ok(())
    }
    pub fn resize(&mut self, columns: u16, rows: u16) -> Result<()> {
        super::terminal::dimensions(columns, rows)?;
        self.request(Action::Resize { columns, rows })
    }
    pub(crate) fn reply(&mut self, bytes: Vec<u8>) -> Result<()> {
        ensure!(bytes.len() <= 8192, "terminal reply exceeds bound");
        self.enqueue(Action::Reply { bytes })?;
        Ok(())
    }
    pub fn input(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(
            self.has_control() && !self.detached,
            "remote input requires Control"
        );
        ensure!(bytes.len() <= 64 * 1024, "remote input exceeds 64 KiB");
        self.enqueue(Action::Input {
            bytes: bytes.to_vec(),
        })?;
        Ok(())
    }
}

fn write_display(bytes: &mut Vec<u8>, writer: &mut impl Write) -> Result<usize> {
    if !bytes.is_empty() {
        match writer.write(&bytes) {
            Ok(0) => bail!("full-screen terminal output closed"),
            Ok(n) => {
                bytes.drain(..n);
                return Ok(n);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, ErrorKind};

    struct SlowWriter {
        steps: VecDeque<io::Result<usize>>,
        written: Vec<u8>,
    }
    impl Write for SlowWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let n = self
                .steps
                .pop_front()
                .expect("write retried without yielding")?
                .min(bytes.len());
            self.written.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn fullscreen_partial_writes_preserve_terminal_bytes() {
        let original = "\x1b[31m界\x1b[0m".as_bytes().to_vec();
        let mut pending = original.clone();
        let mut writer = SlowWriter {
            steps: VecDeque::from([
                Ok(2),
                Err(ErrorKind::WouldBlock.into()),
                Err(ErrorKind::Interrupted.into()),
                Ok(5),
                Ok(100),
            ]),
            written: Vec::new(),
        };
        let mut consumed = 0;
        for expected in [2, 0, 0, 5, original.len() - 7] {
            let n = write_display(&mut pending, &mut writer).unwrap();
            assert_eq!(n, expected);
            consumed += n;
            assert_eq!(pending, original[consumed..]);
        }
        assert_eq!(writer.written, original);
        assert!(pending.is_empty());
    }

    #[test]
    fn fullscreen_output_failure_is_not_credited_or_hidden() {
        for result in [Ok(0), Err(ErrorKind::BrokenPipe.into())] {
            let mut writer = SlowWriter {
                steps: VecDeque::from([result]),
                written: Vec::new(),
            };
            let mut pending = b"still pending".to_vec();
            assert!(write_display(&mut pending, &mut writer).is_err());
            assert_eq!(pending, b"still pending");
            assert!(writer.written.is_empty());
        }
    }
}
