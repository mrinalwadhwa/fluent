//! Validate Codex conversation identities before relaying native input and events.

use anyhow::{Result, bail, ensure};
use serde_json::Value;
use std::sync::{Arc, Mutex};

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
