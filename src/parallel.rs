use anyhow::{bail, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use crate::coder::{BareClaudeCode, SandboxedClaudeCode};
use crate::content::ContentResolver;
use crate::plan::Plan;
use crate::run::{Run, RunStatus};
use crate::session::{self, DefaultHooks, SandboxedHooks, SessionConfig};
use crate::worktree;

/// Context passed to each child thread.
pub(crate) struct ChildContext {
    pub id: String,
    pub worktree_dir: PathBuf,
    pub system_prompt: String,
    pub extra_args: Vec<String>,
    pub sandbox_profile: Option<String>,
}

/// Tracking info kept by the orchestrator for each child.
struct ChildInfo {
    id: String,
    source_dir: PathBuf,
}

/// Execute a parallel plan by launching child runs for each step.
///
/// Groups execute sequentially. Steps within each group execute
/// concurrently, each in its own worktree. After a group completes,
/// child branches are merged into the source branch before the next
/// group begins.
pub fn run_parallel_plan(
    source_root: &Path,
    parent_run: &Run,
    plan: &Plan,
    system_prompt: &str,
    extra_args: &[String],
    sandbox_profile: Option<&str>,
) -> Result<()> {
    execute_plan(source_root, parent_run, plan, system_prompt, extra_args, sandbox_profile, run_child)
}

/// Core orchestrator, parameterized on the child runner for testability.
fn execute_plan<F>(
    source_root: &Path,
    parent_run: &Run,
    plan: &Plan,
    system_prompt: &str,
    extra_args: &[String],
    sandbox_profile: Option<&str>,
    child_runner: F,
) -> Result<()>
where
    F: Fn(ChildContext) -> Result<()> + Send + Clone + 'static,
{
    parent_run.set_status(&RunStatus::Executing)?;

    let total_steps: usize = plan.groups.iter().map(|g| g.steps.len()).sum();
    eprintln!(
        "  Parallel plan: {} group{}, {} total step{}",
        plan.groups.len(),
        if plan.groups.len() == 1 { "" } else { "s" },
        total_steps,
        if total_steps == 1 { "" } else { "s" },
    );

    let mut all_child_ids: Vec<String> = Vec::new();

    for (gi, group) in plan.groups.iter().enumerate() {
        let group_num = gi + 1;
        eprintln!(
            "\n  === Group {} ({} step{}) ===",
            group_num,
            group.steps.len(),
            if group.steps.len() == 1 { "" } else { "s" },
        );

        let mut child_infos: Vec<ChildInfo> = Vec::new();
        let mut handles: Vec<thread::JoinHandle<Result<()>>> = Vec::new();

        for (si, step) in group.steps.iter().enumerate() {
            let step_num = si + 1;
            let child_id = format!("{}-{}-{}", parent_run.id, group_num, step_num);
            let child_dir = source_root.join(format!(".factory/runs/{}", child_id));
            fs::create_dir_all(&child_dir)?;

            let brief_content = format!("# {}\n\n{}", step.title, step.brief);
            fs::write(child_dir.join("brief.md"), &brief_content)?;
            fs::write(child_dir.join("status"), "planned")?;
            fs::write(child_dir.join("parent"), &parent_run.id)?;

            let wt_result =
                worktree::setup_run_worktree(source_root, &child_id, &child_dir)?;
            worktree::disable_commit_signing(&wt_result.worktree_dir)?;

            eprintln!("  Step {}.{}: {}", group_num, step_num, step.title);

            child_infos.push(ChildInfo {
                id: child_id.clone(),
                source_dir: child_dir,
            });
            all_child_ids.push(child_id.clone());

            let ctx = ChildContext {
                id: child_id,
                worktree_dir: wt_result.worktree_dir,
                system_prompt: system_prompt.to_string(),
                extra_args: extra_args.to_vec(),
                sandbox_profile: sandbox_profile.map(String::from),
            };

            let runner = child_runner.clone();
            handles.push(thread::spawn(move || runner(ctx)));
        }

        // Wait for all children
        let mut errors: Vec<String> = Vec::new();
        for (i, handle) in handles.into_iter().enumerate() {
            let child_id = &child_infos[i].id;
            match handle.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => errors.push(format!("{}: {}", child_id, e)),
                Err(_) => errors.push(format!("{}: thread panicked", child_id)),
            }
        }

        if !errors.is_empty() {
            parent_run.set_status(&RunStatus::Failed)?;
            bail!("Group {} failed:\n  {}", group_num, errors.join("\n  "));
        }

        // Verify all children completed
        let mut failed: Vec<String> = Vec::new();
        for info in &child_infos {
            let child_run = Run {
                id: info.id.clone(),
                dir: info.source_dir.clone(),
            };
            let status = child_run.effective_status()?;
            if status != RunStatus::Complete {
                failed.push(format!("{}: {}", info.id, status));
            }
        }

        if !failed.is_empty() {
            parent_run.set_status(&RunStatus::Failed)?;
            bail!(
                "Group {} has failed steps:\n  {}",
                group_num,
                failed.join("\n  ")
            );
        }

        // Merge children back into the source branch
        eprintln!("  Merging group {} results...", group_num);
        for info in &child_infos {
            worktree::land_run(source_root, &info.id, &info.source_dir)?;
            eprintln!("  Merged step {}", info.id);
        }
    }

    let children_list = all_child_ids.join("\n");
    fs::write(parent_run.dir.join("children"), &children_list)?;

    parent_run.set_status(&RunStatus::Complete)?;
    eprintln!("\n  Parallel plan completed ({} steps).", total_steps);

    Ok(())
}

/// Execute a single child run's session loop.
fn run_child(ctx: ChildContext) -> Result<()> {
    let wt_run = Run {
        id: ctx.id.clone(),
        dir: ctx.worktree_dir.join(format!(".factory/runs/{}", ctx.id)),
    };

    let config = SessionConfig {
        run: wt_run,
        system_prompt: ctx.system_prompt,
        working_dir: ctx.worktree_dir.clone(),
        extra_args: ctx.extra_args,
        resolver: ContentResolver::new(Some(&ctx.worktree_dir)),
    };

    match ctx.sandbox_profile {
        Some(profile) => {
            let author = SandboxedClaudeCode {
                sandbox_profile: Some(profile),
            };
            session::run_session_loop(&author, &config, &SandboxedHooks)
        }
        None => {
            let author = BareClaudeCode;
            session::run_session_loop(&author, &config, &DefaultHooks)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Group, Step};
    use std::process::Command;
    use tempfile::TempDir;

    fn setup_git_project() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let main_dir = tmp.path().join("main");
        fs::create_dir_all(&main_dir).unwrap();

        Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&main_dir)
            .output()
            .unwrap();
        for (k, v) in [
            ("commit.gpgsign", "false"),
            ("user.email", "test@test"),
            ("user.name", "test"),
        ] {
            Command::new("git")
                .args(["config", k, v])
                .current_dir(&main_dir)
                .output()
                .unwrap();
        }
        fs::write(main_dir.join("README.md"), "test").unwrap();
        fs::create_dir_all(main_dir.join(".factory/runs")).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(&main_dir)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&main_dir)
            .output()
            .unwrap();

        tmp
    }

    fn make_plan(groups: Vec<Vec<(&str, &str)>>) -> Plan {
        Plan {
            groups: groups
                .into_iter()
                .map(|steps| Group {
                    steps: steps
                        .into_iter()
                        .map(|(title, brief)| Step {
                            title: title.to_string(),
                            brief: brief.to_string(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Mock child runner that commits a file and sets status to complete.
    fn mock_child_runner(ctx: ChildContext) -> Result<()> {
        let wt_run_dir = ctx.worktree_dir.join(format!(".factory/runs/{}", ctx.id));

        // Create a file unique to this child
        let filename = format!("{}.txt", ctx.id);
        fs::write(ctx.worktree_dir.join(&filename), &ctx.id)?;

        // Stage and commit
        Command::new("git")
            .args(["add", &filename])
            .current_dir(&ctx.worktree_dir)
            .output()?;
        Command::new("git")
            .args(["commit", "-m", &format!("Add {}", ctx.id)])
            .current_dir(&ctx.worktree_dir)
            .output()?;

        // Set status to complete
        fs::write(wt_run_dir.join("status"), "complete")?;
        Ok(())
    }

    /// Mock child runner that sets status to failed.
    fn mock_failing_runner(ctx: ChildContext) -> Result<()> {
        let wt_run_dir = ctx.worktree_dir.join(format!(".factory/runs/{}", ctx.id));
        fs::write(wt_run_dir.join("status"), "failed")?;
        Ok(())
    }

    fn cleanup_worktrees(tmp: &TempDir, main_dir: &Path, ids: &[String]) {
        for id in ids {
            let wt = tmp.path().join(id);
            if wt.exists() {
                Command::new("git")
                    .args(["-C", &main_dir.to_string_lossy()])
                    .args(["worktree", "remove", "--force", &wt.to_string_lossy()])
                    .output()
                    .ok();
            }
        }
    }

    #[test]
    fn test_parallel_single_group_completes() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let parent_id = "test-single";
        let parent_dir = main_dir.join(format!(".factory/runs/{parent_id}"));
        fs::create_dir_all(&parent_dir).unwrap();
        fs::write(parent_dir.join("status"), "planned").unwrap();
        fs::write(parent_dir.join("brief.md"), "Brief").unwrap();

        let parent_run = Run {
            id: parent_id.to_string(),
            dir: parent_dir.clone(),
        };

        let plan = make_plan(vec![vec![
            ("Task A", "Do A."),
            ("Task B", "Do B."),
        ]]);

        let result = execute_plan(
            &main_dir,
            &parent_run,
            &plan,
            "test",
            &[],
            None,
            mock_child_runner,
        );

        assert!(result.is_ok(), "Plan should succeed: {:?}", result.err());
        assert_eq!(parent_run.status().unwrap(), RunStatus::Complete);

        // Children files should be merged into main
        assert!(main_dir.join(format!("{parent_id}-1-1.txt")).exists());
        assert!(main_dir.join(format!("{parent_id}-1-2.txt")).exists());

        // Children list recorded
        let children = fs::read_to_string(parent_dir.join("children")).unwrap();
        assert!(children.contains(&format!("{parent_id}-1-1")));
        assert!(children.contains(&format!("{parent_id}-1-2")));
    }

    #[test]
    fn test_parallel_two_groups_sequential() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let parent_id = "test-groups";
        let parent_dir = main_dir.join(format!(".factory/runs/{parent_id}"));
        fs::create_dir_all(&parent_dir).unwrap();
        fs::write(parent_dir.join("status"), "planned").unwrap();
        fs::write(parent_dir.join("brief.md"), "Brief").unwrap();

        let parent_run = Run {
            id: parent_id.to_string(),
            dir: parent_dir.clone(),
        };

        let plan = make_plan(vec![
            vec![("Group1 A", "Do 1A."), ("Group1 B", "Do 1B.")],
            vec![("Group2 A", "Do 2A.")],
        ]);

        let result = execute_plan(
            &main_dir,
            &parent_run,
            &plan,
            "test",
            &[],
            None,
            mock_child_runner,
        );

        assert!(result.is_ok(), "Plan should succeed: {:?}", result.err());
        assert_eq!(parent_run.status().unwrap(), RunStatus::Complete);

        // All children's files merged
        assert!(main_dir.join(format!("{parent_id}-1-1.txt")).exists());
        assert!(main_dir.join(format!("{parent_id}-1-2.txt")).exists());
        assert!(main_dir.join(format!("{parent_id}-2-1.txt")).exists());

        // Children list includes all three
        let children = fs::read_to_string(parent_dir.join("children")).unwrap();
        let ids: Vec<&str> = children.lines().collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn test_parallel_child_failure_stops_plan() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let parent_id = "test-fail";
        let parent_dir = main_dir.join(format!(".factory/runs/{parent_id}"));
        fs::create_dir_all(&parent_dir).unwrap();
        fs::write(parent_dir.join("status"), "planned").unwrap();
        fs::write(parent_dir.join("brief.md"), "Brief").unwrap();

        let parent_run = Run {
            id: parent_id.to_string(),
            dir: parent_dir,
        };

        let plan = make_plan(vec![vec![
            ("Task A", "Do A."),
            ("Task B", "Do B."),
        ]]);

        let result = execute_plan(
            &main_dir,
            &parent_run,
            &plan,
            "test",
            &[],
            None,
            mock_failing_runner,
        );

        assert!(result.is_err());
        assert_eq!(parent_run.status().unwrap(), RunStatus::Failed);
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("failed steps"));

        // Clean up worktrees
        cleanup_worktrees(
            &tmp,
            &main_dir,
            &[
                format!("{parent_id}-1-1"),
                format!("{parent_id}-1-2"),
            ],
        );
    }

    #[test]
    fn test_parallel_creates_correct_briefs() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let parent_id = "test-briefs";
        let parent_dir = main_dir.join(format!(".factory/runs/{parent_id}"));
        fs::create_dir_all(&parent_dir).unwrap();
        fs::write(parent_dir.join("status"), "planned").unwrap();
        fs::write(parent_dir.join("brief.md"), "Brief").unwrap();

        let parent_run = Run {
            id: parent_id.to_string(),
            dir: parent_dir,
        };

        let plan = make_plan(vec![vec![
            ("Auth endpoints", "Implement login and logout."),
            ("DB schema", "Create users table."),
        ]]);

        let result = execute_plan(
            &main_dir,
            &parent_run,
            &plan,
            "test",
            &[],
            None,
            mock_child_runner,
        );
        assert!(result.is_ok());

        // Check briefs were written correctly in source run dirs
        let child1_dir = main_dir.join(format!(".factory/runs/{parent_id}-1-1"));
        let child2_dir = main_dir.join(format!(".factory/runs/{parent_id}-1-2"));
        assert_eq!(
            fs::read_to_string(child1_dir.join("brief.md")).unwrap(),
            "# Auth endpoints\n\nImplement login and logout."
        );
        assert_eq!(
            fs::read_to_string(child2_dir.join("brief.md")).unwrap(),
            "# DB schema\n\nCreate users table."
        );
    }

    #[test]
    fn test_parallel_group2_sees_group1_changes() {
        let tmp = setup_git_project();
        let main_dir = tmp.path().join("main");

        let parent_id = "test-seq";
        let parent_dir = main_dir.join(format!(".factory/runs/{parent_id}"));
        fs::create_dir_all(&parent_dir).unwrap();
        fs::write(parent_dir.join("status"), "planned").unwrap();
        fs::write(parent_dir.join("brief.md"), "Brief").unwrap();

        let parent_run = Run {
            id: parent_id.to_string(),
            dir: parent_dir,
        };

        // Group 1 creates file, group 2 should see it
        let plan = make_plan(vec![
            vec![("First", "Create first.")],
            vec![("Second", "Create second.")],
        ]);

        // Custom runner that verifies group 2 can see group 1's file
        let runner = |ctx: ChildContext| -> Result<()> {
            let wt_run_dir = ctx.worktree_dir.join(format!(".factory/runs/{}", ctx.id));

            // If this is a group-2 child, verify group-1 file exists
            if ctx.id.contains("-2-") {
                let g1_file = ctx.worktree_dir.join(format!(
                    "{}-1-1.txt",
                    ctx.id.rsplit_once("-2-").unwrap().0
                ));
                assert!(
                    g1_file.exists(),
                    "Group 2 should see group 1's file at {}",
                    g1_file.display()
                );
            }

            // Create our file and commit
            let filename = format!("{}.txt", ctx.id);
            fs::write(ctx.worktree_dir.join(&filename), &ctx.id)?;
            Command::new("git")
                .args(["add", &filename])
                .current_dir(&ctx.worktree_dir)
                .output()?;
            Command::new("git")
                .args(["commit", "-m", &format!("Add {}", ctx.id)])
                .current_dir(&ctx.worktree_dir)
                .output()?;

            fs::write(wt_run_dir.join("status"), "complete")?;
            Ok(())
        };

        let result = execute_plan(
            &main_dir,
            &parent_run,
            &plan,
            "test",
            &[],
            None,
            runner,
        );

        assert!(result.is_ok(), "Plan should succeed: {:?}", result.err());
    }
}
