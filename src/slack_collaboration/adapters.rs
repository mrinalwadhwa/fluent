use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::content::ContentResolver;

use super::{
    ConversationProvider, InboundChange, InboundEnvelope, OutboundDelivery, ProviderError,
    ProviderFailure, ProviderOutput, ProviderResult, ProviderScope, ProviderTurnRecovery,
    RecoveryTarget, SchedulerClient, SchedulerRequest, SchedulerStatus, SlackTransport,
    WorkExecutionRequest, WorkQueueOutcome, WorkScheduler,
};

const STRUCTURED_REPLY_INSTRUCTION: &str = "Return exactly one JSON object. Use {\"type\":\"message\",\"text\":\"...\",\"references\":[{\"path\":\"src/lib.rs\",\"line\":1}]} for a conversational reply. Every reference must name an existing repository-relative file and a one-based line; use an empty references array when no source location supports the reply. When you complete or revise a planning artifact, use {\"type\":\"planning_artifact\",\"kind\":\"brief|behaviors|approach|plan\",\"markdown\":\"...\"} with the complete Markdown.";

fn validate_structured_provider_output(raw: &str, scope: &ProviderScope) -> Result<String> {
    let output: ProviderOutput = serde_json::from_str(raw)
        .context("incompatible provider protocol: turn result was not structured JSON")?;
    if let ProviderOutput::Message { references, .. } = &output {
        let checkout = scope
            .checkout
            .canonicalize()
            .context("incompatible provider protocol: canonicalize bound checkout")?;
        for reference in references {
            if reference.line == 0
                || reference.path.is_absolute()
                || reference
                    .path
                    .components()
                    .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                bail!(
                    "incompatible provider protocol: code reference must contain a repository-relative path and one-based line"
                );
            }
            let referenced = scope
                .checkout
                .join(&reference.path)
                .canonicalize()
                .context("incompatible provider protocol: referenced code does not exist")?;
            if !referenced.is_file() || referenced.strip_prefix(&checkout).is_err() {
                bail!(
                    "incompatible provider protocol: code reference is outside the bound checkout"
                );
            }
            let source = fs::read_to_string(&referenced)
                .context("incompatible provider protocol: referenced code is not text")?;
            if source.lines().nth(reference.line - 1).is_none() {
                bail!("incompatible provider protocol: code reference line does not exist");
            }
        }
    }
    serde_json::to_string(&output).context("serialize structured provider result")
}

pub struct LocalSchedulerClient {
    socket_path: PathBuf,
    project_roots: Vec<PathBuf>,
}

impl LocalSchedulerClient {
    pub fn new(home: &std::path::Path, project_roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            socket_path: crate::scheduler_service::socket_path(home),
            project_roots: project_roots.into_iter().collect(),
        }
    }
}

impl SchedulerClient for LocalSchedulerClient {
    fn status(&self, request: &SchedulerRequest) -> Result<SchedulerStatus> {
        if request.version != 1 || request.operation != "status" {
            bail!("unsupported scheduler request")
        }
        let health = crate::scheduler_service::send_health_request(&self.socket_path)?;
        if health.generation != crate::scheduler_service::PROTOCOL_GENERATION {
            bail!("scheduler service protocol generation mismatch");
        }
        Ok(SchedulerStatus { reachable: true })
    }
}

impl WorkScheduler for LocalSchedulerClient {
    fn submit(&self, request: &WorkExecutionRequest) -> Result<WorkQueueOutcome> {
        if request.version != crate::slack_collaboration::WorkControl::REQUEST_VERSION {
            bail!("unsupported Work execution request version");
        }
        let project_root = self
            .project_roots
            .iter()
            .find(|root| {
                crate::work_model::WorkModelStore::new(root)
                    .read_work_item(&request.work_item_id)
                    .is_ok()
            })
            .context("Work execution request does not belong to a configured checkout")?;
        let scheduler_request = crate::scheduler_service::AttemptExecutionRequest {
            id: request.id.clone(),
            work_item_id: request.work_item_id.clone(),
            attempt_id: if request.id.starts_with("discord-work-") {
                format!("discord-{}", request.id)
            } else {
                format!("slack-{}", request.id)
            },
            created_at: request.created_at.clone(),
        };
        let token = crate::scheduler_service::send_execution_request(
            &self.socket_path,
            project_root,
            &scheduler_request,
        )?;
        if token.already_queued {
            Ok(WorkQueueOutcome::AlreadyQueued {
                request_id: token.request_id,
            })
        } else {
            Ok(WorkQueueOutcome::Queued {
                request_id: token.request_id,
            })
        }
    }
}

pub struct SocketModeTransport {
    web: SlackWebApiTransport,
    socket: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>,
}

impl SocketModeTransport {
    fn open_socket(
        api_base: &str,
        app_token: &str,
    ) -> Result<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>>
    {
        let response = ureq::post(&format!(
            "{}/apps.connections.open",
            api_base.trim_end_matches('/')
        ))
        .set("Authorization", &format!("Bearer {app_token}"))
        .send_json(serde_json::json!({}))?;
        let value: serde_json::Value = response.into_json()?;
        let url = value["url"].as_str().context("Slack Socket Mode URL")?;
        Ok(tungstenite::connect(url)?.0)
    }

    pub fn connect(
        api_base: impl Into<String>,
        app_token: String,
        bot_token: String,
    ) -> Result<Self> {
        let api_base = api_base.into();
        let socket = Self::open_socket(&api_base, &app_token)?;
        Ok(Self {
            web: SlackWebApiTransport::new(api_base, app_token, bot_token),
            socket,
        })
    }
}

pub fn decode_socket_mode_envelope(text: &str) -> Result<InboundEnvelope> {
    let value: serde_json::Value = serde_json::from_str(text)?;
    let payload = &value["payload"];
    if payload["type"].as_str() == Some("block_actions") {
        let action = &payload["actions"][0];
        return Ok(InboundEnvelope {
            id: value["envelope_id"]
                .as_str()
                .context("Slack envelope id")?
                .into(),
            acknowledgement_id: value["envelope_id"]
                .as_str()
                .context("Slack envelope id")?
                .into(),
            channel: payload["channel"]["id"]
                .as_str()
                .context("Slack interaction channel")?
                .into(),
            timestamp: payload["message"]["ts"]
                .as_str()
                .context("Slack interaction timestamp")?
                .into(),
            thread_timestamp: payload["message"]["thread_ts"].as_str().map(str::to_owned),
            user: payload["user"]["id"]
                .as_str()
                .context("Slack interaction user")?
                .into(),
            text: action["action_id"].as_str().unwrap_or_default().into(),
            change: InboundChange::Message,
            approval_action_id: action["value"].as_str().map(str::to_owned),
            approval_artifact_revision: action["block_id"].as_str().map(str::to_owned),
        });
    }
    let event = &payload["event"];
    let change = match event["subtype"].as_str() {
        Some("message_changed") => InboundChange::Edit,
        Some("message_deleted") => InboundChange::Delete,
        _ => InboundChange::Message,
    };
    let message = match change {
        InboundChange::Edit => &event["message"],
        InboundChange::Delete => &event["previous_message"],
        InboundChange::Message => event,
    };
    Ok(InboundEnvelope {
        id: value["payload"]["event_id"]
            .as_str()
            .context("Slack event id")?
            .into(),
        acknowledgement_id: value["envelope_id"]
            .as_str()
            .context("Slack envelope id")?
            .into(),
        channel: event["channel"].as_str().context("Slack channel")?.into(),
        timestamp: message["ts"]
            .as_str()
            .or_else(|| event["deleted_ts"].as_str())
            .context("Slack timestamp")?
            .into(),
        thread_timestamp: message["thread_ts"].as_str().map(str::to_owned),
        user: message["user"]
            .as_str()
            .or_else(|| event["previous_message"]["user"].as_str())
            .context("Slack user")?
            .into(),
        text: message["text"].as_str().unwrap_or_default().into(),
        change,
        approval_action_id: event["actions"][0]["value"].as_str().map(str::to_owned),
        approval_artifact_revision: event["actions"][0]["block_id"].as_str().map(str::to_owned),
    })
}

impl SlackTransport for SocketModeTransport {
    fn acknowledge(&mut self, event_id: &str) -> Result<()> {
        self.socket.send(tungstenite::Message::Text(
            serde_json::json!({"envelope_id": event_id}).to_string(),
        ))?;
        Ok(())
    }

    fn post(&mut self, delivery: &OutboundDelivery) -> Result<()> {
        self.web.post(delivery)
    }

    fn respond_private(&mut self, channel: &str, user: &str, text: &str) -> Result<()> {
        self.web.respond_private(channel, user, text)
    }

    fn check(&self) -> Result<()> {
        self.web.check()
    }

    fn receive(&mut self) -> Result<InboundEnvelope> {
        let message = self.socket.read()?;
        decode_socket_mode_envelope(&message.into_text()?)
    }

    fn receive_timeout(&mut self, timeout: std::time::Duration) -> Result<Option<InboundEnvelope>> {
        use tungstenite::stream::MaybeTlsStream;
        let socket = match self.socket.get_mut() {
            MaybeTlsStream::Plain(stream) => stream,
            MaybeTlsStream::Rustls(stream) => &mut stream.sock,
            _ => bail!("unsupported Socket Mode TLS transport"),
        };
        socket.set_read_timeout(Some(timeout))?;
        match self.socket.read() {
            Ok(message) => decode_socket_mode_envelope(&message.into_text()?).map(Some),
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    fn reconnect(&mut self) -> Result<()> {
        self.socket = Self::open_socket(&self.web.api_base, &self.web.app_token)?;
        Ok(())
    }

    fn bounded_history(&mut self, threads: &[RecoveryTarget]) -> Result<Vec<InboundEnvelope>> {
        self.web.recover_history(threads)
    }
}

impl SlackWebApiTransport {
    #[doc(hidden)]
    pub fn recover_history(&self, threads: &[RecoveryTarget]) -> Result<Vec<InboundEnvelope>> {
        let mut events = Vec::new();
        for target in threads {
            let mut cursor: Option<String> = None;
            loop {
                let mut body = serde_json::json!({
                    "channel": target.channel,
                    "ts": target.thread_timestamp,
                    "oldest": target.oldest,
                    "inclusive": false,
                    "limit": 100,
                });
                if let Some(cursor) = &cursor {
                    body["cursor"] = serde_json::Value::String(cursor.clone());
                }
                let value = self.call_value("conversations.replies", &self.bot_token, body)?;
                for message in value["messages"].as_array().into_iter().flatten() {
                    let Some(timestamp) = message["ts"].as_str() else {
                        continue;
                    };
                    if timestamp == target.thread_timestamp {
                        continue;
                    }
                    events.push(InboundEnvelope {
                        id: format!("history:{}:{timestamp}", target.channel),
                        acknowledgement_id: String::new(),
                        channel: target.channel.clone(),
                        timestamp: timestamp.into(),
                        thread_timestamp: Some(target.thread_timestamp.clone()),
                        user: message["user"].as_str().unwrap_or_default().into(),
                        text: message["text"].as_str().unwrap_or_default().into(),
                        change: InboundChange::Message,
                        approval_action_id: None,
                        approval_artifact_revision: None,
                    });
                }
                cursor = value["response_metadata"]["next_cursor"]
                    .as_str()
                    .filter(|cursor| !cursor.is_empty())
                    .map(str::to_owned);
                if cursor.is_none() {
                    break;
                }
            }
        }
        events.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then(left.id.cmp(&right.id))
        });
        Ok(events)
    }
}

impl SlackWebApiTransport {
    fn respond_private(&self, channel: &str, user: &str, text: &str) -> Result<()> {
        self.call(
            "chat.postEphemeral",
            &self.bot_token,
            serde_json::json!({"channel": channel, "user": user, "text": text}),
        )
    }
}

pub struct SlackWebApiTransport {
    api_base: String,
    bot_token: String,
    app_token: String,
}

impl SlackWebApiTransport {
    pub fn new(api_base: impl Into<String>, app_token: String, bot_token: String) -> Self {
        Self {
            api_base: api_base.into().trim_end_matches('/').into(),
            app_token,
            bot_token,
        }
    }

    fn call_value(
        &self,
        method: &str,
        token: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let response = ureq::post(&format!("{}/{method}", self.api_base))
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(body)?;
        let value: serde_json::Value = response.into_json()?;
        if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            bail!("Slack {method} failed")
        }
        Ok(value)
    }

    fn call(&self, method: &str, token: &str, body: serde_json::Value) -> Result<()> {
        self.call_value(method, token, body).map(|_| ())
    }
}

fn slack_client_message_id(delivery: &OutboundDelivery) -> String {
    let mut input = Sha256::new();
    input.update(delivery.id.as_bytes());
    input.update([0]);
    input.update(delivery.channel.as_bytes());
    input.update([0]);
    input.update(delivery.thread_timestamp.as_bytes());
    input.update([0]);
    input.update(delivery.text.as_bytes());
    let digest = input.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

impl SlackTransport for SlackWebApiTransport {
    fn acknowledge(&mut self, event_id: &str) -> Result<()> {
        self.call(
            "apps.connections.ack",
            &self.app_token,
            serde_json::json!({"envelope_id": event_id}),
        )
    }

    fn post(&mut self, delivery: &OutboundDelivery) -> Result<()> {
        let blocks = delivery.approval_control.as_ref().map(|control| {
            serde_json::json!([{
                "type": "actions",
                "block_id": control.artifact_revision,
                "elements": [{
                    "type": "button",
                    "text": {"type": "plain_text", "text": control.label},
                    "action_id": "fluent_approval",
                    "value": control.action_id
                }]
            }])
        });
        self.call(
            "chat.postMessage",
            &self.bot_token,
            serde_json::json!({
                "channel": delivery.channel,
                "thread_ts": delivery.thread_timestamp,
                "text": delivery.text,
                "blocks": blocks,
                "client_msg_id": slack_client_message_id(delivery)
            }),
        )
    }

    fn post_with_identity(&mut self, delivery: &OutboundDelivery) -> Result<String> {
        let value = self.call_value(
            "chat.postMessage",
            &self.bot_token,
            serde_json::json!({
                "channel": delivery.channel,
                "thread_ts": delivery.thread_timestamp,
                "text": delivery.text,
                "client_msg_id": slack_client_message_id(delivery)
            }),
        )?;
        value["ts"]
            .as_str()
            .map(str::to_owned)
            .context("Slack chat.postMessage omitted message timestamp")
    }

    fn update(&mut self, delivery: &OutboundDelivery) -> Result<()> {
        let timestamp = delivery
            .update_timestamp
            .as_deref()
            .context("Slack update omitted message timestamp")?;
        self.call(
            "chat.update",
            &self.bot_token,
            serde_json::json!({
                "channel": delivery.channel,
                "ts": timestamp,
                "text": delivery.text
            }),
        )
    }

    fn respond_private(&mut self, channel: &str, user: &str, text: &str) -> Result<()> {
        self.call(
            "chat.postEphemeral",
            &self.bot_token,
            serde_json::json!({"channel": channel, "user": user, "text": text}),
        )
    }

    fn check(&self) -> Result<()> {
        let response = ureq::post(&format!("{}/auth.test", self.api_base))
            .set("Authorization", &format!("Bearer {}", self.bot_token))
            .send_json(serde_json::json!({}))?;
        let granted: std::collections::BTreeSet<_> = response
            .header("x-oauth-scopes")
            .context("Slack auth.test omitted installed bot scopes")?
            .split(',')
            .map(str::trim)
            .collect();
        for required in [
            "app_mentions:read",
            "channels:history",
            "im:history",
            "chat:write",
        ] {
            if !granted.contains(required) {
                bail!("Slack bot is missing required scope {required}");
            }
        }
        let value: serde_json::Value = response.into_json()?;
        if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
            bail!("Slack auth.test failed")
        }
        Ok(())
    }
}

pub struct CodexAppServerProvider {
    executable: PathBuf,
    scoped: bool,
    test_sandbox_launcher: Option<PathBuf>,
}

struct CodexConnection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    pending: VecDeque<serde_json::Value>,
}

impl CodexConnection {
    fn send(&mut self, value: &serde_json::Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, value)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn read(&mut self) -> Result<serde_json::Value> {
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            bail!("Codex App Server closed its protocol stream")
        }
        serde_json::from_str(&line).context("incompatible Codex provider protocol")
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> Result<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;
        loop {
            let message = self.read()?;
            if message.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    bail!("Codex {method} failed: {error}")
                }
                return message
                    .get("result")
                    .cloned()
                    .context("incompatible Codex provider protocol: response omitted result");
            }
            self.pending.push_back(message);
        }
    }

    fn notification(&mut self, method: &str) -> Result<()> {
        self.send(&serde_json::json!({"jsonrpc": "2.0", "method": method}))
    }

    fn next_message(&mut self) -> Result<serde_json::Value> {
        if let Some(message) = self.pending.pop_front() {
            Ok(message)
        } else {
            self.read()
        }
    }
}

impl Drop for CodexConnection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl CodexAppServerProvider {
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            scoped: false,
            test_sandbox_launcher: None,
        }
    }

    pub fn new_scoped(executable: PathBuf) -> Self {
        Self {
            executable,
            scoped: true,
            test_sandbox_launcher: None,
        }
    }

    #[doc(hidden)]
    pub fn new_scoped_with_test_launcher(executable: PathBuf, launcher: PathBuf) -> Self {
        Self {
            executable,
            scoped: true,
            test_sandbox_launcher: Some(launcher),
        }
    }

    #[doc(hidden)]
    pub fn render_scope_profile(
        &self,
        scope: &ProviderScope,
    ) -> Result<Option<crate::os::SandboxProfile>> {
        if !self.scoped {
            return Ok(None);
        }
        let private_home = scope.slice_draft.join(".provider-home");
        let codex_home = scope.slice_draft.join(".codex-home");
        fs::create_dir_all(&private_home)?;
        fs::create_dir_all(&codex_home)?;
        let resolver = ContentResolver::new(None);
        crate::os::render_profile_for_scoped_codex_provider(
            &resolver,
            &private_home,
            std::slice::from_ref(&scope.slice_draft),
            std::slice::from_ref(&scope.checkout),
            &codex_home,
        )
        .map(Some)
    }

    #[doc(hidden)]
    pub fn configure_scope_environment(&self, command: &mut Command, scope: &ProviderScope) {
        if self.scoped {
            command
                .env("HOME", scope.slice_draft.join(".provider-home"))
                .env("CODEX_HOME", scope.slice_draft.join(".codex-home"));
        }
    }

    fn connect(&self, scope: &ProviderScope) -> Result<CodexConnection> {
        fs::create_dir_all(&scope.slice_draft)?;
        let profile = self.render_scope_profile(scope)?;
        if let Some(profile) = &profile
            && self.test_sandbox_launcher.is_none()
        {
            crate::os::preflight_profile(&profile)?;
        }
        let mut command = if let Some(profile) = &profile {
            if let Some(launcher) = &self.test_sandbox_launcher {
                let mut command = Command::new(launcher);
                command.arg("-f").arg(&profile.path).arg(&self.executable);
                command
            } else {
                crate::os::command_under_profile(profile, &self.executable)
            }
        } else {
            Command::new(&self.executable)
        };
        command.arg("app-server");
        self.configure_scope_environment(&mut command, scope);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().context("open Codex App Server stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("open Codex App Server stdout")?;
        let mut connection = CodexConnection {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            pending: VecDeque::new(),
        };
        connection.request(
            "initialize",
            serde_json::json!({
                "clientInfo": {"name": "fluent-slack", "version": env!("CARGO_PKG_VERSION")}
            }),
        )?;
        connection.notification("initialized")?;
        Ok(connection)
    }

    fn resume_thread(
        &self,
        connection: &mut CodexConnection,
        session: &str,
        scope: &ProviderScope,
    ) -> Result<()> {
        connection.request(
            "thread/resume",
            serde_json::json!({"threadId": session, "cwd": scope.checkout}),
        )?;
        Ok(())
    }

    fn recovered_turn(
        result: &serde_json::Value,
        action_id: &str,
        scope: &ProviderScope,
    ) -> Result<ProviderTurnRecovery> {
        for turn in result["thread"]["turns"].as_array().into_iter().flatten() {
            let matches = turn["items"].as_array().into_iter().flatten().any(|item| {
                item["type"] == "userMessage" && item["clientId"].as_str() == Some(action_id)
            });
            if !matches {
                continue;
            }
            let turn_id = turn["id"].as_str().unwrap_or_default().to_owned();
            if turn["status"] != "completed" {
                return Ok(ProviderTurnRecovery::Accepted { turn_id });
            }
            let reply = turn["items"]
                .as_array()
                .into_iter()
                .flatten()
                .rev()
                .find(|item| item["type"] == "agentMessage")
                .and_then(|item| item["text"].as_str())
                .context("incompatible Codex provider protocol: completed turn omitted reply")?
                .to_owned();
            let reply = validate_structured_provider_output(&reply, scope)?;
            return Ok(ProviderTurnRecovery::Completed { turn_id, reply });
        }
        Ok(ProviderTurnRecovery::Missing)
    }
}

impl ConversationProvider for CodexAppServerProvider {
    fn name(&self) -> &str {
        "codex"
    }
    fn start(
        &mut self,
        action_id: &str,
        scope: &ProviderScope,
        contribution: &str,
        actor: &str,
    ) -> ProviderResult<(String, String)> {
        let session = self
            .prepare_session(scope)?
            .context("Codex did not create a thread")?;
        let (_, reply) = self.run_turn(
            action_id,
            Some(&session),
            scope,
            contribution,
            actor,
            &mut |_| Ok(()),
        )?;
        Ok((session, reply))
    }

    fn resume(
        &mut self,
        action_id: &str,
        session: &str,
        scope: &ProviderScope,
        contribution: &str,
        actor: &str,
    ) -> ProviderResult<String> {
        Ok(self
            .run_turn(
                action_id,
                Some(session),
                scope,
                contribution,
                actor,
                &mut |_| Ok(()),
            )?
            .1)
    }

    fn prepare_session(&mut self, scope: &ProviderScope) -> ProviderResult<Option<String>> {
        let mut connection = self.connect(scope)?;
        let result = connection.request(
            "thread/start",
            serde_json::json!({
                "cwd": scope.checkout,
                "approvalPolicy": "never",
                "sandbox": "workspace-write",
            }),
        )?;
        Ok(Some(
            result["thread"]["id"]
                .as_str()
                .context("incompatible Codex provider protocol: thread/start omitted thread id")?
                .to_owned(),
        ))
    }

    fn recover_turn(
        &mut self,
        action_id: &str,
        session: &str,
        scope: &ProviderScope,
    ) -> ProviderResult<ProviderTurnRecovery> {
        let mut connection = self.connect(scope)?;
        self.resume_thread(&mut connection, session, scope)?;
        let result = connection.request(
            "thread/read",
            serde_json::json!({"threadId": session, "includeTurns": true}),
        )?;
        Ok(Self::recovered_turn(&result, action_id, scope)?)
    }

    fn run_turn(
        &mut self,
        action_id: &str,
        session: Option<&str>,
        scope: &ProviderScope,
        contribution: &str,
        actor: &str,
        accepted: &mut dyn FnMut(&str) -> Result<()>,
    ) -> ProviderResult<(Option<String>, String)> {
        let session = session.context("Codex turn requires a persisted thread id")?;
        let mut connection = self.connect(scope)?;
        self.resume_thread(&mut connection, session, scope)?;
        let result = connection.request(
            "turn/start",
            serde_json::json!({
                "threadId": session,
                "clientUserMessageId": action_id,
                "input": [{"type": "text", "text": format!(
                    "Slack user {actor}: {contribution}\n\n{STRUCTURED_REPLY_INSTRUCTION}"
                )}],
            }),
        )?;
        let turn_id = result["turn"]["id"]
            .as_str()
            .context("incompatible Codex provider protocol: turn/start omitted turn id")?
            .to_owned();
        accepted(&turn_id).map_err(|error| ProviderError::internal(format!("{error:#}")))?;
        let mut reply = None;
        loop {
            let message = connection.next_message()?;
            let method = message["method"].as_str().unwrap_or_default();
            let params = &message["params"];
            if method == "item/completed"
                && params["turnId"].as_str() == Some(&turn_id)
                && params["item"]["type"] == "agentMessage"
            {
                reply = params["item"]["text"].as_str().map(str::to_owned);
            }
            if method == "turn/completed" && params["turn"]["id"].as_str() == Some(&turn_id) {
                if params["turn"]["status"] != "completed" {
                    return Err(ProviderError::new(
                        ProviderFailure::Unavailable,
                        format!("Codex turn {turn_id} did not complete successfully"),
                    ));
                }
                let reply = reply.context(
                    "incompatible Codex provider protocol: completed turn omitted agent message",
                )?;
                return Ok((None, validate_structured_provider_output(&reply, scope)?));
            }
        }
    }

    fn ready(&self) -> ProviderResult<()> {
        (|| -> Result<()> {
            Command::new(&self.executable)
                .arg("app-server")
                .arg("--help")
                .status()?
                .success()
                .then_some(())
                .context("Codex App Server is unavailable")
        })()
        .map_err(ProviderError::from)
    }
}

/// Dispatches persisted provider identities to their owning adapters.
pub struct ProviderRegistry {
    providers: std::collections::BTreeMap<String, Box<dyn ConversationProvider>>,
    selected: String,
}

pub struct ClaudeStructuredSessionProvider {
    executable: PathBuf,
    scoped: bool,
    test_sandbox_launcher: Option<PathBuf>,
}
impl ClaudeStructuredSessionProvider {
    pub fn new(executable: PathBuf) -> Self {
        Self {
            executable,
            scoped: false,
            test_sandbox_launcher: None,
        }
    }

    pub fn new_scoped(executable: PathBuf) -> Self {
        Self {
            executable,
            scoped: true,
            test_sandbox_launcher: None,
        }
    }

    #[doc(hidden)]
    pub fn new_scoped_with_test_launcher(executable: PathBuf, launcher: PathBuf) -> Self {
        Self {
            executable,
            scoped: true,
            test_sandbox_launcher: Some(launcher),
        }
    }

    #[doc(hidden)]
    pub fn render_scope_profile(
        &self,
        scope: &ProviderScope,
    ) -> Result<Option<crate::os::SandboxProfile>> {
        if !self.scoped {
            return Ok(None);
        }
        let private_home = scope.slice_draft.join(".provider-home");
        let claude_home = scope.slice_draft.join(".claude-home");
        fs::create_dir_all(&private_home)?;
        fs::create_dir_all(&claude_home)?;
        crate::os::render_profile_for_scoped_codex_provider(
            &ContentResolver::new(None),
            &private_home,
            std::slice::from_ref(&scope.slice_draft),
            std::slice::from_ref(&scope.checkout),
            &claude_home,
        )
        .map(Some)
    }
}
impl ClaudeStructuredSessionProvider {
    fn invoke(
        &self,
        scope: &ProviderScope,
        session: Option<&str>,
        contribution: &str,
        actor: &str,
    ) -> Result<(String, String)> {
        fs::create_dir_all(&scope.slice_draft)?;
        let profile = self.render_scope_profile(scope)?;
        if let Some(profile) = &profile
            && self.test_sandbox_launcher.is_none()
        {
            crate::os::preflight_profile(profile)?;
        }
        let mut command = if let Some(profile) = &profile {
            if let Some(launcher) = &self.test_sandbox_launcher {
                let mut command = Command::new(launcher);
                command.arg("-f").arg(&profile.path).arg(&self.executable);
                command
            } else {
                crate::os::command_under_profile(profile, &self.executable)
            }
        } else {
            Command::new(&self.executable)
        };
        if self.scoped {
            command
                .env("HOME", scope.slice_draft.join(".provider-home"))
                .env("CLAUDE_CONFIG_DIR", scope.slice_draft.join(".claude-home"));
        }
        command.current_dir(&scope.checkout).args([
            "--print",
            "--output-format",
            "json",
            "--permission-mode",
            "plan",
        ]);
        if let Some(session) = session {
            command.args(["--resume", session]);
        }
        command.arg(format!(
            "Slack user {actor}: {contribution}\n\n{STRUCTURED_REPLY_INSTRUCTION}"
        ));
        let output = command
            .output()
            .context("invoke Claude structured session")?;
        if !output.status.success() {
            bail!(
                "Claude unavailable: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .context("incompatible Claude provider protocol")?;
        Ok((
            value["session_id"]
                .as_str()
                .context("incompatible Claude provider protocol: response omitted session_id")?
                .into(),
            validate_structured_provider_output(
                value["result"]
                    .as_str()
                    .context("incompatible Claude provider protocol: response omitted result")?,
                scope,
            )?,
        ))
    }
}
impl ConversationProvider for ClaudeStructuredSessionProvider {
    fn name(&self) -> &str {
        "claude"
    }
    fn start(
        &mut self,
        _: &str,
        scope: &ProviderScope,
        contribution: &str,
        actor: &str,
    ) -> ProviderResult<(String, String)> {
        self.invoke(scope, None, contribution, actor)
            .map_err(ProviderError::from)
    }
    fn resume(
        &mut self,
        _: &str,
        session: &str,
        scope: &ProviderScope,
        contribution: &str,
        actor: &str,
    ) -> ProviderResult<String> {
        Ok(self
            .invoke(scope, Some(session), contribution, actor)
            .map_err(ProviderError::from)?
            .1)
    }
    fn ready(&self) -> ProviderResult<()> {
        (|| -> Result<()> {
            Command::new(&self.executable)
                .arg("--version")
                .status()?
                .success()
                .then_some(())
                .context("Claude is unavailable")
        })()
        .map_err(ProviderError::from)
    }
}

impl ProviderRegistry {
    pub fn new(providers: impl IntoIterator<Item = Box<dyn ConversationProvider>>) -> Result<Self> {
        let mut registry = std::collections::BTreeMap::new();
        for provider in providers {
            let name = provider.name().to_owned();
            if registry.insert(name.clone(), provider).is_some() {
                bail!("duplicate provider {name}");
            }
        }
        let selected = if registry.contains_key("codex") {
            "codex".into()
        } else {
            registry
                .keys()
                .next()
                .context("provider registry is empty")?
                .clone()
        };
        Ok(Self {
            providers: registry,
            selected,
        })
    }
    fn current(&mut self) -> ProviderResult<&mut (dyn ConversationProvider + '_)> {
        match self.providers.get_mut(&self.selected) {
            Some(provider) => Ok(provider.as_mut()),
            None => Err(ProviderError::new(
                ProviderFailure::Unavailable,
                "selected provider is unavailable",
            )),
        }
    }
}

impl ConversationProvider for ProviderRegistry {
    fn name(&self) -> &str {
        &self.selected
    }
    fn select(&mut self, provider: &str) -> ProviderResult<()> {
        if !self.providers.contains_key(provider) {
            return Err(ProviderError::new(
                ProviderFailure::Unavailable,
                format!("conversation provider {provider} is not available"),
            ));
        }
        self.selected = provider.into();
        Ok(())
    }
    fn start(
        &mut self,
        a: &str,
        s: &ProviderScope,
        c: &str,
        u: &str,
    ) -> ProviderResult<(String, String)> {
        self.current()?.start(a, s, c, u)
    }
    fn resume(
        &mut self,
        a: &str,
        id: &str,
        s: &ProviderScope,
        c: &str,
        u: &str,
    ) -> ProviderResult<String> {
        self.current()?.resume(a, id, s, c, u)
    }
    fn prepare_session(&mut self, s: &ProviderScope) -> ProviderResult<Option<String>> {
        self.current()?.prepare_session(s)
    }
    fn recover_turn(
        &mut self,
        a: &str,
        id: &str,
        s: &ProviderScope,
    ) -> ProviderResult<ProviderTurnRecovery> {
        self.current()?.recover_turn(a, id, s)
    }
    fn run_turn(
        &mut self,
        a: &str,
        id: Option<&str>,
        s: &ProviderScope,
        c: &str,
        u: &str,
        accepted: &mut dyn FnMut(&str) -> Result<()>,
    ) -> ProviderResult<(Option<String>, String)> {
        self.current()?.run_turn(a, id, s, c, u, accepted)
    }
    fn ready(&self) -> ProviderResult<()> {
        self.providers
            .get(&self.selected)
            .ok_or_else(|| {
                ProviderError::new(
                    ProviderFailure::Unavailable,
                    "selected provider is unavailable",
                )
            })?
            .ready()
    }
}
