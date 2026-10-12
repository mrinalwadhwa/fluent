//! Validate Codex conversation identities before relaying native input and events.

use anyhow::{Result, bail, ensure};
use serde::Serialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::sync::mpsc::{TrySendError, sync_channel};
#[cfg(unix)]
use std::time::Duration;

#[derive(Default)]
struct AdmissionState {
    last_sequence: u64,
    settling: bool,
}

/// Serialize native input with the decision to settle an invocation.
/// The execution owner must call `begin_settlement` before it publishes a Task
/// result, and account for every sequence at or below the returned cutoff.
#[derive(Default)]
pub struct NativeAdmission {
    state: Mutex<AdmissionState>,
}

impl NativeAdmission {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn admit(&self) -> Result<u64> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native admission lock poisoned"))?;
        ensure!(
            !state.settling,
            "native invocation is settling; input is closed"
        );
        state.last_sequence = state
            .last_sequence
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("native input sequence overflow"))?;
        Ok(state.last_sequence)
    }

    /// Close admission and return the last accepted input sequence.
    pub fn begin_settlement(&self) -> Result<u64> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("native admission lock poisoned"))?;
        ensure!(
            !state.settling,
            "native invocation already started settling"
        );
        state.settling = true;
        Ok(state.last_sequence)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    Ignored,
    Accepted {
        generation: String,
        turn_id: String,
        input_sequence: u64,
    },
}

/// Retain the provider notification with the Task execution identity. Readers
/// use the schema marker to distinguish app-server events from headless CLI
/// JSONL without changing the original provider event.
#[derive(Serialize)]
struct CodexEvidenceRecord<'a> {
    format: &'static str,
    schema_version: u32,
    provider: &'static str,
    execution_generation: &'a str,
    assigned_conversation_id: &'a str,
    conversation_id: &'a str,
    turn_id: Option<&'a str>,
    input_sequence: u64,
    event: &'a Value,
}

/// Keep one loaded Codex thread and its latest turn bound to a Task generation.
/// The transport must call `admit_request` before forwarding a TUI request and
/// `observe_event_for_generation` before considering a provider notification
/// as completion. The generation comes from the owned transport, not provider
/// JSON, because the provider does not assign Fluent's execution generation.
pub struct CodexRelayGate {
    generation: String,
    thread_id: String,
    admission: Arc<NativeAdmission>,
    input_sequence: u64,
    waiting_for_turn: bool,
    active_turn: Option<String>,
}

impl CodexRelayGate {
    pub fn new(generation: impl Into<String>, thread_id: impl Into<String>) -> Self {
        Self::with_admission(generation, thread_id, Arc::new(NativeAdmission::new()))
    }

    pub fn with_admission(
        generation: impl Into<String>,
        thread_id: impl Into<String>,
        admission: Arc<NativeAdmission>,
    ) -> Self {
        Self {
            generation: generation.into(),
            thread_id: thread_id.into(),
            admission,
            input_sequence: 0,
            waiting_for_turn: false,
            active_turn: None,
        }
    }

    /// Return the sequence assigned to an input request, when one was admitted.
    pub fn admit_request(&mut self, request: &Value) -> Result<Option<u64>> {
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Codex request has no method"))?;
        if method == "thread/start" || method == "thread/fork" {
            bail!("native Codex cannot create another Task conversation");
        }
        let params = request.get("params");
        if let Some(thread_id) = params
            .and_then(|params| params.get("threadId"))
            .and_then(Value::as_str)
        {
            ensure!(
                thread_id == self.thread_id,
                "native Codex request targets another Task conversation"
            );
        } else if method == "thread/resume" || method.starts_with("turn/") {
            bail!("native Codex request omits its Task conversation identity");
        }
        if method == "turn/start" || method == "turn/steer" {
            self.input_sequence = self.admission.admit()?;
            if method == "turn/start" {
                self.waiting_for_turn = true;
                self.active_turn = None;
            }
            return Ok(Some(self.input_sequence));
        }
        Ok(None)
    }

    pub fn observe_event_for_generation(
        &mut self,
        generation: &str,
        event: &Value,
    ) -> Result<Completion> {
        if generation != self.generation {
            return Ok(Completion::Ignored);
        }
        let method = event.get("method").and_then(Value::as_str);
        if !matches!(method, Some("turn/started" | "turn/completed")) {
            return Ok(Completion::Ignored);
        }
        let params = event.get("params");
        if params
            .and_then(|params| params.get("threadId"))
            .and_then(Value::as_str)
            != Some(self.thread_id.as_str())
        {
            return Ok(Completion::Ignored);
        }
        let turn = params.and_then(|params| params.get("turn"));
        let Some(turn_id) = turn.and_then(|turn| turn.get("id")).and_then(Value::as_str) else {
            bail!("native Codex turn event omits its turn identity");
        };
        if method == Some("turn/started") {
            if self.waiting_for_turn {
                self.active_turn = Some(turn_id.into());
                self.waiting_for_turn = false;
            }
            return Ok(Completion::Ignored);
        }
        if self.active_turn.as_deref() != Some(turn_id) {
            return Ok(Completion::Ignored);
        }
        self.active_turn = None;
        if turn
            .and_then(|turn| turn.get("status"))
            .and_then(Value::as_str)
            != Some("completed")
        {
            return Ok(Completion::Ignored);
        }
        Ok(Completion::Accepted {
            generation: self.generation.clone(),
            turn_id: turn_id.into(),
            input_sequence: self.input_sequence,
        })
    }
}

/// Relay one Codex TUI connection to its private app-server connection. Both
/// directions share the invocation gate; original provider messages reach the
/// evidence sink before the TUI receives them. The caller owns process lifetime
/// and Task settlement.
#[cfg(unix)]
pub fn relay_codex_streams<W, F>(
    client: UnixStream,
    server: UnixStream,
    generation: &str,
    thread_id: &str,
    mut evidence: W,
    mut on_completion: F,
) -> Result<()>
where
    W: Write + Send,
    F: FnMut(Completion) -> Result<()> + Send,
{
    let gate = Arc::new(Mutex::new(CodexRelayGate::new(generation, thread_id)));
    let client_reader = client.try_clone()?;
    let server_reader = server.try_clone()?;
    let upstream_abort_client = client.try_clone()?;
    let upstream_abort_server = server.try_clone()?;
    let downstream_abort_client = client.try_clone()?;
    let downstream_abort_server = server.try_clone()?;
    let display_client = client.try_clone()?;
    display_client.set_write_timeout(Some(Duration::from_millis(250)))?;
    let (display_tx, display_rx) = sync_channel::<String>(32);
    std::thread::scope(|scope| {
        let display = scope.spawn(move || {
            let mut writer = display_client;
            while let Ok(line) = display_rx.recv() {
                if writer.write_all(line.as_bytes()).is_err() {
                    break;
                }
            }
        });
        let request_gate = Arc::clone(&gate);
        let upstream = scope.spawn(move || -> Result<()> {
            let result = (|| {
                let mut reader = BufReader::new(client_reader);
                let mut writer = server;
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line)? == 0 {
                        let _ = writer.shutdown(Shutdown::Write);
                        return Ok(());
                    }
                    let request: Value = serde_json::from_str(&line)?;
                    request_gate
                        .lock()
                        .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?
                        .admit_request(&request)?;
                    writer.write_all(line.as_bytes())?;
                    writer.flush()?;
                }
            })();
            if result.is_err() {
                let _ = upstream_abort_client.shutdown(Shutdown::Both);
                let _ = upstream_abort_server.shutdown(Shutdown::Both);
            }
            result
        });
        let downstream = scope.spawn(move || -> Result<()> {
            let result = (|| {
                let mut reader = BufReader::new(server_reader);
                let mut display = Some(display_tx);
                let mut line = String::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line)? == 0 {
                        drop(display);
                        let _ = downstream_abort_client.shutdown(Shutdown::Read);
                        return Ok(());
                    }
                    let event: Value = serde_json::from_str(&line)?;
                    let (completion, input_sequence) = {
                        let mut gate = gate
                            .lock()
                            .map_err(|_| anyhow::anyhow!("native Codex relay gate poisoned"))?;
                        let completion = gate.observe_event_for_generation(generation, &event)?;
                        (completion, gate.input_sequence)
                    };
                    let params = &event["params"];
                    let conversation_id = params["threadId"]
                        .as_str()
                        .or_else(|| params["thread"]["id"].as_str())
                        .unwrap_or(thread_id);
                    let record = CodexEvidenceRecord {
                        format: "fluent-native-agent-event",
                        schema_version: 1,
                        provider: "codex",
                        execution_generation: generation,
                        assigned_conversation_id: thread_id,
                        conversation_id,
                        turn_id: params["turn"]["id"]
                            .as_str()
                            .or_else(|| params["turnId"].as_str()),
                        input_sequence,
                        event: &event,
                    };
                    serde_json::to_writer(&mut evidence, &record)?;
                    evidence.write_all(b"\n")?;
                    evidence.flush()?;
                    if completion != Completion::Ignored {
                        on_completion(completion)?;
                    }
                    if let Some(sender) = display.as_ref() {
                        if matches!(
                            sender.try_send(line.clone()),
                            Err(TrySendError::Full(_) | TrySendError::Disconnected(_))
                        ) {
                            // Detach a stalled display; canonical evidence and
                            // completion still flow from the provider stream.
                            display = None;
                            let _ = downstream_abort_client.shutdown(Shutdown::Both);
                        }
                    }
                }
            })();
            if result.is_err() {
                let _ = downstream_abort_client.shutdown(Shutdown::Both);
                let _ = downstream_abort_server.shutdown(Shutdown::Both);
            }
            result
        });
        let upstream = upstream
            .join()
            .map_err(|_| anyhow::anyhow!("native Codex request relay panicked"))?;
        let downstream = downstream
            .join()
            .map_err(|_| anyhow::anyhow!("native Codex event relay panicked"))?;
        display
            .join()
            .map_err(|_| anyhow::anyhow!("native Codex display relay panicked"))?;
        upstream.and(downstream)
    })
}
