use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::Result;
use fluent::conversation::{Checkout, ConversationProvider, ProviderResult, ProviderScope};
use fluent::discord_collaboration::*;
use tempfile::TempDir;

#[derive(Default)]
struct Provider {
    selected: String,
    calls: Vec<(String, Option<String>, String, String, ProviderScope)>,
}
impl ConversationProvider for Provider {
    fn select(&mut self, provider: &str) -> ProviderResult<()> {
        self.selected = provider.into();
        Ok(())
    }
    fn start(
        &mut self,
        _: &str,
        scope: &ProviderScope,
        text: &str,
        actor: &str,
    ) -> ProviderResult<(String, String)> {
        self.calls.push((
            self.selected.clone(),
            None,
            text.into(),
            actor.into(),
            scope.clone(),
        ));
        Ok(("session-1".into(), "Let us plan this.".into()))
    }
    fn resume(
        &mut self,
        _: &str,
        session: &str,
        scope: &ProviderScope,
        text: &str,
        actor: &str,
    ) -> ProviderResult<String> {
        self.calls.push((
            self.selected.clone(),
            Some(session.into()),
            text.into(),
            actor.into(),
            scope.clone(),
        ));
        Ok("Continued.".into())
    }
}
#[derive(Default)]
struct Discord {
    threads: BTreeMap<String, String>,
    messages: Vec<Delivery>,
}
impl DiscordTransport for Discord {
    fn ensure_thread(&mut self, command: &str, _: &str, _: &str) -> Result<String> {
        Ok(self
            .threads
            .entry(command.into())
            .or_insert("400".into())
            .clone())
    }
    fn deliver(&mut self, delivery: &Delivery) -> Result<String> {
        self.messages.push(delivery.clone());
        Ok((500 + self.messages.len()).to_string())
    }
}
fn git(path: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(path)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn fixture() -> (TempDir, TempDir, DiscordConfig) {
    let home = TempDir::new().unwrap();
    let project = TempDir::new().unwrap();
    git(project.path(), &["init", "-b", "main"]);
    git(project.path(), &["config", "user.name", "Test"]);
    git(
        project.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    fs::write(project.path().join("README.md"), "fixture").unwrap();
    git(project.path(), &["add", "."]);
    git(project.path(), &["commit", "-m", "Fixture"]);
    let checkout = Checkout {
        label: "main".into(),
        path: project.path().into(),
        identity: fluent::slack_collaboration::checkout_identity(project.path()).unwrap(),
        branch: "main".into(),
        commit: git(project.path(), &["rev-parse", "HEAD"]),
    };
    let config = DiscordConfig {
        guild_id: "100".into(),
        channels: BTreeMap::from([("200".into(), BTreeSet::from(["app".into()]))]),
        projects: BTreeMap::from([(
            "app".into(),
            Project {
                checkout,
                participants: BTreeSet::from(["300".into(), "301".into()]),
                approvers: BTreeSet::from(["300".into()]),
                provider: "codex".into(),
            },
        )]),
    };
    (home, project, config)
}
fn command() -> Input {
    Input {
        id: "600".into(),
        guild: "100".into(),
        channel: "200".into(),
        actor: "300".into(),
        kind: InputKind::Command {
            project: "app".into(),
            provider: None,
            text: "plan this".into(),
        },
    }
}
fn reply() -> Input {
    Input {
        id: "601".into(),
        guild: "100".into(),
        channel: "400".into(),
        actor: "301".into(),
        kind: InputKind::Message {
            text: "continue".into(),
        },
    }
}
#[test]
fn discord_command_starts_one_project_bound_thread() {
    let (home, project, config) = fixture();
    let mut runner =
        DiscordRunner::new(home.path(), config, Provider::default(), Discord::default()).unwrap();
    runner.ingest(command()).unwrap();
    runner.ingest(command()).unwrap();
    assert_eq!(runner.transport().threads.len(), 1);
    assert_eq!(runner.provider().calls.len(), 1);
    assert_eq!(runner.provider().calls[0].4.checkout, project.path());
    assert!(
        runner.provider().calls[0]
            .4
            .slice_draft
            .starts_with(project.path().join(".fluent/work/discord"))
    );
    assert_eq!(runner.transport().messages.len(), 1);
    assert_eq!(runner.transport().messages[0].channel, "400");
    assert!(runner.transport().messages[0].text.contains("Project: app"));
    assert!(
        runner.transport().messages[0]
            .text
            .contains("Let us plan this.")
    );
    assert!(
        !runner.transport().messages[0]
            .text
            .contains(project.path().to_str().unwrap())
    );
    assert!(!project.path().join(".fluent/work/slack").exists());
}
#[test]
fn discord_reply_resumes_selected_provider() {
    for provider in ["codex", "claude"] {
        let (home, _project, mut config) = fixture();
        config.projects.get_mut("app").unwrap().provider = provider.into();
        let mut runner =
            DiscordRunner::new(home.path(), config, Provider::default(), Discord::default())
                .unwrap();
        runner.ingest(command()).unwrap();
        runner.ingest(reply()).unwrap();
        assert_eq!(runner.provider().calls.len(), 2);
        assert_eq!(runner.provider().calls[1].0, provider);
        assert_eq!(runner.provider().calls[1].1.as_deref(), Some("session-1"));
        assert_eq!(runner.provider().calls[1].3, "discord:100:301");
        assert_eq!(runner.transport().messages[1].channel, "400");
        assert!(runner.transport().messages[1].text.contains("Continued."));
    }
}
