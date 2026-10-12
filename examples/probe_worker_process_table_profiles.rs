//! Exercise the production worker profile renderers during the host check.

use fluent::coder::CoderKind;
use fluent::content::ContentResolver;
use fluent::os;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let probe = PathBuf::from(args.next().ok_or("missing process-table probe")?);
    let fixture = PathBuf::from(args.next().ok_or("missing fixture root")?);
    let evidence = PathBuf::from(args.next().ok_or("missing evidence directory")?);
    if args.next().is_some() {
        return Err("unexpected argument".into());
    }
    let fixture = fs::canonicalize(fixture)?;
    let evidence = fs::canonicalize(evidence)?;
    let home = std::env::var("HOME")?;
    let resolver = ContentResolver::new(None);

    for role in ["writer", "reviewer", "learner"] {
        let writable = fixture.join(format!("{role}-writable"));
        fs::create_dir_all(&writable)?;
        let readable = [fixture.clone()];
        // Writer and Reviewer use the ordinary coder renderer. Learner adds
        // denied writes through the same restricted renderer used by its Task.
        let profile = if role == "learner" {
            os::render_profile_for_access_for_coder_with_denied_writes(
                &resolver,
                &home,
                &[writable],
                &readable,
                &[fixture.join("writer-workspace")],
                CoderKind::Claude,
            )?
        } else {
            os::render_profile_for_access_for_coder(
                &resolver,
                &home,
                &[writable],
                &readable,
                CoderKind::Claude,
            )?
        };
        fs::copy(&profile.path, evidence.join(format!("{role}-profile.sb")))?;
        let output = Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(&profile.path)
            .arg(&probe)
            .arg("denied")
            .output()?;
        fs::write(
            evidence.join(format!("{role}-probe.stdout")),
            &output.stdout,
        )?;
        fs::write(
            evidence.join(format!("{role}-probe.stderr")),
            &output.stderr,
        )?;
        fs::write(
            evidence.join(format!("{role}-probe.result")),
            format!(
                "expected=denied\nobserved={}\nexit_code={}\n",
                if output.status.success() {
                    "denied"
                } else {
                    "not-denied"
                },
                output.status.code().unwrap_or(-1)
            ),
        )?;
        if !output.status.success() {
            return Err(format!("{role} profile did not deny the process-table query").into());
        }
    }
    Ok(())
}
