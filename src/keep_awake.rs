use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const LABEL: &str = "com.factory.keep-awake";
const SENTINEL: &str = "factory/keep-awake-caffeinate";

const PLIST_TEMPLATE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.factory.keep-awake</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/sh</string>
        <string>{wrapper_path}</string>
    </array>
    <key>RunAtLoad</key>
    {run_at_load}
    <key>KeepAlive</key>
    {keep_alive}
</dict>
</plist>
"#;

const WRAPPER_SCRIPT: &str = r#"#!/bin/sh
/usr/bin/caffeinate -i &
CPID=$!
trap "kill $CPID 2>/dev/null; exit 0" TERM INT HUP
wait $CPID
"#;

pub enum Subcommand {
    On,
    Off,
    Status,
    Uninstall,
}

pub fn run(sub: Subcommand) -> Result<()> {
    ensure_macos()?;
    match sub {
        Subcommand::On => handle_on(),
        Subcommand::Off => handle_off(),
        Subcommand::Status => handle_status(),
        Subcommand::Uninstall => handle_uninstall(),
    }
}

fn ensure_macos() -> Result<()> {
    if cfg!(not(target_os = "macos")) {
        bail!("factory keep-awake is macOS-only");
    }
    Ok(())
}

fn home_dir() -> Result<PathBuf> {
    std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| anyhow::anyhow!("HOME not set"))
}

fn wrapper_script_path() -> Result<PathBuf> {
    Ok(home_dir()?.join(".config/factory/keep-awake-caffeinate"))
}

fn launch_agent_plist_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("Library/LaunchAgents/com.factory.keep-awake.plist"))
}

fn get_uid() -> Result<String> {
    let output = Command::new("id").args(["-u"]).output()?;
    if !output.status.success() {
        bail!("id -u failed");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// --- Process discovery ---

fn find_wrapper_pid() -> Option<u32> {
    let output = Command::new("pgrep").args(["-f", SENTINEL]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .lines()
        .next()?
        .parse()
        .ok()
}

fn find_caffeinate_child_pid(wrapper_pid: u32) -> Option<u32> {
    let output = Command::new("pgrep")
        .args(["-P", &wrapper_pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .lines()
        .next()?
        .parse()
        .ok()
}

fn find_running_pid() -> Option<u32> {
    let wrapper_pid = find_wrapper_pid()?;
    find_caffeinate_child_pid(wrapper_pid).or(Some(wrapper_pid))
}

fn wait_for_running_pid(timeout: Duration) -> Result<u32> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(pid) = find_running_pid() {
            return Ok(pid);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("caffeinate did not start within {timeout:?}");
}

// --- Plist manipulation ---

fn write_plist(path: &Path, wrapper_path: &Path, enabled: bool) -> Result<()> {
    let flag = if enabled { "<true/>" } else { "<false/>" };
    let content = PLIST_TEMPLATE
        .replace("{wrapper_path}", &wrapper_path.to_string_lossy())
        .replace("{run_at_load}", flag)
        .replace("{keep_alive}", flag);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    Ok(())
}

fn read_keepalive_flag(path: &Path) -> Result<bool> {
    let content = std::fs::read_to_string(path)?;
    Ok(content.contains("<key>KeepAlive</key>\n    <true/>"))
}

fn write_wrapper_script(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, WRAPPER_SCRIPT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

// --- launchctl helpers ---

fn bootstrap_launch_agent(plist: &Path) -> Result<()> {
    let uid = get_uid()?;
    let status = Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{uid}"), &plist.to_string_lossy()])
        .status()?;
    if !status.success() {
        bail!(
            "launchctl bootstrap failed (exit {})",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

fn bootout_launch_agent() -> Result<()> {
    let uid = get_uid()?;
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("gui/{uid}/{LABEL}")])
        .status();
    Ok(())
}

fn reload_launch_agent(plist: &Path) -> Result<()> {
    bootout_launch_agent()?;
    bootstrap_launch_agent(plist)
}

fn kickstart_launch_agent() -> Result<()> {
    let uid = get_uid()?;
    let status = Command::new("launchctl")
        .args(["kickstart", &format!("gui/{uid}/{LABEL}")])
        .status()?;
    if !status.success() {
        bail!(
            "launchctl kickstart failed (exit {})",
            status.code().unwrap_or(-1)
        );
    }
    Ok(())
}

// --- Process control ---

fn send_sigterm(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()?;
    if !status.success() {
        bail!("kill -TERM {pid} failed");
    }
    Ok(())
}

fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn wait_for_exit(pid: u32, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if !pid_alive(pid) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("PID {pid} did not exit within {timeout:?}");
}

// --- Subcommand handlers ---

fn handle_on() -> Result<()> {
    if let Some(pid) = find_running_pid() {
        println!("keep-awake already on (caffeinate PID {pid})");
        return Ok(());
    }

    let wrapper_path = wrapper_script_path()?;
    let plist_path = launch_agent_plist_path()?;
    let plist_exists = plist_path.exists();

    if !plist_exists {
        write_wrapper_script(&wrapper_path)?;
        write_plist(&plist_path, &wrapper_path, true)?;
        bootstrap_launch_agent(&plist_path)?;
        println!("LaunchAgent installed at {}", plist_path.display());
    } else {
        let current = read_keepalive_flag(&plist_path)?;
        if !current {
            write_wrapper_script(&wrapper_path)?;
            write_plist(&plist_path, &wrapper_path, true)?;
            reload_launch_agent(&plist_path)?;
        } else {
            write_wrapper_script(&wrapper_path)?;
            kickstart_launch_agent()?;
        }
    }

    let pid = wait_for_running_pid(Duration::from_secs(3))?;
    println!("keep-awake on (caffeinate PID {pid})");
    Ok(())
}

fn handle_off() -> Result<()> {
    let wrapper_pid = find_wrapper_pid();
    let caffeinate_pid = wrapper_pid.and_then(find_caffeinate_child_pid);
    let plist_path = launch_agent_plist_path()?;
    let plist_exists = plist_path.exists();

    let keepalive_on = if plist_exists {
        read_keepalive_flag(&plist_path).unwrap_or(false)
    } else {
        false
    };

    if wrapper_pid.is_none() && !keepalive_on {
        println!("keep-awake already off");
        return Ok(());
    }

    if plist_exists {
        bootout_launch_agent()?;
        let wrapper_path = wrapper_script_path()?;
        write_plist(&plist_path, &wrapper_path, false)?;
    }

    if let Some(pid) = wrapper_pid {
        let _ = wait_for_exit(pid, Duration::from_secs(5));
    }

    if let Some(pid) = caffeinate_pid {
        if pid_alive(pid) {
            let _ = send_sigterm(pid);
            let _ = wait_for_exit(pid, Duration::from_secs(3));
        }
    }

    println!("keep-awake off");
    Ok(())
}

fn handle_status() -> Result<()> {
    match find_running_pid() {
        Some(pid) => println!("on (caffeinate PID {pid})"),
        None => println!("off"),
    }
    Ok(())
}

fn handle_uninstall() -> Result<()> {
    let wrapper_pid = find_wrapper_pid();
    let caffeinate_pid = wrapper_pid.and_then(find_caffeinate_child_pid);

    if let Some(pid) = wrapper_pid {
        bootout_launch_agent()?;
        let _ = wait_for_exit(pid, Duration::from_secs(5));
    }
    if let Some(pid) = caffeinate_pid {
        if pid_alive(pid) {
            let _ = send_sigterm(pid);
            let _ = wait_for_exit(pid, Duration::from_secs(3));
        }
    }

    let plist_path = launch_agent_plist_path()?;
    let wrapper_path = wrapper_script_path()?;
    let had_plist = plist_path.exists();
    let had_wrapper = wrapper_path.exists();

    if had_plist {
        if wrapper_pid.is_none() {
            bootout_launch_agent()?;
        }
        std::fs::remove_file(&plist_path)?;
    }
    if had_wrapper {
        std::fs::remove_file(&wrapper_path)?;
    }

    if had_plist || had_wrapper || wrapper_pid.is_some() {
        println!("keep-awake LaunchAgent uninstalled");
    } else {
        println!("keep-awake LaunchAgent already uninstalled");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn plist_round_trips_keepalive_true() {
        let dir = tempfile::tempdir().unwrap();
        let plist = dir.path().join("test.plist");
        let wrapper = dir.path().join("wrapper");
        write_plist(&plist, &wrapper, true).unwrap();
        assert!(read_keepalive_flag(&plist).unwrap());
    }

    #[test]
    fn plist_round_trips_keepalive_false() {
        let dir = tempfile::tempdir().unwrap();
        let plist = dir.path().join("test.plist");
        let wrapper = dir.path().join("wrapper");
        write_plist(&plist, &wrapper, false).unwrap();
        assert!(!read_keepalive_flag(&plist).unwrap());
    }

    #[test]
    fn read_keepalive_flag_parses_both_states() {
        let dir = tempfile::tempdir().unwrap();
        let plist = dir.path().join("test.plist");
        let wrapper = dir.path().join("wrapper");

        write_plist(&plist, &wrapper, true).unwrap();
        assert!(read_keepalive_flag(&plist).unwrap());

        write_plist(&plist, &wrapper, false).unwrap();
        assert!(!read_keepalive_flag(&plist).unwrap());
    }

    #[test]
    fn plist_contains_valid_xml_structure() {
        let dir = tempfile::tempdir().unwrap();
        let plist = dir.path().join("test.plist");
        let wrapper = PathBuf::from("/mock/.config/factory/keep-awake-caffeinate");
        write_plist(&plist, &wrapper, true).unwrap();
        let content = fs::read_to_string(&plist).unwrap();
        assert!(content.contains("com.factory.keep-awake"));
        assert!(content.contains("/bin/sh"));
        assert!(content.contains("/mock/.config/factory/keep-awake-caffeinate"));
        assert!(content.contains("<key>RunAtLoad</key>"));
        assert!(content.contains("<key>KeepAlive</key>"));
    }

    #[test]
    fn wrapper_script_is_valid_shell() {
        assert!(WRAPPER_SCRIPT.starts_with("#!/bin/sh\n"));
        assert!(WRAPPER_SCRIPT.contains("caffeinate -i"));
        assert!(WRAPPER_SCRIPT.contains("trap"));
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn ensure_macos_errors_on_non_macos() {
        assert!(ensure_macos().is_err());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn ensure_macos_succeeds_on_macos() {
        assert!(ensure_macos().is_ok());
    }

    #[test]
    fn write_then_read_keepalive_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let plist = dir.path().join("test.plist");
        let wrapper = dir.path().join("wrapper");

        for &enabled in &[true, false, true, false] {
            write_plist(&plist, &wrapper, enabled).unwrap();
            assert_eq!(read_keepalive_flag(&plist).unwrap(), enabled);
        }
    }

    #[test]
    fn write_wrapper_script_creates_executable() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("test-wrapper");
        write_wrapper_script(&script).unwrap();
        let content = fs::read_to_string(&script).unwrap();
        assert_eq!(content, WRAPPER_SCRIPT);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&script).unwrap().permissions().mode();
            assert_eq!(mode & 0o755, 0o755);
        }
    }
}
