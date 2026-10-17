use super::protocol::{Action, Request, Response, Wire};
use super::ssh::{nonblocking, remedy};
use super::{OpenSsh, SshSource, validate_descriptor};
use crate::terminal_sessions::TerminalDescriptor;
use anyhow::{Context, Result, bail, ensure};
use std::io::Read;
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
    waiting: Option<(u64, Option<String>)>,
    bytes: Vec<u8>,
    heartbeat: Instant,
    last_response: Instant,
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
            waiting: None,
            bytes: Vec::new(),
            heartbeat: Instant::now(),
            last_response: Instant::now(),
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
        std::mem::take(&mut self.bytes)
    }
    pub fn poll(&mut self) -> Result<()> {
        let result = self.poll_inner();
        if result.is_err() {
            self.control = false;
            self.wire = Wire::default();
        }
        result
    }
    fn poll_inner(&mut self) -> Result<()> {
        let mut errors = [0; 1024];
        match self.stderr.read(&mut errors) {
            Ok(n) => {
                ensure!(
                    self.errors.len() + n <= 16 * 1024,
                    "SSH error output exceeds bound"
                );
                self.errors.extend_from_slice(&errors[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        if self.ended {
            return Ok(());
        }
        let responses = self
            .wire
            .read::<Response>(&mut self.output)
            .with_context(|| {
                format!(
                    "SSH disconnected: {}",
                    String::from_utf8_lossy(&self.errors).trim()
                )
            })?;
        for response in responses {
            self.last_response = Instant::now();
            match response {
                Response::Ready {
                    schema_version,
                    descriptor,
                } => {
                    ensure!(
                        !self.ready && schema_version == 1 && descriptor == self.descriptor,
                        "remote attachment identity or protocol changed"
                    );
                    self.ready = true;
                }
                Response::Ack { id, control, error } => {
                    ensure!(
                        self.ready && id <= self.serial,
                        "unexpected SSH acknowledgement"
                    );
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
                            && self.bytes.len() + bytes.len() <= 1024 * 1024,
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
        self.wire.flush(&mut self.input)?;
        Ok(())
    }
    fn enqueue(&mut self, action: Action) -> Result<u64> {
        ensure!(self.ready && !self.ended, "remote invocation unavailable");
        self.serial += 1;
        self.wire.queue(&Request {
            id: self.serial,
            action,
        })?;
        Ok(self.serial)
    }
    fn request(&mut self, action: Action) -> Result<()> {
        let id = self.enqueue(action)?;
        self.waiting = Some((id, None));
        let start = Instant::now();
        loop {
            self.poll()?;
            ensure!(!self.ended, "remote invocation ended");
            if matches!(self.waiting, Some((0, _))) {
                let (_, error) = self.waiting.take().unwrap();
                if let Some(error) = error {
                    bail!("{error}");
                }
                return Ok(());
            }
            ensure!(
                start.elapsed() < Duration::from_secs(8),
                "SSH request timed out"
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
