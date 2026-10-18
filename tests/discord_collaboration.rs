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
