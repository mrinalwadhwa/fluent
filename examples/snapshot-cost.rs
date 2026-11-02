//! Run with --release --features snapshot-profile on an idle, readable checkout.
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use fluent::{dashboard::project::SourceSnapshot, snapshot_profile};
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use std::time::Instant;

    // Hash names, file types, links, and bytes outside the timed region. Never
    // follow links into another checkout or silently omit unreadable inputs.
    fn input_digest(root: &Path) -> anyhow::Result<String> {
        fn visit(path: &Path, digest: &mut Sha256) -> anyhow::Result<()> {
            let metadata = path.symlink_metadata()?;
            if metadata.is_dir() {
                digest.update(b"directory\0");
                let mut entries = std::fs::read_dir(path)?
                    .map(|entry| entry.map(|entry| entry.path()))
                    .collect::<Result<Vec<_>, _>>()?;
                entries.sort();
                for entry in entries {
                    let name = entry.file_name().unwrap().as_encoded_bytes();
                    digest.update(name.len().to_le_bytes());
                    digest.update(name);
                    visit(&entry, digest)?;
                }
                digest.update(b"end-directory\0");
            } else if metadata.is_symlink() {
                digest.update(b"link\0");
                let target = std::fs::read_link(path)?;
                let bytes = target.as_os_str().as_encoded_bytes();
                digest.update(bytes.len().to_le_bytes());
                digest.update(bytes);
            } else {
                ensure!(metadata.is_file(), "unsupported input {}", path.display());
                digest.update(b"file\0");
                let bytes = std::fs::read(path)?;
                digest.update(bytes.len().to_le_bytes());
                digest.update(bytes);
            }
            Ok(())
        }
        let mut digest = Sha256::new();
        visit(&root.join(".fluent/work"), &mut digest)?;
        Ok(format!("{:x}", digest.finalize()))
    }

    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 3,
        "usage: snapshot-cost ROOT REPETITIONS profile|plain"
    );
    let root = Path::new(&args[0]).canonicalize()?;
    let repetitions: usize = args[1].parse()?;
    ensure!(repetitions >= 3, "use at least three repetitions");
    ensure!(
        matches!(args[2].as_str(), "profile" | "plain"),
        "choose profile or plain"
    );
    let before = input_digest(&root).context("read benchmark input before snapshot")?;
    let mut samples = Vec::new();
    let mut projection = None;
    for index in 0..repetitions {
        let start = Instant::now();
        let (snapshot, profile) = if args[2] == "profile" {
            let (snapshot, profile) = snapshot_profile::capture(|| SourceSnapshot::load(&root));
            (snapshot, Some(profile))
        } else {
            (SourceSnapshot::load(&root), None)
        };
        let total_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut snapshot = snapshot?;
        // Active worker elapsed time depends on the wall clock, not source
        // state. Retain every other local field, including exceptional guidance
        // omitted by the SSH wire schema, in the comparison.
        for work in &mut snapshot.work {
            for worker in &mut work.workers {
                worker.elapsed_ms = None;
            }
        }
        let current = format!("{snapshot:#?}");
        if let Some(prior) = &projection {
            ensure!(prior == &current, "projection changed during benchmark");
        } else {
            projection = Some(current.clone());
        }
        samples.push(serde_json::json!({
            "index": index, "first_read": index == 0, "total_ms": total_ms,
            "work_count": snapshot.work.len(), "profile": profile,
            "projection_sha256": format!("{:x}", Sha256::digest(current.as_bytes())),
        }));
    }
    let after = input_digest(&root).context("read benchmark input after snapshot")?;
    ensure!(
        before == after,
        "Work inputs changed during benchmark; discard timings"
    );
    let mut warm = samples
        .iter()
        .skip(1)
        .map(|sample| sample["total_ms"].as_f64().unwrap())
        .collect::<Vec<_>>();
    warm.sort_by(f64::total_cmp);
    let median = (warm[(warm.len() - 1) / 2] + warm[warm.len() / 2]) / 2.0;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "build_commit": env!("FLUENT_BUILD_COMMIT"),
            "debug_assertions": cfg!(debug_assertions), "root": root,
            "mode": args[2], "input_sha256": before, "inputs_unchanged": true,
            "warm_median_ms": median, "warm_min_ms": warm.first(), "warm_max_ms": warm.last(),
            "samples": samples, "projection": projection,
        }))?
    );
    Ok(())
}
