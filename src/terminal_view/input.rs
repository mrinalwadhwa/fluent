use super::Screen;
use anyhow::{Result, bail, ensure};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use std::ffi::c_void;

pub const MAX_PASTE_BYTES: usize = 64 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Input {
    Bytes(Vec<u8>),
    ReleaseFocus,
}

pub fn releases_focus(event: &Event) -> bool {
    // Crossterm decodes legacy byte 0x1d as Ctrl-5. Enhanced keyboard
    // protocols can report Ctrl-] directly; reserve both representations.
    matches!(event, Event::Key(key) if key.kind != KeyEventKind::Release
        && matches!(key.code, KeyCode::Char(']' | '5'))
        && key.modifiers == KeyModifiers::CONTROL)
}

fn mods(value: KeyModifiers) -> u16 {
    u16::from(value.contains(KeyModifiers::SHIFT))
        | (u16::from(value.contains(KeyModifiers::CONTROL)) << 1)
        | (u16::from(value.contains(KeyModifiers::ALT)) << 2)
        | (u16::from(value.contains(KeyModifiers::SUPER)) << 3)
}

unsafe extern "C" {
    fn fluent_vt_key(
        vt: *mut c_void,
        special: u32,
        cp: u32,
        mods: u16,
        action: i32,
        text: *const u8,
        len: usize,
        out: *mut u8,
        cap: usize,
        written: *mut usize,
    ) -> i32;
    fn fluent_vt_paste(
        vt: *mut c_void,
        text: *mut u8,
        len: usize,
        out: *mut u8,
        cap: usize,
        written: *mut usize,
    ) -> i32;
    fn fluent_vt_mouse(
        vt: *mut c_void,
        action: i32,
        button: i32,
        mods: u16,
        x: u16,
        y: u16,
        out: *mut u8,
        cap: usize,
        written: *mut usize,
    ) -> i32;
}

impl Screen {
    /// Encode operator events against the tmux client's current terminal modes.
    pub fn input(&mut self, event: Event) -> Result<Input> {
        if releases_focus(&event) {
            return Ok(Input::ReleaseFocus);
        }
        self.native_input(event)
    }

    fn native_input(&mut self, event: Event) -> Result<Input> {
        let mut out = vec![0; 256];
        let mut written = 0;
        let result = match event {
            Event::Paste(text) => {
                ensure!(
                    text.len() <= MAX_PASTE_BYTES,
                    "paste exceeds 64 KiB; paste smaller sections"
                );
                let mut text = text.into_bytes();
                out.resize(text.len() + 16, 0);
                unsafe {
                    fluent_vt_paste(
                        self.raw(),
                        text.as_mut_ptr(),
                        text.len(),
                        out.as_mut_ptr(),
                        out.len(),
                        &mut written,
                    )
                }
            }
            Event::Key(key) => {
                let mut text = String::new();
                let mut cp = 0;
                let mut modifiers = key.modifiers;
                let special = match key.code {
                    KeyCode::Char(ch) => {
                        cp = ch.to_ascii_lowercase() as u32;
                        text.push(ch);
                        0
                    }
                    KeyCode::Enter => 1,
                    KeyCode::Esc => 2,
                    KeyCode::Backspace => 3,
                    KeyCode::Tab => 4,
                    KeyCode::BackTab => {
                        modifiers |= KeyModifiers::SHIFT;
                        4
                    }
                    KeyCode::Up => 5,
                    KeyCode::Down => 6,
                    KeyCode::Left => 7,
                    KeyCode::Right => 8,
                    KeyCode::Home => 9,
                    KeyCode::End => 10,
                    KeyCode::PageUp => 11,
                    KeyCode::PageDown => 12,
                    KeyCode::Insert => 13,
                    KeyCode::Delete => 14,
                    KeyCode::F(n) if (1..=25).contains(&n) => 100 + u32::from(n),
                    KeyCode::Null => {
                        cp = ' ' as u32;
                        modifiers |= KeyModifiers::CONTROL;
                        0
                    }
                    _ => bail!(
                        "this key cannot be encoded by the embedded terminal; use full-screen attachment"
                    ),
                };
                let action = match key.kind {
                    KeyEventKind::Press => 1,
                    KeyEventKind::Repeat => 2,
                    KeyEventKind::Release => 0,
                };
                unsafe {
                    fluent_vt_key(
                        self.raw(),
                        special,
                        cp,
                        mods(modifiers),
                        action,
                        text.as_ptr(),
                        text.len(),
                        out.as_mut_ptr(),
                        out.len(),
                        &mut written,
                    )
                }
            }
            Event::Mouse(mouse) => {
                let button = |b| match b {
                    MouseButton::Left => 1,
                    MouseButton::Right => 2,
                    MouseButton::Middle => 3,
                };
                let (action, b) = match mouse.kind {
                    MouseEventKind::Down(b) => (0, button(b)),
                    MouseEventKind::Up(b) => (1, button(b)),
                    MouseEventKind::Drag(b) => (2, button(b)),
                    MouseEventKind::Moved => (2, 0),
                    MouseEventKind::ScrollUp => (0, 4),
                    MouseEventKind::ScrollDown => (0, 5),
                    MouseEventKind::ScrollLeft => (0, 6),
                    MouseEventKind::ScrollRight => (0, 7),
                };
                unsafe {
                    fluent_vt_mouse(
                        self.raw(),
                        action,
                        b,
                        mods(mouse.modifiers),
                        mouse.column,
                        mouse.row,
                        out.as_mut_ptr(),
                        out.len(),
                        &mut written,
                    )
                }
            }
            _ => return Ok(Input::Bytes(Vec::new())),
        };
        ensure!(
            result == 0 && written <= out.len(),
            "encoding terminal input failed"
        );
        out.truncate(written);
        Ok(Input::Bytes(out))
    }
}

/// Encode native full-screen keys without the embedded navigation escape or
/// prefix quoting. Both views keep using Crossterm's same pending input state.
pub(super) fn fullscreen_input(screen: &mut Screen, mut event: Event) -> Result<Input> {
    // Legacy Crossterm reports byte 0x1d as Ctrl-5. Normalize that spelling
    // before encoding against the native client's current keyboard protocol.
    if let Event::Key(key) = &mut event {
        if key.code == KeyCode::Char('5') && key.modifiers == KeyModifiers::CONTROL {
            key.code = KeyCode::Char(']');
        }
    }
    screen.native_input(event)
}

/// Honor native detach in Observe without encoding or forwarding user input.
pub(super) fn observe_detach(event: &Event, prefix: &mut bool) -> bool {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => {
            let detach = *prefix && key.code == KeyCode::Char('d') && key.modifiers.is_empty();
            *prefix = key.code == KeyCode::Char('b') && key.modifiers == KeyModifiers::CONTROL;
            detach
        }
        Event::Mouse(_) | Event::Paste(_) => {
            *prefix = false;
            false
        }
        _ => false,
    }
}
