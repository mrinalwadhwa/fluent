//! Keep Crossterm as the input owner across Dashboard view changes.
use anyhow::{Context, Result, ensure};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

// Permit a slow display to resume, but never let it own local teardown.
const RESTORE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(crate) struct DisplayRestoreFailed;

impl std::fmt::Display for DisplayRestoreFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("terminal display unavailable; dashboard closed without repaint")
    }
}

impl std::error::Error for DisplayRestoreFailed {}

pub(super) struct RawTerminal {
    pub output: File,
    raw: bool,
}
impl RawTerminal {
    pub fn restore_display(&mut self, mut service: impl FnMut() -> Result<()>) -> Result<()> {
        // Discard only after leaving full-screen. Cancel any partial terminal
        // sequence and reset display modes before the dashboard paints again.
        let mut reset: &[u8] = b"\x18\x1b[!p\x1b[0m\x1b[?25h";
        let mut transport_error = None;
        let deadline = Instant::now() + RESTORE_TIMEOUT;
        let restored = (|| -> Result<()> {
            while !reset.is_empty() {
                ensure!(
                    Instant::now() < deadline,
                    "full-screen display restoration timed out"
                );
                let serviced = service();
                if transport_error.is_none() {
                    transport_error = serviced.err();
                }
                let mut ready = libc::pollfd {
                    fd: self.output.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                let polled = unsafe { libc::poll(&mut ready, 1, 0) };
                if polled < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != std::io::ErrorKind::Interrupted {
                        return Err(error.into());
                    }
                } else if polled > 0 {
                    match self.output.write(reset) {
                        Ok(0) => anyhow::bail!("full-screen terminal output closed"),
                        Ok(n) => reset = &reset[n..],
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                if !reset.is_empty() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(())
        })();
        if let Err(error) = restored {
            // Full-screen has ended. Discard its queued paint and attempt one
            // nonblocking reset; neither emulator progress nor a successful
            // write is required to release raw mode or settle the attachment.
            unsafe { libc::tcflush(self.output.as_raw_fd(), libc::TCOFLUSH) };
            let _ = self.output.write(
                b"\x18\x1b[!p\x1b[0m\x1b[?25h\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1015l\x1b[?1006l\x1b[?2004l\x1b[?1049l",
            );
            return Err(error.context(DisplayRestoreFailed));
        }
        match transport_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn new() -> Result<Self> {
        // stdout may share stdin's file description. A separate open keeps
        // both inherited flags intact, including on setup and write failures.
        let output = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open("/dev/tty")
            .context("opening full-screen terminal output")?;
        let mut guard = Self { output, raw: false };
        crossterm::terminal::enable_raw_mode()?;
        guard.raw = true;
        Ok(guard)
    }
}
impl Drop for RawTerminal {
    fn drop(&mut self) {
        if self.raw {
            let _ = crossterm::terminal::disable_raw_mode();
        }
    }
}
