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

#[test]
fn discord_runner_starts_with_scoped_config_and_safe_status() {
    let (home, project, config) = fixture();
    write_config(home.path(), &config).unwrap();
    assert_eq!(read_config(home.path()).unwrap().guild_id, "100");
    let mut service = fluent::slack_collaboration::FakeServiceManager::default();
    enable_runner(&config, &mut service, Path::new("/Applications/Fluent")).unwrap();
    assert!(service.running);
    let status = runner_status(&config, &OperationalState::default());
    assert!(status.contains("app"));
    assert!(!status.contains(project.path().to_str().unwrap()));
    let mut manager = DiscordServiceManager::new(home.path());
    use fluent::slack_collaboration::ServiceManager;
    manager
        .install(Path::new("/Applications/A&B/fluent"))
        .unwrap();
    let xml = fs::read_to_string(manager.plist()).unwrap();
    assert!(xml.contains("<string>discord</string>"));
    assert!(xml.contains("A&amp;B"));
    service.restart().unwrap();
    service.stop().unwrap();
    assert!(!service.running);
}
#[test]
fn discord_runner_rejects_missing_message_content_access() {
    assert!(
        validate_message_content_flags(0)
            .unwrap_err()
            .to_string()
            .contains("Message Content")
    );
    validate_message_content_flags(1 << 18).unwrap();
    validate_message_content_flags(1 << 19).unwrap();
    assert!(gateway_close_failure(4014).contains("Message Content"));
    let (home, project, mut config) = fixture();
    config.channels.clear();
    assert!(
        DiscordRunner::new(home.path(), config, Provider::default(), Discord::default()).is_err()
    );
    let (_, _, mut config) = fixture();
    config.projects.get_mut("app").unwrap().checkout = Checkout {
        path: project.path().join("missing"),
        ..config.projects["app"].checkout.clone()
    };
    assert!(preflight_checkouts(&config).is_err());
}
#[test]
fn discord_runner_gateway_heartbeats_and_resumes() {
    use serde_json::json;
    let mut gateway = GatewaySession::default();
    let identify = gateway
        .handle(&json!({"op":10,"d":{"heartbeat_interval":1000}}), "secret")
        .unwrap();
    assert_eq!(identify[0]["op"], 2);
    assert_eq!(identify[0]["d"]["intents"], 1 | 512 | 32768);
    gateway.handle(&json!({"op":0,"t":"READY","s":4,"d":{"session_id":"session","resume_gateway_url":"wss://gateway.discord.gg"}}), "secret").unwrap();
    let heartbeat = gateway.heartbeat().unwrap();
    assert_eq!(heartbeat["d"], 4);
    assert!(
        gateway.heartbeat().is_err(),
        "missing heartbeat acknowledgement forces reconnect"
    );
    gateway.handle(&json!({"op":11}), "secret").unwrap();
    assert!(gateway.heartbeat().is_ok());
    let resume = gateway
        .handle(&json!({"op":10,"d":{"heartbeat_interval":1000}}), "secret")
        .unwrap();
    assert_eq!(resume[0]["op"], 6);
    assert_eq!(resume[0]["d"]["seq"], 4);
    assert!(!serde_json::to_string(&gateway).unwrap().contains("secret"));
}
#[test]
fn discord_runner_decodes_commands_without_persisting_tokens() {
    use serde_json::json;
    let payload = json!({"op":0,"t":"INTERACTION_CREATE","d":{"id":"600","guild_id":"100","channel_id":"200","token":"never-persist-this", "type":2,"member":{"user":{"id":"300"}},"data":{"name":"fluent","options":[{"name":"project","value":"app"},{"name":"message","value":"plan this"}]}}});
    let decoded = decode_dispatch(&payload).unwrap().unwrap();
    assert_eq!(decoded, command());
    assert!(
        !serde_json::to_string(&decoded)
            .unwrap()
            .contains("never-persist")
    );
    let bot = json!({"op":0,"t":"MESSAGE_CREATE","d":{"id":"601","guild_id":"100","channel_id":"400","author":{"id":"900","bot":true},"content":"ignore"}});
    assert!(decode_dispatch(&bot).unwrap().is_none());
}

fn http_fixture(
    responses: Vec<(&'static str, serde_json::Value)>,
) -> (String, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (status, value) in responses {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "HTTP fixture was not called"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
                if bytes.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let headers = String::from_utf8(bytes.clone()).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|n| n.parse::<usize>().ok())
                })
                .unwrap_or(0);
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            bytes.extend(body);
            requests.push(String::from_utf8(bytes).unwrap());
            if status == "DROP" {
                continue;
            }
            let body = value.to_string();
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    (endpoint, worker)
}
#[test]
fn discord_runner_preflight_uses_live_flags_and_channel_permissions() {
    use serde_json::json;
    let (_home, _project, config) = fixture();
    let (endpoint, server) = http_fixture(vec![
        ("200 OK", json!({"id":"800","flags":1<<19})),
        ("200 OK", json!({"id":"900"})),
        ("200 OK", json!({"owner_id":"999"})),
        ("200 OK", json!({"roles":["101"]})),
        ("200 OK", json!([{"id":"101","permissions":"8"}])),
        (
            "200 OK",
            json!({"id":"200","type":0,"guild_id":"100","permission_overwrites":[]}),
        ),
    ]);
    let mut rest = DiscordRest::with_test_endpoint("test-token".into(), endpoint).unwrap();
    rest.preflight(&config).unwrap();
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET /oauth2/applications/@me "));
    assert!(requests[5].starts_with("GET /channels/200 "));
    let (endpoint, server) = http_fixture(vec![("200 OK", json!({"flags":0}))]);
    let mut rest = DiscordRest::with_test_endpoint("test-token".into(), endpoint).unwrap();
    assert!(
        rest.preflight(&config)
            .unwrap_err()
            .to_string()
            .contains("Message Content")
    );
    assert_eq!(server.join().unwrap().len(), 1);
}
#[test]
fn discord_runner_rest_retries_rate_limits_without_leaking_credentials() {
    use serde_json::json;
    let (endpoint, server) = http_fixture(vec![
        ("429 Too Many Requests", json!({"retry_after":0.0})),
        ("200 OK", json!({"ok":true})),
        (
            "401 Unauthorized",
            json!({"message":"secret-token /Users/private"}),
        ),
    ]);
    let rest = DiscordRest::with_test_endpoint("secret-token".into(), endpoint).unwrap();
    assert_eq!(rest.request("GET", "/test", None).unwrap()["ok"], true);
    let error = rest.request("GET", "/test", None).unwrap_err().to_string();
    assert!(!error.contains("secret-token"));
    assert!(!error.contains("/Users"));
    assert_eq!(server.join().unwrap().len(), 3);
}
#[test]
fn discord_runner_cli_exposes_service_commands() {
    use clap::Parser;
    for name in [
        "setup", "check", "register", "enable", "run", "restart", "stop", "status",
    ] {
        assert!(
            fluent::cli::Cli::try_parse_from(["fluent", "discord", name]).is_ok(),
            "{name}"
        );
    }
}

#[test]
fn discord_restart_reconciles_input_and_output_once() {
    let (home, project, config) = fixture();
    let mut first = DiscordRunner::new(
        home.path(),
        config.clone(),
        Provider::default(),
        Discord::default(),
    )
    .unwrap();
    first.ingest(command()).unwrap();
    let mut recovered = DiscordRunner::new(
        home.path(),
        config.clone(),
        Provider::default(),
        Discord::default(),
    )
    .unwrap();
    recovered.recover().unwrap();
    recovered.ingest(command()).unwrap();
    recovered.ingest(reply()).unwrap();
    assert_eq!(recovered.provider().calls.len(), 1);
    assert_eq!(
        recovered.provider().calls[0].1.as_deref(),
        Some("session-1")
    );
    let path = project.path().join(".fluent/work/discord/threads/400.json");
    let thread: Thread = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(thread.inputs.len(), 2);
    assert_eq!(thread.deliveries.len(), 2);
}
#[test]
fn discord_restart_recovers_accepted_provider_turn_without_repeating_it() {
    use fluent::conversation::{ProviderError, ProviderTurnRecovery};
    struct Interrupted {
        dispatched: usize,
        recover: bool,
    }
    impl ConversationProvider for Interrupted {
        fn start(
            &mut self,
            _: &str,
            _: &ProviderScope,
            _: &str,
            _: &str,
        ) -> ProviderResult<(String, String)> {
            panic!("use run_turn")
        }
        fn resume(
            &mut self,
            _: &str,
            _: &str,
            _: &ProviderScope,
            _: &str,
            _: &str,
        ) -> ProviderResult<String> {
            panic!("use run_turn")
        }
        fn prepare_session(&mut self, _: &ProviderScope) -> ProviderResult<Option<String>> {
            Ok(Some("session".into()))
        }
        fn run_turn(
            &mut self,
            _: &str,
            _: Option<&str>,
            _: &ProviderScope,
            _: &str,
            _: &str,
            accepted: &mut dyn FnMut(&str) -> Result<()>,
        ) -> ProviderResult<(Option<String>, String)> {
            self.dispatched += 1;
            accepted("turn").unwrap();
            Err(ProviderError::internal("connection lost after acceptance"))
        }
        fn recover_turn(
            &mut self,
            _: &str,
            _: &str,
            _: &ProviderScope,
        ) -> ProviderResult<ProviderTurnRecovery> {
            assert!(self.recover);
            Ok(ProviderTurnRecovery::Completed {
                turn_id: "turn".into(),
                reply: "Recovered result".into(),
            })
        }
    }
    let (home, _project, config) = fixture();
    let mut first = DiscordRunner::new(
        home.path(),
        config.clone(),
        Interrupted {
            dispatched: 0,
            recover: false,
        },
        Discord::default(),
    )
    .unwrap();
    assert!(first.ingest(command()).is_err());
    assert_eq!(first.provider().dispatched, 1);
    let mut next = DiscordRunner::new(
        home.path(),
        config,
        Interrupted {
            dispatched: 0,
            recover: true,
        },
        Discord::default(),
    )
    .unwrap();
    next.recover().unwrap();
    assert_eq!(next.provider().dispatched, 0);
    assert!(
        next.transport().messages[0]
            .text
            .contains("Recovered result")
    );
}
#[test]
fn discord_edits_preserve_accepted_history() {
    let (home, project, config) = fixture();
    let mut runner =
        DiscordRunner::new(home.path(), config, Provider::default(), Discord::default()).unwrap();
    runner.ingest(command()).unwrap();
    runner.ingest(reply()).unwrap();
    let edit = Input {
        kind: InputKind::Edit {
            message: "601".into(),
            revision: "2026-10-18T01:00:00Z".into(),
            text: "correction".into(),
        },
        ..reply()
    };
    runner.ingest(edit.clone()).unwrap();
    runner.ingest(edit).unwrap();
    runner
        .ingest(Input {
            kind: InputKind::Delete {
                message: "601".into(),
            },
            ..reply()
        })
        .unwrap();
    let thread: Thread = serde_json::from_slice(
        &fs::read(project.path().join(".fluent/work/discord/threads/400.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(thread.inputs.len(), 4);
    assert_eq!(thread.inputs[1], reply());
    assert!(runner.provider().calls[2].2.contains("correction"));
    assert!(runner.provider().calls[3].2.contains("deleted"));
    assert_eq!(runner.provider().calls[3].3, "discord:100:301");
}
#[test]
fn discord_authority_denies_unconfigured_actors_and_channels() {
    let (home, project, config) = fixture();
    write_config(home.path(), &config).unwrap();
    let mut runner = DiscordRunner::new(
        home.path(),
        config.clone(),
        Provider::default(),
        Discord::default(),
    )
    .unwrap();
    for input in [
        Input {
            actor: "999".into(),
            ..command()
        },
        Input {
            channel: "999".into(),
            ..command()
        },
        Input {
            guild: "999".into(),
            ..command()
        },
    ] {
        assert!(runner.ingest(input).is_err());
    }
    assert!(runner.provider().calls.is_empty());
    assert!(!project.path().join(".fluent/work/discord").exists());
    runner.ingest(command()).unwrap();
    let mut revoked = config;
    revoked
        .projects
        .get_mut("app")
        .unwrap()
        .participants
        .remove("301");
    write_config(home.path(), &revoked).unwrap();
    assert!(runner.ingest(reply()).is_err());
    assert_eq!(runner.provider().calls.len(), 1);
}

#[test]
fn discord_restart_reconciles_lost_delivery_ack_and_paginated_history() {
    use std::sync::{Arc, Mutex};
    #[derive(Default)]
    struct Remote {
        deliveries: BTreeMap<String, String>,
        sends: usize,
        fail: bool,
        pages: std::collections::VecDeque<HistoryPage>,
    }
    struct Transport(Arc<Mutex<Remote>>);
    impl DiscordTransport for Transport {
        fn ensure_thread(&mut self, _: &str, _: &str, _: &str) -> Result<String> {
            Ok("400".into())
        }
        fn deliver(&mut self, d: &Delivery) -> Result<String> {
            let mut remote = self.0.lock().unwrap();
            if let Some(id) = remote.deliveries.get(&d.id) {
                return Ok(id.clone());
            }
            remote.sends += 1;
            let id = (700 + remote.sends).to_string();
            remote.deliveries.insert(d.id.clone(), id.clone());
            if remote.fail {
                remote.fail = false;
                anyhow::bail!("lost acknowledgement")
            }
            Ok(id)
        }
        fn history(&mut self, _: &str, _: &str, _: &str) -> Result<HistoryPage> {
            Ok(self.0.lock().unwrap().pages.pop_front().unwrap_or_default())
        }
    }
    let (home, project, config) = fixture();
    let remote = Arc::new(Mutex::new(Remote {
        fail: true,
        ..Remote::default()
    }));
    let mut first = DiscordRunner::new(
        home.path(),
        config.clone(),
        Provider::default(),
        Transport(remote.clone()),
    )
    .unwrap();
    assert!(first.ingest(command()).is_err());
    remote.lock().unwrap().pages.extend([
        HistoryPage {
            inputs: vec![reply()],
            last_message: Some("601".into()),
            has_more: true,
        },
        HistoryPage {
            inputs: vec![Input {
                id: "602".into(),
                ..reply()
            }],
            last_message: Some("602".into()),
            has_more: false,
        },
    ]);
    let mut next = DiscordRunner::new(
        home.path(),
        config,
        Provider::default(),
        Transport(remote.clone()),
    )
    .unwrap();
    next.recover().unwrap();
    next.recover().unwrap();
    assert_eq!(next.provider().calls.len(), 2);
    assert_eq!(remote.lock().unwrap().sends, 3);
    let thread: Thread = serde_json::from_slice(
        &fs::read(project.path().join(".fluent/work/discord/threads/400.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(thread.history_cursor, "602");
    assert!(thread.deliveries.iter().all(|d| d.message_id.is_some()));
}

#[test]
fn discord_approval_binds_current_action_and_revision() {
    let (home, project, config) = fixture();
    write_config(home.path(), &config).unwrap();
    let mut runner = DiscordRunner::new(
        home.path(),
        config.clone(),
        Provider::default(),
        Discord::default(),
    )
    .unwrap();
    runner.ingest(command()).unwrap();
    let first = runner
        .present_planning("400", "brief", "First brief")
        .unwrap();
    let second = runner
        .present_planning("400", "brief", "Revised brief")
        .unwrap();
    let approve = |id: &str, control: &ApprovalButton, actor: &str| Input {
        id: id.into(),
        guild: "100".into(),
        channel: "400".into(),
        actor: actor.into(),
        kind: InputKind::Approval {
            action: control.action.clone(),
            revision: control.revision.clone(),
        },
    };
    assert!(runner.ingest(approve("610", &first, "300")).is_err());
    assert!(runner.ingest(approve("611", &second, "301")).is_err());
    runner.ingest(approve("612", &second, "300")).unwrap();
    runner.ingest(approve("612", &second, "300")).unwrap();
    let store = fluent::work_model::WorkModelStore::new(project.path());
    let item = store.read_work_item("discord-400").unwrap();
    assert_eq!(
        item.planning_context.unwrap().brief.as_deref(),
        Some("Revised brief")
    );
    assert!(!item.authorization.is_execution_ready());
    let third = runner
        .present_planning("400", "behaviors", "Expected behaviors")
        .unwrap();
    let mut revoked = config;
    revoked.projects.get_mut("app").unwrap().approvers.clear();
    write_config(home.path(), &revoked).unwrap();
    assert!(runner.ingest(approve("613", &third, "300")).is_err());
    assert!(!project.path().join(".fluent/work/slack").exists());
}
#[test]
fn discord_work_controls_use_existing_boundaries() {
    use fluent::conversation::{WorkExecutionRequest, WorkQueueOutcome};
    #[derive(Default)]
    struct Effects {
        requests: std::sync::Mutex<Vec<WorkExecutionRequest>>,
        lands: std::sync::Mutex<Vec<(String, String)>>,
    }
    struct SharedEffects(std::sync::Arc<Effects>);
    impl WorkEffects for SharedEffects {
        fn submit(
            &self,
            _: &Path,
            _: &Path,
            request: &WorkExecutionRequest,
        ) -> Result<WorkQueueOutcome> {
            self.0.requests.lock().unwrap().push(request.clone());
            Ok(WorkQueueOutcome::Queued {
                request_id: request.id.clone(),
            })
        }
        fn land(
            &self,
            config: fluent::work_merge_executor::WorkMergeConfig<'_>,
            approval: &fluent::work_approval::WorkApproval,
        ) -> Result<fluent::work_merge_executor::WorkMergeOutcome> {
            let item = config.store.read_work_item(config.work_item_id)?;
            approval.check(&item)?;
            let candidate = item
                .merge_candidates
                .iter()
                .find(|c| c.id == config.merge_candidate_id)
                .unwrap();
            self.0
                .lands
                .lock()
                .unwrap()
                .push((candidate.id.clone(), candidate.candidate_commit.clone()));
            let commit = candidate.candidate_commit.clone();
            config.store.mutate_work_item(config.work_item_id, |item| {
                let c = item
                    .merge_candidates
                    .iter_mut()
                    .find(|c| c.id == config.merge_candidate_id)
                    .unwrap();
                c.merge_state.status = fluent::work_model::MergeCandidateMergeStatus::Merged;
                c.merge_state.merged_commit = Some(commit.clone());
                Ok(())
            })?;
            Ok(fluent::work_merge_executor::WorkMergeOutcome {
                merge_candidate_id: config.merge_candidate_id.into(),
                merged_commit: commit,
            })
        }
    }
    let effects = std::sync::Arc::new(Effects::default());
    let (home, project, config) = fixture();
    let mut runner =
        DiscordRunner::new(home.path(), config, Provider::default(), Discord::default())
            .unwrap()
            .with_work_effects(SharedEffects(effects.clone()));
    runner.ingest(command()).unwrap();
    for (i, kind) in ["brief", "behaviors", "approach", "plan"]
        .iter()
        .enumerate()
    {
        let control = runner
            .present_planning("400", kind, &format!("Approved {kind}"))
            .unwrap();
        runner
            .ingest(Input {
                id: (620 + i).to_string(),
                guild: "100".into(),
                channel: "400".into(),
                actor: "300".into(),
                kind: InputKind::Approval {
                    action: control.action,
                    revision: control.revision,
                },
            })
            .unwrap();
    }
    runner
        .ingest(Input {
            id: "628".into(),
            actor: "300".into(),
            kind: InputKind::Message {
                text: "work execute".into(),
            },
            ..reply()
        })
        .unwrap();
    let execute = runner
        .transport()
        .messages
        .last()
        .unwrap()
        .approval
        .clone()
        .unwrap();
    fluent::work_model::WorkModelStore::new(project.path())
        .mutate_work_item("discord-400", |item| {
            item.instructions = Some("Owner updated the Work instructions".into());
            Ok(())
        })
        .unwrap();
    assert!(runner.ingest(click("629", &execute)).is_err());
    assert!(effects.requests.lock().unwrap().is_empty());
    runner
        .ingest(Input {
            id: "6281".into(),
            actor: "300".into(),
            kind: InputKind::Message {
                text: "work execute".into(),
            },
            ..reply()
        })
        .unwrap();
    let execute = runner
        .transport()
        .messages
        .last()
        .unwrap()
        .approval
        .clone()
        .unwrap();
    runner.ingest(click("6291", &execute)).unwrap();
    runner.ingest(click("6291", &execute)).unwrap();
    assert_eq!(effects.requests.lock().unwrap().len(), 1);
    assert_eq!(
        effects.requests.lock().unwrap()[0].work_item_id,
        "discord-400"
    );
    assert!(
        effects.requests.lock().unwrap()[0]
            .id
            .starts_with("discord-work-")
    );
    assert!(
        fluent::work_model::WorkModelStore::new(project.path())
            .read_work_item("discord-400")
            .unwrap()
            .authorization
            .is_execution_ready()
    );
    runner
        .ingest(Input {
            id: "630".into(),
            kind: InputKind::Message {
                text: "work status".into(),
            },
            ..reply()
        })
        .unwrap();
    assert!(
        runner
            .transport()
            .messages
            .last()
            .unwrap()
            .text
            .contains("discord-400")
    );
    use fluent::work_model::{AttemptStatus, PauseKind, TaskStatus};
    let store = fluent::work_model::WorkModelStore::new(project.path());
    store
        .mutate_work_item("discord-400", |item| {
            item.add_initial_attempt("attempt-1").map(|_| ())
        })
        .unwrap();
    fluent::queue::add(project.path(), "discord-400", None).unwrap();
    fluent::queue::claim(project.path(), "discord-400", "attempt-1")
        .unwrap()
        .unwrap();
    store
        .mutate_work_item("discord-400", |item| {
            item.attempts[0].status = AttemptStatus::NeedsUser;
            item.attempts[0].pause_kind = Some(PauseKind::WriterCompletion);
            item.attempts[0].tasks[0].status = TaskStatus::NeedsUser;
            Ok(())
        })
        .unwrap();
    fluent::queue::reconcile_active(
        project.path(),
        "discord-400",
        fluent::queue::DispatchStatus::NeedsUser,
    )
    .unwrap();
    runner
        .ingest(Input {
            id: "640".into(),
            actor: "300".into(),
            kind: InputKind::Message {
                text: "work resume".into(),
            },
            ..reply()
        })
        .unwrap();
    let control = runner
        .transport()
        .messages
        .last()
        .unwrap()
        .approval
        .clone()
        .unwrap();
    runner
        .ingest(Input {
            id: "641".into(),
            actor: "300".into(),
            kind: InputKind::Approval {
                action: control.action,
                revision: control.revision,
            },
            ..reply()
        })
        .unwrap();
    let ledger = fluent::queue::read_ledger(project.path(), "discord-400")
        .unwrap()
        .unwrap();
    assert_eq!(
        ledger.active().unwrap().intent,
        fluent::queue::DispatchIntent::ResumeAttempt
    );
    assert_eq!(
        ledger.active().unwrap().bound_attempt_id.as_deref(),
        Some("attempt-1")
    );
    let mut interrupted = thread_snapshot(project.path());
    interrupted.approvals.last_mut().unwrap().completed = false;
    interrupted.approval = Some(interrupted.approvals.last().unwrap().pending.clone());
    save_thread_snapshot(project.path(), &interrupted);
    let resume_click = interrupted.inputs.last().unwrap().clone();
    fluent::queue::reconcile_active(
        project.path(),
        "discord-400",
        fluent::queue::DispatchStatus::Canceled,
    )
    .unwrap();
    let before = fluent::queue::read_ledger(project.path(), "discord-400").unwrap();
    runner.ingest_recovered(resume_click).unwrap();
    assert_eq!(
        fluent::queue::read_ledger(project.path(), "discord-400").unwrap(),
        before
    );

    use fluent::work_model::{AttemptReviewState, MergeReviewState, TaskOutput};
    store
        .mutate_work_item("discord-400", |item| {
            let attempt = &mut item.attempts[0];
            attempt.status = AttemptStatus::Complete;
            attempt.pause_kind = None;
            attempt.review_state = Some(AttemptReviewState::Passed);
            attempt.tasks[0].status = TaskStatus::Complete;
            attempt.tasks[0].output = Some(TaskOutput {
                workspace_id: "candidate".into(),
                workspace_path: ".".into(),
                source_branch: "main".into(),
                base_commit: None,
                commit: "candidate-one".into(),
                no_change: None,
                learner_canonicalization: None,
            });
            item.create_or_get_merge_candidate("attempt-1")?;
            item.merge_candidates[0].merge_review_state = MergeReviewState::Passed;
            Ok(())
        })
        .unwrap();
    runner
        .ingest(Input {
            id: "650".into(),
            actor: "300".into(),
            kind: InputKind::Message {
                text: "work land".into(),
            },
            ..reply()
        })
        .unwrap();
    let stale = runner
        .transport()
        .messages
        .last()
        .unwrap()
        .approval
        .clone()
        .unwrap();
    store
        .mutate_work_item("discord-400", |item| {
            item.merge_candidates[0].candidate_commit = "candidate-two".into();
            item.attempts[0].tasks[0].output.as_mut().unwrap().commit = "candidate-two".into();
            Ok(())
        })
        .unwrap();
    assert!(runner.ingest(click("651", &stale)).is_err());
    assert!(effects.lands.lock().unwrap().is_empty());
    runner
        .ingest(Input {
            id: "652".into(),
            actor: "300".into(),
            kind: InputKind::Message {
                text: "work land".into(),
            },
            ..reply()
        })
        .unwrap();
    let land = runner
        .transport()
        .messages
        .last()
        .unwrap()
        .approval
        .clone()
        .unwrap();
    runner.ingest(click("653", &land)).unwrap();
    runner.ingest(click("653", &land)).unwrap();
    assert_eq!(
        *effects.lands.lock().unwrap(),
        vec![("attempt-1-merge-candidate".into(), "candidate-two".into())]
    );
    assert_eq!(
        store
            .read_work_item("discord-400")
            .unwrap()
            .merge_candidates[0]
            .merge_state
            .status,
        fluent::work_model::MergeCandidateMergeStatus::Merged
    );
    assert_eq!(runner.transport().messages.last().unwrap().channel, "400");
    assert!(
        runner
            .transport()
            .messages
            .last()
            .unwrap()
            .text
            .contains("merged")
    );
}

#[test]
fn discord_approval_recovery_requires_fresh_work_confirmation() {
    let (home, project, config) = fixture();
    let mut runner =
        DiscordRunner::new(home.path(), config, Provider::default(), Discord::default()).unwrap();
    runner.ingest(command()).unwrap();
    runner
        .ingest(Input {
            id: "650".into(),
            actor: "300".into(),
            kind: InputKind::Message {
                text: "work create".into(),
            },
            ..reply()
        })
        .unwrap();
    let button = runner
        .transport()
        .messages
        .last()
        .unwrap()
        .approval
        .clone()
        .unwrap();
    let approval = Input {
        id: "651".into(),
        actor: "300".into(),
        kind: InputKind::Approval {
            action: button.action,
            revision: button.revision,
        },
        ..reply()
    };
    assert!(
        runner
            .ingest_recovered(approval.clone())
            .unwrap_err()
            .to_string()
            .contains("fresh")
    );
    assert!(
        fluent::work_model::WorkModelStore::new(project.path())
            .read_work_item("discord-400")
            .is_err()
    );
    runner
        .ingest(Input {
            id: "652".into(),
            ..approval
        })
        .unwrap();
    assert!(
        fluent::work_model::WorkModelStore::new(project.path())
            .read_work_item("discord-400")
            .is_ok()
    );
}

#[test]
fn discord_delivery_splits_artifacts_and_suppresses_mentions() {
    use serde_json::json;
    let (home, project, config) = fixture();
    let mut runner = DiscordRunner::new(
        home.path(),
        config.clone(),
        Provider::default(),
        Discord::default(),
    )
    .unwrap();
    runner.ingest(command()).unwrap();
    let markdown = format!(
        "@everyone <@300> <@&123>\n{}\n{}",
        "🦀 readable text\n".repeat(500),
        project.path().display()
    );
    runner.present_planning("400", "brief", &markdown).unwrap();
    let parts = &runner.transport().messages[1..];
    assert!(parts.len() > 2);
    assert!(
        parts
            .iter()
            .all(|part| part.text.encode_utf16().count() <= 2000)
    );
    assert!(
        parts
            .iter()
            .all(|part| !part.text.contains(project.path().to_str().unwrap()))
    );
    assert!(
        parts[..parts.len() - 1]
            .iter()
            .all(|part| part.approval.is_none())
    );
    assert!(parts.last().unwrap().approval.is_some());
    let (endpoint, server) = http_fixture(vec![
        ("200 OK", json!({"id":"800","flags":1<<19})),
        ("200 OK", json!({"id":"900"})),
        ("200 OK", json!({"owner_id":"900"})),
        ("200 OK", json!({"roles":[]})),
        ("200 OK", json!([])),
        ("200 OK", json!({"id":"200","type":0,"guild_id":"100"})),
        ("200 OK", json!([])),
        ("200 OK", json!({"id":"700"})),
    ]);
    let mut rest = DiscordRest::with_test_endpoint("test-token".into(), endpoint).unwrap();
    rest.preflight(&config).unwrap();
    rest.deliver(parts.last().unwrap()).unwrap();
    let requests = server.join().unwrap();
    let body: serde_json::Value =
        serde_json::from_str(requests.last().unwrap().split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["allowed_mentions"]["parse"], json!([]));
    assert_eq!(body["allowed_mentions"]["replied_user"], false);
    assert_eq!(body["enforce_nonce"], true);
    assert!(
        body["components"][0]["components"][0]["custom_id"]
            .as_str()
            .unwrap()
            .len()
            <= 100
    );
}

#[test]
fn discord_runner_provider_login_uses_bound_private_state() {
    use std::os::unix::fs::PermissionsExt;
    let (home, project, config) = fixture();
    write_config(home.path(), &config).unwrap();
    let mut runner =
        DiscordRunner::new(home.path(), config, Provider::default(), Discord::default()).unwrap();
    runner.ingest(command()).unwrap();
    let executable = project.path().join("login-fixture");
    fs::write(&executable,"#!/bin/sh\nprintf '%s\\n' \"$HOME\" \"$CODEX_HOME\" \"${DISCORD_BOT_TOKEN-unset}\" > login-environment\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    provider_login_with_executable(home.path(), "app", "400", "codex", &executable).unwrap();
    let environment = fs::read_to_string(project.path().join("login-environment")).unwrap();
    assert!(environment.contains(".fluent/work/discord/drafts/400/.provider-home"));
    assert!(environment.contains(".fluent/work/discord/drafts/400/.codex-home"));
    assert!(environment.ends_with("unset\n"));
    assert!(
        provider_login_with_executable(home.path(), "app", "401", "codex", &executable).is_err()
    );
}

#[test]
fn discord_approval_rejection_reports_without_blocking_later_inputs() {
    let (home, _project, config) = fixture();
    let mut runner =
        DiscordRunner::new(home.path(), config, Provider::default(), Discord::default()).unwrap();
    runner.ingest(command()).unwrap();
    let approval = runner.present_planning("400", "brief", "Plan").unwrap();
    let stale = Input {
        id: "670".into(),
        actor: "301".into(),
        kind: InputKind::Approval {
            action: approval.action,
            revision: approval.revision,
        },
        ..reply()
    };
    assert!(runner.ingest(stale).is_err());
    runner
        .ingest(Input {
            id: "671".into(),
            ..reply()
        })
        .unwrap();
    process_inbox(&mut runner, &std::sync::Mutex::new(BTreeSet::new())).unwrap();
    assert!(
        runner
            .transport()
            .messages
            .iter()
            .any(|m| m.text.contains("approver"))
    );
    assert_eq!(runner.provider().calls.len(), 2);
}

#[test]
fn discord_approval_out_of_order_artifact_keeps_conversation_usable() {
    struct PlanningProvider;
    impl ConversationProvider for PlanningProvider {
        fn start(
            &mut self,
            _: &str,
            _: &ProviderScope,
            _: &str,
            _: &str,
        ) -> ProviderResult<(String, String)> {
            Ok(("session".into(),serde_json::json!({"type":"planning_artifact","kind":"plan","markdown":"Premature plan"}).to_string()))
        }
        fn resume(
            &mut self,
            _: &str,
            _: &str,
            _: &ProviderScope,
            _: &str,
            _: &str,
        ) -> ProviderResult<String> {
            Ok(serde_json::json!({"type":"planning_artifact","kind":"brief","markdown":"Correct brief"}).to_string())
        }
    }
    let (home, _project, config) = fixture();
    let mut runner =
        DiscordRunner::new(home.path(), config, PlanningProvider, Discord::default()).unwrap();
    runner.ingest(command()).unwrap();
    assert!(
        runner
            .transport()
            .messages
            .last()
            .unwrap()
            .text
            .contains("planning order")
    );
    assert!(
        runner
            .transport()
            .messages
            .last()
            .unwrap()
            .approval
            .is_none()
    );
    runner.ingest(reply()).unwrap();
    assert!(
        runner
            .transport()
            .messages
            .last()
            .unwrap()
            .approval
            .is_some()
    );
}

#[test]
fn discord_command_rest_reuses_the_existing_public_thread() {
    use serde_json::json;
    use sha2::{Digest, Sha256};
    let (_home, _project, config) = fixture();
    let identity = format!("fluent-discord:{:x}", Sha256::digest(b"command:600"));
    let (endpoint, server) = http_fixture(vec![
        ("200 OK", json!({"id":"800","flags":1<<19})),
        ("200 OK", json!({"id":"900"})),
        ("200 OK", json!({"owner_id":"900"})),
        ("200 OK", json!({"roles":[]})),
        ("200 OK", json!([])),
        ("200 OK", json!({"id":"200","type":0,"guild_id":"100"})),
        ("200 OK", json!([])),
        ("200 OK", json!({"id":"400"})),
        ("404 Not Found", json!({"code":10003})),
        ("200 OK", json!({"id":"400","type":11,"parent_id":"200"})),
        (
            "200 OK",
            json!([{"id":"400","author":{"id":"900"},"embeds":[{"footer":{"text":identity}}]}]),
        ),
        ("200 OK", json!({"id":"400","type":11,"parent_id":"200"})),
    ]);
    let mut client = DiscordRest::with_test_endpoint("test-token".into(), endpoint).unwrap();
    client.preflight(&config).unwrap();
    assert_eq!(client.ensure_thread("600", "200", "app").unwrap(), "400");
    assert_eq!(client.ensure_thread("600", "200", "app").unwrap(), "400");
    let requests = server.join().unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("POST /channels/200/messages/400/threads "))
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("POST /channels/200/messages "))
            .count(),
        1
    );
}

#[test]
fn discord_runner_gateway_rejects_untrusted_resume_hosts() {
    assert_eq!(
        gateway_url("wss://gateway.discord.gg").unwrap(),
        "wss://gateway.discord.gg/?v=10&encoding=json"
    );
    assert!(gateway_url("wss://gateway-us-east1-b.discord.gg").is_ok());
    for url in [
        "wss://gateway.discord.gg.attacker.invalid",
        "ws://gateway.discord.gg",
        "wss://gateway.discord.gg@attacker.invalid",
        "wss://attacker.invalid/path",
        "wss://gateway.discord.gg:1234",
    ] {
        assert!(gateway_url(url).is_err(), "{url}");
    }
}

#[test]
fn discord_state_never_changes_existing_slack_runtime() {
    use fluent::slack_collaboration as slack;
    fn snapshot(root: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
        fn visit(root: &Path, path: &Path, files: &mut BTreeMap<std::path::PathBuf, Vec<u8>>) {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(root, &path, files)
                } else {
                    files.insert(
                        path.strip_prefix(root).unwrap().into(),
                        fs::read(path).unwrap(),
                    );
                }
            }
        }
        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }
    let (home, project, config) = fixture();
    let slack_config = slack::RunnerConfig {
        workspace: "workspace".into(),
        projects: BTreeMap::from([(
            "app".into(),
            slack::ConfiguredProject {
                label: "app".into(),
                checkout: config.projects["app"].checkout.clone(),
                allowed_participants: BTreeSet::from(["U1".into()]),
                default_provider: "codex".into(),
                enabled_providers: BTreeSet::from(["codex".into()]),
                project_approvers: BTreeSet::from(["U1".into()]),
                github_remote: None,
            },
        )]),
        channel_defaults: BTreeMap::from([("CSLACK".into(), "app".into())]),
        direct_message_defaults: BTreeMap::new(),
        enabled_providers: BTreeSet::from(["codex".into()]),
    };
    let mut slack_runner = slack::CollaborationRunner::new(
        home.path(),
        slack_config,
        slack::InMemoryProvider::default(),
        slack::InMemorySlack::default(),
    );
    slack_runner
        .ingest(slack::InboundEnvelope {
            id: "E1".into(),
            acknowledgement_id: "E1".into(),
            channel: "CSLACK".into(),
            timestamp: "400".into(),
            thread_timestamp: None,
            user: "U1".into(),
            text: "@Fluent discuss".into(),
            change: slack::InboundChange::Message,
            approval_action_id: None,
            approval_artifact_revision: None,
        })
        .unwrap();
    let owner_root = slack::state_root(home.path());
    let project_root = project.path().join(".fluent/work/slack");
    let owner_before = snapshot(&owner_root);
    let project_before = snapshot(&project_root);
    let mut discord =
        DiscordRunner::new(home.path(), config, Provider::default(), Discord::default()).unwrap();
    discord.ingest(command()).unwrap();
    discord.ingest(reply()).unwrap();
    discord
        .present_planning("400", "brief", "A Discord brief")
        .unwrap();
    assert_eq!(snapshot(&owner_root), owner_before);
    assert_eq!(snapshot(&project_root), project_before);
    assert_eq!(slack_runner.provider().calls.len(), 1);
    assert_eq!(discord.provider().calls.len(), 2);
}

#[test]
fn discord_runner_rate_limit_keeps_other_routes_available() {
    use serde_json::json;
    let (endpoint, server) = http_fixture(vec![
        (
            "429 Too Many Requests",
            json!({"retry_after":65.0,"global":false}),
        ),
        ("200 OK", json!({"ok":true})),
    ]);
    let rest = DiscordRest::with_test_endpoint("test-token".into(), endpoint).unwrap();
    assert!(rest.request("GET", "/channels/200/messages", None).is_err());
    let unrelated = rest.request("GET", "/gateway/bot", None);
    let requests = server.join();
    assert_eq!(unrelated.unwrap()["ok"], true);
    assert_eq!(requests.unwrap().len(), 2);
}

#[test]
fn discord_runner_honors_global_rate_limit_deadline() {
    use serde_json::json;
    let (endpoint, server) = http_fixture(vec![(
        "429 Too Many Requests",
        json!({"retry_after":65.0,"global":true}),
    )]);
    let rest = DiscordRest::with_test_endpoint("test-token".into(), endpoint).unwrap();
    assert!(rest.request("GET", "/channels/200/messages", None).is_err());
    assert_eq!(server.join().unwrap().len(), 1);
    assert!(
        rest.request("GET", "/gateway/bot", None)
            .unwrap_err()
            .to_string()
            .contains("rate limit is still active")
    );
}

#[test]
fn discord_restart_requires_established_snapshot() {
    let (home, project, config) = fixture();
    let mut runner =
        DiscordRunner::new(home.path(), config, Provider::default(), Discord::default()).unwrap();
    runner.ingest(command()).unwrap();
    fs::remove_file(project.path().join(".fluent/work/discord/threads/400.json")).unwrap();
    assert!(
        runner.ingest(command()).is_err(),
        "missing established history must fail closed"
    );
    assert!(runner.recover().is_err());
    assert_eq!(runner.provider().calls.len(), 1);
}

#[test]
fn discord_restart_recovers_initial_snapshot_before_index() {
    for snapshot_exists in [false, true] {
        let (home, project, config) = fixture();
        let root = state_root(home.path());
        fs::create_dir_all(&root).unwrap();
        // Interrupted initialization: the project snapshot was written, but the routing
        // index was not. No provider turn can have begun before index publication.
        let binding = serde_json::json!({"guild":"100","parent":"200","project":"app",
        "checkout":config.projects["app"].checkout.identity,"provider":"codex","thread":"400"});
        fs::write(
            root.join("initializing.json"),
            serde_json::to_vec(&serde_json::json!({"600":binding})).unwrap(),
        )
        .unwrap();
        let path = project.path().join(".fluent/work/discord/threads/400.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        if snapshot_exists {
            fs::write(&path, serde_json::to_vec(&Thread::default()).unwrap()).unwrap();
        }
        let mut runner =
            DiscordRunner::new(home.path(), config, Provider::default(), Discord::default())
                .unwrap();
        runner.ingest(command()).unwrap();
        runner.ingest(command()).unwrap();
        assert_eq!(runner.provider().calls.len(), 1);
        let journal: BTreeMap<String, serde_json::Value> =
            serde_json::from_slice(&fs::read(root.join("initializing.json")).unwrap()).unwrap();
        assert!(journal.is_empty());
    }
}

#[test]
fn discord_restart_skips_denied_history_and_isolates_threads() {
    struct History(Discord, std::sync::Arc<std::sync::Mutex<Vec<String>>>);
    impl DiscordTransport for History {
        fn ensure_thread(&mut self, command: &str, _: &str, _: &str) -> Result<String> {
            Ok(if command == "600" { "400" } else { "401" }.into())
        }
        fn deliver(&mut self, d: &Delivery) -> Result<String> {
            self.0.deliver(d)
        }
        fn history(&mut self, _: &str, channel: &str, after: &str) -> Result<HistoryPage> {
            self.1.lock().unwrap().push(channel.into());
            if !after.is_empty() {
                return Ok(HistoryPage::default());
            }
            Ok(HistoryPage {
                inputs: vec![
                    Input {
                        id: "800".into(),
                        actor: "999".into(),
                        channel: channel.into(),
                        ..reply()
                    },
                    Input {
                        id: "801".into(),
                        actor: "300".into(),
                        channel: channel.into(),
                        ..reply()
                    },
                ],
                last_message: Some("801".into()),
                has_more: false,
            })
        }
    }
    let (home, project, mut config) = fixture();
    let visited = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut runner = DiscordRunner::new(
        home.path(),
        config.clone(),
        Provider::default(),
        History(Discord::default(), visited.clone()),
    )
    .unwrap();
    runner.ingest(command()).unwrap();
    runner
        .ingest(Input {
            id: "602".into(),
            ..command()
        })
        .unwrap();
    runner.ingest(reply()).unwrap();
    config
        .projects
        .get_mut("app")
        .unwrap()
        .participants
        .remove("301");
    write_config(home.path(), &config).unwrap();
    runner.recover().unwrap();
    runner.recover().unwrap();
    assert_eq!(runner.provider().calls.len(), 5);
    assert!(
        runner
            .provider()
            .calls
            .iter()
            .all(|c| !c.3.ends_with(":999"))
    );
    for id in ["400", "401"] {
        let thread: Thread = serde_json::from_slice(
            &fs::read(
                project
                    .path()
                    .join(format!(".fluent/work/discord/threads/{id}.json")),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(thread.history_cursor, "801");
    }
    // A retryable failure in one established thread must not suppress the other.
    fs::remove_file(project.path().join(".fluent/work/discord/threads/400.json")).unwrap();
    visited.lock().unwrap().clear();
    assert!(runner.recover().is_err());
    assert_eq!(*visited.lock().unwrap(), vec!["401"]);
    assert_eq!(runner.provider().calls.len(), 5);
}

fn click(id: &str, button: &ApprovalButton) -> Input {
    Input {
        id: id.into(),
        actor: "300".into(),
        kind: InputKind::Approval {
            action: button.action.clone(),
            revision: button.revision.clone(),
        },
        ..reply()
    }
}
fn thread_snapshot(project: &Path) -> Thread {
    serde_json::from_slice(
        &fs::read(project.join(".fluent/work/discord/threads/400.json")).unwrap(),
    )
    .unwrap()
}
fn save_thread_snapshot(project: &Path, thread: &Thread) {
    fs::write(
        project.join(".fluent/work/discord/threads/400.json"),
        serde_json::to_vec(thread).unwrap(),
    )
    .unwrap();
}
#[test]
fn discord_approval_interruption_never_replaces_newer_planning() {
    for effect_completed in [false, true] {
        let (home, project, config) = fixture();
        let mut runner =
            DiscordRunner::new(home.path(), config, Provider::default(), Discord::default())
                .unwrap();
        runner.ingest(command()).unwrap();
        let initial = runner
            .present_planning("400", "brief", "Initial brief")
            .unwrap();
        runner.ingest(click("610", &initial)).unwrap();
        let old = runner
            .present_planning("400", "brief", "Old brief")
            .unwrap();
        if effect_completed {
            runner.ingest(click("611", &old)).unwrap();
            let mut thread = thread_snapshot(project.path());
            thread.approvals.last_mut().unwrap().completed = false;
            save_thread_snapshot(project.path(), &thread);
        } else {
            let mut thread = thread_snapshot(project.path());
            thread.approvals.push(AcceptedApproval {
                input: "611".into(),
                actor: "discord:100:300".into(),
                pending: thread.approval.clone().unwrap(),
                completed: false,
            });
            save_thread_snapshot(project.path(), &thread);
        }
        let store = fluent::work_model::WorkModelStore::new(project.path());
        store
            .mutate_work_item("discord-400", |item| {
                item.approve_planning_revision(
                    "brief",
                    &fluent::conversation::ArtifactRevision::new("brief", "New local brief").id,
                    "New local brief".into(),
                )
            })
            .unwrap();
        let newer = runner
            .present_planning("400", "brief", "Newest pending brief")
            .unwrap();
        let result = runner.ingest_recovered(click("611", &old));
        if !effect_completed {
            assert!(result.is_err());
        }
        assert_eq!(
            store
                .read_work_item("discord-400")
                .unwrap()
                .planning_context
                .unwrap()
                .brief
                .as_deref(),
            Some("New local brief")
        );
        assert_eq!(
            thread_snapshot(project.path())
                .approval
                .unwrap()
                .button
                .action,
            newer.action,
            "old recovery cannot clear the new control"
        );
    }
}

#[test]
fn discord_runner_preflight_checks_role_and_overwrite_permissions() {
    use serde_json::json;
    let (_home, _project, config) = fixture();
    let required: u64 = 1024 | 2048 | 16384 | 65536 | (1 << 35) | (1 << 38);
    let overwrite = |id: &str, kind: u8, allow: u64, deny: u64| json!({"id":id,"type":kind,"allow":allow.to_string(),"deny":deny.to_string()});
    let mut cases = vec![(required, vec![], true)];
    for bit in [1024, 2048, 16384, 65536, 1 << 35, 1 << 38] {
        cases.push((required & !bit, vec![], false));
    }
    cases.extend([
        (required, vec![overwrite("100", 0, 0, 1024)], false),
        (
            required,
            vec![overwrite("100", 0, 0, 1024), overwrite("101", 0, 1024, 0)],
            true,
        ),
        (
            required,
            vec![overwrite("101", 0, 0, 1024), overwrite("102", 0, 1024, 0)],
            true,
        ),
        (
            required,
            vec![overwrite("101", 0, 1024, 0), overwrite("900", 1, 0, 1024)],
            false,
        ),
        (
            required,
            vec![overwrite("101", 0, 0, 1024), overwrite("900", 1, 1024, 0)],
            true,
        ),
        (required & !1024, vec![overwrite("999", 0, 1024, 0)], false),
    ]);
    for (permissions, overwrites, allowed) in cases {
        let (endpoint, server) = http_fixture(vec![
            ("200 OK", json!({"flags":1<<19})),
            ("200 OK", json!({"id":"900"})),
            ("200 OK", json!({"owner_id":"999"})),
            ("200 OK", json!({"roles":["101","102"]})),
            (
                "200 OK",
                json!([{"id":"100","permissions":permissions.to_string()},{"id":"101","permissions":"0"},{"id":"102","permissions":"0"}]),
            ),
            (
                "200 OK",
                json!({"id":"200","type":0,"guild_id":"100","permission_overwrites":overwrites}),
            ),
        ]);
        let result = DiscordRest::with_test_endpoint("test-token".into(), endpoint)
            .unwrap()
            .preflight(&config);
        assert_eq!(result.is_ok(), allowed, "{permissions}: {result:?}");
        assert_eq!(server.join().unwrap().len(), 6);
    }
}

fn rest_preflight_responses() -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    vec![
        ("200 OK", json!({"flags":1<<19})),
        ("200 OK", json!({"id":"900"})),
        ("200 OK", json!({"owner_id":"900"})),
        ("200 OK", json!({"roles":[]})),
        ("200 OK", json!([])),
        ("200 OK", json!({"type":0,"guild_id":"100"})),
    ]
}
#[test]
fn discord_restart_rest_recovers_partial_multipart_delivery() {
    use serde_json::json;
    use sha2::{Digest, Sha256};
    let (_home, project, config) = fixture();
    let delivery = Delivery {
        id: "artifact".into(),
        channel: "400".into(),
        text: "Readable artifact.\n".repeat(240),
        message_id: None,
        approval: Some(ApprovalButton {
            action: "a".repeat(16),
            revision: "b".repeat(64),
            label: "Approve".into(),
        }),
    };
    let parts = render_delivery(&delivery, project.path());
    assert_eq!(parts.len(), 3);
    let marker = format!(
        "fluent-discord:{:x}",
        Sha256::digest(parts[1].id.as_bytes())
    );
    let page = (1000..1100).rev().map(|id| json!({"id":id.to_string(),"author":{"id":"999"},"embeds":[{"footer":{"text":marker}}]})).collect::<Vec<_>>();
    let mut responses = rest_preflight_responses();
    responses.extend([
        ("200 OK", json!([])),
        ("200 OK", json!({"id":"800"})),
        ("200 OK", json!([])),
        ("DROP", json!(null)),
    ]);
    responses.extend(rest_preflight_responses());
    responses.extend([
        ("200 OK", json!(page)),
        (
            "200 OK",
            json!([{"id":"801","author":{"id":"900"},"embeds":[{"footer":{"text":marker}}]}]),
        ),
        ("200 OK", json!([])),
        ("200 OK", json!({"id":"802"})),
    ]);
    let (endpoint, server) = http_fixture(responses);
    let mut rest = DiscordRest::with_test_endpoint("test-token".into(), endpoint.clone()).unwrap();
    rest.preflight(&config).unwrap();
    assert_eq!(rest.deliver(&parts[0]).unwrap(), "800");
    assert!(rest.deliver(&parts[1]).is_err());
    drop(rest);
    let mut restarted = DiscordRest::with_test_endpoint("test-token".into(), endpoint).unwrap();
    restarted.preflight(&config).unwrap();
    // The first receipt was saved; the second POST reached Discord but its
    // response was lost. Production history must find it beyond an impostor page.
    assert_eq!(restarted.deliver(&parts[1]).unwrap(), "801");
    assert_eq!(restarted.deliver(&parts[2]).unwrap(), "802");
    let requests = server.join().unwrap();
    assert!(
        requests
            .iter()
            .any(|r| r.starts_with("GET /channels/400/messages?limit=100&before=1000 "))
    );
    let posts = requests
        .iter()
        .filter(|r| r.starts_with("POST /channels/400/messages "))
        .collect::<Vec<_>>();
    assert_eq!(posts.len(), 3);
    for (i, post) in posts.iter().enumerate() {
        let payload: serde_json::Value =
            serde_json::from_str(post.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(payload["content"], parts[i].text);
        assert_eq!(payload["allowed_mentions"]["parse"], json!([]));
        assert_eq!(!payload["components"].is_null(), i == 2);
    }
}
#[test]
fn discord_restart_rest_reads_cursors_and_attributed_corrections() {
    use serde_json::json;
    let mut page = (1000..1100)
        .rev()
        .map(|id| json!({"id":id.to_string(),"author":{"id":"900","bot":true},"content":"bot"}))
        .collect::<Vec<_>>();
    page[0] = json!({"id":"1099","author":{"id":"301"},"content":"First recovered reply"});
    let (endpoint, server) = http_fixture(vec![
        ("200 OK", json!(page)),
        (
            "200 OK",
            json!([{"id":"1100","author":{"id":"300"},"content":"Second recovered reply"}]),
        ),
        ("404 Not Found", json!({})),
        (
            "200 OK",
            json!({"id":"1099","content":"Corrected reply","edited_timestamp":"2026-10-17T23:12:00Z"}),
        ),
    ]);
    let mut rest = DiscordRest::with_test_endpoint("test-token".into(), endpoint).unwrap();
    let first = rest.history("100", "400", "").unwrap();
    assert!(first.has_more);
    assert_eq!(first.inputs.len(), 1);
    assert_eq!(first.last_message.as_deref(), Some("1099"));
    let second = rest
        .history("100", "400", first.last_message.as_deref().unwrap())
        .unwrap();
    assert!(!second.has_more);
    let originals = [first.inputs, second.inputs].concat();
    let corrections = rest.corrections("100", "400", &originals).unwrap();
    assert!(matches!(&corrections[1].kind, InputKind::Delete { message } if message == "1100"));
    assert_eq!(corrections[1].actor, "300");
    assert!(
        matches!(&corrections[0].kind, InputKind::Edit { message, text, .. } if message == "1099" && text == "Corrected reply")
    );
    assert_eq!(corrections[0].actor, "301");
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET /channels/400/messages?limit=100&after=0 "));
    assert!(requests[1].starts_with("GET /channels/400/messages?limit=100&after=1099 "));
    let (home, project, config) = fixture();
    let mut runner =
        DiscordRunner::new(home.path(), config, Provider::default(), Discord::default()).unwrap();
    runner.ingest(command()).unwrap();
    for input in originals.iter().chain(corrections.iter()) {
        runner.ingest_recovered(input.clone()).unwrap();
    }
    for input in originals.iter().chain(corrections.iter()) {
        runner.ingest_recovered(input.clone()).unwrap();
    }
    assert_eq!(runner.provider().calls.len(), 5);
    assert_eq!(thread_snapshot(project.path()).inputs[1].actor, "301");
}
#[test]
fn discord_restart_rest_fails_closed_at_delivery_history_bound() {
    use serde_json::json;
    let page = (1000..1100)
        .rev()
        .map(|id| json!({"id":id.to_string(),"author":{"id":"900"}}))
        .collect::<Vec<_>>();
    let (endpoint, server) = http_fixture(vec![("200 OK", json!(page)); 100]);
    let mut rest = DiscordRest::with_test_endpoint("test-token".into(), endpoint).unwrap();
    let error = rest
        .deliver(&Delivery {
            id: "missing".into(),
            channel: "400".into(),
            text: "Pending".into(),
            message_id: None,
            approval: None,
        })
        .unwrap_err();
    assert!(error.to_string().contains("recovery bound"));
    assert_eq!(server.join().unwrap().len(), 100);
}

#[test]
fn discord_runner_gateway_preflight_blocks_provider_dispatch() {
    use serde_json::json;
    let (home, _project, config) = fixture();
    write_config(home.path(), &config).unwrap();
    let (endpoint, server) = http_fixture(vec![("200 OK", json!({"flags":0}))]);
    let client = DiscordRest::with_test_endpoint("test-token".into(), endpoint).unwrap();
    let error = run_runner_with_dependencies(
        home.path(),
        config,
        client,
        "test-token",
        Provider::default(),
        None,
    )
    .unwrap_err();
    assert!(error.to_string().contains("Message Content"));
    assert_eq!(server.join().unwrap().len(), 1);
    assert!(!state_root(home.path()).join("gateway.json").exists());
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "local runner fixture timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
struct LiveHttp {
    endpoint: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl LiveHttp {
    fn new(mut handle: impl FnMut(&str) -> (u16, serde_json::Value) + Send + 'static) -> Self {
        use std::io::{Read, Write};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(v) => v,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut bytes = vec![];
                while !bytes.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    bytes.push(byte[0]);
                }
                let header = String::from_utf8(bytes).unwrap();
                let length = header
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|n| n.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let mut bytes = vec![0; length];
                stream.read_exact(&mut bytes).unwrap();
                let request = format!("{header}{}", String::from_utf8(bytes).unwrap());
                let (status, body) = handle(&request);
                let body = body.to_string();
                write!(stream, "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        Self {
            endpoint,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for LiveHttp {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[test]
fn discord_runner_gateway_admits_acknowledges_and_recovers() {
    use serde_json::{Value, json};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    use tungstenite::{
        Message,
        protocol::{CloseFrame, frame::coding::CloseCode},
    };
    let (home, project, config) = fixture();
    write_config(home.path(), &config).unwrap();
    let acknowledged = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(Mutex::new(Provider::default()));
    struct SharedProvider(Arc<Mutex<Provider>>, Arc<AtomicBool>);
    impl ConversationProvider for SharedProvider {
        fn start(
            &mut self,
            key: &str,
            scope: &ProviderScope,
            text: &str,
            actor: &str,
        ) -> ProviderResult<(String, String)> {
            assert!(
                self.1.load(Ordering::SeqCst),
                "provider ran before acknowledgement"
            );
            self.0.lock().unwrap().start(key, scope, text, actor)
        }
        fn resume(
            &mut self,
            key: &str,
            session: &str,
            scope: &ProviderScope,
            text: &str,
            actor: &str,
        ) -> ProviderResult<String> {
            self.0
                .lock()
                .unwrap()
                .resume(key, session, scope, text, actor)
        }
    }
    let ack = acknowledged.clone();
    let http_home = home.path().to_path_buf();
    let http_project = project.path().to_path_buf();
    let mut messages: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut created = false;
    let http = LiveHttp::new(move |request| {
        let words = request
            .lines()
            .next()
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>();
        let (method, path) = (words[0], words[1]);
        let body: Value =
            serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap_or(Value::Null);
        if path.starts_with("/interactions/") {
            let id = path.split('/').nth(2).unwrap();
            let inbox = fs::read_dir(state_root(&http_home).join("inbox"))
                .unwrap()
                .map(|e| fs::read(e.unwrap().path()).unwrap())
                .collect::<Vec<_>>();
            assert!(
                inbox
                    .iter()
                    .any(|bytes| serde_json::from_slice::<Input>(bytes).unwrap().id == id),
                "ack preceded durable ingress"
            );
            assert!(
                inbox
                    .iter()
                    .all(|bytes| !String::from_utf8_lossy(bytes).contains("test-token"))
            );
            assert_eq!(body["data"]["allowed_mentions"]["parse"], json!([]));
            if id == "600" {
                let session: GatewaySession = serde_json::from_slice(
                    &fs::read(state_root(&http_home).join("gateway.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    session.sequence,
                    Some(1),
                    "sequence advanced before durable ingress and ack"
                );
                assert!(
                    !http_project
                        .join(".fluent/work/discord/threads/400.json")
                        .exists()
                );
                ack.store(true, Ordering::SeqCst);
            }
            return (if path.contains("expired") { 404 } else { 200 }, json!({}));
        }
        match path {
            "/oauth2/applications/@me" => return (200, json!({"flags":1<<19})),
            "/users/@me" => return (200, json!({"id":"900"})),
            "/guilds/100" => return (200, json!({"owner_id":"900"})),
            "/guilds/100/members/900" => return (200, json!({"roles":[]})),
            "/guilds/100/roles" => return (200, json!([])),
            "/channels/200" => return (200, json!({"type":0,"guild_id":"100"})),
            "/gateway/bot" => return (200, json!({"url":"wss://gateway.discord.gg"})),
            "/channels/400" => {
                return (
                    if created { 200 } else { 404 },
                    json!({"type":11,"parent_id":"200"}),
                );
            }
            "/channels/200/messages/400/threads" => {
                created = true;
                return (200, json!({"id":"400"}));
            }
            _ => {}
        }
        let channel = path.split('/').nth(2).unwrap().to_owned();
        if method == "GET" {
            if path.contains("after=") {
                return (200, json!([]));
            }
            return (
                200,
                json!(messages.get(&channel).cloned().unwrap_or_default()),
            );
        }
        let mut message = body;
        let id = if channel == "200" {
            "400".into()
        } else {
            (700 + messages.get(&channel).map_or(0, Vec::len)).to_string()
        };
        message["id"] = json!(id);
        message["author"] = json!({"id":"900","bot":true});
        messages.entry(channel).or_default().push(message);
        (200, json!({"id":id}))
    });
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let ws_url = format!("ws://{}", listener.local_addr().unwrap());
    let gateway_home = home.path().to_path_buf();
    let gateway_project = project.path().to_path_buf();
    let gateway = std::thread::spawn(move || {
        let accept = || {
            let mut accepted = None;
            wait_until(|| {
                accepted = listener.accept().ok();
                accepted.is_some()
            });
            let stream = accepted.unwrap().0;
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            tungstenite::accept(stream).unwrap()
        };
        let send = |socket: &mut tungstenite::WebSocket<std::net::TcpStream>, value: Value| {
            socket.send(Message::Text(value.to_string())).unwrap()
        };
        let mut socket = accept();
        send(
            &mut socket,
            json!({"op":10,"d":{"heartbeat_interval":60000}}),
        );
        let identify: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(identify["op"], 2);
        send(
            &mut socket,
            json!({"op":0,"s":1,"t":"READY","d":{"session_id":"session","resume_gateway_url":"wss://gateway.discord.gg"}}),
        );
        send(
            &mut socket,
            json!({"op":0,"s":2,"t":"INTERACTION_CREATE","d":{"id":"600","guild_id":"100","channel_id":"200","token":"test-token","type":2,"member":{"user":{"id":"300"}},"data":{"name":"fluent","options":[{"name":"project","value":"app"},{"name":"message","value":"plan this"}]}}}),
        );
        let snapshot = || {
            fs::read(gateway_project.join(".fluent/work/discord/threads/400.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Thread>(&bytes).ok())
        };
        wait_until(|| {
            snapshot().is_some_and(|t| {
                t.session.is_some() && t.deliveries.iter().all(|d| d.message_id.is_some())
            })
        });
        assert!(read_operational_state(&gateway_home).unwrap().connected);
        let reply = json!({"op":0,"s":3,"t":"MESSAGE_CREATE","d":{"id":"601","guild_id":"100","channel_id":"400","author":{"id":"301"},"content":"continue"}});
        send(&mut socket, reply.clone());
        wait_until(|| snapshot().is_some_and(|t| t.inputs.len() == 2 && t.pending.is_none()));
        send(
            &mut socket,
            json!({"op":0,"s":4,"t":"MESSAGE_UPDATE","d":{"id":"601","guild_id":"100","channel_id":"400","content":"correction","edited_timestamp":"revision-1"}}),
        );
        wait_until(|| snapshot().is_some_and(|t| t.inputs.len() == 3 && t.pending.is_none()));
        send(
            &mut socket,
            json!({"op":0,"s":5,"t":"MESSAGE_DELETE","d":{"id":"601","guild_id":"100","channel_id":"400"}}),
        );
        wait_until(|| snapshot().is_some_and(|t| t.inputs.len() == 4 && t.pending.is_none()));
        send(
            &mut socket,
            json!({"op":0,"s":6,"t":"MESSAGE_CREATE","d":{"id":"610","guild_id":"100","channel_id":"400","author":{"id":"300"},"content":"work create"}}),
        );
        wait_until(|| snapshot().is_some_and(|t| t.approval.is_some()));
        let button = snapshot().unwrap().approval.unwrap().button;
        let component = |id: &str, token: &str, seq: u64| json!({"op":0,"s":seq,"t":"INTERACTION_CREATE","d":{"id":id,"guild_id":"100","channel_id":"400","token":token,"type":3,"member":{"user":{"id":"300"}},"data":{"custom_id":format!("fluent:{}:{}",button.action,button.revision)}}});
        send(&mut socket, component("611", "expired", 7));
        wait_until(|| {
            snapshot().is_some_and(|t| {
                t.deliveries
                    .iter()
                    .any(|d| d.id.contains("611") && d.message_id.is_some())
            })
        });
        assert!(
            !gateway_project
                .join(".fluent/work/items/discord-400.json")
                .exists()
        );
        send(&mut socket, component("612", "valid", 8));
        wait_until(|| {
            snapshot().is_some_and(|t| t.approvals.iter().any(|a| a.input == "612" && a.completed))
        });
        send(&mut socket, json!({"op":7}));
        drop(socket);
        let mut socket = accept();
        send(
            &mut socket,
            json!({"op":10,"d":{"heartbeat_interval":60000}}),
        );
        let resume: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(resume["op"], 6);
        assert_eq!(resume["d"]["seq"], 8);
        send(&mut socket, json!({"op":0,"s":9,"t":"RESUMED","d":{}}));
        let mut reply = reply;
        reply["s"] = json!(10);
        send(&mut socket, reply);
        wait_until(|| {
            fs::read(state_root(&gateway_home).join("gateway.json"))
                .ok()
                .and_then(|b| serde_json::from_slice::<GatewaySession>(&b).ok())
                .is_some_and(|s| s.sequence == Some(10))
        });
        socket
            .close(Some(CloseFrame {
                code: CloseCode::Library(4014),
                reason: "Missing intent".into(),
            }))
            .unwrap();
    });
    let client =
        DiscordRest::with_test_endpoint("test-token".into(), http.endpoint.clone()).unwrap();
    let result = run_runner_with_dependencies(
        home.path(),
        config,
        client,
        "test-token",
        SharedProvider(calls.clone(), acknowledged),
        Some(GatewayFixture {
            url: ws_url,
            connections: 2,
        }),
    );
    gateway.join().unwrap();
    assert!(result.unwrap_err().to_string().contains("Message Content"));
    assert!(!read_operational_state(home.path()).unwrap().connected);
    let calls = calls.lock().unwrap();
    assert_eq!(calls.calls.len(), 4);
    assert!(calls.calls[2].3.ends_with(":301"));
    assert!(calls.calls[3].2.contains("deleted"));
}

#[test]
fn discord_approval_checks_revision_inside_model_mutation() {
    use fluent::work_approval::WorkApproval;
    use fluent::work_model::{WorkItem, WorkModelStore};
    let (_home, project, _) = fixture();
    let store = WorkModelStore::new(project.path());
    store
        .create_work_item(&WorkItem {
            id: "guarded".into(),
            title: "Original".into(),
            ..Default::default()
        })
        .unwrap();
    let approval = WorkApproval {
        operation: "discord-operation".into(),
        expected_state: WorkApproval::fingerprint(&store.read_work_item("guarded").unwrap()),
    };
    let (entered, held) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let path = project.path().to_path_buf();
    let owner = std::thread::spawn(move || {
        WorkModelStore::new(&path)
            .mutate_work_item("guarded", |item| {
                entered.send(()).unwrap();
                wait.recv().unwrap();
                item.title = "Changed by owner".into();
                Ok(())
            })
            .unwrap()
    });
    held.recv().unwrap();
    let path = project.path().to_path_buf();
    let checked = approval.clone();
    let effect = std::thread::spawn(move || {
        checked.mutate(&path, "guarded", |item| {
            item.title = "Stale Discord change".into();
            Ok(())
        })
    });
    release.send(()).unwrap();
    owner.join().unwrap();
    assert!(effect.join().unwrap().is_err());
    assert_eq!(
        store.read_work_item("guarded").unwrap().title,
        "Changed by owner"
    );
    assert!(!approval.applied(project.path(), "guarded").unwrap());
}

#[test]
fn discord_approval_landing_admission_never_reauthorizes_changed_work() {
    use fluent::work_approval::WorkApproval;
    use fluent::work_model::{WorkItem, WorkModelStore};
    let (_home, project, _) = fixture();
    let store = WorkModelStore::new(project.path());
    store
        .create_work_item(&WorkItem {
            id: "landing".into(),
            title: "Approved".into(),
            ..Default::default()
        })
        .unwrap();
    let approval = WorkApproval {
        operation: "land-operation".into(),
        expected_state: WorkApproval::fingerprint(&store.read_work_item("landing").unwrap()),
    };
    approval.admit(project.path(), "landing").unwrap();
    assert!(
        !approval.applied(project.path(), "landing").unwrap(),
        "admission is not effect completion"
    );
    store
        .mutate_work_item("landing", |item| {
            item.title = "Changed".into();
            Ok(())
        })
        .unwrap();
    assert!(approval.admit(project.path(), "landing").is_err());
}
