use super::{Frame, client::LocalTerminal, remote::SshTerminal};
use crate::dashboard_sources::{OpenSsh, SshSource};
use crate::terminal_sessions::{Attachment, TerminalDescriptor};
use anyhow::Result;
use crossterm::event::Event;

/// Select a transport without moving execution or lease authority into rendering.
pub enum EmbeddedTerminal {
    Local(LocalTerminal),
    Ssh(SshTerminal),
}
macro_rules! dispatch {
    ($self:ident, $method:ident $(, $arg:expr)*) => {
        match $self { Self::Local(t) => t.$method($($arg),*), Self::Ssh(t) => t.$method($($arg),*) }
    };
}
impl EmbeddedTerminal {
    pub fn observe(attachment: Attachment, columns: u16, rows: u16) -> Result<Self> {
        Ok(Self::Local(LocalTerminal::observe(
            attachment, columns, rows,
        )?))
    }
    pub fn observe_ssh(
        ssh: OpenSsh,
        source: SshSource,
        descriptor: TerminalDescriptor,
        columns: u16,
        rows: u16,
    ) -> Result<Self> {
        Ok(Self::Ssh(SshTerminal::observe(
            ssh, source, descriptor, columns, rows,
        )?))
    }
    pub fn reconnect(&mut self) -> Result<()> {
        dispatch!(self, reconnect)
    }
    pub fn fullscreen(&mut self, columns: u16, rows: u16) -> Result<i32> {
        dispatch!(self, fullscreen, columns, rows)
    }
    pub fn frame(&self) -> &Frame {
        dispatch!(self, frame)
    }
    pub fn descriptor(&self) -> &TerminalDescriptor {
        dispatch!(self, descriptor)
    }
    pub fn focused(&self) -> bool {
        dispatch!(self, focused)
    }
    pub fn release_focus(&mut self) {
        dispatch!(self, release_focus)
    }
    pub fn ended(&self) -> bool {
        dispatch!(self, ended)
    }
    pub fn has_control(&self) -> bool {
        dispatch!(self, has_control)
    }
    pub fn state_label(&self) -> String {
        dispatch!(self, state_label)
    }
    pub fn acquire_control(&mut self) -> Result<()> {
        dispatch!(self, acquire_control)
    }
    pub fn observe_only(&mut self) -> Result<()> {
        dispatch!(self, observe_only)
    }
    pub fn send(&mut self, event: Event) -> Result<()> {
        dispatch!(self, send, event)
    }
    pub fn send_literal_escape(&mut self) -> Result<()> {
        dispatch!(self, send_literal_escape)
    }
    pub fn resize(&mut self, columns: u16, rows: u16) -> Result<()> {
        dispatch!(self, resize, columns, rows)
    }
    pub fn refresh(&mut self) -> bool {
        dispatch!(self, refresh)
    }
    pub fn identity(&self) -> String {
        match self {
            Self::Local(t) => super::identity(t.descriptor()),
            Self::Ssh(t) => t.identity(),
        }
    }
}
