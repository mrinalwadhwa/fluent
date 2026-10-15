//! Bind Pi extension events to one Task generation and durable session.

use super::{Completion, PiSettlementGate};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

pub fn select_local_model_config(source: &Value, model: &str) -> Result<Value> {
    let provider = &source["providers"]["local-openai"];
    let models = provider["models"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Pi local-openai model list is missing"))?;
    let chosen = models
        .iter()
        .find(|entry| entry["id"].as_str() == Some(model))
        .ok_or_else(|| {
            anyhow::anyhow!("assigned Pi model is absent from local-openai configuration")
        })?;
    let mut provider = provider.clone();
    provider["models"] = json!([chosen]);
    ensure!(
        provider["baseUrl"]
            .as_str()
            .is_some_and(|url| !url.is_empty()),
        "Pi local-openai endpoint is missing"
    );
    Ok(json!({"providers":{"local-openai":provider}}))
}

pub fn select_local_model_config_with_endpoint(
    source: &Value,
    model: &str,
    endpoint: &str,
    api_key: Option<&str>,
) -> Result<Value> {
    let mut selected = select_local_model_config(source, model)?;
    let local = endpoint.strip_prefix("http://").and_then(|rest| {
        let authority = rest.split(['/', '?', '#']).next()?;
        let (host, port) = authority.rsplit_once(':')?;
        (matches!(host, "localhost" | "127.0.0.1")
            && port.parse::<u16>().is_ok_and(|port| port > 0)
            && !rest.contains('@'))
        .then_some(())
    });
    ensure!(
        local.is_some(),
        "native Pi endpoint must be a local HTTP server"
    );
    ensure!(
        selected["providers"]["local-openai"]["api"] == "openai-completions",
        "native Pi selected provider is not openai-completions"
    );
    selected["providers"]["local-openai"]["baseUrl"] = json!(endpoint);
    if let Some(key) = api_key {
        selected["providers"]["local-openai"]["apiKey"] = json!(key);
    }
    Ok(selected)
}

pub struct PiEventGate {
    generation: String,
    expected_session_file: PathBuf,
    session_id: Option<String>,
    settlement: Option<PiSettlementGate>,
    outcome: Option<String>,
    last_sequence: u64,
}

pub struct PiObservation {
    pub input_sequence: Option<u64>,
    pub completion: Completion,
}

impl PiEventGate {
    pub fn new(generation: impl Into<String>, session_file: impl Into<PathBuf>) -> Self {
        Self {
            generation: generation.into(),
            expected_session_file: session_file.into(),
            session_id: None,
            settlement: None,
            outcome: None,
            last_sequence: 0,
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    pub fn observe(&mut self, envelope: &Value) -> Result<PiObservation> {
        let generation = envelope["generation"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Pi event has no generation"))?;
        ensure!(
            generation == self.generation,
            "Pi event belongs to another Task generation"
        );
        let session_id = envelope["session_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Pi event has no session identity"))?;
        let session_file = envelope["session_file"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Pi event has no session file"))?;
        ensure!(
            Path::new(session_file) == self.expected_session_file,
            "Pi switched Task session files"
        );
        let event = &envelope["event"];
        let kind = event["type"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Pi event has no type"))?;
        if kind == "session_start" {
            ensure!(
                self.session_id.is_none(),
                "Pi reloaded or switched its Task session"
            );
            ensure!(
                matches!(event["reason"].as_str(), Some("startup" | "resume")),
                "Pi Task session started through an unexpected operation"
            );
            self.session_id = Some(session_id.into());
            self.settlement = Some(PiSettlementGate::new(&self.generation, session_id));
            return Ok(PiObservation {
                input_sequence: None,
                completion: Completion::Ignored,
            });
        }
        ensure!(
            self.session_id.as_deref() == Some(session_id),
            "Pi switched Task conversations"
        );
        let settlement = self
            .settlement
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Pi input preceded session_start"))?;
        let mut input_sequence = None;
        let completion = match kind {
            "input" => {
                let sequence = settlement.admit_input(generation, session_id)?;
                self.last_sequence = sequence;
                input_sequence = Some(sequence);
                Completion::Ignored
            }
            "input_entry" => {
                let sequence = envelope["input_sequence"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("Pi entry has no admitted sequence"))?;
                let entry_id = event["entry_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Pi entry has no identity"))?;
                settlement.observe_input_entry(generation, session_id, sequence, entry_id)?;
                Completion::Ignored
            }
            "agent_before_settle" => {
                let outcome = event["outcome"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Pi omitted its activity outcome"))?;
                self.outcome = Some(outcome.into());
                Completion::Ignored
            }
            "agent_settled" => {
                ensure!(
                    self.outcome.as_deref() == Some("completed"),
                    "Pi did not complete its agent activity"
                );
                let completion = settlement.observe_event(generation, session_id, event)?;
                ensure!(
                    completion != Completion::Ignored,
                    "Pi settled before admitted input reached the session"
                );
                completion
            }
            "session_shutdown" => bail!("Pi session ended before Task settlement"),
            "owner_heartbeat" => Completion::Ignored,
            "message_end"
            | "tool_execution_start"
            | "tool_execution_end"
            | "turn_start"
            | "turn_end"
            | "agent_start"
            | "agent_end"
            | "session_compact"
            | "session_compact_failed" => Completion::Ignored,
            _ => bail!("unexpected native Pi event {kind}"),
        };
        Ok(PiObservation {
            input_sequence,
            completion,
        })
    }
}

/// Persist the exact event before approving the extension callback. A failed
/// receiver never acknowledges input or completion to Pi.
pub fn receive_event(
    mut stream: UnixStream,
    gate: &mut PiEventGate,
    evidence: &mut File,
) -> Result<Completion> {
    const MAX_EVENT_BYTES: u64 = 16 * 1024 * 1024;
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(8)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(8)))?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?)
        .take(MAX_EVENT_BYTES)
        .read_line(&mut line)?;
    ensure!(
        !line.is_empty() && line.len() < MAX_EVENT_BYTES as usize,
        "Pi event is empty or oversized"
    );
    let envelope: Value = serde_json::from_str(&line)?;
    let observation = match gate.observe(&envelope) {
        Ok(observation) => observation,
        Err(error) => {
            if envelope["event"]["type"] == "input" {
                serde_json::to_writer(
                    &mut *evidence,
                    &serde_json::json!({
                        "format":"fluent-native-agent-event", "schema_version":1,
                        "provider":"pi", "execution_generation":envelope["generation"],
                        "assigned_conversation_id":gate.session_id(),
                        "conversation_id":envelope["session_id"], "input_sequence":0,
                        "event":{"type":"input_rejected", "request":envelope["event"],
                            "reason":error.to_string()},
                    }),
                )?;
                evidence.write_all(b"\n")?;
                evidence.sync_all()?;
            }
            return Err(error);
        }
    };
    serde_json::to_writer(
        &mut *evidence,
        &serde_json::json!({
            "format":"fluent-native-agent-event", "schema_version":1,
            "provider":"pi", "execution_generation":envelope["generation"],
            "assigned_conversation_id":gate.session_id(), "conversation_id":envelope["session_id"],
            "model":envelope["model"],
            "turn_id":envelope["event"]["entry_id"], "input_sequence":gate.last_sequence(),
            "event":envelope["event"],
        }),
    )?;
    evidence.write_all(b"\n")?;
    evidence.sync_all()?;
    stream.write_all(
        format!(
            "{}\n",
            serde_json::json!({"ok":true,"input_sequence":observation.input_sequence})
        )
        .as_bytes(),
    )?;
    Ok(observation.completion)
}
