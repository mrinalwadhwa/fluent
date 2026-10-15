use super::{Frame, Screen, screen::dimensions};
use crate::terminal_sessions::Attachment;
use anyhow::{Context, Result, ensure};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::Child;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

struct Update {
    frame: Option<Frame>,
    error: Option<String>,
}

/// A client owns only the display PTY. Dropping it detaches without cancelling
/// the invocation; the execution runtime remains the authority for liveness.
pub struct EmbeddedTerminal {
    attachment: Attachment,
    child: Child,
    stop: Arc<AtomicBool>,
    reader: Option<JoinHandle<()>>,
    latest: Arc<Mutex<Update>>,
    frame: Frame,
    pub diagnostic: Option<String>,
}
impl EmbeddedTerminal {
    pub fn observe(attachment: Attachment, columns: u16, rows: u16) -> Result<Self> {
        dimensions(columns, rows)?;
        let frame = Screen::new(columns, rows)?.snapshot()?;
        let (master, slave) = open_pty(columns, rows)?;
        let mut child = attachment.spawn_in_pty(&slave)?;
        drop(slave);
        let latest = Arc::new(Mutex::new(Update {
            frame: None,
            error: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let latest = Arc::clone(&latest);
            let stop = Arc::clone(&stop);
            match thread::Builder::new()
                .name("terminal-display".into())
                .spawn(move || {
                    if let Err(error) = drain(master, columns, rows, &stop, &latest) {
                        latest.lock().unwrap().error = Some(format!("{error:#}"));
                    }
                }) {
                Ok(reader) => reader,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error.into());
                }
            }
        };
        Ok(Self {
            attachment,
            child,
            stop,
            reader: Some(reader),
            latest,
            frame,
            diagnostic: None,
        })
    }
    pub fn frame(&self) -> &Frame {
        &self.frame
    }
    pub fn descriptor(&self) -> &crate::terminal_sessions::TerminalDescriptor {
        self.attachment.descriptor()
    }
    pub fn refresh(&mut self) -> bool {
        let mut latest = self.latest.lock().unwrap();
        let changed = latest.frame.is_some() || latest.error.is_some();
        if let Some(frame) = latest.frame.take() {
            self.frame = frame;
        }
        if let Some(error) = latest.error.take() {
            self.diagnostic = Some(error);
        }
        changed
    }
}
impl Drop for EmbeddedTerminal {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn open_pty(columns: u16, rows: u16) -> Result<(File, File)> {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: rows,
        ws_col: columns,
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

fn drain(
    mut master: File,
    columns: u16,
    rows: u16,
    stop: &AtomicBool,
    latest: &Mutex<Update>,
) -> Result<()> {
    let mut screen = Screen::new(columns, rows)?;
    let mut bytes = [0u8; 8192];
    let mut next_frame = Instant::now();
    let mut dirty = false;
    while !stop.load(Ordering::Relaxed) {
        match master.read(&mut bytes) {
            Ok(0) => break,
            Ok(n) => {
                screen.write(&bytes[..n]);
                dirty = true;
                let replies = screen.replies();
                if !replies.is_empty() {
                    master
                        .write_all(&replies)
                        .context("terminal reply lost; reconnect to repaint")?;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => return Err(error.into()),
        }
        if dirty && Instant::now() >= next_frame {
            let frame = screen.snapshot()?;
            latest.lock().unwrap().frame = Some(frame);
            dirty = false;
            next_frame = Instant::now() + Duration::from_millis(33);
        }
    }
    if dirty {
        latest.lock().unwrap().frame = Some(screen.snapshot()?);
    }
    Ok(())
}
