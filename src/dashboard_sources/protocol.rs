//! Bound each message and buffered write on the persistent SSH attachment.
use crate::terminal_sessions::TerminalDescriptor;
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const MAX_MESSAGE: usize = 384 * 1024;
const MAX_BUFFER: usize = 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u64,
    pub action: Action,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    Control,
    Observe,
    Repaint,
    Ping,
    Input { bytes: Vec<u8> },
    Reply { bytes: Vec<u8> },
    Resize { columns: u16, rows: u16 },
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Response {
    Ready {
        schema_version: u32,
        descriptor: TerminalDescriptor,
    },
    Ack {
        id: u64,
        control: bool,
        error: Option<String>,
    },
    Output {
        bytes: Vec<u8>,
    },
    Detached,
    Ended,
}

#[derive(Default)]
pub struct Wire {
    input: Vec<u8>,
    output: Vec<u8>,
    offset: usize,
}
impl Wire {
    pub fn queue(&mut self, message: &impl Serialize) -> Result<()> {
        let mut bytes = serde_json::to_vec(message)?;
        ensure!(bytes.len() <= MAX_MESSAGE, "SSH message exceeds bound");
        bytes.push(b'\n');
        if self.offset > 0 {
            self.output.drain(..self.offset);
            self.offset = 0;
        }
        ensure!(
            self.output.len() + bytes.len() <= MAX_BUFFER,
            "SSH output stalled; disconnecting"
        );
        self.output.extend(bytes);
        Ok(())
    }
    pub fn flush(&mut self, writer: &mut impl Write) -> Result<()> {
        if self.offset < self.output.len() {
            match writer.write(&self.output[self.offset..]) {
                Ok(0) => bail!("SSH disconnected"),
                Ok(n) => self.offset += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
        if self.offset == self.output.len() {
            self.output.clear();
            self.offset = 0;
        }
        Ok(())
    }
    pub fn read<T: serde::de::DeserializeOwned>(
        &mut self,
        reader: &mut impl Read,
    ) -> Result<Vec<T>> {
        let mut buffer = [0; 8192];
        match reader.read(&mut buffer) {
            Ok(0) => bail!("SSH disconnected"),
            Ok(n) => self.input.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        let mut messages = Vec::new();
        while let Some(end) = self.input.iter().position(|b| *b == b'\n') {
            ensure!(end <= MAX_MESSAGE, "SSH message exceeds bound");
            messages.push(serde_json::from_slice(&self.input[..end])?);
            self.input.drain(..=end);
        }
        ensure!(self.input.len() <= MAX_MESSAGE, "SSH message exceeds bound");
        Ok(messages)
    }
}
