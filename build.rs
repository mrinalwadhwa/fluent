use std::process::Command;

fn main() {
    print_git_rerun_paths();

    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|stdout| stdout.trim().to_string())
        .filter(|commit| !commit.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=FACTORY_BUILD_COMMIT={commit}");
}

fn print_git_rerun_paths() {
    for args in [
        ["rev-parse", "--git-path", "HEAD"].as_slice(),
        ["rev-parse", "--git-path", "index"].as_slice(),
    ] {
        if let Some(path) = git_stdout(args) {
            println!("cargo:rerun-if-changed={path}");
        }
    }

    if let Some(head_ref) = git_stdout(["symbolic-ref", "-q", "HEAD"].as_slice())
        && let Some(path) = git_stdout(["rev-parse", "--git-path", &head_ref].as_slice())
    {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn git_stdout(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|stdout| stdout.trim().to_string())
        .filter(|stdout| !stdout.is_empty())
}
