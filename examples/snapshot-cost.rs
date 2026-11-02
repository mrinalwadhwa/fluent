//! Run with --release --features snapshot-profile on an idle, readable checkout.
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use fluent::{dashboard::project::SourceSnapshot, snapshot_profile};
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use std::time::Instant;

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
    let before =
        snapshot_profile::input_digest(&root).context("read benchmark input before snapshot")?;
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
    let after =
        snapshot_profile::input_digest(&root).context("read benchmark input after snapshot")?;
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
