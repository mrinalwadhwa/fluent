use std::path::Path;

pub fn render_review_diff_command(workspace: &Path, range: &str) -> String {
    format!(
        "git -C {} diff {}",
        shell_quote(&workspace.display().to_string()),
        shell_quote(range)
    )
}

fn shell_quote(word: &str) -> String {
    if word.is_empty() {
        return "''".to_string();
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    #[test]
    fn review_diff_command_survives_apostrophes_through_sh() {
        let tmp = tempfile::TempDir::new().unwrap();
        let bin_dir = tmp.path().join("bin");
        fs::create_dir(&bin_dir).unwrap();
        let git_path = bin_dir.join("git");
        let log_path = tmp.path().join("args.log");
        let script = format!(
            "#!/bin/sh\nprintf '<%s>\\n' \"$@\" > '{}'\n",
            log_path.display()
        );
        fs::write(&git_path, script).unwrap();
        let mut permissions = fs::metadata(&git_path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&git_path, permissions).unwrap();

        let injected_path = tmp
            .path()
            .join("work'space'; touch adjacent-path-ran; echo '");
        let injected_range = "main's; touch adjacent-range-ran; echo '..abc'123";
        let command = render_review_diff_command(&injected_path, injected_range);
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(&command)
            .env("PATH", format!("{}:/usr/bin:/bin", bin_dir.display()))
            .current_dir(tmp.path())
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "command failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!tmp.path().join("adjacent-path-ran").exists());
        assert!(!tmp.path().join("adjacent-range-ran").exists());
        assert_eq!(
            fs::read_to_string(log_path).unwrap(),
            format!(
                "<-C>\n<{}>\n<diff>\n<{}>\n",
                injected_path.display(),
                injected_range
            )
        );
    }
}
