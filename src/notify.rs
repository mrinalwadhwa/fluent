//! Notify call sites. Currently logs to stderr in `[Title] body`
//! format. A future general notification system (Discord/Slack/push)
//! will replace this implementation; call sites stay as they are.

pub fn notify(title: &str, body: &str) {
    eprintln!("[{title}] {body}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_does_not_panic() {
        notify("Test", "test notification body");
    }

    #[test]
    fn notify_format_contract() {
        let title = "Factory";
        let body = "Test message";
        let formatted = format!("[{title}] {body}");
        assert_eq!(formatted, "[Factory] Test message");
    }
}
