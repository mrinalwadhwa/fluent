//! Notify call sites. Currently logs to stderr in `[Title] body`
//! format. A future general notification system (Discord/Slack/push)
//! will replace this implementation; call sites stay as they are.

fn format_notification(title: &str, body: &str) -> String {
    format!("[{title}] {body}")
}

pub fn notify(title: &str, body: &str) {
    eprintln!("{}", format_notification(title, body));
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
        assert_eq!(format_notification("Factory", "Test message"), "[Factory] Test message");
        assert_eq!(format_notification("Rate Limit", "paused for 30s"), "[Rate Limit] paused for 30s");
    }
}
