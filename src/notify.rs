use std::process::Command;

/// Send a desktop notification. On macOS, use osascript to display a
/// native notification. On other platforms, log to stderr.
pub fn notify(title: &str, body: &str) {
    if cfg!(target_os = "macos") {
        notify_macos(title, body);
    } else {
        eprintln!("[{title}] {body}");
    }
}

fn notify_macos(title: &str, body: &str) {
    let escaped_title = title.replace('\\', "\\\\").replace('"', "\\\"");
    let escaped_body = body.replace('\\', "\\\\").replace('"', "\\\"");
    Command::new("osascript")
        .args([
            "-e",
            &format!("display notification \"{escaped_body}\" with title \"{escaped_title}\""),
        ])
        .output()
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_does_not_panic() {
        // On non-macOS this logs to stderr; on macOS it shells out to
        // osascript. Either way, it must not panic.
        notify("Test", "test notification body");
    }
}
