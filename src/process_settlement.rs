use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ProcessEntry {
    pub pid: u32,
    pub kind: String,
    pub settlement_directory: PathBuf,
}

pub(crate) trait ProcessInventoryCollector {
    fn collect(&self, settlement_directory: &Path) -> Result<Vec<ProcessEntry>>;
}

pub(crate) struct HostProcessInventoryCollector;

impl ProcessInventoryCollector for HostProcessInventoryCollector {
    fn collect(&self, settlement_directory: &Path) -> Result<Vec<ProcessEntry>> {
        let output = Command::new("ps")
            .args(["-axo", "pid=,command="])
            .output()
            .context("collecting the host process table for process settlement")?;
        if !output.status.success() {
            anyhow::bail!(
                "collecting the host process table for process settlement exited {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        if output.stdout.iter().all(u8::is_ascii_whitespace) {
            anyhow::bail!(
                "the host process table collector returned no inventory for process settlement"
            );
        }

        collect_scoped_processes(
            &String::from_utf8_lossy(&output.stdout),
            settlement_directory,
            process_cwd,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProcessCwd {
    Exited,
    Directory(PathBuf),
}

fn collect_scoped_processes<F>(
    process_table: &str,
    settlement_directory: &Path,
    mut cwd_for_pid: F,
) -> Result<Vec<ProcessEntry>>
where
    F: FnMut(u32) -> Result<ProcessCwd>,
{
    let mut entries = Vec::new();
    for line in process_table.lines() {
        let Some((pid, command)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = pid.trim().parse() else {
            continue;
        };
        let Some(kind) = classify_process(command.trim()) else {
            continue;
        };
        let ProcessCwd::Directory(cwd) = cwd_for_pid(pid)? else {
            continue;
        };
        if cwd.starts_with(settlement_directory) {
            entries.push(ProcessEntry {
                pid,
                kind: kind.to_string(),
                settlement_directory: settlement_directory.to_path_buf(),
            });
        }
    }
    Ok(entries)
}

fn process_cwd(pid: u32) -> Result<ProcessCwd> {
    let output = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-d", "cwd", "-Fn"])
        .output()
        .with_context(|| format!("reading cwd for host process {pid} during process settlement"))?;
    if output.status.success() {
        return parse_cwd_record(&String::from_utf8_lossy(&output.stdout), pid);
    }
    if output.status.code() == Some(1) && process_exited(pid)? {
        return Ok(ProcessCwd::Exited);
    }
    {
        anyhow::bail!(
            "reading cwd for host process {pid} during process settlement exited {}: {}",
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
}

fn parse_cwd_record(output: &str, pid: u32) -> Result<ProcessCwd> {
    output
        .lines()
        .find_map(|line| line.strip_prefix('n'))
        .map(|cwd| ProcessCwd::Directory(PathBuf::from(cwd)))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "reading cwd for host process {pid} during process settlement returned no cwd record"
            )
        })
}

fn process_exited(pid: u32) -> Result<bool> {
    let output = Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .with_context(|| {
            format!("checking whether host process {pid} exited during process settlement")
        })?;
    if output.status.success() {
        return Ok(false);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    if stderr.contains("no such process") {
        return Ok(true);
    }
    anyhow::bail!(
        "checking whether host process {pid} exited during process settlement failed: {}",
        stderr.trim()
    )
}

fn classify_process(command: &str) -> Option<&'static str> {
    let command = command.to_ascii_lowercase();
    if command.contains("fluent scheduler") {
        Some("fluent scheduler")
    } else if command.contains("fluent auto-merge") {
        Some("fluent auto-merge")
    } else if command.contains("fluent post-merge-review") {
        Some("fluent post-merge-review")
    } else if command.contains("fluent") {
        Some("fluent")
    } else if command.contains("claude") {
        Some("claude")
    } else if command.contains("codex") {
        Some("codex")
    } else if command
        .split_whitespace()
        .any(|word| word == "pi" || word.ends_with("/pi"))
    {
        Some("pi")
    } else {
        None
    }
}

pub(crate) struct ProcessSettlement {
    pub directory: PathBuf,
    before: BTreeSet<ProcessEntry>,
}

impl ProcessSettlement {
    pub(crate) fn prepare_directory(directory: PathBuf) -> Result<PathBuf> {
        let existed = directory.exists();
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("creating settlement directory {}", directory.display()))?;
        if !directory.is_dir() {
            anyhow::bail!(
                "settlement path {} exists but is not a directory",
                directory.display()
            );
        }
        match std::fs::canonicalize(&directory) {
            Ok(directory) => Ok(directory),
            Err(source) => {
                if !existed {
                    let _ = std::fs::remove_dir_all(&directory);
                }
                Err(source).with_context(|| {
                    format!(
                        "canonicalizing settlement directory {}",
                        directory.display()
                    )
                })
            }
        }
    }

    pub(crate) fn begin_prepared(
        directory: PathBuf,
        collector: &dyn ProcessInventoryCollector,
    ) -> Result<Self> {
        let directory = std::fs::canonicalize(&directory).with_context(|| {
            format!(
                "canonicalizing prepared settlement directory {}",
                directory.display()
            )
        })?;
        let before = collector.collect(&directory)?.into_iter().collect();
        Ok(Self { directory, before })
    }

    pub(crate) fn new_processes(
        &self,
        collector: &dyn ProcessInventoryCollector,
    ) -> Result<Vec<ProcessEntry>> {
        let mut leaks = Vec::new();
        for attempt in 0..5 {
            let after = collector.collect(&self.directory)?;
            leaks = after
                .into_iter()
                .filter(|entry| !self.before.contains(entry))
                .collect();
            if leaks.is_empty() {
                return Ok(leaks);
            }
            if attempt < 4 {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        Ok(leaks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_classifies_only_scoped_provider_processes() {
        let root = Path::new("/tmp/settlement");
        let entries = collect_scoped_processes(
            "101 fluent scheduler\n102 /usr/local/bin/claude\n103 codex exec\n104 /opt/bin/pi\n105 fluent auto-merge\n106 codex exec\n",
            root,
            |pid| match pid {
                101..=105 => Ok(ProcessCwd::Directory(root.join(pid.to_string()))),
                106 => Ok(ProcessCwd::Directory(PathBuf::from("/tmp/elsewhere"))),
                _ => unreachable!(),
            },
        )
        .unwrap();

        assert_eq!(
            entries
                .iter()
                .map(|entry| (entry.pid, entry.kind.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (101, "fluent scheduler"),
                (102, "claude"),
                (103, "codex"),
                (104, "pi"),
                (105, "fluent auto-merge"),
            ]
        );
    }

    #[test]
    fn collector_rejects_unavailable_or_malformed_cwd_evidence() {
        let root = Path::new("/tmp/settlement");
        let unavailable =
            collect_scoped_processes("101 codex exec\n", root, |_| anyhow::bail!("lsof denied"));
        assert!(unavailable.unwrap_err().to_string().contains("lsof denied"));

        let malformed = collect_scoped_processes("101 codex exec\n", root, |_| {
            anyhow::bail!("returned no cwd record")
        });
        assert!(
            malformed
                .unwrap_err()
                .to_string()
                .contains("returned no cwd record")
        );

        let malformed_record = parse_cwd_record("p101\nf1\n", 101);
        assert!(
            malformed_record
                .unwrap_err()
                .to_string()
                .contains("returned no cwd record")
        );
    }

    #[test]
    fn collector_accepts_an_empty_inventory() {
        let entries =
            collect_scoped_processes("", Path::new("/tmp/settlement"), |_| unreachable!()).unwrap();
        assert!(entries.is_empty());
    }
}
