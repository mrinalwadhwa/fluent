use assert_cmd::Command;
use assert_cmd::assert::{Assert, OutputAssertExt};
use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Output;

pub struct LoggedCommand {
    inner: Command,
    cmd_args: Vec<String>,
}

impl LoggedCommand {
    pub fn cargo_bin(name: &str) -> Self {
        Self {
            inner: Command::cargo_bin(name).unwrap(),
            cmd_args: vec![name.to_string()],
        }
    }

    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        if let Some(s) = arg.as_ref().to_str() {
            self.cmd_args.push(s.to_string());
        }
        self.inner.arg(arg);
        self
    }

    pub fn args<I, S>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args_vec: Vec<S> = args.into_iter().collect();
        for a in &args_vec {
            if let Some(s) = a.as_ref().to_str() {
                self.cmd_args.push(s.to_string());
            }
        }
        self.inner.args(args_vec);
        self
    }

    pub fn current_dir<P: AsRef<std::path::Path>>(&mut self, dir: P) -> &mut Self {
        self.inner.current_dir(dir);
        self
    }

    pub fn env<K, V>(&mut self, key: K, val: V) -> &mut Self
    where
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        self.inner.env(key, val);
        self
    }

    pub fn env_remove<K: AsRef<OsStr>>(&mut self, key: K) -> &mut Self {
        self.inner.env_remove(key);
        self
    }

    pub fn write_stdin<S: Into<Vec<u8>>>(&mut self, buffer: S) -> &mut Self {
        self.inner.write_stdin(buffer);
        self
    }

    pub fn output(&mut self) -> Result<Output, std::io::Error> {
        let output = self.inner.output()?;
        write_log(&self.cmd_args, &output);
        Ok(output)
    }

    pub fn assert(&mut self) -> Assert {
        let output = self.inner.output().expect("failed to execute command");
        write_log(&self.cmd_args, &output);
        output.assert()
    }
}

fn write_log(cmd_args: &[String], output: &Output) {
    if std::env::var("FACTORY_TESTS_SKIP_LOG").as_deref() == Ok("1") {
        return;
    }

    let test_name = current_test_name();
    let log_dir = log_dir_path();

    if let Err(e) = fs::create_dir_all(&log_dir) {
        eprintln!(
            "warning: cannot create test log directory {}: {e}",
            log_dir.display()
        );
        return;
    }

    let log_path = log_dir.join(format!("{test_name}.log"));
    let exit_display = output
        .status
        .code()
        .map_or("signal".to_string(), |c| c.to_string());
    let content = format!(
        "=== {test_name} ===\ncommand: {}\nexit: {exit_display}\n---stdout---\n{}---stderr---\n{}",
        cmd_args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    if let Err(e) = fs::write(&log_path, &content) {
        eprintln!(
            "warning: cannot write test log to {}: {e}",
            log_path.display()
        );
        return;
    }

    if !output.status.success() {
        let failed_path = log_dir.join(".failed");
        let abs_log = fs::canonicalize(&log_path).unwrap_or(log_path);
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&failed_path)
        {
            let _ = writeln!(f, "{}", abs_log.display());
        }
    }
}

fn current_test_name() -> String {
    std::thread::current()
        .name()
        .unwrap_or("unknown_test")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn log_dir_path() -> PathBuf {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(manifest).join("tests").join("output")
}

#[allow(dead_code)]
pub fn print_failed_summary() {
    let log_dir = log_dir_path();
    let failed_path = log_dir.join(".failed");
    let content = match fs::read_to_string(&failed_path) {
        Ok(c) if !c.trim().is_empty() => c,
        _ => return,
    };

    eprintln!("\nFailing case logs:");
    for line in content.lines() {
        let path = line.trim();
        if path.is_empty() {
            continue;
        }
        eprintln!("  {path}");
        if let Ok(log_content) = fs::read_to_string(path) {
            eprintln!("  --- last 20 lines ---");
            let lines: Vec<&str> = log_content.lines().collect();
            let start = lines.len().saturating_sub(20);
            for l in &lines[start..] {
                eprintln!("    {l}");
            }
        }
    }
}

#[allow(dead_code)]
pub fn test_log_dir_path() -> PathBuf {
    log_dir_path()
}

#[allow(dead_code)]
pub fn test_current_test_name() -> String {
    current_test_name()
}

#[allow(dead_code)]
pub fn clear_failed_sentinel() {
    let log_dir = log_dir_path();
    let failed_path = log_dir.join(".failed");
    let _ = fs::remove_file(failed_path);
}
