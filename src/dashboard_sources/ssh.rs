use super::{MAX_SNAPSHOT_BYTES, SshSource, decode_snapshot};
use crate::dashboard::project::SourceSnapshot;
use anyhow::{Context, Result, bail, ensure};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct OpenSsh {
    executable: PathBuf,
}
impl Default for OpenSsh {
    fn default() -> Self {
        Self {
            executable: "/usr/bin/ssh".into(),
        }
    }
}
impl OpenSsh {
    #[cfg(feature = "test-support")]
    pub fn with_executable(executable: PathBuf) -> Self {
        Self { executable }
    }

    pub fn command(
        &self,
        source: &SshSource,
        verb: &str,
        operands: &[&str],
        pty: bool,
    ) -> Result<Command> {
        source.validate()?;
        ensure!(
            matches!(verb, "snapshot" | "terminal"),
            "unknown SSH helper verb"
        );
        let mut remote = vec![
            "fluent".to_string(),
            "agent-source".into(),
            verb.into(),
            "--project-root".into(),
            source.project_root.to_string_lossy().into_owned(),
        ];
        remote.extend(operands.iter().map(|s| s.to_string()));
        let command = remote
            .iter()
            .map(|arg| quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let mut ssh = Command::new(&self.executable);
        ssh.args([
            "-o",
            "BatchMode=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "ForwardX11=no",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "ServerAliveInterval=2",
            "-o",
            "ServerAliveCountMax=2",
            "-o",
            "EscapeChar=none",
        ])
        .arg(if pty { "-tt" } else { "-T" })
        .arg("--")
        .arg(&source.host)
        .arg(command);
        // SSH may use the operator's authentication agent, but never forwards it.
        // Do not put provider secrets or environment assignments on the wire.
        Ok(ssh)
    }

    pub fn snapshot(&self, source: &SshSource) -> Result<SourceSnapshot> {
        let result = (|| {
            let mut command = self.command(source, "snapshot", &[], false)?;
            let bytes = bounded_output(&mut command, Duration::from_secs(8))?;
            decode_snapshot(source, &bytes)
        })();
        result
            .map_err(|error: anyhow::Error| {
                anyhow::anyhow!("{}", super::display_diagnostic(&format!("{error:#}")))
            })
            .with_context(|| remedy(source))
    }
}

pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn remedy(source: &SshSource) -> String {
    format!(
        "SSH source {} ({}): establish SSH access and verify the host key with `ssh {}` outside the dashboard; ensure macOS, compatible Fluent and tmux are installed, and use the canonical project root {}",
        source.name,
        source.host,
        source.host,
        source.project_root.display()
    )
}

pub(crate) fn nonblocking(fd: &impl AsRawFd) -> Result<()> {
    let fd = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "setting SSH pipe nonblocking mode failed"
    );
    Ok(())
}

fn bounded_output(command: &mut Command, timeout: Duration) -> Result<Vec<u8>> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let result = (|| {
        let mut stdout = child.stdout.take().context("SSH stdout unavailable")?;
        let mut stderr = child.stderr.take().context("SSH stderr unavailable")?;
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let start = Instant::now();
        let mut output = Vec::new();
        let mut errors = Vec::new();
        let mut ended = [false; 2];
        loop {
            for (index, reader, bytes, limit) in [
                (
                    0,
                    &mut stdout as &mut dyn Read,
                    &mut output,
                    MAX_SNAPSHOT_BYTES,
                ),
                (1, &mut stderr as &mut dyn Read, &mut errors, 16 * 1024),
            ] {
                let mut buffer = [0; 8192];
                match reader.read(&mut buffer) {
                    Ok(0) => ended[index] = true,
                    Ok(n) => {
                        ensure!(
                            bytes.len() + n <= limit,
                            "SSH response exceeds protocol bound"
                        );
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.into()),
                }
            }
            if let Some(status) = child.try_wait()? {
                if ended == [true, true] {
                    ensure!(
                        status.success(),
                        "SSH helper failed: {}",
                        String::from_utf8_lossy(&errors)
                    );
                    return Ok(output);
                }
            }
            if start.elapsed() >= timeout {
                bail!("SSH helper timed out");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    result
}
