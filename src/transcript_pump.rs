//! Drain coder stdout into the canonical transcript exactly.
//!
//! The pump reads raw fixed-size byte chunks and writes them, unmodified, to
//! the transcript file. It never decodes the stream as UTF-8 and never splits a
//! record before persisting it, so invalid bytes and records larger than the
//! enclosing pipe capacity are captured losslessly. Record boundaries are
//! detected incrementally only to drive a bounded, best-effort console preview
//! and to count records; they never gate the canonical byte path.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Schema version of the adjacent `transcript-pump.json` status document. Bump
/// this when the persisted shape changes so readers can detect the format.
///
/// v2 adds `periodic_error`: a best-effort periodic status write may fail without
/// failing capture, and that last failure is retained on the terminal status.
///
/// v3 adds `transport`: the status coordinator's exact accounting of every status
/// submission (written, coalesced, dropped, disconnected, write-failed) plus the
/// last error, so a terminal status proves it lost nothing silently.
///
/// v4 adds provider retry timing observed directly from streamed provider records.
///
/// v5 adds the current Codex execution phase used by response-liveness supervision.
pub const PUMP_STATUS_SCHEMA_VERSION: u32 = 5;

/// Built-in size, in bytes, of each read chunk pulled from coder stdout.
pub const DEFAULT_READ_CHUNK_SIZE: usize = 64 * 1024;
/// Built-in upper bound, in bytes, on the TOTAL rendered console preview
/// (payload plus any truncation marker). Beyond it the pump renders a bounded,
/// lossy preview; the full record always lands in the transcript.
pub const DEFAULT_CONSOLE_PREVIEW_LIMIT: usize = 8 * 1024;

/// Appended to a preview whose record exceeded the console preview limit. It
/// points a reader at the canonical transcript, which alone holds every byte.
/// The marker is counted against the preview limit, so a truncated preview's
/// payload is capped to leave room for it.
pub const TRUNCATION_MARKER: &[u8] = b"...[preview truncated; full record in transcript]";

/// Immutable settings for one transcript-enabled launch: pump thresholds plus
/// the Claude recovery policy whose evidence the pump observes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptPumpConfig {
    /// Bytes read per stdout chunk.
    pub read_chunk_size: usize,
    /// Maximum bytes of the TOTAL rendered console preview for one record,
    /// including any truncation marker.
    pub console_preview_limit: usize,
    /// Minimum interval between periodic `Running` status flushes.
    pub status_flush_interval: Duration,
    /// Maximum time a Claude launch may remain in the recognized pre-progress state.
    pub claude_no_progress_timeout: Duration,
    /// Same-Task relaunches allowed after a safe Claude no-progress timeout.
    pub claude_no_progress_retries: u32,
    /// Initial delay between safe no-progress launches; each retry doubles it.
    pub claude_no_progress_backoff: Duration,
    /// Maximum time Codex may remain between streamed records while waiting for
    /// the model. Active tool execution is never subject to this deadline.
    pub codex_response_timeout: Duration,
    /// Same-Task, same-session continuations allowed after a Codex response timeout.
    pub codex_response_continuations: u32,
}

impl Default for TranscriptPumpConfig {
    fn default() -> Self {
        Self {
            read_chunk_size: DEFAULT_READ_CHUNK_SIZE,
            console_preview_limit: DEFAULT_CONSOLE_PREVIEW_LIMIT,
            status_flush_interval: Duration::from_millis(1000),
            claude_no_progress_timeout: Duration::from_secs(60),
            claude_no_progress_retries: 2,
            claude_no_progress_backoff: Duration::from_secs(5),
            codex_response_timeout: Duration::from_secs(10 * 60),
            codex_response_continuations: 2,
        }
    }
}

// Transcript-pump thresholds are resolved once per launch and threaded into the
// coder as an immutable value (see `coder::TranscriptCapture`). There is no
// process-global config: a concurrent launch cannot overwrite another capture's
// resolved thresholds between resolution and pump spawn.

/// Resolve this project's layered transcript launch settings into an immutable
/// value the caller threads into its launch. A malformed or unreadable
/// configuration keeps safe pump defaults but disables automatic Claude
/// relaunch, because capture diagnostics may fail open while recovery may not.
///
/// This lives beside the pump (rather than in an executor) so every
/// transcript-enabled entry point — Writer, Reviewer, Learner, rebase agent —
/// resolves it without depending on any one executor.
pub(crate) fn resolve_config(project_root: &Path) -> TranscriptPumpConfig {
    map_resolved_config(crate::config::resolve_transcript_pump_config(project_root))
}

/// Resolve from explicit project and user config paths, bypassing HOME. Tests use
/// this to exercise layering and fail-closed behavior hermetically.
#[cfg(test)]
pub(crate) fn resolve_config_from(
    project_path: &Path,
    user_path: Option<&Path>,
) -> TranscriptPumpConfig {
    map_resolved_config(crate::config::resolve_transcript_pump_config_from(
        project_path,
        user_path,
    ))
}

fn map_resolved_config(
    resolved: Result<
        crate::config::ResolvedTranscriptPumpConfig,
        crate::config::FollowUpConfigError,
    >,
) -> TranscriptPumpConfig {
    match resolved {
        Ok(resolved) => TranscriptPumpConfig {
            console_preview_limit: resolved.console_preview_limit.value as usize,
            status_flush_interval: Duration::from_millis(
                resolved.status_flush_interval_ms.value as u64,
            ),
            claude_no_progress_timeout: Duration::from_secs(
                resolved.claude_no_progress_timeout_seconds.value as u64,
            ),
            claude_no_progress_retries: resolved.claude_no_progress_retries.value,
            claude_no_progress_backoff: Duration::from_secs(
                resolved.claude_no_progress_backoff_seconds.value as u64,
            ),
            codex_response_timeout: Duration::from_secs(
                resolved.codex_response_timeout_seconds.value as u64,
            ),
            codex_response_continuations: resolved.codex_response_continuations.value,
            ..TranscriptPumpConfig::default()
        },
        Err(error) => {
            eprintln!(
                "warning: {error}; automatic Claude no-progress recovery is disabled for this launch"
            );
            TranscriptPumpConfig {
                // Capture diagnostics may safely use defaults, but automatic provider
                // relaunch is correctness-sensitive. A malformed layer disables it.
                claude_no_progress_retries: 0,
                codex_response_continuations: 0,
                ..TranscriptPumpConfig::default()
            }
        }
    }
}

/// The adjacent status document path for a transcript: `transcript-pump.json`
/// beside the transcript file.
pub fn status_path_for(transcript_path: &Path) -> PathBuf {
    transcript_path.with_file_name("transcript-pump.json")
}

/// Cap a retained or persisted error message so a pathological error string can
/// never bloat the shared diagnostics or the status document. This is the TOTAL
/// cap: a truncated message (payload plus marker) never exceeds it.
const MAX_STATUS_ERROR_LEN: usize = 2000;

/// The marker appended to a truncated error. It is counted against the total cap,
/// so a truncated payload is shortened to leave room for it.
const STATUS_TRUNCATION_MARKER: &str = "…[truncated]";

fn bound_error(message: &str) -> String {
    if message.len() <= MAX_STATUS_ERROR_LEN {
        return message.to_string();
    }
    // Reserve room for the marker so the TOTAL bounded string is within the cap,
    // then walk back to a char boundary so a multibyte code point is never split.
    let mut end = MAX_STATUS_ERROR_LEN.saturating_sub(STATUS_TRUNCATION_MARKER.len());
    while end > 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{STATUS_TRUNCATION_MARKER}", &message[..end])
}

/// A best-effort console sink for bounded record previews. Delivery is
/// synchronous and must never block the pump: an implementation renders the
/// preview without waiting and returns `false` when it could not — because there
/// is no live console, a nonblocking write would stall, or the write failed — so
/// the pump counts the loss immediately and keeps draining. Because delivery is
/// synchronous, the returned outcome is the true fate of the preview, so the
/// pump's drop accounting is exact at every status write.
pub trait PreviewSink: Send + Sync {
    /// Offer one bounded preview. Returns `false` when it could not be delivered.
    fn deliver(&self, preview: &[u8]) -> bool;
}

/// A typed transcript-pump infrastructure failure. Coder supervision converts
/// this into a terminal error that bypasses the generic coder retry budget, so a
/// capture failure never masquerades as a retryable coder error.
///
/// The `message` is the immutable primary fault — the first thing that went
/// wrong. Bounded secondary diagnostics ride alongside it rather than overwriting
/// it, so a Complete-to-Failed fallback failure, a periodic write failure, or a
/// status-worker panic that happens WHILE the primary fault is being reported can
/// all be preserved without ever masking the primary cause.
#[derive(Debug, Clone, Default)]
pub struct TranscriptPumpError {
    message: String,
    /// The last best-effort periodic status write failure, retained as evidence.
    periodic_error: Option<String>,
    /// A terminal-settlement failure (the Complete/Failed status could not be
    /// persisted) observed while reporting the primary fault.
    settlement_error: Option<String>,
    /// A Complete-to-Failed fallback write failure.
    fallback_error: Option<String>,
    /// A status-coordinator worker panic or join failure.
    worker_error: Option<String>,
    /// The coordinator's balanced transport accounting, when a status coordinator
    /// was involved.
    transport: Option<StatusTransportDiagnostics>,
}

impl TranscriptPumpError {
    pub fn new(message: impl Into<String>) -> Self {
        // Bound the primary at the one constructor every typed pump failure flows
        // through — a required-status ack error, a spawn error, or a settlement
        // message — so no pathological store error can bloat the returned failure.
        Self {
            message: bound_error(&message.into()),
            ..Self::default()
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn periodic_error(&self) -> Option<&str> {
        self.periodic_error.as_deref()
    }

    pub fn settlement_error(&self) -> Option<&str> {
        self.settlement_error.as_deref()
    }

    pub fn fallback_error(&self) -> Option<&str> {
        self.fallback_error.as_deref()
    }

    pub fn worker_error(&self) -> Option<&str> {
        self.worker_error.as_deref()
    }

    pub fn transport(&self) -> Option<&StatusTransportDiagnostics> {
        self.transport.as_ref()
    }

    /// Fold a completed status settlement's secondary diagnostics onto this
    /// primary fault without overwriting the primary `message`.
    fn with_settlement(mut self, settlement: &StatusSettlement) -> Self {
        // Bound every secondary as it is folded on, so the composite typed error is
        // bounded at the primary and at each of its secondary diagnostics.
        self.periodic_error = settlement.periodic_error.as_deref().map(bound_error);
        self.settlement_error = settlement.settlement_error.as_deref().map(bound_error);
        self.fallback_error = settlement.fallback_error.as_deref().map(bound_error);
        self.worker_error = settlement.worker_error.as_deref().map(bound_error);
        self.transport = Some(settlement.diagnostics.clone());
        self
    }
}

impl std::fmt::Display for TranscriptPumpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transcript pump failure: {}", self.message)?;
        if let Some(err) = &self.settlement_error {
            write!(f, "; terminal-status settlement failed: {err}")?;
        }
        if let Some(err) = &self.fallback_error {
            write!(f, "; failed-status fallback failed: {err}")?;
        }
        if let Some(err) = &self.periodic_error {
            write!(f, "; last periodic status error: {err}")?;
        }
        if let Some(err) = &self.worker_error {
            write!(f, "; status worker error: {err}")?;
        }
        Ok(())
    }
}

impl std::error::Error for TranscriptPumpError {}

/// What a completed drain observed: total bytes persisted, records seen, previews a
/// saturated or disconnected console could not accept, and — on a settled capture —
/// the coordinator's final balanced status transport accounting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PumpSummary {
    pub bytes: u64,
    pub records: u64,
    pub dropped_console: u64,
    pub provider_timing: ProviderTimingDiagnostics,
    pub provider_liveness: ProviderLivenessDiagnostics,
    /// The final balanced status transport accounting, populated when the capture
    /// settles successfully (it is carried on the typed error otherwise).
    pub transport: StatusTransportDiagnostics,
}

/// Transcript-derived state used to decide whether a Codex launch is safely
/// waiting for a model response. Unknown or partial records fail closed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderLivenessDiagnostics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_idle_since_ms: Option<u64>,
}

/// Best-effort timing derived from provider-owned retry records in the canonical
/// stream. `wait_ms` is elapsed wait observed so far; `resume_at_ms` is present
/// only while the latest declared retry delay remains active.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderTimingDiagnostics {
    #[serde(default)]
    pub retry_count: u64,
    #[serde(default)]
    pub wait_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_since_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_at_ms: Option<u64>,
}

/// The lifecycle state of a transcript pump, persisted in its status document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PumpState {
    /// Capture has begun; the transcript is open.
    Running,
    /// The coder closed stdout and every byte was persisted.
    Complete,
    /// Capture ended on an infrastructure failure; `error` names the cause.
    Failed,
}

/// Durable diagnostic state written beside the transcript. It records what the
/// pump observed so an operator can distinguish a quiet coder, a blocked
/// console, a failed pump, and completed capture. It is diagnostics only and
/// never an execution lease or liveness authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PumpStatus {
    pub schema_version: u32,
    pub state: PumpState,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
    pub bytes: u64,
    pub records: u64,
    pub dropped_console: u64,
    #[serde(default)]
    pub provider_timing: ProviderTimingDiagnostics,
    #[serde(default)]
    pub provider_liveness: ProviderLivenessDiagnostics,
    /// The terminal failure cause, present only on a `Failed` state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The last best-effort periodic status-write failure, if any. It is retained
    /// on the terminal status — including a successful `Complete` — so a slow or
    /// flaky status filesystem is observable without failing canonical capture.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub periodic_error: Option<String>,
    /// The status coordinator's transport accounting at the time this status was
    /// written. On a terminal status it balances every submission and proves no
    /// snapshot remained pending; on an intermediate status it is the running tally.
    #[serde(default)]
    pub transport: StatusTransportDiagnostics,
}

/// Exact, balanced accounting of every status submission a [`StatusCoordinator`]
/// handled. Each submission lands in exactly one terminal category, so a terminal
/// status can prove it discarded nothing without an operator noticing.
///
/// The balance invariant
/// `submitted == written + coalesced + dropped + disconnected + write_failures`
/// holds at every terminal settlement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusTransportDiagnostics {
    /// Total status submissions the coordinator was asked to persist.
    pub submitted: u64,
    /// Submissions the store persisted successfully.
    pub written: u64,
    /// Periodic snapshots replaced in the pending slot by a newer snapshot before
    /// the worker could write them.
    pub coalesced: u64,
    /// Periodic snapshots dropped because terminal sealing had already begun.
    pub dropped: u64,
    /// Submissions refused because the coordinator's worker had already shut down.
    pub disconnected: u64,
    /// Submissions the store attempted but failed to persist.
    pub write_failures: u64,
    /// The most recent write failure message, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl StatusTransportDiagnostics {
    /// Whether the balance invariant holds: every submission is accounted for in
    /// exactly one terminal category.
    pub fn is_balanced(&self) -> bool {
        self.submitted
            == self.written
                + self.coalesced
                + self.dropped
                + self.disconnected
                + self.write_failures
    }
}

/// Shared, panic-safe pump counters. They live behind atomics so the pump's
/// panic path can read the values accumulated before the panic instead of
/// reporting zeros.
struct SharedCounters {
    bytes: AtomicU64,
    records: AtomicU64,
    dropped_console: AtomicU64,
    provider_timing: Mutex<ProviderTimingState>,
    claude_progress: AtomicU8,
    codex_liveness: Mutex<CodexLivenessState>,
}

const CLAUDE_PROGRESS_SAFE: u8 = 0;
const CLAUDE_PROGRESS_PARTIAL: u8 = 1;
const CLAUDE_PROGRESS_OBSERVED: u8 = 2;

impl Default for SharedCounters {
    fn default() -> Self {
        Self {
            bytes: AtomicU64::new(0),
            records: AtomicU64::new(0),
            dropped_console: AtomicU64::new(0),
            provider_timing: Mutex::new(ProviderTimingState::default()),
            claude_progress: AtomicU8::new(CLAUDE_PROGRESS_SAFE),
            codex_liveness: Mutex::new(CodexLivenessState::default()),
        }
    }
}

#[derive(Default)]
struct ProviderTimingState {
    retry_count: u64,
    settled_wait_ms: u64,
    waiting_since_ms: Option<u64>,
    resume_at_ms: Option<u64>,
}

#[derive(Default)]
struct CodexLivenessState {
    session_id: Option<String>,
    phase: CodexPhase,
    last_event: Option<String>,
    last_progress_at_ms: Option<u64>,
    response_idle_since_ms: Option<u64>,
    partial_record: bool,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CodexPhase {
    #[default]
    Unknown,
    AwaitingResponse,
    ActiveTool,
    Complete,
}

impl SharedCounters {
    fn add_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
    }
    fn add_record(&self) {
        self.records.fetch_add(1, Ordering::Relaxed);
    }
    fn add_dropped(&self) {
        self.dropped_console.fetch_add(1, Ordering::Relaxed);
    }
    fn sub_dropped(&self) {
        self.dropped_console.fetch_sub(1, Ordering::Relaxed);
    }
    fn snapshot(&self) -> PumpSummary {
        let provider_timing = self.provider_timing.lock().unwrap().snapshot(now_ms());
        let provider_liveness = self.codex_liveness.lock().unwrap().diagnostics();
        PumpSummary {
            transport: StatusTransportDiagnostics::default(),
            bytes: self.bytes.load(Ordering::Relaxed),
            records: self.records.load(Ordering::Relaxed),
            dropped_console: self.dropped_console.load(Ordering::Relaxed),
            provider_timing,
            provider_liveness,
        }
    }

    fn observe_record(&self, record: &[u8]) -> bool {
        if !is_safe_claude_pre_progress_record(record) {
            self.claude_progress
                .store(CLAUDE_PROGRESS_OBSERVED, Ordering::Release);
        }
        let now = now_ms();
        self.codex_liveness.lock().unwrap().observe(record, now);
        let mut timing = self.provider_timing.lock().unwrap();
        timing.finish_wait(now);
        let Some(delay_ms) = claude_api_retry_delay_ms(record) else {
            return false;
        };
        timing.retry_count = timing.retry_count.saturating_add(1);
        timing.waiting_since_ms = Some(now);
        timing.resume_at_ms = Some(now.saturating_add(delay_ms));
        true
    }

    fn observe_partial_record(&self) {
        let _ = self.claude_progress.compare_exchange(
            CLAUDE_PROGRESS_SAFE,
            CLAUDE_PROGRESS_PARTIAL,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.codex_liveness.lock().unwrap().partial_record = true;
    }

    fn finish_complete_record_boundary(&self) {
        let _ = self.claude_progress.compare_exchange(
            CLAUDE_PROGRESS_PARTIAL,
            CLAUDE_PROGRESS_SAFE,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
        self.codex_liveness.lock().unwrap().partial_record = false;
    }

    fn claude_no_progress_retry_is_safe(&self) -> bool {
        self.claude_progress.load(Ordering::Acquire) == CLAUDE_PROGRESS_SAFE
    }

    fn claude_progress_observed(&self) -> bool {
        self.claude_progress.load(Ordering::Acquire) == CLAUDE_PROGRESS_OBSERVED
    }

    fn finish_provider_wait(&self) {
        self.provider_timing.lock().unwrap().finish_wait(now_ms());
    }

    fn codex_response_idle(&self, timeout: Duration) -> Option<ProviderLivenessDiagnostics> {
        let state = self.codex_liveness.lock().unwrap();
        if state.partial_record || state.phase != CodexPhase::AwaitingResponse {
            return None;
        }
        let idle_since = state.response_idle_since_ms?;
        (now_ms().saturating_sub(idle_since) >= timeout.as_millis() as u64)
            .then(|| state.diagnostics())
    }
}

impl CodexLivenessState {
    fn observe(&mut self, record: &[u8], now: u64) {
        let Ok(event) = serde_json::from_slice::<serde_json::Value>(record) else {
            self.phase = CodexPhase::Unknown;
            self.last_event = Some("malformed".to_string());
            self.last_progress_at_ms = Some(now);
            self.response_idle_since_ms = None;
            return;
        };
        let Some(event_type) = event["type"].as_str() else {
            self.phase = CodexPhase::Unknown;
            self.last_event = Some("unknown".to_string());
            self.last_progress_at_ms = Some(now);
            self.response_idle_since_ms = None;
            return;
        };
        match event_type {
            "thread.started" => {
                self.session_id = event["thread_id"].as_str().map(str::to_string);
                self.last_event = Some(event_type.to_string());
                self.last_progress_at_ms = Some(now);
            }
            "turn.started" => self.await_response(event_type, now),
            "item.started" => {
                let item_type = event["item"]["type"].as_str().unwrap_or("unknown");
                self.last_event = Some(format!("item.started:{item_type}"));
                self.last_progress_at_ms = Some(now);
                if codex_item_is_model_response(item_type) {
                    self.phase = CodexPhase::AwaitingResponse;
                    self.response_idle_since_ms = Some(now);
                } else {
                    // Unknown item kinds fail closed because they may own a live tool.
                    self.phase = CodexPhase::ActiveTool;
                    self.response_idle_since_ms = None;
                }
            }
            "item.completed" => {
                let item_type = event["item"]["type"].as_str().unwrap_or("unknown");
                self.await_response(&format!("item.completed:{item_type}"), now);
            }
            "turn.completed" | "turn.failed" | "error" => {
                self.phase = CodexPhase::Complete;
                self.last_event = Some(event_type.to_string());
                self.last_progress_at_ms = Some(now);
                self.response_idle_since_ms = None;
            }
            _ => {
                // Non-Codex records and future Codex events cannot prove that an
                // external action is absent, so automatic interruption is disabled.
                self.phase = CodexPhase::Unknown;
                self.last_event = Some(event_type.to_string());
                self.last_progress_at_ms = Some(now);
                self.response_idle_since_ms = None;
            }
        }
    }

    fn await_response(&mut self, event: &str, now: u64) {
        self.phase = CodexPhase::AwaitingResponse;
        self.last_event = Some(event.to_string());
        self.last_progress_at_ms = Some(now);
        self.response_idle_since_ms = Some(now);
    }

    fn diagnostics(&self) -> ProviderLivenessDiagnostics {
        ProviderLivenessDiagnostics {
            provider: self.session_id.as_ref().map(|_| "codex".to_string()),
            session_id: self.session_id.clone(),
            phase: Some(
                match self.phase {
                    CodexPhase::Unknown => "unknown",
                    CodexPhase::AwaitingResponse => "awaiting-response",
                    CodexPhase::ActiveTool => "active-tool",
                    CodexPhase::Complete => "complete",
                }
                .to_string(),
            ),
            last_event: self.last_event.clone(),
            last_progress_at_ms: self.last_progress_at_ms,
            response_idle_since_ms: self.response_idle_since_ms,
        }
    }
}

fn codex_item_is_model_response(item_type: &str) -> bool {
    matches!(item_type, "agent_message" | "reasoning")
}

fn is_safe_claude_pre_progress_record(record: &[u8]) -> bool {
    let Ok(event) = serde_json::from_slice::<serde_json::Value>(record) else {
        return false;
    };
    event["type"].as_str() == Some("system")
        && matches!(event["subtype"].as_str(), Some("init") | Some("api_retry"))
}

impl ProviderTimingState {
    fn finish_wait(&mut self, now: u64) {
        if let (Some(started), Some(resume)) = (self.waiting_since_ms, self.resume_at_ms) {
            self.settled_wait_ms = self
                .settled_wait_ms
                .saturating_add(now.min(resume).saturating_sub(started));
        }
        self.waiting_since_ms = None;
        self.resume_at_ms = None;
    }

    fn snapshot(&self, now: u64) -> ProviderTimingDiagnostics {
        let live_wait = match (self.waiting_since_ms, self.resume_at_ms) {
            (Some(started), Some(resume)) => now.min(resume).saturating_sub(started),
            _ => 0,
        };
        ProviderTimingDiagnostics {
            retry_count: self.retry_count,
            wait_ms: self.settled_wait_ms.saturating_add(live_wait),
            waiting_since_ms: self.waiting_since_ms,
            resume_at_ms: self.resume_at_ms.filter(|resume| *resume > now),
        }
    }
}

fn claude_api_retry_delay_ms(record: &[u8]) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_slice(record).ok()?;
    (value["type"].as_str() == Some("system") && value["subtype"].as_str() == Some("api_retry"))
        .then(|| value["retry_delay_ms"].as_u64())
        .flatten()
}

/// Drain `reader` into the transcript at `transcript_path`, writing every byte
/// exactly and in order. Record boundaries drive a bounded preview through
/// `preview` and increment the record count; they never transform or withhold
/// canonical bytes.
///
/// When `status_path` is set, the initial `Running` and the terminal
/// `Complete`/`Failed` status are persisted atomically and **synchronously**;
/// a failure to persist either is a typed terminal infrastructure failure, so
/// the durable diagnostic is truthful. Periodic `Running` snapshots between them
/// are best-effort: they are coalesced through a background writer that never
/// backpressures canonical capture, and a slow or failing status filesystem
/// cannot stall stdout draining. Returns the observed counters, or a typed
/// failure if the transcript could not be opened, written, or read, or if a
/// required status could not be persisted.
///
/// Production capture runs through [`spawn_pump`], which shares the counters for
/// panic-safe reporting; this synchronous entry point drives the same logic for
/// focused tests.
#[cfg(test)]
pub fn drain(
    reader: impl Read,
    transcript_path: &Path,
    status_path: Option<&Path>,
    preview: &dyn PreviewSink,
    config: &TranscriptPumpConfig,
) -> Result<PumpSummary, TranscriptPumpError> {
    let counters = SharedCounters::default();
    let store = status_path.map(file_status_store);
    drain_with_first_fault(
        reader,
        transcript_path,
        store,
        preview,
        config,
        &counters,
        None,
    )
}

/// Drive a drain against an injected [`StatusStore`], so tests can gate, fail, or
/// disconnect status writes deterministically instead of relying on timing.
#[cfg(test)]
pub(crate) fn drain_with_store(
    reader: impl Read,
    transcript_path: &Path,
    store: Option<Box<dyn StatusStore>>,
    preview: &dyn PreviewSink,
    config: &TranscriptPumpConfig,
) -> Result<PumpSummary, TranscriptPumpError> {
    let counters = SharedCounters::default();
    drain_with_first_fault(
        reader,
        transcript_path,
        store,
        preview,
        config,
        &counters,
        None,
    )
}

/// Drain into the transcript, owning the status coordinator across a caught capture
/// panic. The immutable first fault — whether capture returns it or panics — is
/// published to `first_fault` BEFORE any terminal settlement, so a blocked or slow
/// status store can never hide the fault from coder supervision.
fn drain_with_first_fault(
    reader: impl Read,
    transcript_path: &Path,
    store: Option<Box<dyn StatusStore>>,
    preview: &dyn PreviewSink,
    config: &TranscriptPumpConfig,
    counters: &SharedCounters,
    first_fault: Option<Arc<FirstFault>>,
) -> Result<PumpSummary, TranscriptPumpError> {
    let started = now_ms();
    // One coordinator owns every persisted status write for this capture. It is
    // owned here — not dropped through capture's unwind — so a store that blocks
    // can never delay first-fault notification.
    let mut coordinator = match store {
        Some(store) => Some(StatusCoordinator::spawn(store, first_fault.clone())?),
        None => None,
    };

    // Catch a capture panic so it settles through the coordinator like any other
    // fault: publish the first fault, then write the terminal status.
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        capture(
            reader,
            transcript_path,
            coordinator.as_ref(),
            preview,
            config,
            counters,
            started,
        )
    }))
    .unwrap_or_else(|_| Err(TranscriptPumpError::new("transcript pump panicked")));

    // Publish the first fault BEFORE terminal settlement so a blocked status store
    // cannot hide it from supervision.
    if let (Err(err), Some(latch)) = (&result, &first_fault) {
        latch.publish(err);
    }

    let mut summary = counters.snapshot();

    match result {
        Ok(()) => match coordinator.as_mut() {
            Some(coordinator) => {
                let settlement =
                    coordinator.finish(TerminalStatusSpec::complete(started, summary.clone()));
                // A terminal-settlement failure OR a status-worker panic is a typed
                // terminal failure: capture is not independently observable.
                if let Some(err) = settlement.terminal_failure() {
                    return Err(err);
                }
                // Return the final balanced transport accounting on success too.
                summary.transport = settlement.diagnostics.clone();
                Ok(summary)
            }
            None => Ok(summary),
        },
        Err(err) => {
            // Record the failure terminally without masking the primary fault. The
            // terminal Failed status and its settlement diagnostics ride alongside
            // the primary cause rather than replacing it.
            if let Some(coordinator) = coordinator.as_mut() {
                let settlement =
                    coordinator.finish(TerminalStatusSpec::failed(started, summary, err.message()));
                return Err(err.with_settlement(&settlement));
            }
            Err(err)
        }
    }
}

/// Retain a transport failure that prevented launch, including any failure to
/// persist its diagnostic. No provider ran, so capture counters remain empty.
pub(crate) fn record_prelaunch_failure(
    transcript_path: &Path,
    mut error: TranscriptPumpError,
) -> TranscriptPumpError {
    match StatusCoordinator::spawn(file_status_store(&status_path_for(transcript_path)), None) {
        Ok(mut coordinator) => {
            let spec =
                TerminalStatusSpec::failed(now_ms(), PumpSummary::default(), error.message());
            error.with_settlement(&coordinator.finish(spec))
        }
        Err(settlement) => {
            error.settlement_error = Some(bound_error(&settlement.to_string()));
            error
        }
    }
}

fn capture(
    reader: impl Read,
    transcript_path: &Path,
    coordinator: Option<&StatusCoordinator>,
    preview: &dyn PreviewSink,
    config: &TranscriptPumpConfig,
    counters: &SharedCounters,
    started: u64,
) -> Result<(), TranscriptPumpError> {
    let mut file = std::fs::File::create(transcript_path).map_err(|err| {
        TranscriptPumpError::new(format!(
            "create transcript at {}: {err}",
            transcript_path.display()
        ))
    })?;
    // The initial Running status is required and typed: it is submitted to the
    // coordinator and its acknowledged persistence failure fails the drain.
    if let Some(coordinator) = coordinator {
        coordinator.submit_required(build_status(
            PumpState::Running,
            started,
            &counters.snapshot(),
            None,
            None,
            StatusTransportDiagnostics::default(),
        ))?;
    }

    let mut reader = reader;
    let chunk_size = config.read_chunk_size.max(1);
    let mut buf = vec![0u8; chunk_size];
    let mut line = PreviewLine::new(config.console_preview_limit);
    let mut diagnostic_line = DiagnosticLine::default();
    let mut last_flush = Instant::now();

    loop {
        let read = reader
            .read(&mut buf)
            .map_err(|err| TranscriptPumpError::new(format!("read coder stdout: {err}")))?;
        if read == 0 {
            break;
        }
        let chunk = &buf[..read];
        let provider_retry_observed = persist_chunk(
            &mut file,
            chunk,
            &mut line,
            &mut diagnostic_line,
            preview,
            counters,
            transcript_path,
        )?;

        if provider_retry_observed && let Some(coordinator) = coordinator {
            coordinator.submit_periodic(build_status(
                PumpState::Running,
                started,
                &counters.snapshot(),
                None,
                None,
                StatusTransportDiagnostics::default(),
            ));
        }

        if last_flush.elapsed() >= config.status_flush_interval {
            // Periodic snapshots go through the coordinator's coalescing slot, never
            // blocking canonical capture on a slow status filesystem.
            if let Some(coordinator) = coordinator {
                coordinator.submit_periodic(build_status(
                    PumpState::Running,
                    started,
                    &counters.snapshot(),
                    None,
                    None,
                    StatusTransportDiagnostics::default(),
                ));
            }
            last_flush = Instant::now();
        }
    }

    // A trailing record without a final newline is still a record the coder
    // emitted; count it and offer its preview before completing.
    if line.has_bytes() {
        counters.add_record();
        counters.observe_record(diagnostic_line.as_slice());
        counters.finish_complete_record_boundary();
        deliver_preview(&mut line, preview, counters);
    }
    counters.finish_provider_wait();

    file.flush().map_err(|err| {
        TranscriptPumpError::new(format!(
            "flush transcript at {}: {err}",
            transcript_path.display()
        ))
    })?;

    Ok(())
}

/// Persist one read chunk to the transcript, accounting each successful partial
/// write BEFORE the next fallible write and driving record and preview accounting
/// only over the bytes that actually reached the transcript.
///
/// A single `write` may persist fewer bytes than requested; the byte counter and
/// record/preview parsing must reflect exactly the persisted prefix, so a later
/// write in the same chunk that fails leaves truthful counters rather than
/// crediting bytes that never landed.
fn persist_chunk<W: Write>(
    writer: &mut W,
    chunk: &[u8],
    line: &mut PreviewLine,
    diagnostic_line: &mut DiagnosticLine,
    preview: &dyn PreviewSink,
    counters: &SharedCounters,
    transcript_path: &Path,
) -> Result<bool, TranscriptPumpError> {
    let mut written = 0;
    let mut provider_retry_observed = false;
    while written < chunk.len() {
        match writer.write(&chunk[written..]) {
            Ok(0) => {
                return Err(TranscriptPumpError::new(format!(
                    "write transcript at {}: wrote zero bytes",
                    transcript_path.display()
                )));
            }
            Ok(n) => {
                // Count the persisted prefix and parse only those bytes before the
                // next fallible write.
                counters.add_bytes(n as u64);
                // The complete persisted prefix must be classified before the
                // supervisor can treat the stream as safely pre-progress.
                for &byte in &chunk[written..written + n] {
                    if byte == b'\n' {
                        counters.add_record();
                        provider_retry_observed |=
                            counters.observe_record(diagnostic_line.take().as_slice());
                        deliver_preview(line, preview, counters);
                    } else {
                        if diagnostic_line.as_slice().is_empty() {
                            counters.observe_partial_record();
                        }
                        line.push(byte);
                        diagnostic_line.push(byte);
                    }
                }
                if diagnostic_line.as_slice().is_empty() {
                    counters.finish_complete_record_boundary();
                }
                written += n;
            }
            Err(ref err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                return Err(TranscriptPumpError::new(format!(
                    "write transcript at {}: {err}",
                    transcript_path.display()
                )));
            }
        }
    }
    Ok(provider_retry_observed)
}

/// Offer one record's bounded preview to the console sink, accounting the loss
/// BEFORE the call and undoing it only once delivery is confirmed.
///
/// Pre-accounting is what makes a sink that panics — or unwinds — safe: the
/// record's dropped-preview count is already committed, so the caught-panic
/// terminal status can never show a record whose preview simply vanished.
fn deliver_preview(line: &mut PreviewLine, preview: &dyn PreviewSink, counters: &SharedCounters) {
    let rendered = line.render_and_reset();
    counters.add_dropped();
    if preview.deliver(&rendered) {
        counters.sub_dropped();
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn build_status(
    state: PumpState,
    started_at_ms: u64,
    summary: &PumpSummary,
    error: Option<&str>,
    periodic_error: Option<&str>,
    transport: StatusTransportDiagnostics,
) -> PumpStatus {
    PumpStatus {
        schema_version: PUMP_STATUS_SCHEMA_VERSION,
        state,
        started_at_ms,
        updated_at_ms: now_ms(),
        bytes: summary.bytes,
        records: summary.records,
        dropped_console: summary.dropped_console,
        provider_timing: summary.provider_timing.clone(),
        provider_liveness: summary.provider_liveness.clone(),
        // Bound the persisted primary and periodic errors so a pathological store
        // error can never bloat the on-disk status document either.
        error: error.map(bound_error),
        periodic_error: periodic_error.map(bound_error),
        transport,
    }
}

/// Serialize and atomically persist a status document, returning a message on
/// failure so the caller can decide whether the failure is terminal.
fn persist_status_to(path: &Path, status: &PumpStatus) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(status).map_err(|err| err.to_string())?;
    crate::atomic_write::atomic_write(path, &bytes).map_err(|err| err.to_string())
}

/// The durable sink for pump status documents. It is the coordinator's sole
/// persistence dependency, injected so tests can gate, delay, fail, disconnect, or
/// panic status writes deterministically rather than relying on timing.
///
/// A `write` returns a fully-formed error message on failure — including the
/// target path — so the message can flow into a required-status failure, a
/// terminal-settlement failure, or a retained periodic error unchanged.
pub(crate) trait StatusStore: Send {
    fn write(&mut self, status: &PumpStatus) -> Result<(), String>;
}

/// The production status store: atomically persist to `transcript-pump.json`.
struct FileStatusStore {
    path: PathBuf,
}

impl StatusStore for FileStatusStore {
    fn write(&mut self, status: &PumpStatus) -> Result<(), String> {
        persist_status_to(&self.path, status)
            .map_err(|err| format!("persist pump status at {}: {err}", self.path.display()))
    }
}

fn file_status_store(path: &Path) -> Box<dyn StatusStore> {
    Box::new(FileStatusStore {
        path: path.to_path_buf(),
    })
}

/// A partially-built terminal status. The drain thread decides the terminal state,
/// counters, and primary error; the coordinator worker fills in the balanced
/// transport diagnostics and proves no snapshot remained pending before writing it.
struct TerminalStatusSpec {
    state: PumpState,
    started_at_ms: u64,
    summary: PumpSummary,
    error: Option<String>,
}

impl TerminalStatusSpec {
    fn complete(started_at_ms: u64, summary: PumpSummary) -> Self {
        Self {
            state: PumpState::Complete,
            started_at_ms,
            summary,
            error: None,
        }
    }

    fn failed(started_at_ms: u64, summary: PumpSummary, error: &str) -> Self {
        Self {
            state: PumpState::Failed,
            started_at_ms,
            summary,
            error: Some(error.to_string()),
        }
    }

    /// The Failed fallback for a Complete status that could not be persisted.
    fn as_failed(&self, settlement_error: &str) -> Self {
        Self {
            state: PumpState::Failed,
            started_at_ms: self.started_at_ms,
            summary: self.summary.clone(),
            error: Some(format!(
                "complete status could not be persisted: {settlement_error}"
            )),
        }
    }

    fn build(
        &self,
        transport: StatusTransportDiagnostics,
        periodic_error: Option<&str>,
    ) -> PumpStatus {
        build_status(
            self.state,
            self.started_at_ms,
            &self.summary,
            self.error.as_deref(),
            periodic_error,
            transport,
        )
    }
}

/// The outcome of settling a status coordinator at terminal time. It carries the
/// balanced transport diagnostics and any terminal-settlement, fallback, or worker
/// failure so the drain can build a composite typed error that never masks the
/// primary fault.
#[derive(Debug, Default)]
struct StatusSettlement {
    diagnostics: StatusTransportDiagnostics,
    /// The terminal status (Complete or Failed) could not be persisted.
    settlement_error: Option<String>,
    /// A Complete-to-Failed fallback also failed to persist.
    fallback_error: Option<String>,
    /// The last best-effort periodic write failure, kept distinct from required and
    /// terminal failures.
    periodic_error: Option<String>,
    /// The coordinator worker panicked or could not be joined.
    worker_error: Option<String>,
}

impl StatusSettlement {
    /// A settled capture is a terminal infrastructure failure when the terminal
    /// status could not be persisted (even if a Failed fallback landed, capture is
    /// not independently observable) OR when the status worker panicked. Build the
    /// composite typed error, or `None` when settlement succeeded cleanly.
    fn terminal_failure(&self) -> Option<TranscriptPumpError> {
        let message = self
            .settlement_error
            .clone()
            .or_else(|| self.worker_error.clone())?;
        Some(TranscriptPumpError {
            message: bound_error(&message),
            periodic_error: self.periodic_error.as_deref().map(bound_error),
            // Attach the terminal-settlement failure to its dedicated field rather
            // than clearing it: the message may name either the settlement or the
            // worker failure, but a caller inspecting the typed error must still find
            // the settlement failure distinctly, kept separate from periodic, fallback,
            // and worker diagnostics.
            settlement_error: self.settlement_error.as_deref().map(bound_error),
            fallback_error: self.fallback_error.as_deref().map(bound_error),
            worker_error: self.worker_error.as_deref().map(bound_error),
            transport: Some(self.diagnostics.clone()),
        })
    }
}

/// A write-once latch that publishes a capture's immutable first infrastructure
/// fault to coder supervision. The capture path and the status worker both publish
/// to it BEFORE attempting terminal status settlement, so a delayed or blocked
/// status write can never hide the first fault from the supervisor: supervision
/// observes the latch independently of joining the pump's terminal outcome.
///
/// The payload is a `OnceLock`, so publishing the message and making the fault
/// observable are the same atomic write-once step — a reader that observes the
/// fault always sees its message — and the notification path never takes a blocking
/// mutex that a saturated writer could contend.
#[derive(Default)]
pub struct FirstFault {
    fault: OnceLock<String>,
}

impl FirstFault {
    /// Publish the first fault. Only the first caller wins; later faults never
    /// overwrite it, so the latch names the immutable first cause.
    fn publish(&self, err: &TranscriptPumpError) {
        let _ = self.fault.set(err.message().to_string());
    }

    /// Whether a first fault has been published. Supervision polls this so it can
    /// terminate and reap the coder before waiting for terminal status settlement.
    pub fn observed(&self) -> bool {
        self.fault.get().is_some()
    }

    #[cfg(test)]
    fn message(&self) -> Option<String> {
        self.fault.get().cloned()
    }
}

/// A monotonic identity for one required status command, assigned when it is
/// submitted. It follows the command from the required FIFO into its active record or
/// into shared resolving ownership, so cleanup and reconciliation act on one exact
/// identity and never clear unrelated work.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct RequiredCommandId(u64);

/// The shared cell behind an idempotent one-shot acknowledgement. The submitter holds
/// a [`RequiredAckWaiter`] and blocks on it; the resolver authority — held by the
/// queued command and then the active record — fixes exactly one immutable
/// `Result<(), String>` and wakes the waiter. Repeated resolution leaves the first
/// stored result unchanged, with no second delivery, replacement, or block;
/// `RequiredAckResolver::resolve_once` returns `()`, and only `RequiredAckWaiter::wait`
/// returns the stored result. Its lock is never held together with the coordinator
/// mutex.
struct AckCell {
    result: Mutex<Option<Result<(), String>>>,
    ready: Condvar,
}

/// The resolver authority for a required acknowledgement. Cloning it shares the one
/// underlying cell, so the queued command, the active record, and any reconciler hold
/// the same idempotent one-shot.
#[derive(Clone)]
struct RequiredAckResolver {
    cell: Arc<AckCell>,
}

/// The submitter side of a required acknowledgement. It blocks until the one-shot
/// result becomes observable.
struct RequiredAckWaiter {
    cell: Arc<AckCell>,
}

impl RequiredAckResolver {
    /// Fix the one-shot result if it is not already fixed, waking the waiter. A second
    /// call observes the stored result and neither replaces nor re-delivers it, so
    /// normal and reconciliation paths converge on one immutable result.
    fn resolve_once(&self, result: Result<(), String>) {
        let mut slot = self.cell.result.lock().unwrap();
        if slot.is_none() {
            *slot = Some(result);
            self.cell.ready.notify_all();
        }
    }

    /// Whether the one-shot result is already observable. Read with the coordinator
    /// mutex released, so the two locks are never held together.
    fn is_observable(&self) -> bool {
        self.cell.result.lock().unwrap().is_some()
    }
}

impl RequiredAckWaiter {
    /// Whether the one-shot result is already observable, read without consuming the
    /// waiter. A test reads its own shared acknowledgement cell here to prove the result
    /// was fixed by a specific settlement path before it blocks on `wait`.
    #[cfg(test)]
    fn test_is_observable(&self) -> bool {
        self.cell.result.lock().unwrap().is_some()
    }

    /// Block until the one-shot result is observable and return it.
    fn wait(self) -> Result<(), String> {
        let mut slot = self.cell.result.lock().unwrap();
        while slot.is_none() {
            slot = self.cell.ready.wait(slot).unwrap();
        }
        slot.clone().unwrap()
    }
}

/// Create a fresh idempotent one-shot acknowledgement: the resolver authority and the
/// submitter's waiter, sharing one cell.
fn new_ack() -> (RequiredAckResolver, RequiredAckWaiter) {
    let cell = Arc::new(AckCell {
        result: Mutex::new(None),
        ready: Condvar::new(),
    });
    (
        RequiredAckResolver {
            cell: Arc::clone(&cell),
        },
        RequiredAckWaiter { cell },
    )
}

/// The lifecycle of a never-accepted required command. It stays `Queued` in the single
/// shared required deque until authorized worker termination changes it — in place,
/// under the coordinator mutex — to `Resolving`. There is no second container: a
/// retired command keeps its position, identity, and original resolver in the same
/// deque, and only matching cleanup removes it once its one-shot is observable.
enum QueuedLifecycle {
    Queued,
    /// Authorized termination retired this never-accepted command in place. It keeps its
    /// original resolver in the shared deque; its immutable disconnected result is the
    /// static `immutable_error`, and its disconnected accounting was applied once during
    /// the lock-held retirement commit. `RetiringBeforeCleanup` — the resolver is
    /// observable but the entry is not yet removed — is derived from the resolver's
    /// observable state, not stored here as a second lifecycle truth.
    Resolving {
        immutable_error: &'static str,
    },
}

/// A required status command with its acknowledgement resolver and lifecycle. It lives
/// in the single shared required deque from enqueue through acknowledgement
/// observability: while `Queued` it awaits selection; once authorized termination
/// retires it in place to `Resolving` it stays in the same deque, keeping its identity
/// and original resolver until its one-shot result is observable and matching cleanup
/// removes it.
struct RequiredCommand {
    id: RequiredCommandId,
    status: PumpStatus,
    resolver: RequiredAckResolver,
    lifecycle: QueuedLifecycle,
}

/// A required status accepted for the worker: its id and the status to persist. Its
/// acknowledgement resolver already lives in the shared active record, installed as
/// one lock-held mutation with the queue removal.
struct AcceptedRequired {
    id: RequiredCommandId,
    status: PumpStatus,
}

/// The active phase of an accepted required write. Acceptance versus resolved/retiring
/// is derived from the one-shot's observable state, not stored here: while the
/// resolver is unresolved the record is `Accepted`; once resolved it is
/// `Resolved/Retiring` until matching cleanup.
enum ActivePhase {
    /// Accepted but the store has not returned. Only a caller holding
    /// [`AbandonmentAuthority`] may turn this into a bounded worker-unwind result.
    Pending,
    /// The store returned this exact result; the first transition after return.
    Observed(Result<(), String>),
    /// The immutable caller result is fixed and accounting is applied; the one-shot
    /// resolves from `result` outside the coordinator lock.
    Prepared { result: Result<(), String> },
}

/// The shared record for one accepted required write, owning its acknowledgement
/// resolver from the acceptance linearization point through matching cleanup.
struct ActiveWrite {
    id: RequiredCommandId,
    resolver: RequiredAckResolver,
    phase: ActivePhase,
    /// Whether this write's success/failure has been applied to the diagnostics
    /// exactly once, so repeated reconciliation observes rather than re-applies it.
    accounting_applied: bool,
}

/// An owned proof that no live store frame can still return and no possible real
/// result remains unpublished. Only boundaries that establish that fact construct it:
/// the worker's own panic catch after the stack unwound, the worker observing a
/// disconnected wake outside any store call and committed to exit, or `finish`/`drop`
/// after the worker join completes. Holding the proof does not mean the worker has
/// already exited — the wake-disconnected worker constructs it while still running its
/// final reconciliation. An exact result already shared in `Observed` or `Prepared` is
/// preserved, not erased: reconciliation only fixes the bounded worker-unwind outcome
/// for a `Pending` frame that can never return. A direct or concurrent reconciler
/// without this proof cannot synthesize a pending outcome — it leaves `Pending` owned
/// and unresolved.
struct AbandonmentAuthority {
    _private: (),
}

impl AbandonmentAuthority {
    /// Construct the proof at a boundary that has established the worker can no longer
    /// publish a store result. Private to this module and never handed to an arbitrary
    /// reconciler, so `Pending` synthesis cannot occur without it.
    fn assume_worker_abandoned() -> Self {
        Self { _private: () }
    }
}

/// What the worker does next, decided atomically under one lock so required statuses
/// drain FIFO, the newest periodic drains before the terminal, and the terminal write
/// is always last.
enum Work {
    Required(AcceptedRequired),
    Periodic(PumpStatus),
    Terminal(TerminalStatusSpec),
    Idle,
}

/// Which non-required submission a write persisted, so periodic and Complete-to-Failed
/// fallback failures stay distinct from terminal failures in the diagnostics. Required
/// writes track their in-flight state in the shared active record instead.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteKind {
    Periodic,
    Terminal,
    /// The Failed fallback written after a Complete status could not be persisted.
    /// Tracked distinctly so a fallback failure — including a panic mid-fallback —
    /// lands in its own `fallback_error` rather than masquerading as a generic
    /// terminal or worker failure.
    Fallback,
}

/// The bounded diagnostic fixed as an accepted required write's one-shot result when
/// the worker provably unwound before the store returned. "panicked" keeps it
/// attributable to a worker unwind in the transport `last_error`.
const ACCEPTED_WRITE_WORKER_UNWIND: &str =
    "persist pump status: status worker panicked before acknowledging an accepted required write";

/// The bounded disconnected result fixed on a never-accepted queued required write
/// when authorized termination retires it.
const QUEUED_REQUIRED_DISCONNECT: &str = "persist pump status: status worker disconnected";

/// Shared coordinator state. Every submission mutates it under one lock and every
/// category the submitter can decide (coalesced, dropped, disconnected) is recorded
/// there immediately, so the balance invariant holds even for submissions the
/// worker never sees.
struct CoordinatorInner {
    /// The newest pending periodic snapshot; a replaced one is counted coalesced.
    periodic: Option<PumpStatus>,
    /// The single shared required deque. It owns every never-accepted command and its
    /// original acknowledgement resolver from enqueue through acknowledgement
    /// observability. A `Queued` entry awaits the worker (processed front-first); an
    /// entry authorized termination retired in place is `Resolving` and stays in this
    /// same deque — there is no second resolving container — until its one-shot is
    /// observable and matching cleanup removes it.
    required: VecDeque<RequiredCommand>,
    /// The accepted required write the worker is currently persisting, owning its
    /// acknowledgement resolver from the acceptance linearization point through
    /// matching cleanup. `Pending`, `Observed`, and `Prepared` phases and the derived
    /// `Resolved/Retiring` state all live here.
    active_required: Option<ActiveWrite>,
    /// The next required command identity to assign.
    next_command_id: u64,
    /// The terminal status, set once by `finish`; the worker writes it last.
    terminal: Option<TerminalStatusSpec>,
    /// Exact accounting of every submission.
    diagnostics: StatusTransportDiagnostics,
    /// The category of a *non-required* write (periodic, terminal, or fallback) the
    /// worker is currently attempting, set before each `store.write` and cleared once
    /// its result is recorded. A still-set marker means the worker unwound *inside*
    /// `store.write`, so reconciliation accounts the attempted write truthfully as a
    /// write failure rather than sweeping it into the disconnected bucket (which would
    /// hide that the store was reached). Required writes track their in-flight state in
    /// `active_required` instead.
    in_flight: Option<WriteKind>,
    /// The last best-effort *periodic* write failure, kept distinct from required
    /// and terminal failures so a required failure never masquerades as periodic.
    periodic_error: Option<String>,
    /// A terminal (Complete) status write failure, recorded before any Failed
    /// fallback so a fallback panic cannot lose the primary Complete error.
    settlement_error: Option<String>,
    /// The Complete-to-Failed fallback write failure, kept distinct from the primary
    /// settlement failure so a fallback that fails — or panics mid-write — is
    /// attributable to the fallback rather than folded into the generic worker error.
    fallback_error: Option<String>,
    /// Terminal sealing has begun; no further periodic snapshot is written.
    sealed: bool,
    /// The worker has fully shut down; further submissions are disconnected.
    shutdown: bool,
}

impl CoordinatorInner {
    /// The initial coordinator state: nothing pending, nothing accounted, not yet
    /// sealed or shut down. Shared by production spawn and the deterministic test
    /// harness so both start from one canonical state.
    fn new() -> Self {
        Self {
            periodic: None,
            required: VecDeque::new(),
            active_required: None,
            next_command_id: 0,
            terminal: None,
            diagnostics: StatusTransportDiagnostics::default(),
            in_flight: None,
            periodic_error: None,
            settlement_error: None,
            fallback_error: None,
            sealed: false,
            shutdown: false,
        }
    }
}

struct SharedStatusState {
    inner: Mutex<CoordinatorInner>,
    /// Test-only settlement probes. Each is invoked outside both the coordinator and
    /// acknowledgement-cell mutexes at one exact boundary so a test can inject an unwind
    /// or force concurrent settlers there. Production compiles without this field, so no
    /// probe exists off the test path.
    #[cfg(test)]
    hooks: TestHooks,
}

/// A test-only settlement probe with no argument, shared as an `Arc` so it can be
/// cloned out and invoked with every coordinator lock released.
#[cfg(test)]
type ProbeHook = Arc<dyn Fn() + Send + Sync>;

/// A test-only settlement probe carrying the required identity at the boundary.
#[cfg(test)]
type IdProbeHook = Arc<dyn Fn(RequiredCommandId) + Send + Sync>;

/// The installable test-only settlement probes. Each slot defaults to empty, so an
/// un-instrumented coordinator invokes nothing. A test installs a probe to observe or
/// perturb one exact boundary; the probe runs with both mutexes released.
#[cfg(test)]
#[derive(Default)]
struct TestHooks {
    /// Runs before the lock-held queued-retirement commit is taken.
    before_queued_retirement_commit: Mutex<Option<ProbeHook>>,
    /// Runs after a resolving queued one-shot is observable and before its matching
    /// cleanup.
    queued_retiring_before_cleanup: Mutex<Option<IdProbeHook>>,
    /// Runs after an active record is installed as `Prepared` and before its one-shot
    /// resolves.
    active_prepared: Mutex<Option<IdProbeHook>>,
    /// Runs after an active one-shot result is observable and before its matching
    /// cleanup.
    active_retiring_before_cleanup: Mutex<Option<IdProbeHook>>,
    /// Runs each time the worker is about to block on the wake channel, so a test knows
    /// the worker is idle before it withholds a wake.
    worker_before_wake_receive: Mutex<Option<ProbeHook>>,
    /// Runs inside `finish` after the terminal is queued and before the worker join.
    finish_before_join: Mutex<Option<ProbeHook>>,
    /// Runs inside `drop` after the wake sender is removed and before the worker join.
    drop_wake_disconnected_before_join: Mutex<Option<ProbeHook>>,
}

impl SharedStatusState {
    /// Invoke the `BeforeQueuedRetirementCommit` probe, if installed, with both mutexes
    /// released. Production compiles this to nothing.
    #[cfg(test)]
    fn emit_before_queued_retirement_commit(&self) {
        let hook = self
            .hooks
            .before_queued_retirement_commit
            .lock()
            .unwrap()
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    #[inline]
    fn emit_before_queued_retirement_commit(&self) {}

    /// Invoke the `QueuedRetiringBeforeCleanup` probe for `id`, if installed, with both
    /// mutexes released. Production compiles this to nothing.
    #[cfg(test)]
    fn emit_queued_retiring_before_cleanup(&self, id: RequiredCommandId) {
        let hook = self
            .hooks
            .queued_retiring_before_cleanup
            .lock()
            .unwrap()
            .clone();
        if let Some(hook) = hook {
            hook(id);
        }
    }

    #[cfg(not(test))]
    #[inline]
    fn emit_queued_retiring_before_cleanup(&self, _id: RequiredCommandId) {}

    /// Invoke the `ActivePrepared` probe for `id`, if installed, with both mutexes
    /// released. Production compiles this to nothing.
    #[cfg(test)]
    fn emit_active_prepared(&self, id: RequiredCommandId) {
        let hook = self.hooks.active_prepared.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(id);
        }
    }

    #[cfg(not(test))]
    #[inline]
    fn emit_active_prepared(&self, _id: RequiredCommandId) {}

    /// Invoke the `ActiveRetiringBeforeCleanup` probe for `id`, if installed, with both
    /// mutexes released. Production compiles this to nothing.
    #[cfg(test)]
    fn emit_active_retiring_before_cleanup(&self, id: RequiredCommandId) {
        let hook = self
            .hooks
            .active_retiring_before_cleanup
            .lock()
            .unwrap()
            .clone();
        if let Some(hook) = hook {
            hook(id);
        }
    }

    #[cfg(not(test))]
    #[inline]
    fn emit_active_retiring_before_cleanup(&self, _id: RequiredCommandId) {}

    /// Invoke the `WorkerBeforeWakeReceive` probe, if installed, with both mutexes
    /// released. Production compiles this to nothing.
    #[cfg(test)]
    fn emit_worker_before_wake_receive(&self) {
        let hook = self
            .hooks
            .worker_before_wake_receive
            .lock()
            .unwrap()
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    #[inline]
    fn emit_worker_before_wake_receive(&self) {}

    /// Invoke the `FinishBeforeJoin` probe, if installed, with both mutexes released.
    /// Production compiles this to nothing.
    #[cfg(test)]
    fn emit_finish_before_join(&self) {
        let hook = self.hooks.finish_before_join.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    #[inline]
    fn emit_finish_before_join(&self) {}

    /// Invoke the `DropWakeDisconnectedBeforeJoin` probe, if installed, with both mutexes
    /// released. Production compiles this to nothing.
    #[cfg(test)]
    fn emit_drop_wake_disconnected_before_join(&self) {
        let hook = self
            .hooks
            .drop_wake_disconnected_before_join
            .lock()
            .unwrap()
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }

    #[cfg(not(test))]
    #[inline]
    fn emit_drop_wake_disconnected_before_join(&self) {}

    /// Atomically remove the next queued required command and install its
    /// acknowledgement resolver as an active record in `Accepted/Pending`, returning
    /// the status to persist. Queue removal and active installation are one lock-held
    /// mutation with no intervening hook, probe, callback, or fallible work; the
    /// projected transport is stamped under the same lock. Returns `None` when no
    /// required command is queued or a write is already active (the single worker
    /// never selects a second).
    fn accept_next_required(&self) -> Option<AcceptedRequired> {
        let mut inner = self.inner.lock().unwrap();
        if inner.active_required.is_some() {
            return None;
        }
        // Refuse acceptance once the coordinator has shut down, before examining or
        // removing any front entry. A shut-down coordinator's queued ownership belongs to
        // authorized reconciliation, which disconnects it; the worker must never move a
        // queued entry into an active write after shutdown.
        if inner.shutdown {
            return None;
        }
        // Accept only a `Queued` front entry. A `Resolving` entry is retired shared
        // state — the single deque now holds both — and is never acceptable work.
        match inner.required.front() {
            Some(front) if matches!(front.lifecycle, QueuedLifecycle::Queued) => {}
            _ => return None,
        }
        let RequiredCommand {
            id,
            mut status,
            resolver,
            ..
        } = inner.required.pop_front()?;
        // Install shared active ownership before releasing the lock: from here the
        // command is neither queued nor lost — it is the active record owning its
        // acknowledgement resolver.
        inner.active_required = Some(ActiveWrite {
            id,
            resolver,
            phase: ActivePhase::Pending,
            accounting_applied: false,
        });
        // Stamp a self-consistent projected accounting under the same lock so the
        // persisted Running document carries a balanced view, not zeros.
        let mut projected = inner.diagnostics.clone();
        projected.written += 1;
        status.transport = projected;
        Some(AcceptedRequired { id, status })
    }

    fn next_work(&self) -> Work {
        // Accept the next required command (atomic queue-removal + active install).
        if let Some(accepted) = self.accept_next_required() {
            return Work::Required(accepted);
        }
        let mut inner = self.inner.lock().unwrap();
        // Drain the newest periodic before the terminal so no Running write can
        // follow the terminal state.
        if let Some(status) = inner.periodic.take() {
            return Work::Periodic(status);
        }
        if let Some(spec) = inner.terminal.take() {
            return Work::Terminal(spec);
        }
        Work::Idle
    }

    /// Publish the store's returned result into the matching active record as
    /// `Observed`. This is the first state transition after `store.write` returns,
    /// before any classification, accounting, probe, or acknowledgement work, so a
    /// later unwind reproduces the exact caller and ledger result.
    fn publish_observed(&self, id: RequiredCommandId, result: Result<(), String>) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(active) = inner.active_required.as_mut() {
            if active.id == id {
                active.phase = ActivePhase::Observed(result);
            }
        }
    }

    /// Prepare the active record for resolution under the coordinator lock: derive the
    /// caller result from its own stored truth, apply success/failure accounting
    /// exactly once, move it to `Prepared`, and return its id, a resolver handle, and
    /// the result to resolve outside the lock. Returns `None` when there is no active
    /// record, or when the record is `Pending` and no `authority` proves the worker
    /// abandoned it — leaving `Pending` owned and unresolved.
    fn prepare_active(
        &self,
        authority: Option<&AbandonmentAuthority>,
    ) -> Option<(RequiredCommandId, RequiredAckResolver, Result<(), String>)> {
        let mut inner = self.inner.lock().unwrap();
        let (id, resolver, result, needs_accounting) = {
            let active = inner.active_required.as_ref()?;
            let result: Result<(), String> = match &active.phase {
                ActivePhase::Pending => {
                    // Synthesizing an unwind requires explicit abandonment authority; a
                    // reconciler without it must leave the live worker's Pending owned.
                    authority?;
                    Err(bound_error(ACCEPTED_WRITE_WORKER_UNWIND))
                }
                ActivePhase::Observed(Ok(())) => Ok(()),
                ActivePhase::Observed(Err(err)) => Err(bound_error(err)),
                ActivePhase::Prepared { result } => result.clone(),
            };
            (
                active.id,
                active.resolver.clone(),
                result,
                !active.accounting_applied,
            )
        };
        if needs_accounting {
            match &result {
                Ok(()) => inner.diagnostics.written += 1,
                Err(err) => {
                    inner.diagnostics.write_failures += 1;
                    inner.diagnostics.last_error = Some(err.clone());
                }
            }
        }
        let active = inner
            .active_required
            .as_mut()
            .expect("active record present under the same lock");
        active.accounting_applied = true;
        active.phase = ActivePhase::Prepared {
            result: result.clone(),
        };
        Some((id, resolver, result))
    }

    /// Remove the active record for `id` once its one-shot result is observable,
    /// transitioning it from `Resolved/Retiring` to cleaned. `observable` is read with
    /// the coordinator lock released, so the two locks are never held together.
    fn cleanup_active_if_observable(&self, id: RequiredCommandId, observable: bool) {
        if !observable {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.active_required.as_ref().is_some_and(|a| a.id == id) {
            inner.active_required = None;
        }
    }

    /// Settle the active required write end to end: prepare it under the lock, resolve
    /// its one-shot outside the lock (idempotent), then clean up the record once the
    /// result is observable. Returns without effect when there is no active record or
    /// when a `Pending` record has no abandonment authority. Safe to call repeatedly
    /// and concurrently: accounting and resolution each happen once.
    fn settle_active(&self, authority: Option<&AbandonmentAuthority>) {
        let Some((id, resolver, result)) = self.prepare_active(authority) else {
            return;
        };
        // Probe the shared `Prepared` boundary — the record is installed with its exact
        // result and accounting fixed — before the one-shot resolves. Outside both locks.
        self.emit_active_prepared(id);
        // Resolve outside the coordinator lock; the two locks are never held together.
        resolver.resolve_once(result);
        let observable = resolver.is_observable();
        // Probe the `RetiringBeforeCleanup` boundary — the exact result is observable —
        // before matching cleanup removes the record. Outside both locks.
        self.emit_active_retiring_before_cleanup(id);
        self.cleanup_active_if_observable(id, observable);
    }

    /// Retire every never-accepted queued required command in place, under one
    /// coordinator-lock section, changing each `Queued` entry to `Resolving` without
    /// leaving the shared deque. A test-only `BeforeQueuedRetirementCommit` probe runs
    /// first, outside the lock, so a test can force an unwind before the commit; if it
    /// unwinds, the lock is never taken and every entry stays `Queued`, unresolved, and
    /// unaccounted for a later authorized reconciliation to resume.
    ///
    /// The lock-held commit performs no allocation, callback, hook, acknowledgement-cell
    /// operation, queue move, append, reserve, collect, local command container, or
    /// resolver clone: it counts the `Queued` entries, computes the final disconnected
    /// count, changes each `Queued` entry's lifecycle to `Resolving` with the static
    /// immutable error, and assigns the disconnected count. Requires abandonment
    /// authority. Idempotent: an already-`Resolving` entry is left as it is, and a later
    /// call resumes any newly queued command.
    fn retire_queued(&self, _authority: &AbandonmentAuthority) {
        // The retirement probe runs before the lock so an injected unwind leaves the
        // commit untaken and every entry still queued and unaccounted.
        self.emit_before_queued_retirement_commit();
        let mut inner = self.inner.lock().unwrap();
        inner.shutdown = true;
        // Count the never-accepted entries and compute the final disconnected count
        // before mutating any lifecycle — no allocation or clone in the commit itself.
        let mut queued_count = 0u64;
        for entry in &inner.required {
            if matches!(entry.lifecycle, QueuedLifecycle::Queued) {
                queued_count += 1;
            }
        }
        let final_disconnected = inner.diagnostics.disconnected + queued_count;
        // Change every `Queued` entry to `Resolving` in place with the static immutable
        // error. The entry keeps its position, identity, and original resolver.
        for entry in inner.required.iter_mut() {
            if matches!(entry.lifecycle, QueuedLifecycle::Queued) {
                entry.lifecycle = QueuedLifecycle::Resolving {
                    immutable_error: QUEUED_REQUIRED_DISCONNECT,
                };
            }
        }
        inner.diagnostics.disconnected = final_disconnected;
    }

    /// Resolve the shared resolving queued commands one identity at a time. Each
    /// iteration selects one `Resolving` entry under the lock and clones only its
    /// `(id, resolver, immutable_error)`; the original resolver stays in the shared
    /// deque. The lock is released before the bounded `Err` is constructed, before the
    /// idempotent one-shot resolves, before its observability is read, and before the
    /// `QueuedRetiringBeforeCleanup` probe — every step outside both mutexes. Only then,
    /// under the lock, is the matching id removed, and only when it is still `Resolving`
    /// and its one-shot became observable. An unwind or a concurrent reconciler that
    /// selected the same identity loses nothing: the one-shot is idempotent and the
    /// matching-id cleanup is a no-op on an already-removed entry.
    fn resolve_queued(&self) {
        loop {
            // Select one shared resolving identity and clone only its handle and static
            // error under the lock — never the resolver's observability, which would
            // require the acknowledgement-cell lock while the coordinator lock is held.
            let selected = {
                let inner = self.inner.lock().unwrap();
                inner
                    .required
                    .iter()
                    .find_map(|entry| match &entry.lifecycle {
                        QueuedLifecycle::Resolving { immutable_error } => {
                            Some((entry.id, entry.resolver.clone(), *immutable_error))
                        }
                        QueuedLifecycle::Queued => None,
                    })
            };
            let Some((id, resolver, immutable_error)) = selected else {
                break;
            };
            // Construct the bounded error and resolve the idempotent one-shot outside all
            // locks; a repeat or concurrent resolution observes the same immutable result.
            let result: Result<(), String> = Err(bound_error(immutable_error));
            resolver.resolve_once(result);
            let observable = resolver.is_observable();
            // Probe the post-observability, pre-cleanup boundary outside both mutexes.
            self.emit_queued_retiring_before_cleanup(id);
            // Remove only this matching identity, and only when it is still `Resolving`
            // and its one-shot became observable.
            if observable {
                let mut inner = self.inner.lock().unwrap();
                if let Some(pos) = inner.required.iter().position(|e| {
                    e.id == id && matches!(e.lifecycle, QueuedLifecycle::Resolving { .. })
                }) {
                    inner.required.remove(pos);
                }
            }
        }
    }

    /// Mark that the worker is about to attempt a non-required write of `kind`. Paired
    /// with `record_write`, which clears it. A still-set marker means the worker
    /// unwound inside `store.write`.
    fn begin_write(&self, kind: WriteKind) {
        self.inner.lock().unwrap().in_flight = Some(kind);
    }

    fn record_write(&self, result: &Result<(), String>, kind: WriteKind) {
        let mut inner = self.inner.lock().unwrap();
        inner.in_flight = None;
        match result {
            Ok(()) => inner.diagnostics.written += 1,
            Err(err) => {
                let bounded = bound_error(err);
                inner.diagnostics.write_failures += 1;
                inner.diagnostics.last_error = Some(bounded.clone());
                match kind {
                    WriteKind::Periodic => inner.periodic_error = Some(bounded),
                    WriteKind::Fallback => inner.fallback_error = Some(bounded),
                    WriteKind::Terminal => {}
                }
            }
        }
    }

    /// Record a Complete write failure before attempting a Failed fallback, so a
    /// fallback that itself panics still surfaces the original Complete error.
    fn set_settlement_error(&self, err: &str) {
        self.inner.lock().unwrap().settlement_error = Some(bound_error(err));
    }

    fn diagnostics(&self) -> StatusTransportDiagnostics {
        self.inner.lock().unwrap().diagnostics.clone()
    }

    /// A live snapshot of the transport accounting with the in-flight write
    /// optimistically projected, so a Running document carries a self-consistent,
    /// balanced view rather than a zeroed default.
    fn projected_write_diagnostics(&self) -> StatusTransportDiagnostics {
        let inner = self.inner.lock().unwrap();
        let mut projected = inner.diagnostics.clone();
        projected.written += 1;
        projected
    }

    fn periodic_error(&self) -> Option<String> {
        self.inner.lock().unwrap().periodic_error.clone()
    }

    fn fallback_error(&self) -> Option<String> {
        self.inner.lock().unwrap().fallback_error.clone()
    }

    /// Test-only proof that settlement left nothing pending: no coalescing slot, no
    /// queued or resolving required status in the single shared deque, no active
    /// required write, no unwritten terminal, and no in-flight write.
    #[cfg(test)]
    fn is_quiescent(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.periodic.is_none()
            && inner.required.is_empty()
            && inner.active_required.is_none()
            && inner.terminal.is_none()
            && inner.in_flight.is_none()
    }

    /// Account a non-required write that was in flight when the worker unwound as a
    /// write failure carrying the panic as its error — never swept into the
    /// disconnected bucket, which would falsely report the store was never attempted.
    /// A periodic or fallback write also latches into its distinct field.
    fn account_in_flight_unwind(&self) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(kind) = inner.in_flight.take() {
            let bounded = bound_error(STATUS_WORKER_PANIC);
            inner.diagnostics.write_failures += 1;
            inner.diagnostics.last_error = Some(bounded.clone());
            match kind {
                WriteKind::Periodic => inner.periodic_error = Some(bounded),
                WriteKind::Fallback => inner.fallback_error = Some(bounded),
                WriteKind::Terminal => {}
            }
        }
    }

    /// Account and clear a pending periodic snapshot the dead worker never wrote as
    /// one disconnected submission, so the balance invariant holds.
    fn retire_pending_periodic(&self) {
        let mut inner = self.inner.lock().unwrap();
        if inner.periodic.take().is_some() {
            inner.diagnostics.disconnected += 1;
        }
    }

    /// Reconcile all work the worker abandoned when it disconnected or panicked, with
    /// abandonment authority proving it can no longer publish a real result. Marks
    /// shutdown first so no new command can enter the FIFO, then settles the active
    /// required write from its own stored truth (synthesizing a bounded unwind for a
    /// `Pending` record), retires every never-accepted queued command in place to
    /// `Resolving` and resolves it, and clears any pending periodic. Idempotent
    /// under repeated and concurrent calls: every acknowledgement resolves exactly once
    /// and every category is accounted exactly once.
    fn reconcile_abandoned(&self, authority: &AbandonmentAuthority) {
        // Shut the FIFO first: any submission after this point is disconnected at
        // submit, and every command already queued is drained below.
        self.inner.lock().unwrap().shutdown = true;
        self.account_in_flight_unwind();
        self.settle_active(Some(authority));
        self.retire_queued(authority);
        self.resolve_queued();
        self.retire_pending_periodic();
    }
}

/// The single worker that owns a [`StatusStore`] and performs every persisted write
/// for one capture. Periodic snapshots coalesce through a latest-only slot; required
/// statuses are acknowledged FIFO; the terminal state is written last after all
/// pending work drains, proving no snapshot remained pending.
struct StatusCoordinator {
    shared: Arc<SharedStatusState>,
    /// A capacity-one wake: it only signals that shared state changed. A full slot
    /// is harmless because the newest value already lives in the shared slot.
    wake: Option<SyncSender<()>>,
    join: Option<JoinHandle<StatusSettlement>>,
}

/// The message a status-worker panic publishes and records.
const STATUS_WORKER_PANIC: &str = "status coordinator worker panicked while persisting a status";

impl StatusCoordinator {
    /// Spawn a coordinator with its single worker. Production construction supplies no
    /// test hooks, so no probe exists off the test path.
    fn spawn(
        store: Box<dyn StatusStore>,
        first_fault: Option<Arc<FirstFault>>,
    ) -> Result<Self, TranscriptPumpError> {
        let shared = Arc::new(SharedStatusState {
            inner: Mutex::new(CoordinatorInner::new()),
            #[cfg(test)]
            hooks: TestHooks::default(),
        });
        Self::spawn_worker(shared, store, first_fault)
    }

    /// Spawn a coordinator whose test hooks are already installed BEFORE the worker
    /// thread starts, so the worker can never emit `WorkerBeforeWakeReceive` (or any
    /// probe) before the test's hook exists. Only tests use it; production goes through
    /// [`StatusCoordinator::spawn`], which installs no hooks.
    #[cfg(test)]
    fn spawn_with_hooks(
        store: Box<dyn StatusStore>,
        first_fault: Option<Arc<FirstFault>>,
        hooks: TestHooks,
    ) -> Result<Self, TranscriptPumpError> {
        let shared = Arc::new(SharedStatusState {
            inner: Mutex::new(CoordinatorInner::new()),
            hooks,
        });
        Self::spawn_worker(shared, store, first_fault)
    }

    /// Start the worker thread over an already-constructed shared state and wire up the
    /// wake channel and join handle. Because the caller has fully built `shared` — hooks
    /// included — before this runs, the worker observes every installed probe from its
    /// first idle boundary onward.
    fn spawn_worker(
        shared: Arc<SharedStatusState>,
        mut store: Box<dyn StatusStore>,
        first_fault: Option<Arc<FirstFault>>,
    ) -> Result<Self, TranscriptPumpError> {
        let (wake_tx, wake_rx) = sync_channel::<()>(1);
        let worker_shared = Arc::clone(&shared);
        let join = std::thread::Builder::new()
            .name("transcript-pump-status".to_string())
            .spawn(move || {
                // Catch a store panic so the worker still settles: it publishes the
                // panic to the first-fault latch (so supervision sees it without
                // joining) and returns a settlement carrying the worker error rather
                // than poisoning the join.
                std::panic::catch_unwind(AssertUnwindSafe(|| {
                    run_status_worker(&worker_shared, &wake_rx, &mut *store, first_fault.as_ref())
                }))
                .unwrap_or_else(|_| {
                    if let Some(latch) = &first_fault {
                        latch.publish(&TranscriptPumpError::new(STATUS_WORKER_PANIC));
                    }
                    // The worker stack — and any store call — has unwound, so the catch
                    // owns abandonment authority: it can no longer publish a real store
                    // result. Acknowledge and account any abandoned work so no submitter
                    // hangs and the balance holds, then surface the panic while
                    // preserving any Complete and periodic errors already seen.
                    let authority = AbandonmentAuthority::assume_worker_abandoned();
                    worker_shared.reconcile_abandoned(&authority);
                    let inner = worker_shared.inner.lock().unwrap();
                    StatusSettlement {
                        diagnostics: inner.diagnostics.clone(),
                        settlement_error: inner.settlement_error.clone(),
                        fallback_error: inner.fallback_error.clone(),
                        periodic_error: inner.periodic_error.clone(),
                        worker_error: Some(STATUS_WORKER_PANIC.to_string()),
                    }
                })
            })
            .map_err(|err| {
                TranscriptPumpError::new(format!("spawn transcript pump status writer: {err}"))
            })?;
        Ok(Self {
            shared,
            wake: Some(wake_tx),
            join: Some(join),
        })
    }

    fn wake(&self) {
        if let Some(wake) = &self.wake {
            let _ = wake.try_send(());
        }
    }

    /// Enqueue a required status into the shared deque and return the submitter's
    /// blocking waiter, or a typed shutdown error accounted as disconnected. This does
    /// NOT wake the worker: `submit_required` wakes immediately afterwards, while a test
    /// may withhold the wake to drive wake disconnection around a blocked submitter.
    fn enqueue_required(
        &self,
        status: PumpStatus,
    ) -> Result<RequiredAckWaiter, TranscriptPumpError> {
        let (resolver, waiter) = new_ack();
        let mut inner = self.shared.inner.lock().unwrap();
        inner.diagnostics.submitted += 1;
        if inner.shutdown {
            inner.diagnostics.disconnected += 1;
            return Err(TranscriptPumpError::new(
                "persist pump status: status coordinator already shut down",
            ));
        }
        let id = RequiredCommandId(inner.next_command_id);
        inner.next_command_id += 1;
        inner.required.push_back(RequiredCommand {
            id,
            status,
            resolver,
            lifecycle: QueuedLifecycle::Queued,
        });
        Ok(waiter)
    }

    /// Submit a required status and block until the worker acknowledges its
    /// persistence. A write failure or a worker that already shut down is a typed
    /// terminal infrastructure failure, because the durable diagnostic must be
    /// independently observable.
    fn submit_required(&self, status: PumpStatus) -> Result<(), TranscriptPumpError> {
        let waiter = self.enqueue_required(status)?;
        self.wake();
        // Block on the idempotent one-shot until the worker or an authorized
        // reconciler fixes this command's exact result. A published store
        // result or an authorized termination boundary resolves the waiter; a
        // live `Pending` store frame that never returns can still block it.
        match waiter.wait() {
            Ok(()) => Ok(()),
            Err(err) => Err(TranscriptPumpError::new(err)),
        }
    }

    /// Submit a best-effort periodic snapshot. It never blocks canonical capture:
    /// a newer snapshot replaces an older pending one (counted coalesced), and once
    /// terminal sealing has begun the snapshot is dropped rather than written.
    fn submit_periodic(&self, status: PumpStatus) {
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.diagnostics.submitted += 1;
            if inner.shutdown {
                inner.diagnostics.disconnected += 1;
                return;
            }
            if inner.sealed {
                inner.diagnostics.dropped += 1;
                return;
            }
            if inner.periodic.replace(status).is_some() {
                inner.diagnostics.coalesced += 1;
            }
        }
        self.wake();
    }

    /// The coordinator's current transport accounting.
    #[cfg(test)]
    fn diagnostics(&self) -> StatusTransportDiagnostics {
        self.shared.diagnostics()
    }

    /// Test-only proof that every slot and queue is empty after settlement.
    #[cfg(test)]
    fn is_quiescent(&self) -> bool {
        self.shared.is_quiescent()
    }

    /// Seal the coordinator, drain pending work, write the terminal status last, and
    /// join the worker. Returns the balanced diagnostics and any terminal-settlement,
    /// fallback, or worker failure.
    fn finish(&mut self, spec: TerminalStatusSpec) -> StatusSettlement {
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.sealed = true;
            inner.terminal = Some(spec);
        }
        self.wake();
        // Probe the pre-join boundary outside both locks: the terminal is queued and the
        // worker may still be inside a live store call. A blocked or unauthorized
        // reconciler here must not synthesize a result before the join.
        self.shared.emit_finish_before_join();
        let mut settlement = match self.join.take() {
            Some(join) => join.join().unwrap_or_else(|_| {
                // The worker panicked while persisting a status. Its terminal write
                // may not have landed; surface it without losing the diagnostics.
                StatusSettlement {
                    diagnostics: self.shared.diagnostics(),
                    worker_error: Some(STATUS_WORKER_PANIC.to_string()),
                    ..StatusSettlement::default()
                }
            }),
            None => StatusSettlement {
                diagnostics: self.shared.diagnostics(),
                ..StatusSettlement::default()
            },
        };
        // The worker join has completed, so `finish` owns abandonment authority: the
        // worker can no longer publish a real store result.
        let authority = AbandonmentAuthority::assume_worker_abandoned();
        // If the worker exited (panicked or disconnected) before processing the
        // terminal, its spec lingers unprocessed. Account it as a disconnected
        // submission and reconcile any other abandoned work, so the returned
        // diagnostics include every submission and remain balanced — never a stale
        // pre-finish snapshot with an uncounted terminal pending.
        {
            let mut inner = self.shared.inner.lock().unwrap();
            if inner.terminal.take().is_some() {
                inner.diagnostics.submitted += 1;
                inner.diagnostics.disconnected += 1;
            }
        }
        self.shared.reconcile_abandoned(&authority);
        // Drop the wake sender and mark shutdown so any later submission is accounted
        // as disconnected rather than silently ignored.
        self.wake = None;
        self.shared.inner.lock().unwrap().shutdown = true;
        // Refresh the returned diagnostics so they reflect all reconciliation.
        settlement.diagnostics = self.shared.diagnostics();
        if settlement.periodic_error.is_none() {
            settlement.periodic_error = self.shared.periodic_error();
        }
        settlement
    }
}

impl Drop for StatusCoordinator {
    fn drop(&mut self) {
        // On an unwind that skipped `finish`, end the worker so no periodic write lands
        // after the caller stops using the coordinator. `drop` mirrors the authoritative
        // part of `finish`: remove the wake sender so the worker's receive disconnects,
        // join the worker, and only THEN — with the store frame provably gone — create
        // abandonment authority and reconcile any residual work idempotently. Neither
        // path may synthesize a `Pending` result before the join proves it is safe.
        if let Some(join) = self.join.take() {
            self.wake = None;
            // Probe the post-disconnect, pre-join boundary outside both locks: the worker
            // may still be inside a live store call, and an unauthorized reconciler here
            // must not preempt its real result.
            self.shared.emit_drop_wake_disconnected_before_join();
            let _ = join.join();
            // The join completed, so `drop` now owns abandonment authority. Reconcile any
            // residual abandoned work once more (idempotent) and mark shutdown.
            let authority = AbandonmentAuthority::assume_worker_abandoned();
            self.shared.reconcile_abandoned(&authority);
            if let Ok(mut inner) = self.shared.inner.lock() {
                inner.shutdown = true;
            }
        }
    }
}

/// The coordinator worker loop. It drains all available work, then blocks on the
/// wake channel; a disconnected wake with no terminal settles the worker with the
/// diagnostics observed so far.
fn run_status_worker(
    shared: &Arc<SharedStatusState>,
    wake_rx: &Receiver<()>,
    store: &mut dyn StatusStore,
    first_fault: Option<&Arc<FirstFault>>,
) -> StatusSettlement {
    loop {
        loop {
            match shared.next_work() {
                Work::Required(accepted) => {
                    // The acknowledgement resolver already lives in the shared active
                    // record (installed atomically with the queue removal) and the
                    // status already carries its projected transport.
                    let AcceptedRequired { id, status } = accepted;
                    let result = store.write(&status);
                    // The first transition after the store returns publishes the exact
                    // raw result as `Observed`, before any classification, accounting,
                    // probe, or acknowledgement work.
                    shared.publish_observed(id, result);
                    // Settle from that observed truth: no abandonment authority is
                    // needed because the store already returned. This applies accounting
                    // once, resolves the one-shot outside the lock, and cleans up.
                    shared.settle_active(None);
                }
                Work::Periodic(mut status) => {
                    status.transport = shared.projected_write_diagnostics();
                    shared.begin_write(WriteKind::Periodic);
                    let result = store.write(&status);
                    shared.record_write(&result, WriteKind::Periodic);
                }
                Work::Terminal(spec) => {
                    return finalize_terminal(shared, store, spec, first_fault);
                }
                Work::Idle => break,
            }
        }
        // Probe the idle boundary outside both locks: the worker has drained all work and
        // is about to block on the wake channel, so a test can withhold a wake to drive
        // wake disconnection around a queued submitter.
        shared.emit_worker_before_wake_receive();
        if wake_rx.recv().is_err() {
            // The coordinator was dropped without a terminal (an unwind that skipped
            // finish). The worker is outside any store call and commits to loop exit,
            // so it owns abandonment authority: no store frame or owned real result
            // remains. Reconcile any abandoned work so no submitter hangs and the
            // balance holds.
            let authority = AbandonmentAuthority::assume_worker_abandoned();
            shared.reconcile_abandoned(&authority);
            return StatusSettlement {
                diagnostics: shared.diagnostics(),
                periodic_error: shared.periodic_error(),
                ..StatusSettlement::default()
            };
        }
    }
}

/// Construct and persist the terminal status. The pending periodic slot is already
/// drained (required-then-periodic-then-terminal ordering), so the embedded
/// diagnostics balance and prove no snapshot remained pending. The terminal write
/// — and any Failed fallback — is itself counted as a real submission, and the
/// persisted document projects its own write so the accounting is self-consistent.
fn finalize_terminal(
    shared: &Arc<SharedStatusState>,
    store: &mut dyn StatusStore,
    spec: TerminalStatusSpec,
    first_fault: Option<&Arc<FirstFault>>,
) -> StatusSettlement {
    // Bound every retained/returned error so a pathological message cannot bloat the
    // settlement (and, UTF-8-safe, cannot split a multibyte boundary).
    let settlement_error = match write_accounted_terminal(shared, store, &spec, WriteKind::Terminal)
    {
        Ok(()) => {
            return StatusSettlement {
                diagnostics: shared.diagnostics(),
                periodic_error: shared.periodic_error(),
                ..StatusSettlement::default()
            };
        }
        Err(err) => bound_error(&err),
    };
    // Record the Complete error in shared state BEFORE the fallback, so a fallback
    // that itself panics still surfaces the original Complete error.
    shared.set_settlement_error(&settlement_error);
    // Publish the terminal-settlement failure to the first-fault latch BEFORE the
    // fallback, so supervision reacts even if the Failed fallback write blocks. A
    // capture fault already published first wins; this is a no-op then.
    if let Some(latch) = first_fault {
        latch.publish(&TranscriptPumpError::new(settlement_error.clone()));
    }
    // A Complete that could not be persisted attempts exactly one Failed fallback,
    // itself accounted as a real submission.
    if spec.state == PumpState::Complete {
        let fallback = spec.as_failed(&settlement_error);
        // The fallback write records its own failure into the distinct `fallback_error`
        // (via `record_write`), so a fallback that returns Err — or panics, which
        // `reconcile_abandoned` then attributes to the fallback — is never folded into
        // the generic terminal or worker error.
        let _ = write_accounted_terminal(shared, store, &fallback, WriteKind::Fallback);
        StatusSettlement {
            diagnostics: shared.diagnostics(),
            settlement_error: Some(settlement_error),
            fallback_error: shared.fallback_error(),
            periodic_error: shared.periodic_error(),
            worker_error: None,
        }
    } else {
        StatusSettlement {
            diagnostics: shared.diagnostics(),
            settlement_error: Some(settlement_error),
            periodic_error: shared.periodic_error(),
            ..StatusSettlement::default()
        }
    }
}

/// Account a terminal (or fallback) status as a real submission, persist it with a
/// self-consistent projected accounting, and record its actual persistence result.
fn write_accounted_terminal(
    shared: &Arc<SharedStatusState>,
    store: &mut dyn StatusStore,
    spec: &TerminalStatusSpec,
    kind: WriteKind,
) -> Result<(), String> {
    // Count this terminal write as a submission and project it as written so the
    // persisted document balances including itself. The `kind` (Terminal or the
    // Complete-to-Failed Fallback) marks the active write so an unwind mid-write is
    // attributed to the right category.
    let (projected, periodic_error) = {
        let mut inner = shared.inner.lock().unwrap();
        inner.diagnostics.submitted += 1;
        inner.in_flight = Some(kind);
        let mut projected = inner.diagnostics.clone();
        projected.written += 1;
        (projected, inner.periodic_error.clone())
    };
    let result = store.write(&spec.build(projected, periodic_error.as_deref()));
    shared.record_write(&result, kind);
    result
}

/// The thread names whose panics are caught and reported through durable status
/// rather than the default hook: the capture pump and its status worker. Both are
/// recovered — the status worker publishes to the first-fault latch and reconciles
/// its work — so their panics must never block writing a saturated stderr first.
const PUMP_THREAD_NAMES: [&str; 2] = ["transcript-pump", "transcript-pump-status"];

/// Install, once per process, a panic hook that suppresses the default hook's
/// blocking stderr write for transcript-pump threads. A pump or status-worker panic
/// is caught and reported through durable status instead, so a saturated stderr can
/// never block panic recovery. Non-pump panics keep the previous hook's behavior.
/// This is a single process-wide install, not a racy per-thread swap of the hook.
fn ensure_pump_panic_hook() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let is_pump = std::thread::current()
                .name()
                .is_some_and(|name| PUMP_THREAD_NAMES.contains(&name));
            if !is_pump {
                previous(info);
            }
        }));
    });
}

/// A running pump on its own thread. The supervisor polls `try_terminal` while
/// the coder is alive and calls `wait_terminal` once the coder exits, so a pump
/// failure is observed promptly rather than only after the coder finishes. It also
/// exposes `first_fault_observed` so supervision can react to the immutable first
/// fault BEFORE the pump's terminal outcome — which a blocked status store can
/// delay — is available.
pub struct PumpHandle {
    terminal: Receiver<Result<PumpSummary, TranscriptPumpError>>,
    first_fault: Arc<FirstFault>,
    counters: Arc<SharedCounters>,
    join: Option<JoinHandle<()>>,
}

impl PumpHandle {
    /// Whether the pump published its immutable first infrastructure fault. This is
    /// set before terminal status settlement, so supervision can terminate and reap
    /// the coder without waiting for a delayed or blocked terminal status write.
    pub fn first_fault_observed(&self) -> bool {
        self.first_fault.observed()
    }

    /// Whether the captured Claude stream still proves that no model or tool
    /// activity occurred. Partial, malformed, and unknown records fail closed.
    pub fn claude_no_progress_retry_is_safe(&self) -> bool {
        self.counters.claude_no_progress_retry_is_safe()
    }

    /// Whether a complete malformed, unknown, model, or tool record permanently
    /// disabled no-progress recovery for this launch.
    pub fn claude_progress_observed(&self) -> bool {
        self.counters.claude_progress_observed()
    }

    /// Return transcript-derived diagnostics only when Codex has remained in a
    /// response-wait phase for at least `timeout`. Active and unknown tool phases
    /// return `None` and therefore cannot be interrupted by liveness recovery.
    pub fn codex_response_idle(&self, timeout: Duration) -> Option<ProviderLivenessDiagnostics> {
        self.counters.codex_response_idle(timeout)
    }

    /// The pump's terminal outcome if it has finished, or `None` while it is
    /// still draining. A pump thread that vanished without reporting (a panic
    /// that escaped the guard) surfaces as a typed failure rather than a hang.
    pub fn try_terminal(&mut self) -> Option<Result<PumpSummary, TranscriptPumpError>> {
        match self.terminal.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(TranscriptPumpError::new(
                "transcript pump thread vanished",
            ))),
        }
    }

    /// Block until the pump reports its terminal outcome.
    pub fn wait_terminal(&mut self) -> Result<PumpSummary, TranscriptPumpError> {
        self.terminal
            .recv()
            .unwrap_or_else(|_| Err(TranscriptPumpError::new("transcript pump thread vanished")))
    }

    /// Join the pump thread, releasing its resources.
    pub fn join(&mut self) {
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn a pump on its own thread. The drain owns the status coordinator across a
/// caught capture panic and publishes the first fault to the shared latch before
/// terminal settlement, so a crashed or blocked pump can never silently stop
/// capture — or hide the fault from supervision — while the coder keeps running.
/// The pump thread is named so a process-wide hook can keep its panic off the
/// blocking default stderr path.
pub fn spawn_pump<R>(
    reader: R,
    transcript_path: PathBuf,
    status_path: Option<PathBuf>,
    preview: &'static dyn PreviewSink,
    config: TranscriptPumpConfig,
) -> Result<PumpHandle, TranscriptPumpError>
where
    R: Read + Send + 'static,
{
    let store = status_path.as_deref().map(file_status_store);
    spawn_pump_with_store(reader, transcript_path, store, preview, config)
}

/// Spawn a pump against an explicit [`StatusStore`]. Production uses [`spawn_pump`];
/// tests inject a store to gate, fail, or delay status writes deterministically.
pub(crate) fn spawn_pump_with_store<R>(
    reader: R,
    transcript_path: PathBuf,
    store: Option<Box<dyn StatusStore>>,
    preview: &'static dyn PreviewSink,
    config: TranscriptPumpConfig,
) -> Result<PumpHandle, TranscriptPumpError>
where
    R: Read + Send + 'static,
{
    ensure_pump_panic_hook();
    let (tx, rx) = sync_channel(1);
    let first_fault = Arc::new(FirstFault::default());
    let first_fault_for_thread = Arc::clone(&first_fault);
    let counters = Arc::new(SharedCounters::default());
    let counters_for_thread = Arc::clone(&counters);
    let join = std::thread::Builder::new()
        .name("transcript-pump".to_string())
        .spawn(move || {
            // The drain catches capture panics internally and settles through the
            // coordinator; this outer catch is a backstop for any residual panic. It
            // still publishes the first fault so supervision never waits for a pump
            // that vanished.
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                drain_with_first_fault(
                    reader,
                    &transcript_path,
                    store,
                    preview,
                    &config,
                    &counters_for_thread,
                    Some(Arc::clone(&first_fault_for_thread)),
                )
            }))
            .unwrap_or_else(|_| {
                let err = TranscriptPumpError::new("transcript pump panicked");
                first_fault_for_thread.publish(&err);
                Err(err)
            });
            let _ = tx.send(outcome);
        })
        .map_err(|err| TranscriptPumpError::new(format!("spawn transcript pump thread: {err}")))?;
    Ok(PumpHandle {
        terminal: rx,
        first_fault,
        counters,
        join: Some(join),
    })
}

/// The process-wide console preview sink. For this landing it synchronously
/// declines every preview and counts it as dropped: it spawns no renderer and
/// writes preview bytes to no descriptor.
///
/// Live previews are deferred, not merely disabled for redirected output.
/// Mirroring previews into a redirected (non-terminal) stderr is the flood that
/// first stalled Fluent. Writing them to the terminal is no safer here: even a
/// nonblocking write to an independent `/dev/tty` consumes the terminal's
/// remaining queue capacity, so the very next blocking control-plane write to
/// fd 2 could stall on the space the preview just took. An independent file
/// description does not reserve capacity for fd 2. Until every Fluent-owned
/// stderr write moves behind one independently nonblocking console bus, the safe
/// contract is to decline previews; the canonical transcript already holds every
/// byte, and declining keeps drop accounting exact (`dropped_console == records`)
/// without ever touching Rust's process-global stderr lock.
pub fn console_preview_sink() -> &'static dyn PreviewSink {
    static SINK: ConsoleSink = ConsoleSink;
    &SINK
}

/// The production preview sink. It declines every preview so no preview transport
/// can ever backpressure capture or stall control-plane output.
struct ConsoleSink;

impl PreviewSink for ConsoleSink {
    fn deliver(&self, _preview: &[u8]) -> bool {
        false
    }
}

const MAX_DIAGNOSTIC_RECORD_BYTES: usize = 64 * 1024;

/// Retains a bounded copy of one record for best-effort provider diagnostics.
/// Oversized records remain canonical transcript evidence but are ignored by the
/// diagnostic parser, so parsing can never make capture memory unbounded.
#[derive(Default)]
struct DiagnosticLine {
    buf: Vec<u8>,
    oversized: bool,
}

impl DiagnosticLine {
    fn push(&mut self, byte: u8) {
        if self.buf.len() < MAX_DIAGNOSTIC_RECORD_BYTES {
            self.buf.push(byte);
        } else {
            self.oversized = true;
        }
    }

    fn take(&mut self) -> Vec<u8> {
        let record = if self.oversized {
            Vec::new()
        } else {
            std::mem::take(&mut self.buf)
        };
        self.buf.clear();
        self.oversized = false;
        record
    }

    fn as_slice(&self) -> &[u8] {
        if self.oversized { &[] } else { &self.buf }
    }
}

/// Accumulates one record's bytes up to a bound so an oversized record yields a
/// bounded, lossy preview with a truncation marker instead of an unbounded
/// console write. The full record is untouched in the canonical transcript.
struct PreviewLine {
    limit: usize,
    buf: Vec<u8>,
    truncated: bool,
    any: bool,
}

impl PreviewLine {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            buf: Vec::new(),
            truncated: false,
            any: false,
        }
    }

    fn push(&mut self, byte: u8) {
        self.any = true;
        if self.buf.len() < self.limit {
            self.buf.push(byte);
        } else {
            self.truncated = true;
        }
    }

    fn has_bytes(&self) -> bool {
        self.any
    }

    /// Render the accumulated record as a bounded preview and reset for the next
    /// record.
    ///
    /// The configured limit bounds the TOTAL rendered preview for EVERY value. A
    /// truncated preview reserves room for the marker, capping its payload at
    /// `limit - marker.len()`. When the limit is even smaller than the marker
    /// itself, only a bounded prefix of the marker is emitted, so the rendered
    /// bytes never exceed the limit for any configured value — including 0 and 1.
    fn render_and_reset(&mut self) -> Vec<u8> {
        let rendered = if self.truncated {
            if self.limit < TRUNCATION_MARKER.len() {
                TRUNCATION_MARKER[..self.limit].to_vec()
            } else {
                let payload_cap = self.limit - TRUNCATION_MARKER.len();
                let keep = payload_cap.min(self.buf.len());
                let mut bounded = self.buf[..keep].to_vec();
                bounded.extend_from_slice(TRUNCATION_MARKER);
                bounded
            }
        } else {
            // A non-truncated record is capped at `limit` bytes by `push`.
            self.buf.clone()
        };
        self.buf.clear();
        self.truncated = false;
        self.any = false;
        rendered
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Mutex;
    use std::sync::mpsc;

    #[test]
    fn prelaunch_failure_retains_primary_when_status_is_obstructed() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::create_dir(status_path_for(&transcript)).unwrap();
        let error =
            record_prelaunch_failure(&transcript, TranscriptPumpError::new("read evidence"));
        assert_eq!(error.message(), "read evidence");
        assert!(
            error
                .settlement_error()
                .unwrap()
                .contains("transcript-pump.json")
        );
        assert!(error.transport().unwrap().is_balanced());
        assert!(!transcript.exists());
    }

    /// An in-memory status store that records every status it is asked to persist.
    struct RecordingStore {
        writes: Arc<Mutex<Vec<PumpStatus>>>,
    }

    impl RecordingStore {
        fn new() -> (Self, Arc<Mutex<Vec<PumpStatus>>>) {
            let writes = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    writes: Arc::clone(&writes),
                },
                writes,
            )
        }
    }

    impl StatusStore for RecordingStore {
        fn write(&mut self, status: &PumpStatus) -> Result<(), String> {
            self.writes.lock().unwrap().push(status.clone());
            Ok(())
        }
    }

    /// A status store whose every write blocks on a gate until the test releases it,
    /// so the coalescing slot can be observed deterministically. Each write announces
    /// itself on `entered` before blocking on `gate`.
    struct GatedStore {
        writes: Arc<Mutex<Vec<PumpStatus>>>,
        entered: mpsc::Sender<PumpState>,
        gate: mpsc::Receiver<()>,
    }

    impl StatusStore for GatedStore {
        fn write(&mut self, status: &PumpStatus) -> Result<(), String> {
            let _ = self.entered.send(status.state);
            let _ = self.gate.recv();
            self.writes.lock().unwrap().push(status.clone());
            Ok(())
        }
    }

    /// A status store that fails writes of a chosen terminal state, recording each
    /// attempt so a fallback can be observed.
    struct FailStateStore {
        attempts: Arc<Mutex<Vec<(PumpState, bool)>>>,
        fail: Vec<PumpState>,
    }

    impl StatusStore for FailStateStore {
        fn write(&mut self, status: &PumpStatus) -> Result<(), String> {
            let ok = !self.fail.contains(&status.state);
            self.attempts.lock().unwrap().push((status.state, ok));
            if ok {
                Ok(())
            } else {
                Err(format!(
                    "persist pump status: simulated {:?} write failure",
                    status.state
                ))
            }
        }
    }

    fn running_status() -> PumpStatus {
        build_status(
            PumpState::Running,
            0,
            &PumpSummary::default(),
            None,
            None,
            StatusTransportDiagnostics::default(),
        )
    }

    /// Build a bare shared coordinator state with no worker thread, so each active and
    /// queued transition can be driven and probed deterministically. Tests reach the
    /// coordinator's private state directly (a child module sees its parent's private
    /// items), which is exactly the deterministic-probe surface the packet asks for.
    fn test_shared() -> Arc<SharedStatusState> {
        Arc::new(SharedStatusState {
            inner: Mutex::new(CoordinatorInner::new()),
            hooks: TestHooks::default(),
        })
    }

    /// Enqueue a required command directly (as `submit_required` would, minus the
    /// blocking wait), returning its id and the submitter's waiter.
    fn test_enqueue_required(
        shared: &SharedStatusState,
        status: PumpStatus,
    ) -> (RequiredCommandId, RequiredAckWaiter) {
        let (resolver, waiter) = new_ack();
        let mut inner = shared.inner.lock().unwrap();
        inner.diagnostics.submitted += 1;
        let id = RequiredCommandId(inner.next_command_id);
        inner.next_command_id += 1;
        inner.required.push_back(RequiredCommand {
            id,
            status,
            resolver,
            lifecycle: QueuedLifecycle::Queued,
        });
        (id, waiter)
    }

    /// Clone every shared `Resolving` entry's id and resolver from the single required
    /// deque, so a test can probe one-shot observability without holding the coordinator
    /// lock.
    fn test_resolving_resolvers(
        shared: &SharedStatusState,
    ) -> Vec<(RequiredCommandId, RequiredAckResolver)> {
        shared
            .inner
            .lock()
            .unwrap()
            .required
            .iter()
            .filter(|e| matches!(e.lifecycle, QueuedLifecycle::Resolving { .. }))
            .map(|e| (e.id, e.resolver.clone()))
            .collect()
    }

    /// Enqueue a required command already retired to `Resolving` with a chosen static
    /// immutable error, applying its disconnected accounting once — the state authorized
    /// retirement produces — so a test can prepare distinct shared resolving identities
    /// and detect any result swap between them. Returns its id and the submitter's
    /// waiter.
    fn test_enqueue_resolving(
        shared: &SharedStatusState,
        status: PumpStatus,
        immutable_error: &'static str,
    ) -> (RequiredCommandId, RequiredAckWaiter) {
        let (resolver, waiter) = new_ack();
        let mut inner = shared.inner.lock().unwrap();
        inner.diagnostics.submitted += 1;
        inner.diagnostics.disconnected += 1;
        let id = RequiredCommandId(inner.next_command_id);
        inner.next_command_id += 1;
        inner.required.push_back(RequiredCommand {
            id,
            status,
            resolver,
            lifecycle: QueuedLifecycle::Resolving { immutable_error },
        });
        (id, waiter)
    }

    /// Install a `BeforeQueuedRetirementCommit` probe.
    fn set_before_queued_retirement_commit(
        shared: &SharedStatusState,
        hook: impl Fn() + Send + Sync + 'static,
    ) {
        *shared.hooks.before_queued_retirement_commit.lock().unwrap() = Some(Arc::new(hook));
    }

    /// Install a `QueuedRetiringBeforeCleanup` probe.
    fn set_queued_retiring_before_cleanup(
        shared: &SharedStatusState,
        hook: impl Fn(RequiredCommandId) + Send + Sync + 'static,
    ) {
        *shared.hooks.queued_retiring_before_cleanup.lock().unwrap() = Some(Arc::new(hook));
    }

    /// Install an `ActivePrepared` probe.
    fn set_active_prepared(
        shared: &SharedStatusState,
        hook: impl Fn(RequiredCommandId) + Send + Sync + 'static,
    ) {
        *shared.hooks.active_prepared.lock().unwrap() = Some(Arc::new(hook));
    }

    /// Install an `ActiveRetiringBeforeCleanup` probe.
    fn set_active_retiring_before_cleanup(
        shared: &SharedStatusState,
        hook: impl Fn(RequiredCommandId) + Send + Sync + 'static,
    ) {
        *shared.hooks.active_retiring_before_cleanup.lock().unwrap() = Some(Arc::new(hook));
    }

    /// Install a `FinishBeforeJoin` probe.
    fn set_finish_before_join(shared: &SharedStatusState, hook: impl Fn() + Send + Sync + 'static) {
        *shared.hooks.finish_before_join.lock().unwrap() = Some(Arc::new(hook));
    }

    /// Install a `DropWakeDisconnectedBeforeJoin` probe.
    fn set_drop_wake_disconnected_before_join(
        shared: &SharedStatusState,
        hook: impl Fn() + Send + Sync + 'static,
    ) {
        *shared
            .hooks
            .drop_wake_disconnected_before_join
            .lock()
            .unwrap() = Some(Arc::new(hook));
    }

    /// Clone the current active record's resolver, if any, with both locks released.
    fn test_active_resolver(shared: &SharedStatusState) -> Option<RequiredAckResolver> {
        shared
            .inner
            .lock()
            .unwrap()
            .active_required
            .as_ref()
            .map(|a| a.resolver.clone())
    }

    #[test]
    fn required_acceptance_atomically_moves_ack_from_queue_to_active() {
        // Atomic acceptance B1: selecting the next required write removes the exact
        // command from the FIFO and installs its acknowledgement resolver as an active
        // record in Accepted/Pending as one lock-held transition — never an interval
        // where the command is neither queued nor active.
        let shared = test_shared();
        let (id, waiter) = test_enqueue_required(&shared, running_status());

        // Before acceptance: queued, not active.
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(inner.required.len(), 1, "the command is queued");
            assert!(inner.active_required.is_none(), "nothing is active yet");
        }

        let accepted = shared
            .accept_next_required()
            .expect("the queued required command is accepted");
        assert_eq!(accepted.id, id, "the exact queued command is accepted");

        // After acceptance: removed from the FIFO and owned by the active record in
        // Accepted/Pending, with the SAME identity — the two mutations are one step.
        {
            let inner = shared.inner.lock().unwrap();
            assert!(inner.required.is_empty(), "the command left the FIFO");
            let active = inner
                .active_required
                .as_ref()
                .expect("its resolver is now the active record");
            assert_eq!(active.id, id, "the exact command is active");
            assert!(
                matches!(active.phase, ActivePhase::Pending),
                "the accepted write is Accepted/Pending"
            );
            // The projected transport was stamped under the same lock.
            assert_eq!(
                accepted.status.transport.written, 1,
                "the accepted status carries a self-consistent projected accounting"
            );
        }

        // The resolver moved (not recreated): resolving the active record's resolver
        // delivers to the original submitter's exact waiter.
        let resolver = test_active_resolver(&shared).expect("an active resolver");
        resolver.resolve_once(Ok(()));
        assert_eq!(
            waiter.wait(),
            Ok(()),
            "the moved resolver wakes the exact submitter"
        );

        // A second selection while a write is already active is refused: the single
        // worker never selects a second required write.
        assert!(
            shared.accept_next_required().is_none(),
            "no second required is selected while one is active"
        );
    }

    #[test]
    fn pending_accepted_write_unwind_resolves_once_as_write_failure() {
        // Atomic acceptance B2: a Pending accepted write settled with abandonment
        // authority fixes the caller's one-shot to the bounded accepted-write
        // worker-unwind diagnostic, counts it once as a write failure (never
        // disconnected or written), and invents no store result. Repeating the settle
        // neither re-accounts nor re-resolves it.
        let shared = test_shared();
        let (_id, waiter) = test_enqueue_required(&shared, running_status());
        shared.accept_next_required().expect("accepted");

        // The worker provably unwound (abandonment began right after acceptance).
        let authority = AbandonmentAuthority::assume_worker_abandoned();
        shared.settle_active(Some(&authority));

        let result = waiter.wait();
        let err = result.expect_err("a Pending unwind resolves as a failure");
        assert!(
            err.contains("panicked"),
            "the caller's one-shot carries the accepted-write worker-unwind diagnostic: {err}"
        );

        let d = shared.diagnostics();
        assert_eq!(
            d.write_failures, 1,
            "counted once as a write failure: {d:?}"
        );
        assert_eq!(d.written, 0, "no store result was invented: {d:?}");
        assert_eq!(d.disconnected, 0, "not disconnected: {d:?}");
        assert!(
            d.last_error
                .as_deref()
                .is_some_and(|e| e.contains("panicked")),
            "the unwind is the transport last_error: {d:?}"
        );
        assert!(
            shared.inner.lock().unwrap().active_required.is_none(),
            "the record is cleaned after its one-shot is observable"
        );

        // Idempotent: a repeated authorized settle neither re-accounts nor re-resolves.
        shared.settle_active(Some(&authority));
        let d = shared.diagnostics();
        assert_eq!(
            d.write_failures, 1,
            "still exactly one write failure: {d:?}"
        );
    }

    #[test]
    fn live_worker_pending_result_cannot_be_synthesized_by_direct_reconcile() {
        // Atomic acceptance B2: while a live gated worker is inside the store call
        // (Accepted/Pending), a direct reconciler WITHOUT abandonment authority leaves
        // the record owned and unresolved and changes no accounting. The worker's
        // eventual actual store result then wins — invented unwind never preempts it.
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel();
        let store = GatedStore {
            writes: Arc::clone(&writes),
            entered: entered_tx,
            gate: gate_rx,
        };
        let mut coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();
        let shared = Arc::clone(&coordinator.shared);

        // Submit a required status on another thread (so it wakes the worker); it
        // blocks on its one-shot until the store returns.
        std::thread::scope(|s| {
            let submitter = s.spawn(|| coordinator.submit_required(running_status()));

            // The worker accepted it and is now blocked inside the store call: Pending.
            assert_eq!(entered_rx.recv().unwrap(), PumpState::Running);
            assert!(
                matches!(
                    shared
                        .inner
                        .lock()
                        .unwrap()
                        .active_required
                        .as_ref()
                        .map(|a| &a.phase),
                    Some(ActivePhase::Pending)
                ),
                "the accepted write is Pending while the store call is in flight"
            );

            // Repeated direct reconcilers without authority — through both the whole
            // settle path and the raw prepare step — leave Pending owned and unresolved
            // and change no accounting. No authority means no synthesis at any layer.
            shared.settle_active(None);
            shared.settle_active(None);
            assert!(
                shared.prepare_active(None).is_none(),
                "prepare without authority never synthesizes a Pending result"
            );
            let resolver = test_active_resolver(&shared).expect("still active and owned");
            assert!(
                !resolver.is_observable(),
                "the live worker's one-shot is not resolved by an unauthorized reconcile"
            );
            {
                let inner = shared.inner.lock().unwrap();
                assert!(
                    matches!(
                        inner.active_required.as_ref().map(|a| &a.phase),
                        Some(ActivePhase::Pending)
                    ),
                    "the record stays Pending, never advanced by an unauthorized reconcile"
                );
            }
            let d = shared.diagnostics();
            assert_eq!(
                (d.written, d.write_failures, d.disconnected),
                (0, 0, 0),
                "an unauthorized reconcile changes no accounting: {d:?}"
            );
            assert!(
                writes.lock().unwrap().is_empty(),
                "the gated store has not yet recorded anything"
            );

            // Release the store: the worker's actual success is the one exact result.
            for _ in 0..4 {
                let _ = gate_tx.send(());
            }
            submitter
                .join()
                .unwrap()
                .expect("the exact real store success wins, not an invented unwind");
        });

        // The store actually recorded the exact required status; the real write is what
        // reached durability.
        assert_eq!(
            writes.lock().unwrap().first().map(|s| s.state),
            Some(PumpState::Running),
            "the real required status was persisted by the worker"
        );

        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));
        // Exactly the required write and the terminal write are written; nothing was
        // synthesized as a failure or a disconnect.
        assert_eq!(
            settlement.diagnostics.written, 2,
            "exactly the real required and terminal writes landed: {:?}",
            settlement.diagnostics
        );
        assert_eq!(
            (
                settlement.diagnostics.write_failures,
                settlement.diagnostics.disconnected
            ),
            (0, 0),
            "no failure or disconnect was synthesized: {:?}",
            settlement.diagnostics
        );
        assert!(
            settlement.diagnostics.is_balanced(),
            "diagnostics balance independently: {:?}",
            settlement.diagnostics
        );
        assert!(coordinator.is_quiescent(), "settlement is quiescent");
    }

    #[test]
    fn live_pending_write_overlapping_finish_keeps_real_result() {
        // Active boundary recovery B2: `finish` overlapping a live pending required store
        // call leaves the pending result owned until the real store result returns. At
        // the FinishBeforeJoin boundary an unauthorized reconciler cannot preempt it;
        // only the worker's real result wins, and post-join authority reconciles the rest.
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel();
        let store = GatedStore {
            writes: Arc::clone(&writes),
            entered: entered_tx,
            gate: gate_rx,
        };
        let mut coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();
        let shared = Arc::clone(&coordinator.shared);

        // Enqueue and wake; the worker accepts and blocks inside the gated store: Pending.
        let waiter = coordinator
            .enqueue_required(running_status())
            .expect("enqueued");
        coordinator.wake();
        assert_eq!(entered_rx.recv().unwrap(), PumpState::Running);

        // The submitter blocks on its standalone waiter; it does not borrow the
        // coordinator, so `finish` can still take &mut self.
        let submitter = std::thread::spawn(move || waiter.wait());

        // FinishBeforeJoin: assert the live write is still Pending, prove an unauthorized
        // reconciler has no effect, then release the store so the real result wins.
        {
            let hook_shared = Arc::clone(&shared);
            let gate_tx = gate_tx.clone();
            set_finish_before_join(&shared, move || {
                assert!(
                    matches!(
                        hook_shared
                            .inner
                            .lock()
                            .unwrap()
                            .active_required
                            .as_ref()
                            .map(|a| &a.phase),
                        Some(ActivePhase::Pending)
                    ),
                    "the accepted write is still Pending at FinishBeforeJoin"
                );
                hook_shared.settle_active(None);
                assert!(
                    test_active_resolver(&hook_shared).is_some_and(|r| !r.is_observable()),
                    "an unauthorized reconciler cannot resolve the live pending write"
                );
                for _ in 0..8 {
                    let _ = gate_tx.send(());
                }
            });
        }

        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));
        assert_eq!(
            submitter.join().unwrap(),
            Ok(()),
            "the real store result wins"
        );
        assert_eq!(
            writes.lock().unwrap().first().map(|s| s.state),
            Some(PumpState::Running),
            "the real required status was persisted"
        );
        assert_eq!(
            settlement.diagnostics.written, 2,
            "the real required and terminal writes landed: {:?}",
            settlement.diagnostics
        );
        assert_eq!(
            (
                settlement.diagnostics.write_failures,
                settlement.diagnostics.disconnected
            ),
            (0, 0),
            "no synthesized failure or disconnect: {:?}",
            settlement.diagnostics
        );
        assert!(
            settlement.diagnostics.is_balanced(),
            "diagnostics balance independently: {:?}",
            settlement.diagnostics
        );
        assert!(coordinator.is_quiescent(), "quiescent after finish");
    }

    #[test]
    fn live_pending_write_overlapping_drop_keeps_real_result() {
        // Active boundary recovery B2 (the active wake-disconnect case): dropping the
        // coordinator removes the wake sender while a real required store call is still
        // gated. At DropWakeDisconnectedBeforeJoin an unauthorized reconciler has no
        // effect; only the worker's real result wins, and post-join authority reconciles
        // residual state.
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel();
        let store = GatedStore {
            writes: Arc::clone(&writes),
            entered: entered_tx,
            gate: gate_rx,
        };
        let mut coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();
        let shared = Arc::clone(&coordinator.shared);

        let waiter = coordinator
            .enqueue_required(running_status())
            .expect("enqueued");
        coordinator.wake();
        assert_eq!(entered_rx.recv().unwrap(), PumpState::Running);

        let submitter = std::thread::spawn(move || waiter.wait());

        // DropWakeDisconnectedBeforeJoin: the wake sender is already removed, but the
        // worker is still inside the gated required store call (Pending). Prove an
        // unauthorized reconciler has no effect, then release the store.
        {
            let hook_shared = Arc::clone(&shared);
            let gate_tx = gate_tx.clone();
            set_drop_wake_disconnected_before_join(&shared, move || {
                assert!(
                    matches!(
                        hook_shared
                            .inner
                            .lock()
                            .unwrap()
                            .active_required
                            .as_ref()
                            .map(|a| &a.phase),
                        Some(ActivePhase::Pending)
                    ),
                    "the accepted write is still Pending at DropWakeDisconnectedBeforeJoin"
                );
                hook_shared.settle_active(None);
                assert!(
                    test_active_resolver(&hook_shared).is_some_and(|r| !r.is_observable()),
                    "an unauthorized reconciler cannot resolve the live pending write"
                );
                for _ in 0..4 {
                    let _ = gate_tx.send(());
                }
            });
        }

        drop(coordinator);
        assert_eq!(
            submitter.join().unwrap(),
            Ok(()),
            "the real store result wins before post-join authority"
        );
        assert_eq!(
            writes.lock().unwrap().first().map(|s| s.state),
            Some(PumpState::Running),
            "the real required status was persisted, not synthesized"
        );
        // After the drop join, residual state is reconciled and balanced. There is no
        // terminal in the drop path, so only the real required write landed.
        let d = shared.diagnostics();
        assert_eq!(
            d.written, 1,
            "exactly the real required write landed: {d:?}"
        );
        assert_eq!(
            (d.write_failures, d.disconnected),
            (0, 0),
            "nothing was synthesized: {d:?}"
        );
        assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
        assert!(shared.is_quiescent(), "quiescent after drop");
    }

    #[test]
    fn wake_disconnect_resolves_a_blocked_queued_submitter() {
        // Wake-disconnect proof B1: a never-woken queued submitter is resolved by the
        // worker's OWN ordinary wake-disconnect reconciliation, proven by joining the raw
        // worker handle directly — before coordinator Drop or any post-join reconciliation
        // could run. The idle probe is installed BEFORE the worker starts, and the worker
        // is held at that probe (a barrier) while the test enqueues without a wake, so the
        // proof uses synchronization rather than scheduling probability.
        let (store, writes) = RecordingStore::new();

        // Build the idle barrier and install it into the hooks BEFORE the worker thread
        // starts: the worker signals it is idle and then blocks until the test releases
        // it. Pre-installing the probe is what makes the worker unable to reach `recv`
        // before the hook exists.
        let (idle_tx, idle_rx) = mpsc::channel();
        let release = Arc::new(std::sync::Barrier::new(2));
        let hooks = TestHooks::default();
        {
            let release = Arc::clone(&release);
            *hooks.worker_before_wake_receive.lock().unwrap() = Some(Arc::new(move || {
                let _ = idle_tx.send(());
                release.wait();
            }));
        }
        let mut coordinator =
            StatusCoordinator::spawn_with_hooks(Box::new(store), None, hooks).unwrap();
        let shared = Arc::clone(&coordinator.shared);

        // Wait until the worker has drained all work and is held inside the idle probe,
        // before `recv`. Anything enqueued next can never be seen as work.
        idle_rx.recv().unwrap();

        // Enqueue a required command WITHOUT waking the idle worker.
        let waiter = coordinator
            .enqueue_required(running_status())
            .expect("enqueued");

        // Take and drop the wake sender while the coordinator remains alive, so the
        // worker's next `recv` observes disconnection.
        coordinator.wake = None;
        // Take the raw worker join handle out of the coordinator WITHOUT dropping the
        // coordinator, so the join below invokes neither coordinator Drop nor its
        // post-join reconciliation.
        let worker = coordinator
            .join
            .take()
            .expect("the raw worker handle is present");

        // Release the idle barrier: the worker enters `recv`, observes disconnection,
        // reconciles the never-woken queued submitter, and exits.
        release.wait();

        // Join the raw handle directly — only the worker path has run so far. Capture the
        // raw worker settlement so the ordinary wake-disconnect path is proven directly:
        // if the ordinary disconnected-receive branch panicked, the worker wrapper's catch
        // would perform fallback reconciliation and return `worker_error=Some(...)`, so the
        // per-field checks below would fail even though the caller was still resolved.
        let settlement = worker.join().expect("the worker joins cleanly");

        // Before consuming the waiter, prove the acknowledgement is ALREADY observable:
        // the worker's own wake-disconnect reconciliation resolved it, not Drop. Removing
        // that reconciliation makes this assertion fail here, without hanging.
        assert!(
            waiter.test_is_observable(),
            "the worker's wake-disconnect reconciliation resolved the caller before Drop"
        );

        // All four error fields are absent: the settlement came from the ordinary worker
        // path, not from a periodic write failure, a terminal settlement failure, a
        // Complete-to-Failed fallback, or the panic-catch worker fallback.
        assert!(
            settlement.periodic_error.is_none(),
            "no periodic write failed on the wake-disconnect path: {:?}",
            settlement.periodic_error
        );
        assert!(
            settlement.settlement_error.is_none(),
            "no terminal settlement failed on the wake-disconnect path: {:?}",
            settlement.settlement_error
        );
        assert!(
            settlement.fallback_error.is_none(),
            "no Complete-to-Failed fallback ran on the wake-disconnect path: {:?}",
            settlement.fallback_error
        );
        assert!(
            settlement.worker_error.is_none(),
            "the worker exited the ordinary disconnect path without a panic-catch fallback: {:?}",
            settlement.worker_error
        );

        // The raw worker settlement's own diagnostics equal the exact ordinary-disconnect
        // tuple, independently of the shared-diagnostics check below.
        let s = &settlement.diagnostics;
        assert_eq!(
            (
                s.submitted,
                s.written,
                s.coalesced,
                s.dropped,
                s.disconnected,
                s.write_failures,
            ),
            (1, 0, 0, 0, 1, 0),
            "the raw worker settlement carries the exact queued-disconnect tuple: {s:?}"
        );

        assert_eq!(
            waiter
                .wait()
                .expect_err("the queued submitter is resolved disconnected"),
            QUEUED_REQUIRED_DISCONNECT,
            "the exact immutable disconnected result reaches the blocked caller"
        );
        assert!(
            writes.lock().unwrap().is_empty(),
            "the never-woken command never reached the store"
        );

        // Exact diagnostics before Drop, balanced independently, and quiescent.
        let d = shared.diagnostics();
        assert_eq!(
            (
                d.submitted,
                d.written,
                d.coalesced,
                d.dropped,
                d.disconnected,
                d.write_failures,
            ),
            (1, 0, 0, 0, 1, 0),
            "the exact queued-disconnect diagnostic tuple: {d:?}"
        );
        assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
        assert!(
            shared.is_quiescent(),
            "shared state is quiescent after wake disconnection"
        );

        // Drop the still-alive coordinator last. Its join handle is already gone, so Drop
        // performs no reconciliation.
        drop(coordinator);
    }

    #[test]
    fn shutdown_refuses_required_acceptance() {
        // Shutdown acceptance B1: once the coordinator is shut down, acceptance refuses a
        // still-queued entry under the coordinator lock and leaves its identity and
        // resolver owned by the shared queue. Authorized reconciliation later delivers the
        // exact disconnected result, and the final diagnostics are the exact balanced
        // tuple with a quiescent shared state.
        let shared = test_shared();
        let (id, waiter) = test_enqueue_required(&shared, running_status());

        // Shut the coordinator down while the entry is still queued and shared.
        shared.inner.lock().unwrap().shutdown = true;

        // Acceptance refuses under the lock and moves nothing.
        assert!(
            shared.accept_next_required().is_none(),
            "acceptance refuses a queued entry after shutdown"
        );

        // The same identity, resolver, and unresolved waiter remain owned by the shared
        // queue; nothing became active.
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(
                inner.required.len(),
                1,
                "the entry stays in the shared deque"
            );
            let front = inner.required.front().expect("the entry is still owned");
            assert_eq!(front.id, id, "the same identity is owned by the queue");
            assert!(
                matches!(front.lifecycle, QueuedLifecycle::Queued),
                "shutdown does not itself retire the entry"
            );
            assert!(
                inner.active_required.is_none(),
                "nothing was moved to an active write"
            );
        }
        assert!(
            !waiter.test_is_observable(),
            "the queued waiter stays unresolved while it remains shared"
        );

        // Authorized reconciliation delivers the exact disconnected result to the caller.
        let authority = AbandonmentAuthority::assume_worker_abandoned();
        shared.reconcile_abandoned(&authority);
        assert_eq!(
            waiter
                .wait()
                .expect_err("the queued submitter is resolved disconnected"),
            QUEUED_REQUIRED_DISCONNECT,
            "authorized reconciliation delivers exactly the disconnected result"
        );

        let d = shared.diagnostics();
        assert_eq!(
            (
                d.submitted,
                d.written,
                d.coalesced,
                d.dropped,
                d.disconnected,
                d.write_failures,
            ),
            (1, 0, 0, 0, 1, 0),
            "the exact shutdown-disconnect diagnostic tuple: {d:?}"
        );
        assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
        assert!(
            shared.is_quiescent(),
            "shared state is quiescent after reconciliation"
        );
    }

    #[test]
    fn active_and_queued_required_writes_reconcile_distinctly() {
        // Atomic acceptance B2: with one Pending active write and a distinct queued
        // write, an unauthorized reconcile leaves both owned; an authorized reconcile
        // settles the active write from its own truth and disconnects the queued one,
        // each caller observing its exact own result with no diagnostic crossing.
        let shared = test_shared();
        let (_active_id, active_waiter) = test_enqueue_required(&shared, running_status());
        shared.accept_next_required().expect("active accepted");
        let (_queued_id, queued_waiter) = test_enqueue_required(&shared, running_status());

        // Unauthorized: active Pending stays owned, the queued command stays queued.
        shared.settle_active(None);
        shared.resolve_queued();
        {
            let inner = shared.inner.lock().unwrap();
            assert!(
                matches!(
                    inner.active_required.as_ref().map(|a| &a.phase),
                    Some(ActivePhase::Pending)
                ),
                "the active write is still Pending"
            );
            assert_eq!(
                inner.required.len(),
                1,
                "the queued command is still queued"
            );
            assert!(
                inner
                    .required
                    .iter()
                    .all(|e| matches!(e.lifecycle, QueuedLifecycle::Queued)),
                "an unauthorized reconcile never retires a queued command"
            );
        }

        // Authorized: the active write settles as an unwind failure; the queued write
        // is disconnected. The two callers observe distinct results.
        let authority = AbandonmentAuthority::assume_worker_abandoned();
        shared.reconcile_abandoned(&authority);

        let active_result = active_waiter
            .wait()
            .expect_err("active resolves to a failure");
        let queued_result = queued_waiter
            .wait()
            .expect_err("queued resolves disconnected");
        assert!(
            active_result.contains("before acknowledging an accepted required write"),
            "the active caller gets the accepted-write unwind diagnostic: {active_result}"
        );
        assert!(
            queued_result.contains("disconnected"),
            "the queued caller gets the disconnected diagnostic: {queued_result}"
        );
        assert_ne!(
            active_result, queued_result,
            "no diagnostic crosses between the active and queued callers"
        );

        let d = shared.diagnostics();
        assert_eq!(d.write_failures, 1, "one active write failure: {d:?}");
        assert_eq!(d.disconnected, 1, "one queued disconnect: {d:?}");
        assert!(shared.is_quiescent(), "settlement is quiescent: {d:?}");
    }

    #[test]
    fn observed_required_store_error_survives_unwind_before_ack() {
        // Observed store outcomes B1: once the store's exact error is published as
        // Observed (the first transition after return), a later worker unwind before
        // acknowledgement preserves that exact bounded error in the caller, the
        // transport last_error, and the write-failure accounting — a generic
        // worker-unwind diagnostic never replaces it.
        let shared = test_shared();
        let (_id, waiter) = test_enqueue_required(&shared, running_status());
        let accepted = shared.accept_next_required().expect("accepted");

        // The store returned this exact error; publish it as the first transition.
        shared.publish_observed(accepted.id, Err("exact store boom".to_string()));

        // The worker then unwinds before acknowledgement: reconcile with authority.
        let authority = AbandonmentAuthority::assume_worker_abandoned();
        shared.reconcile_abandoned(&authority);

        let err = waiter.wait().expect_err("the caller keeps the store error");
        assert_eq!(err, "exact store boom", "the exact observed error survives");
        assert!(
            !err.contains("panicked"),
            "a generic worker-unwind diagnostic never replaces the observed error: {err}"
        );
        let d = shared.diagnostics();
        assert_eq!(d.write_failures, 1, "one write failure: {d:?}");
        assert_eq!(
            d.last_error.as_deref(),
            Some("exact store boom"),
            "the transport last_error keeps the exact error: {d:?}"
        );
    }

    #[test]
    fn observed_required_success_survives_unwind_before_ack() {
        // Observed store outcomes B2: an observed success survives a later worker
        // unwind as success, counted as one written submission, not a write failure.
        let shared = test_shared();
        let (_id, waiter) = test_enqueue_required(&shared, running_status());
        let accepted = shared.accept_next_required().expect("accepted");
        shared.publish_observed(accepted.id, Ok(()));

        let authority = AbandonmentAuthority::assume_worker_abandoned();
        shared.reconcile_abandoned(&authority);

        assert_eq!(waiter.wait(), Ok(()), "observed success stays success");
        let d = shared.diagnostics();
        assert_eq!(d.written, 1, "counted once as written: {d:?}");
        assert_eq!(d.write_failures, 0, "no write failure: {d:?}");
    }

    #[test]
    fn observed_required_results_resolve_exactly_once() {
        // Observed store outcomes B2: the caller resolves from the observed result
        // exactly once and accounting is applied once, whether success or failure, even
        // under repeated settlement.
        for (observed, expect_written, expect_failures) in
            [(Ok(()), 1u64, 0u64), (Err("boom".to_string()), 0u64, 1u64)]
        {
            let shared = test_shared();
            let (_id, waiter) = test_enqueue_required(&shared, running_status());
            let accepted = shared.accept_next_required().expect("accepted");
            shared.publish_observed(accepted.id, observed.clone());

            // Normal worker settlement needs no authority: the store already returned.
            shared.settle_active(None);
            // Repeated settlement neither re-resolves nor re-accounts.
            shared.settle_active(None);
            let authority = AbandonmentAuthority::assume_worker_abandoned();
            shared.settle_active(Some(&authority));

            match &observed {
                Ok(()) => assert_eq!(waiter.wait(), Ok(()), "success resolves once"),
                Err(e) => assert_eq!(
                    waiter.wait().expect_err("failure resolves once"),
                    *e,
                    "the exact error resolves once"
                ),
            }
            let d = shared.diagnostics();
            assert_eq!(d.written, expect_written, "written once: {d:?}");
            assert_eq!(d.write_failures, expect_failures, "failure once: {d:?}");
            assert!(
                shared.inner.lock().unwrap().active_required.is_none(),
                "the record is cleaned after its result is observable"
            );
        }
    }

    #[test]
    fn resolved_active_record_remains_recoverable_until_matching_cleanup() {
        // Observed store outcomes B2: after accounting/resolution preparation the
        // active record stays recoverable until its one-shot is observable; resolving
        // it makes it Resolved/Retiring (still present); only matching cleanup removes
        // it, and a non-matching identity never clears it.
        let shared = test_shared();
        let (_id, waiter) = test_enqueue_required(&shared, running_status());
        let accepted = shared.accept_next_required().expect("accepted");
        let id = accepted.id;
        shared.publish_observed(id, Ok(()));

        // Prepare (Accepted/Prepared): accounting applied, record recoverable, one-shot
        // not yet observable.
        let (prepared_id, resolver, result) = shared.prepare_active(None).expect("prepared");
        assert_eq!(prepared_id, id);
        {
            let inner = shared.inner.lock().unwrap();
            let active = inner.active_required.as_ref().expect("still recoverable");
            assert!(matches!(active.phase, ActivePhase::Prepared { .. }));
        }
        assert!(
            !resolver.is_observable(),
            "Accepted/Prepared: the one-shot is not yet observable"
        );

        // Resolve the one-shot: the still-present record is now Resolved/Retiring.
        resolver.resolve_once(result);
        assert!(resolver.is_observable(), "the one-shot is now observable");
        assert!(
            shared.inner.lock().unwrap().active_required.is_some(),
            "Resolved/Retiring: the record is recoverable until matching cleanup"
        );

        // A non-matching cleanup identity never clears this record.
        shared.cleanup_active_if_observable(RequiredCommandId(9999), true);
        assert!(
            shared.inner.lock().unwrap().active_required.is_some(),
            "cleanup targets one exact identity"
        );

        // Matching cleanup removes it.
        shared.cleanup_active_if_observable(id, true);
        assert!(
            shared.inner.lock().unwrap().active_required.is_none(),
            "matching cleanup transitions Resolved/Retiring to cleaned"
        );
        assert_eq!(waiter.wait(), Ok(()), "the submitter observed its result");
    }

    #[test]
    fn active_prepared_unwind_resumes_exact_result_once() {
        // Active boundary recovery B1: an unwind at the ActivePrepared probe — the shared
        // record is installed as Prepared with its exact result and accounting fixed, but
        // its one-shot has not resolved — leaves the record recoverable. A resumed settler
        // delivers the same exact result once, re-accounts nothing, and cleans one id.
        let shared = test_shared();
        let (_id, waiter) = test_enqueue_required(&shared, running_status());
        let accepted = shared.accept_next_required().expect("accepted");
        shared.publish_observed(accepted.id, Err("exact active boom".to_string()));

        set_active_prepared(&shared, |_id| panic!("injected prepared unwind"));
        let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| shared.settle_active(None)));
        assert!(
            unwound.is_err(),
            "the probe forced an unwind at the Prepared boundary"
        );

        // The record is Prepared and recoverable; accounting was applied once; the
        // one-shot has not resolved.
        {
            let inner = shared.inner.lock().unwrap();
            let active = inner.active_required.as_ref().expect("still recoverable");
            assert_eq!(active.id, accepted.id, "the same identity is recoverable");
            assert!(
                matches!(active.phase, ActivePhase::Prepared { .. }),
                "Prepared boundary"
            );
        }
        let resolver = test_active_resolver(&shared).expect("still owned");
        assert!(
            !resolver.is_observable(),
            "the one-shot did not resolve before the unwind"
        );
        assert_eq!(
            shared.diagnostics().write_failures,
            1,
            "accounting applied once"
        );

        // Resume: the same exact result is delivered once and the record is cleaned.
        set_active_prepared(&shared, |_id| {});
        shared.settle_active(None);
        assert_eq!(
            waiter.wait().expect_err("resumes to the exact result"),
            "exact active boom",
            "the resumed settler delivers the exact result, never a replacement"
        );
        let d = shared.diagnostics();
        assert_eq!(d.write_failures, 1, "no re-accounting on resume: {d:?}");
        assert_eq!(d.written, 0, "no success invented: {d:?}");
        assert!(
            shared.inner.lock().unwrap().active_required.is_none(),
            "matching cleanup removed exactly the resumed identity"
        );
        assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
        assert!(
            shared.is_quiescent(),
            "shared state is quiescent after resume"
        );
    }

    #[test]
    fn active_prepared_concurrent_settler_converges() {
        // Active boundary recovery B1: several settlers held together at the ActivePrepared
        // boundary converge on one exact result, one accounting transition, and one
        // cleanup — never a duplicate, replacement, or crossed identity.
        let shared = test_shared();
        let (_id, waiter) = test_enqueue_required(&shared, running_status());
        let accepted = shared.accept_next_required().expect("accepted");
        shared.publish_observed(accepted.id, Err("converge boom".to_string()));

        let settlers = 6;
        let barrier = Arc::new(std::sync::Barrier::new(settlers));
        {
            let barrier = Arc::clone(&barrier);
            set_active_prepared(&shared, move |_id| {
                // Hold every settler at the Prepared boundary before any resolves.
                barrier.wait();
            });
        }
        std::thread::scope(|s| {
            for _ in 0..settlers {
                let shared = &shared;
                s.spawn(move || shared.settle_active(None));
            }
        });

        assert_eq!(
            waiter.wait().expect_err("resolves once"),
            "converge boom",
            "every settler converges on the one exact result"
        );
        let d = shared.diagnostics();
        assert_eq!(
            d.write_failures, 1,
            "accounting applied exactly once: {d:?}"
        );
        assert_eq!(d.written, 0, "no success invented: {d:?}");
        assert!(
            shared.inner.lock().unwrap().active_required.is_none(),
            "the active identity is cleaned exactly once"
        );
        assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
        assert!(
            shared.is_quiescent(),
            "shared state is quiescent after convergence"
        );
    }

    #[test]
    fn active_retiring_unwind_resumes_without_replacement() {
        // Active boundary recovery B1: an unwind at the ActiveRetiringBeforeCleanup probe
        // — the exact result is already observable but the record is not yet cleaned —
        // leaves the record recoverable. A resumed settler cleans the matching id without
        // replacing the delivered result or re-accounting it.
        let shared = test_shared();
        let (_id, waiter) = test_enqueue_required(&shared, running_status());
        let accepted = shared.accept_next_required().expect("accepted");
        shared.publish_observed(accepted.id, Err("retiring boom".to_string()));

        set_active_retiring_before_cleanup(&shared, |_id| panic!("injected retiring unwind"));
        let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| shared.settle_active(None)));
        assert!(
            unwound.is_err(),
            "the probe forced an unwind at the retiring boundary"
        );

        // The one-shot is observable but the record is not yet cleaned.
        let resolver = test_active_resolver(&shared).expect("still recoverable");
        assert!(
            resolver.is_observable(),
            "the exact result is already observable"
        );
        assert!(
            shared.inner.lock().unwrap().active_required.is_some(),
            "RetiringBeforeCleanup: the record is recoverable until matching cleanup"
        );

        // Resume: the delivered result is unchanged and the matching id is cleaned once.
        set_active_retiring_before_cleanup(&shared, |_id| {});
        shared.settle_active(None);
        assert_eq!(
            waiter.wait().expect_err("keeps the delivered result"),
            "retiring boom",
            "the resumed settler never replaces the delivered result"
        );
        let d = shared.diagnostics();
        assert_eq!(d.write_failures, 1, "no re-accounting on resume: {d:?}");
        assert!(
            shared.inner.lock().unwrap().active_required.is_none(),
            "matching cleanup removed exactly the retiring identity"
        );
        assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
        assert!(
            shared.is_quiescent(),
            "shared state is quiescent after resume"
        );
    }

    #[test]
    fn active_retiring_concurrent_cleanup_targets_one_id() {
        // Active boundary recovery B1: several settlers held together at the retiring
        // boundary — each with the one-shot already observable — converge so that cleanup
        // targets exactly one matching id, delivering the result once with one accounting
        // transition.
        let shared = test_shared();
        let (_id, waiter) = test_enqueue_required(&shared, running_status());
        let accepted = shared.accept_next_required().expect("accepted");
        shared.publish_observed(accepted.id, Ok(()));

        let settlers = 6;
        let barrier = Arc::new(std::sync::Barrier::new(settlers));
        {
            let barrier = Arc::clone(&barrier);
            set_active_retiring_before_cleanup(&shared, move |_id| {
                // Hold every settler at the retiring boundary before any cleans up.
                barrier.wait();
            });
        }
        std::thread::scope(|s| {
            for _ in 0..settlers {
                let shared = &shared;
                s.spawn(move || shared.settle_active(None));
            }
        });

        assert_eq!(
            waiter.wait(),
            Ok(()),
            "the one exact result is delivered once"
        );
        let d = shared.diagnostics();
        assert_eq!(d.written, 1, "accounting applied exactly once: {d:?}");
        assert_eq!(d.write_failures, 0, "no failure invented: {d:?}");
        assert!(
            shared.inner.lock().unwrap().active_required.is_none(),
            "cleanup targeted exactly one matching id"
        );
        assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
        assert!(
            shared.is_quiescent(),
            "shared state is quiescent after convergence"
        );
    }

    #[test]
    fn pending_reconciliation_requires_worker_abandonment_authority() {
        // Reconciliation convergence B1: at the Pending boundary a reconciler without
        // abandonment authority leaves the record owned, unresolved, and unaccounted;
        // only after the explicit worker-exit token may pending reconciliation proceed.
        let shared = test_shared();
        let (_id, waiter) = test_enqueue_required(&shared, running_status());
        shared.accept_next_required().expect("accepted");

        // Released before any worker-exit event: no state, accounting, or result moves.
        shared.settle_active(None);
        let resolver = test_active_resolver(&shared).expect("still owned");
        assert!(!resolver.is_observable(), "Pending stays unresolved");
        let d = shared.diagnostics();
        assert_eq!(
            (d.written, d.write_failures),
            (0, 0),
            "no accounting: {d:?}"
        );

        // After the worker-exit/join token, pending reconciliation proceeds once.
        let authority = AbandonmentAuthority::assume_worker_abandoned();
        shared.settle_active(Some(&authority));
        assert!(
            waiter.wait().is_err(),
            "authorized reconciliation resolves the pending write"
        );
        assert_eq!(shared.diagnostics().write_failures, 1);
    }

    #[test]
    fn concurrent_reconciliation_resolves_active_ack_once() {
        // Reconciliation convergence B1: several authorized reconcilers racing one
        // Pending active write converge on one immutable caller result, one accounting
        // transition, and one cleanup — never a duplicate or replacement.
        let shared = test_shared();
        let (_id, waiter) = test_enqueue_required(&shared, running_status());
        shared.accept_next_required().expect("accepted");

        let racers = 8;
        let barrier = std::sync::Barrier::new(racers);
        std::thread::scope(|s| {
            for _ in 0..racers {
                let shared = &shared;
                let barrier = &barrier;
                s.spawn(move || {
                    barrier.wait();
                    let authority = AbandonmentAuthority::assume_worker_abandoned();
                    shared.settle_active(Some(&authority));
                });
            }
        });

        assert!(waiter.wait().is_err(), "the one-shot resolves exactly once");
        let d = shared.diagnostics();
        assert_eq!(d.write_failures, 1, "accounting applied once: {d:?}");
        assert_eq!(d.written, 0, "no success invented: {d:?}");
        assert!(
            shared.inner.lock().unwrap().active_required.is_none(),
            "the active identity is cleaned exactly once"
        );
    }

    #[test]
    fn coordinator_worker_finish_drop_reconcile_interleavings_converge() {
        // Reconciliation convergence B1: normal completion, worker-panic recovery, wake
        // disconnection via drop, and repeated authorized reconciliation each converge
        // on one balanced, quiescent settlement without blocking, losing, replacing, or
        // duplicating a diagnostic.

        // Normal finish: the caller sees success; settlement is balanced and quiescent.
        {
            let (store, _w) = RecordingStore::new();
            let mut c = StatusCoordinator::spawn(Box::new(store), None).unwrap();
            assert!(c.submit_required(running_status()).is_ok());
            let s = c.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));
            assert!(s.diagnostics.is_balanced(), "balanced: {:?}", s.diagnostics);
            assert!(c.is_quiescent(), "quiescent after normal finish");
            assert_eq!(s.diagnostics.write_failures, 0);
        }

        // Worker panic + finish, then a repeated authorized reconcile: the crashed
        // write is one write failure, the terminal is disconnected, balance holds, and
        // repeated reconciliation neither duplicates nor replaces the diagnostic.
        {
            let mut c = StatusCoordinator::spawn(Box::new(PanicStore), None).unwrap();
            assert!(c.submit_required(running_status()).is_err());
            let s = c.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));
            assert!(s.diagnostics.is_balanced(), "balanced: {:?}", s.diagnostics);
            assert!(c.is_quiescent(), "quiescent after panic finish");
            assert_eq!(s.diagnostics.write_failures, 1);
            assert_eq!(s.diagnostics.disconnected, 1);

            let before = c.shared.diagnostics();
            let authority = AbandonmentAuthority::assume_worker_abandoned();
            c.shared.reconcile_abandoned(&authority);
            let after = c.shared.diagnostics();
            assert_eq!(
                (before.written, before.write_failures, before.disconnected),
                (after.written, after.write_failures, after.disconnected),
                "repeated reconciliation is idempotent: {before:?} vs {after:?}"
            );
            assert!(c.is_quiescent());
        }

        // Wake disconnection: dropping an idle coordinator ends the worker through its
        // wake-disconnect reconcile path without hanging.
        {
            let (store, _w) = RecordingStore::new();
            let c = StatusCoordinator::spawn(Box::new(store), None).unwrap();
            drop(c); // returns only if the worker converged and joined
        }
    }

    #[test]
    fn worker_termination_preserves_each_required_ack_identity_and_accounting() {
        // Reconciliation convergence B2: authorized termination finds one active write
        // and three distinct queued writes. It settles the active caller from its own
        // truth and, under the lock, retires every queued command in place to Resolving
        // in the single shared deque — keeping its resolver, stable identity, immutable
        // disconnected result, and exactly-once accounting BEFORE any outside-lock
        // delivery. Each caller then observes its exact own result once; no diagnostic
        // crosses identities.
        let shared = test_shared();

        // Active write with its own distinct observed error.
        let (_a, active_waiter) = test_enqueue_required(&shared, running_status());
        let active = shared.accept_next_required().expect("active accepted");
        shared.publish_observed(active.id, Err("active boom".to_string()));

        // Three distinct queued writes.
        let (b_id, b_wait) = test_enqueue_required(&shared, running_status());
        let (c_id, c_wait) = test_enqueue_required(&shared, running_status());
        let (d_id, d_wait) = test_enqueue_required(&shared, running_status());

        let authority = AbandonmentAuthority::assume_worker_abandoned();
        // Settle the active caller from its own truth first.
        shared.settle_active(Some(&authority));
        assert_eq!(
            active_waiter
                .wait()
                .expect_err("active keeps its own error"),
            "active boom",
            "the active caller keeps its own observed error, not a disconnect"
        );

        // Retire every queued command in place to Resolving under the lock.
        shared.retire_queued(&authority);
        // Before any delivery: all three are shared, accounted, and unresolved.
        let resolvers = test_resolving_resolvers(&shared);
        assert_eq!(
            resolvers.len(),
            3,
            "all queued retired in place to Resolving"
        );
        let ids: Vec<_> = resolvers.iter().map(|(id, _)| *id).collect();
        assert!(
            ids.contains(&b_id) && ids.contains(&c_id) && ids.contains(&d_id),
            "each stable command identity is preserved in shared ownership"
        );
        for (_, r) in &resolvers {
            assert!(!r.is_observable(), "no one-shot resolves before delivery");
        }
        let d = shared.diagnostics();
        assert_eq!(
            d.disconnected, 3,
            "disconnected accounting applied once: {d:?}"
        );

        // Now deliver outside the lock; each caller observes its own result once.
        shared.resolve_queued();
        assert!(b_wait.wait().is_err(), "queued b resolves");
        assert!(c_wait.wait().is_err(), "queued c resolves");
        assert!(d_wait.wait().is_err(), "queued d resolves");

        let d = shared.diagnostics();
        assert_eq!(d.write_failures, 1, "one active write failure: {d:?}");
        assert_eq!(d.disconnected, 3, "three queued disconnects: {d:?}");
        assert!(d.is_balanced(), "balanced: {d:?}");
        assert!(shared.is_quiescent(), "settlement is quiescent");
    }

    #[test]
    fn queued_resolvers_remain_shared_across_pre_delivery_unwind() {
        // Reconciliation convergence B2: an unwind after every queued command is retired
        // in place to Resolving but before any one-shot resolution leaves every resolver
        // authority in the shared deque; a resumed/concurrent reconciler completes them.
        // A local vector is never the last owner.
        let shared = test_shared();
        let (_b, b_wait) = test_enqueue_required(&shared, running_status());
        let (_c, c_wait) = test_enqueue_required(&shared, running_status());

        let authority = AbandonmentAuthority::assume_worker_abandoned();
        shared.retire_queued(&authority);

        // The reconciler unwinds here — before calling resolve_queued. Every resolver
        // remains owned by shared state and unresolved.
        let resolvers = test_resolving_resolvers(&shared);
        assert_eq!(
            resolvers.len(),
            2,
            "both resolvers stay in shared ownership"
        );
        for (_, r) in &resolvers {
            assert!(
                !r.is_observable(),
                "no resolver was lost to (or resolved by) a local vector"
            );
        }

        // A resumed reconciler completes delivery; no waiter was lost.
        shared.resolve_queued();
        assert!(b_wait.wait().is_err(), "b resumes to disconnect");
        assert!(c_wait.wait().is_err(), "c resumes to disconnect");
        assert!(shared.is_quiescent(), "quiescent after resume");
    }

    #[test]
    fn queued_resolution_resumes_after_one_delivery_without_loss_or_swap() {
        // Reconciliation convergence B2: an unwind after one queued result becomes
        // observable and before the rest resolve is resumed by a later reconciler; every
        // caller observes its own result once, and the delivered entry's original
        // resolver stays shared until matching cleanup.
        let shared = test_shared();
        let (b_id, b_wait) = test_enqueue_required(&shared, running_status());
        let (_c, c_wait) = test_enqueue_required(&shared, running_status());

        let authority = AbandonmentAuthority::assume_worker_abandoned();
        shared.retire_queued(&authority);

        // Deliver exactly one one-shot (the FIFO-first entry), then "unwind" before the
        // rest resolve — the shared entry is not yet cleaned.
        let resolvers = test_resolving_resolvers(&shared);
        assert_eq!(
            resolvers[0].0, b_id,
            "the first shared entry is FIFO-first (b)"
        );
        resolvers[0].1.resolve_once(Err("x".to_string()));
        assert!(resolvers[0].1.is_observable(), "b's one-shot is observable");
        assert!(!resolvers[1].1.is_observable(), "c is not yet resolved");
        // The delivered entry's original resolver is still shared (not the last owner).
        assert_eq!(
            test_resolving_resolvers(&shared).len(),
            2,
            "both entries remain shared until matching cleanup"
        );

        // Resume: a later reconciler completes the rest and cleans up; no loss.
        shared.resolve_queued();
        assert!(b_wait.wait().is_err(), "b keeps its own delivered result");
        assert!(c_wait.wait().is_err(), "c resumes without loss");
        assert!(shared.is_quiescent(), "quiescent after resume");
    }

    #[test]
    fn queued_precommit_unwind_keeps_every_resolver_shared() {
        // Shared queued ownership B1: an unwind at the BeforeQueuedRetirementCommit probe
        // — before the lock-held Queued->Resolving commit — leaves every never-accepted
        // command shared, queued, unresolved, and unaccounted, so a later authorized
        // reconciliation resumes it. The single required deque is the only owner
        // throughout: no ownership transfer container exists to lose a resolver.
        let shared = test_shared();
        let (_b, b_wait) = test_enqueue_required(&shared, running_status());
        let (_c, c_wait) = test_enqueue_required(&shared, running_status());
        let (_d, d_wait) = test_enqueue_required(&shared, running_status());

        // Clone each queued resolver up front so observability can be probed without the
        // coordinator lock.
        let queued_resolvers: Vec<RequiredAckResolver> = shared
            .inner
            .lock()
            .unwrap()
            .required
            .iter()
            .map(|e| e.resolver.clone())
            .collect();

        // Inject an unwind at the pre-commit probe.
        set_before_queued_retirement_commit(&shared, || panic!("injected pre-commit unwind"));
        let authority = AbandonmentAuthority::assume_worker_abandoned();
        let unwound =
            std::panic::catch_unwind(AssertUnwindSafe(|| shared.retire_queued(&authority)));
        assert!(
            unwound.is_err(),
            "the probe forced an unwind before the commit"
        );

        // Every entry stays shared, queued, and unresolved; nothing was accounted, and
        // the FIFO was never shut, because the lock-held commit was never taken.
        {
            let inner = shared.inner.lock().unwrap();
            assert_eq!(
                inner.required.len(),
                3,
                "every command stays in the shared deque"
            );
            assert!(
                inner
                    .required
                    .iter()
                    .all(|e| matches!(e.lifecycle, QueuedLifecycle::Queued)),
                "no entry was retired before the commit"
            );
            assert!(!inner.shutdown, "the untaken commit left shutdown unset");
            assert_eq!(inner.diagnostics.disconnected, 0, "nothing was accounted");
        }
        for r in &queued_resolvers {
            assert!(!r.is_observable(), "no one-shot resolved before the commit");
        }

        // Resume: clear the probe and retire again. Every entry retires in place and
        // resolves once; every waiter observes its own disconnect and settlement is
        // quiescent.
        set_before_queued_retirement_commit(&shared, || {});
        shared.retire_queued(&authority);
        {
            let inner = shared.inner.lock().unwrap();
            assert!(inner.shutdown, "the resumed commit shut the FIFO");
            assert_eq!(
                inner.diagnostics.disconnected, 3,
                "each entry accounted disconnected exactly once on resume"
            );
            assert!(
                inner.required.iter().all(|e| matches!(
                    e.lifecycle,
                    QueuedLifecycle::Resolving { immutable_error }
                        if immutable_error == QUEUED_REQUIRED_DISCONNECT
                )),
                "every entry retired in place to Resolving with the static disconnect error"
            );
        }
        shared.resolve_queued();
        assert_eq!(
            b_wait.wait().expect_err("b resolves"),
            QUEUED_REQUIRED_DISCONNECT,
            "b resumes to exactly its own disconnect result"
        );
        assert_eq!(
            c_wait.wait().expect_err("c resolves"),
            QUEUED_REQUIRED_DISCONNECT,
            "c resumes to exactly its own disconnect result"
        );
        assert_eq!(
            d_wait.wait().expect_err("d resolves"),
            QUEUED_REQUIRED_DISCONNECT,
            "d resumes to exactly its own disconnect result"
        );
        let d = shared.diagnostics();
        assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
        assert!(shared.is_quiescent(), "quiescent after resume");
    }

    #[test]
    fn queued_resolution_resumes_after_one_without_result_swap() {
        // Shared queued ownership B2: shared Resolving entries with DISTINCT static
        // immutable errors resolve without swapping results, whether two concurrent
        // reconcilers select the same identity or one delivery unwinds after a result is
        // observable. Each original waiter receives its own exact string, accounting
        // applies once, and settlement reaches quiescence. A final block proves ordinary
        // production retirement fixes exactly QUEUED_REQUIRED_DISCONNECT for every entry.
        static ERR_A: &str = "persist pump status: queued disconnect A";
        static ERR_B: &str = "persist pump status: queued disconnect B";

        // --- Concurrent same-identity delivery preserves each exact result ---
        // Two reconcilers race one deque of two shared Resolving entries. Selection
        // always returns the front Resolving entry, so both reconcilers select the same
        // identity; a barrier holds both at the post-observability, pre-cleanup boundary
        // for that identity before either cleans it. The idempotent one-shot and
        // matching-id cleanup keep each caller's exact own result and account once.
        {
            let shared = test_shared();
            let (a_id, a_wait) = test_enqueue_resolving(&shared, running_status(), ERR_A);
            let (b_id, b_wait) = test_enqueue_resolving(&shared, running_status(), ERR_B);

            let seen = Arc::new(Mutex::new(Vec::new()));
            let barrier = Arc::new(std::sync::Barrier::new(2));
            {
                let seen = Arc::clone(&seen);
                let barrier = Arc::clone(&barrier);
                set_queued_retiring_before_cleanup(&shared, move |id| {
                    seen.lock().unwrap().push(id);
                    // Hold both reconcilers at the same identity's boundary before either
                    // removes it, so the same stable id is provably selected concurrently.
                    barrier.wait();
                });
            }

            std::thread::scope(|s| {
                let shared = &shared;
                s.spawn(move || shared.resolve_queued());
                s.spawn(move || shared.resolve_queued());
            });

            // Each ORIGINAL waiter receives its OWN exact string — never the other's.
            assert_eq!(
                a_wait.wait().expect_err("a resolves"),
                ERR_A,
                "a keeps its own result"
            );
            assert_eq!(
                b_wait.wait().expect_err("b resolves"),
                ERR_B,
                "b keeps its own result"
            );

            // Both reconcilers selected each stable identity — proof of concurrent same-id
            // selection — and accounting applied once per entry.
            let seen = seen.lock().unwrap();
            assert_eq!(
                seen.iter().filter(|id| **id == a_id).count(),
                2,
                "both reconcilers selected a's stable id: {seen:?}"
            );
            assert_eq!(
                seen.iter().filter(|id| **id == b_id).count(),
                2,
                "both reconcilers selected b's stable id: {seen:?}"
            );
            let d = shared.diagnostics();
            assert_eq!(
                d.disconnected, 2,
                "disconnected accounting applied once per entry: {d:?}"
            );
            assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
            assert!(
                shared.is_quiescent(),
                "quiescent after concurrent resolution"
            );
        }

        // --- Post-observability unwind resumes without loss or swap ---
        // An unwind fires after one result becomes observable but before cleanup. The
        // delivered entry's ORIGINAL resolver stays shared; a resumed reconciler
        // completes every delivery, and each caller keeps its own exact string.
        {
            let shared = test_shared();
            let (_a, a_wait) = test_enqueue_resolving(&shared, running_status(), ERR_A);
            let (_b, b_wait) = test_enqueue_resolving(&shared, running_status(), ERR_B);

            let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let fired = Arc::clone(&fired);
                set_queued_retiring_before_cleanup(&shared, move |_id| {
                    if !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        panic!("injected post-observability unwind");
                    }
                });
            }
            let unwound = std::panic::catch_unwind(AssertUnwindSafe(|| shared.resolve_queued()));
            assert!(
                unwound.is_err(),
                "the probe forced an unwind after one result was observable"
            );
            assert_eq!(
                test_resolving_resolvers(&shared).len(),
                2,
                "both entries — including the delivered one — remain shared until cleanup"
            );

            // Resume completes delivery; each caller keeps its own exact result.
            shared.resolve_queued();
            assert_eq!(
                a_wait.wait().expect_err("a resumes"),
                ERR_A,
                "a keeps its own result"
            );
            assert_eq!(
                b_wait.wait().expect_err("b resumes"),
                ERR_B,
                "b keeps its own result"
            );
            let d = shared.diagnostics();
            assert_eq!(
                d.disconnected, 2,
                "accounting applied once per entry across resume: {d:?}"
            );
            assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
            assert!(shared.is_quiescent(), "quiescent after resume");
        }

        // --- Production retirement fixes exactly QUEUED_REQUIRED_DISCONNECT ---
        {
            let shared = test_shared();
            let (_p, p_wait) = test_enqueue_required(&shared, running_status());
            let (_q, q_wait) = test_enqueue_required(&shared, running_status());
            let authority = AbandonmentAuthority::assume_worker_abandoned();
            shared.retire_queued(&authority);
            {
                let inner = shared.inner.lock().unwrap();
                assert!(
                    inner.required.iter().all(|e| matches!(
                        e.lifecycle,
                        QueuedLifecycle::Resolving { immutable_error }
                            if immutable_error == QUEUED_REQUIRED_DISCONNECT
                    )),
                    "ordinary retirement fixes exactly QUEUED_REQUIRED_DISCONNECT for every entry"
                );
                assert_eq!(
                    inner.diagnostics.disconnected, 2,
                    "every entry accounted once"
                );
            }
            shared.resolve_queued();
            assert_eq!(
                p_wait.wait().expect_err("p resolves"),
                QUEUED_REQUIRED_DISCONNECT,
                "the production caller gets exactly the disconnect diagnostic"
            );
            assert_eq!(
                q_wait.wait().expect_err("q resolves"),
                QUEUED_REQUIRED_DISCONNECT,
                "every production caller gets exactly the disconnect diagnostic"
            );
            let d = shared.diagnostics();
            assert!(d.is_balanced(), "diagnostics balance independently: {d:?}");
            assert!(
                shared.is_quiescent(),
                "quiescent after production retirement"
            );
        }
    }

    /// A store that fails every Running write, fails the Complete write with a
    /// distinct error, and panics on the Failed fallback, so periodic, terminal
    /// settlement, fallback, and worker termination can all fail in one capture.
    struct CompositeFailureStore;

    impl StatusStore for CompositeFailureStore {
        fn write(&mut self, status: &PumpStatus) -> Result<(), String> {
            match status.state {
                PumpState::Running => Err("running write failure".to_string()),
                PumpState::Complete => Err("Complete write failure".to_string()),
                PumpState::Failed => panic!("simulated Failed fallback panic"),
            }
        }
    }

    #[test]
    fn settlement_result_retains_all_distinct_failures() {
        // Composite settlement and quiescence B1: when periodic persistence, terminal
        // settlement, the Failed fallback, and worker termination each fail during one
        // capture, the typed result retains every diagnostic in its distinct field
        // without replacing the immutable primary or the exact observed required-write
        // result delivered to its caller.
        let latch = Arc::new(FirstFault::default());
        let mut coordinator =
            StatusCoordinator::spawn(Box::new(CompositeFailureStore), Some(Arc::clone(&latch)))
                .unwrap();

        // The required caller observes its own exact store error, resolved once.
        let required = coordinator
            .submit_required(running_status())
            .expect_err("the required write fails");
        assert!(
            required.message().contains("running write failure"),
            "the observed required-write result reaches its caller unchanged: {required}"
        );

        // A best-effort periodic write also fails (drained before the terminal).
        coordinator.submit_periodic(running_status());
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        let err = settlement
            .terminal_failure()
            .expect("a composite terminal failure, never silent success");
        assert!(
            err.periodic_error()
                .is_some_and(|e| e.contains("running write failure")),
            "the periodic failure is retained distinctly: {err:?}"
        );
        assert!(
            err.settlement_error()
                .is_some_and(|e| e.contains("Complete write failure")),
            "the terminal-settlement failure is retained distinctly: {err:?}"
        );
        assert!(
            err.fallback_error()
                .is_some_and(|e| e.contains(STATUS_WORKER_PANIC)),
            "the fallback failure is retained distinctly: {err:?}"
        );
        assert_eq!(
            err.worker_error(),
            Some(STATUS_WORKER_PANIC),
            "the worker termination is retained distinctly: {err:?}"
        );
        assert!(
            err.message().contains("Complete write failure"),
            "the immutable primary is not replaced by a later diagnostic: {err}"
        );
        assert!(
            err.transport().is_some_and(|t| t.is_balanced()),
            "the balanced transport is retained: {err:?}"
        );
    }

    #[test]
    fn failure_settlement_is_exact_and_quiescent() {
        // Composite settlement and quiescence B2: an ordinary terminal store failure
        // settles with exact terminal-inclusive counts satisfying the balance equation
        // and leaves no active, accepted, prepared, resolved/retiring, queued, or
        // shared-resolving state.
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let store = FailStateStore {
            attempts: Arc::clone(&attempts),
            fail: vec![PumpState::Complete],
        };
        let mut coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();

        coordinator
            .submit_required(running_status())
            .expect("the required write succeeds");
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        let d = &settlement.diagnostics;
        // Required (written) + Complete terminal (write failure) + Failed fallback
        // (written): three submissions, two written, one write failure.
        assert_eq!(d.submitted, 3, "exact submissions: {d:?}");
        assert_eq!(d.written, 2, "exact written: {d:?}");
        assert_eq!(d.write_failures, 1, "exact write failures: {d:?}");
        assert_eq!(d.coalesced, 0, "exact coalesced: {d:?}");
        assert_eq!(d.dropped, 0, "exact dropped: {d:?}");
        assert_eq!(d.disconnected, 0, "exact disconnected: {d:?}");
        assert!(d.is_balanced(), "terminal-inclusive balance holds: {d:?}");
        assert!(
            coordinator.is_quiescent(),
            "no residual state after failure"
        );
        {
            let inner = coordinator.shared.inner.lock().unwrap();
            assert!(
                inner.active_required.is_none(),
                "no active/accepted/prepared record"
            );
            assert!(
                inner
                    .required
                    .iter()
                    .all(|e| !matches!(e.lifecycle, QueuedLifecycle::Resolving { .. })),
                "no shared-resolving state"
            );
        }
    }

    #[test]
    fn successful_settlement_is_exact_and_quiescent() {
        // Composite settlement and quiescence B2: a successful terminal settlement
        // exposes exact terminal-inclusive counts and leaves no active, queued, or
        // shared-resolving acknowledgement state behind.
        let (store, _writes) = RecordingStore::new();
        let mut coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();

        coordinator
            .submit_required(running_status())
            .expect("the required write succeeds");
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        let d = &settlement.diagnostics;
        // Required (written) + Complete terminal (written): two submissions, both written.
        assert_eq!(d.submitted, 2, "exact submissions: {d:?}");
        assert_eq!(d.written, 2, "exact written: {d:?}");
        assert_eq!(
            (d.coalesced, d.dropped, d.disconnected, d.write_failures),
            (0, 0, 0, 0),
            "no loss on success: {d:?}"
        );
        assert!(d.is_balanced(), "terminal-inclusive balance holds: {d:?}");
        assert!(settlement.terminal_failure().is_none(), "clean success");
        assert!(
            coordinator.is_quiescent(),
            "no residual state after success"
        );
        {
            let inner = coordinator.shared.inner.lock().unwrap();
            assert!(
                inner.active_required.is_none(),
                "no active/accepted/prepared record"
            );
            assert!(
                inner
                    .required
                    .iter()
                    .all(|e| !matches!(e.lifecycle, QueuedLifecycle::Resolving { .. })),
                "no shared-resolving state"
            );
        }
    }

    #[test]
    fn latest_periodic_snapshot_replaces_older_pending_snapshot() {
        // B1: while the worker is blocked writing the first snapshot, two more are
        // submitted; the newest replaces the older pending one in the single slot,
        // and the replaced snapshot is accounted exactly once as coalesced.
        let writes = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (gate_tx, gate_rx) = mpsc::channel();
        let store = GatedStore {
            writes: Arc::clone(&writes),
            entered: entered_tx,
            gate: gate_rx,
        };
        let mut coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();

        // A: taken from the slot and blocked mid-write.
        coordinator.submit_periodic(running_status());
        assert_eq!(entered_rx.recv().unwrap(), PumpState::Running);

        // B then C: B lands in the slot, C replaces B → B is coalesced.
        coordinator.submit_periodic(running_status());
        coordinator.submit_periodic(running_status());

        // Release every blocked write (A, C, and the terminal) and settle.
        for _ in 0..8 {
            let _ = gate_tx.send(());
        }
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        // A, B, C periodics plus the terminal write are all submissions.
        assert_eq!(settlement.diagnostics.submitted, 4);
        assert_eq!(
            settlement.diagnostics.coalesced, 1,
            "the replaced snapshot B"
        );
        assert_eq!(
            settlement.diagnostics.written, 3,
            "A, C, and the terminal reach the store"
        );
        assert!(settlement.diagnostics.is_balanced());
        // Only A and C were persisted from the periodics; B never reached the store.
        assert_eq!(writes.lock().unwrap().len(), 3, "A, C, and the terminal");
    }

    #[test]
    fn required_statuses_are_fifo_and_acknowledged() {
        // B1: required statuses are persisted in submission order, and each submitter
        // is acknowledged only after its own status is written.
        let (store, writes) = RecordingStore::new();
        let coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();

        for records in [1u64, 2, 3] {
            let status = build_status(
                PumpState::Running,
                0,
                &PumpSummary {
                    records,
                    ..PumpSummary::default()
                },
                None,
                None,
                StatusTransportDiagnostics::default(),
            );
            // Blocks until the worker acknowledges this exact status.
            coordinator
                .submit_required(status)
                .expect("a writable required status is acknowledged");
            // By the time submit_required returns, the store already holds it.
            let persisted = writes.lock().unwrap();
            assert_eq!(
                persisted.last().unwrap().records,
                records,
                "acknowledgement follows the write of this required status"
            );
        }
        let order: Vec<u64> = writes.lock().unwrap().iter().map(|s| s.records).collect();
        assert_eq!(order, vec![1, 2, 3], "required statuses persist FIFO");
    }

    #[test]
    fn terminal_ack_cannot_be_followed_by_running() {
        // B1: once the terminal status is acknowledged, no later Running status can
        // reach the store — a late submission is refused, never written.
        let (store, writes) = RecordingStore::new();
        let mut coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();

        coordinator.submit_required(running_status()).unwrap();
        let _ = coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        // A Running submitted after the terminal acknowledgement is disconnected.
        coordinator.submit_periodic(running_status());
        assert!(
            coordinator.submit_required(running_status()).is_err(),
            "a required Running after the terminal is refused"
        );

        let persisted = writes.lock().unwrap();
        assert_eq!(
            persisted.last().unwrap().state,
            PumpState::Complete,
            "the terminal Complete is the final persisted state"
        );
        assert_eq!(
            persisted
                .iter()
                .filter(|s| s.state == PumpState::Running)
                .count(),
            1,
            "only the initial Running was ever written; none followed the terminal"
        );
    }

    #[test]
    fn terminal_status_diagnostics_balance() {
        // B3: the terminal diagnostics account every submission exactly once and
        // satisfy submitted = written + coalesced + dropped + disconnected +
        // write_failures, with no snapshot left pending. The persisted terminal
        // document carries the same balanced accounting.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript-pump.json");
        let mut coordinator = StatusCoordinator::spawn(file_status_store(&path), None).unwrap();

        coordinator.submit_required(running_status()).unwrap();
        for _ in 0..5 {
            coordinator.submit_periodic(running_status());
        }
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        assert!(
            settlement.diagnostics.is_balanced(),
            "diagnostics balance: {:?}",
            settlement.diagnostics
        );
        assert_eq!(
            settlement.diagnostics.submitted,
            settlement.diagnostics.written
                + settlement.diagnostics.coalesced
                + settlement.diagnostics.dropped
                + settlement.diagnostics.disconnected
                + settlement.diagnostics.write_failures,
            "every submission is accounted in exactly one category"
        );
        assert_eq!(
            settlement.diagnostics.submitted, 7,
            "one required Running, five periodics, and the terminal"
        );

        // The persisted terminal document carries the same balanced accounting.
        let persisted: PumpStatus = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted.state, PumpState::Complete);
        assert!(persisted.transport.is_balanced());
    }

    #[test]
    fn disconnected_status_submission_is_accounted() {
        // B3: a status submitted after the coordinator has shut down is accounted as
        // disconnected rather than silently ignored, and never reaches the store.
        let (store, writes) = RecordingStore::new();
        let mut coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();
        coordinator.submit_required(running_status()).unwrap();
        let _ = coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));
        let written_before = writes.lock().unwrap().len();

        coordinator.submit_periodic(running_status());
        assert!(coordinator.submit_required(running_status()).is_err());

        let diagnostics = coordinator.diagnostics();
        assert_eq!(
            diagnostics.disconnected, 2,
            "both post-shutdown submissions are accounted disconnected"
        );
        assert_eq!(
            writes.lock().unwrap().len(),
            written_before,
            "no disconnected submission reaches the store"
        );
        assert!(diagnostics.is_balanced());
    }

    #[test]
    fn complete_write_failure_falls_back_to_failed() {
        // B4: when a Complete status cannot be persisted, the coordinator attempts a
        // Failed fallback and the drain surfaces a typed failure that names the
        // Complete write problem.
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let store = FailStateStore {
            attempts: Arc::clone(&attempts),
            fail: vec![PumpState::Complete],
        };
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let sink = SaturatedSink::new();
        let err = drain_with_store(
            Cursor::new(b"{\"a\":1}\n".to_vec()),
            &transcript,
            Some(Box::new(store)),
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .expect_err("an unpersistable Complete status fails the drain");
        assert!(
            err.message().contains("simulated Complete write failure"),
            "the primary names the Complete write failure: {err}"
        );
        assert!(
            err.settlement_error()
                .is_some_and(|s| s.contains("simulated Complete write failure")),
            "the terminal-settlement failure is attached to its dedicated field, not \
             only folded into the message: {err:?}"
        );
        assert!(
            err.fallback_error().is_none(),
            "the Failed fallback persisted, so no fallback error"
        );

        let attempts = attempts.lock().unwrap();
        assert!(
            attempts
                .iter()
                .any(|(s, ok)| *s == PumpState::Complete && !ok),
            "a Complete write was attempted and failed"
        );
        assert!(
            attempts
                .iter()
                .any(|(s, ok)| *s == PumpState::Failed && *ok),
            "a Failed fallback was attempted and succeeded"
        );
    }

    #[test]
    fn composite_status_failures_preserve_primary_and_settlement_errors() {
        // B4: when capture fails AND the terminal status cannot be persisted, the
        // typed error preserves the immutable primary fault alongside the settlement
        // diagnostics rather than discarding or overwriting either.
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let store = FailStateStore {
            attempts: Arc::clone(&attempts),
            fail: vec![PumpState::Failed],
        };
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let sink = SaturatedSink::new();
        let err = drain_with_store(
            ErrorAfterOneRecord { emitted: false },
            &transcript,
            Some(Box::new(store)),
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .expect_err("a mid-stream read failure fails the drain");

        assert!(
            err.message().contains("read coder stdout"),
            "the immutable primary fault is preserved: {err}"
        );
        assert!(
            err.settlement_error()
                .is_some_and(|s| s.contains("simulated Failed write failure")),
            "the terminal-settlement failure rides alongside the primary: {err:?}"
        );
        assert!(
            err.transport().is_some_and(|t| t.is_balanced()),
            "the balanced transport diagnostics are preserved"
        );
    }

    /// A status store that fails every periodic Running write and then panics on
    /// the terminal write, so one settlement carries a distinct periodic failure
    /// AND a worker-join failure at the same time.
    struct PeriodicFailThenTerminalPanicStore;

    impl StatusStore for PeriodicFailThenTerminalPanicStore {
        fn write(&mut self, status: &PumpStatus) -> Result<(), String> {
            match status.state {
                PumpState::Running => Err("simulated periodic write failure".to_string()),
                _ => panic!("simulated terminal status store panic"),
            }
        }
    }

    #[test]
    fn composite_status_failure_retains_periodic_and_worker_diagnostics() {
        // B4: a composite status failure preserves every distinct diagnostic — the
        // immutable primary, the best-effort periodic failure, and the worker-join
        // (panic) failure — in separate typed fields rather than collapsing them.
        // `next_work` drains the pending periodic before the terminal, so the
        // periodic write fails (latching `periodic_error`) before the terminal write
        // panics (latching `worker_error`), regardless of worker scheduling.
        let latch = Arc::new(FirstFault::default());
        let mut coordinator = StatusCoordinator::spawn(
            Box::new(PeriodicFailThenTerminalPanicStore),
            Some(Arc::clone(&latch)),
        )
        .unwrap();

        coordinator.submit_periodic(running_status());
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        let err = settlement
            .terminal_failure()
            .expect("a worker panic is a terminal failure, never silent success");
        assert_eq!(
            err.worker_error(),
            Some(STATUS_WORKER_PANIC),
            "the worker-join failure is retained in its own field: {err:?}"
        );
        assert!(
            err.periodic_error()
                .is_some_and(|e| e.contains("periodic write failure")),
            "the earlier best-effort periodic failure is retained distinctly, not \
             overwritten by the worker panic: {err:?}"
        );
        assert!(
            err.message().contains("panicked"),
            "the immutable primary names the worker panic: {err}"
        );
        assert!(
            err.transport().is_some_and(|t| t.is_balanced()),
            "the balanced transport diagnostics are preserved: {err:?}"
        );
    }

    /// A status store whose every write panics, modelling a status worker that
    /// unwinds mid-persist.
    struct PanicStore;

    impl StatusStore for PanicStore {
        fn write(&mut self, _status: &PumpStatus) -> Result<(), String> {
            panic!("simulated status store panic");
        }
    }

    #[test]
    fn status_worker_panic_latches_first_fault() {
        // B2: a status worker that panics while persisting publishes the immutable
        // first fault to the latch, so supervision observes it without joining the
        // pump — and the settlement carries a worker error rather than success.
        let latch = Arc::new(FirstFault::default());
        let mut coordinator =
            StatusCoordinator::spawn(Box::new(PanicStore), Some(Arc::clone(&latch))).unwrap();
        // The worker panics writing this required status; the submitter is
        // disconnected rather than acknowledged.
        assert!(coordinator.submit_required(running_status()).is_err());
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        assert!(latch.observed(), "the worker panic latched the first fault");
        assert_eq!(latch.message().as_deref(), Some(STATUS_WORKER_PANIC));
        assert!(
            settlement.terminal_failure().is_some(),
            "a worker panic is a terminal failure, never silent success"
        );
        // The write the store actually attempted and crashed on is accounted as a
        // write failure carrying an error — never a silent `disconnected` with an
        // empty `last_error`, which would report the store was never reached.
        assert!(
            settlement.diagnostics.write_failures >= 1,
            "the in-flight write the store crashed on is a write failure: {:?}",
            settlement.diagnostics
        );
        assert!(
            settlement
                .diagnostics
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("panicked")),
            "the crashed write records its panic as the last error: {:?}",
            settlement.diagnostics
        );
    }

    #[test]
    fn worker_panic_reconciles_abandoned_work_and_keeps_balance() {
        // Audit regression: when the worker panics mid-persist, a required submitter
        // is disconnected rather than left hanging, and every counted submission is
        // reconciled to a terminal category so the balance still holds.
        let latch = Arc::new(FirstFault::default());
        let mut coordinator =
            StatusCoordinator::spawn(Box::new(PanicStore), Some(Arc::clone(&latch))).unwrap();
        assert!(
            coordinator.submit_required(running_status()).is_err(),
            "the submitter is disconnected, not hung, when the worker panics"
        );
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));
        // Exactly the initial Running and the terminal are submitted. The Running
        // write is the one the worker crashed on mid-persist, so it is accounted as
        // a write failure; the terminal was never reached by the dead worker, so it
        // is the only disconnected submission. Nothing is written and no snapshot is
        // pending, and the balance still holds.
        assert_eq!(settlement.diagnostics.submitted, 2, "Running + terminal");
        assert_eq!(
            settlement.diagnostics.write_failures, 1,
            "the crashed Running write is a write failure, not disconnected: {:?}",
            settlement.diagnostics
        );
        assert_eq!(
            settlement.diagnostics.disconnected, 1,
            "only the never-attempted terminal is disconnected: {:?}",
            settlement.diagnostics
        );
        assert_eq!(settlement.diagnostics.written, 0);
        assert!(
            settlement.diagnostics.is_balanced(),
            "abandoned work is reconciled so the balance holds: {:?}",
            settlement.diagnostics
        );
    }

    #[test]
    fn required_write_failure_is_not_labeled_periodic() {
        // Audit regression: a required (initial Running) write failure must not be
        // copied into the periodic_error channel; periodic_error names only
        // best-effort periodic failures.
        let attempts = Arc::new(Mutex::new(Vec::new()));
        let store = FailStateStore {
            attempts: Arc::clone(&attempts),
            fail: vec![PumpState::Running],
        };
        let mut coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();
        assert!(
            coordinator.submit_required(running_status()).is_err(),
            "a failed required write is a typed failure"
        );
        let settlement = coordinator.finish(TerminalStatusSpec::failed(
            0,
            PumpSummary::default(),
            "primary",
        ));
        assert!(
            settlement.periodic_error.is_none(),
            "a required write failure is not a periodic error: {:?}",
            settlement.periodic_error
        );
    }

    /// A store that persists Running but panics on the terminal write, modelling a
    /// status worker that unwinds while persisting the terminal state.
    struct PanicOnTerminalStore;

    impl StatusStore for PanicOnTerminalStore {
        fn write(&mut self, status: &PumpStatus) -> Result<(), String> {
            if status.state == PumpState::Running {
                Ok(())
            } else {
                panic!("simulated terminal status store panic");
            }
        }
    }

    #[test]
    fn status_worker_panic_recovers_pump_promptly() {
        // Re-audit regression: a status worker panic (the `transcript-pump-status`
        // thread) is caught and recovered without blocking — the process-wide hook
        // suppresses its blocking default stderr output — so the pump surfaces a
        // typed failure promptly and latches the first fault.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let mut pump = spawn_pump_with_store(
            Cursor::new(b"{\"rec\":1}\n".to_vec()),
            transcript,
            Some(Box::new(PanicOnTerminalStore)),
            console_preview_sink(),
            TranscriptPumpConfig::default(),
        )
        .unwrap();

        let started = Instant::now();
        let outcome = pump.wait_terminal();
        let elapsed = started.elapsed();
        pump.join();

        assert!(
            outcome.is_err(),
            "a status worker panic surfaces a typed pump failure"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "panic recovery must be prompt, not blocked; took {elapsed:?}"
        );
        assert!(
            pump.first_fault_observed(),
            "the status worker panic latched the first fault"
        );
    }

    #[test]
    fn status_worker_panic_recovers_under_saturated_fd2() {
        // B2: the process-wide pump panic hook must suppress the default hook's
        // blocking stderr write for pump threads, so a status-worker panic recovers
        // even when fd 2 is a genuinely full, non-drained pipe. The in-process panic
        // tests cannot prove this — they never saturate fd 2 — so re-exec this test
        // binary's child body with fd 2 bound to an unread pipe. The child saturates
        // the pipe, drives a status-worker panic, and exits 0 only if recovery
        // completed without blocking on the saturated stderr; a blocking default hook
        // would hang until the child's alarm watchdog killed it (a non-zero status).
        use std::process::{Command, Stdio};
        let exe = std::env::current_exe().expect("test binary path");
        let mut child = Command::new(exe)
            .args([
                "--exact",
                "transcript_pump::tests::saturated_fd2_panic_child",
                "--nocapture",
            ])
            .env("PUMP_FD2_CHILD", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // The parent holds the pipe's read end but never drains it, so once the
            // child fills the pipe its fd 2 is genuinely full; keeping the handle
            // open (rather than dropping it) means a blocked write stays blocked
            // instead of failing with EPIPE.
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn saturated-fd2 child");
        let _saturated_pipe = child.stderr.take();
        let status = child.wait().expect("await saturated-fd2 child");
        assert!(
            status.success(),
            "the pump panic hook must recover a status-worker panic under a saturated \
             fd 2 (child status: {status:?}); a blocking default hook would hang until \
             the alarm killed it"
        );
    }

    /// Child-process body for `status_worker_panic_recovers_under_saturated_fd2`.
    /// A no-op in an ordinary run; the parent re-executes this binary with
    /// `PUMP_FD2_CHILD=1` and fd 2 bound to an unread pipe to drive the real check.
    #[test]
    fn saturated_fd2_panic_child() {
        if std::env::var_os("PUMP_FD2_CHILD").is_none() {
            return;
        }
        // Saturate the inherited fd 2: fill it with nonblocking writes until EAGAIN,
        // then restore blocking mode so any further write — including the default
        // panic hook's — would block on the full, unread pipe.
        unsafe {
            let flags = libc::fcntl(2, libc::F_GETFL);
            assert!(flags != -1, "F_GETFL on fd 2");
            assert!(
                libc::fcntl(2, libc::F_SETFL, flags | libc::O_NONBLOCK) != -1,
                "set fd 2 nonblocking"
            );
            let buf = [b'x'; 4096];
            loop {
                let n = libc::write(2, buf.as_ptr() as *const libc::c_void, buf.len());
                if n < 0 {
                    match std::io::Error::last_os_error().raw_os_error() {
                        Some(libc::EINTR) => continue,
                        _ => break, // EAGAIN: the pipe is now full
                    }
                }
                if n == 0 {
                    break;
                }
            }
            assert!(
                libc::fcntl(2, libc::F_SETFL, flags) != -1,
                "restore fd 2 to blocking"
            );
            // Watchdog: if recovery blocks on the saturated fd 2, SIGALRM terminates
            // the process and the parent observes a non-zero status.
            libc::alarm(15);
        }

        // Install the production hook and drive a status-worker panic. If the hook
        // suppresses the pump thread's blocking stderr write, recovery completes and
        // we reach the clean exit below; otherwise the default hook blocks on fd 2.
        ensure_pump_panic_hook();
        let latch = Arc::new(FirstFault::default());
        let mut coordinator =
            StatusCoordinator::spawn(Box::new(PanicStore), Some(Arc::clone(&latch))).unwrap();
        let _ = coordinator.submit_required(running_status());
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        assert!(latch.observed(), "the worker panic latched the first fault");
        assert!(
            settlement.terminal_failure().is_some(),
            "a worker panic is a terminal failure, never silent success"
        );
        // Bypass libtest's result printing (which would write to the saturated fd 2)
        // and report success through the exit status the parent checks.
        std::process::exit(0);
    }

    /// A store that fails both the Complete write and the Failed fallback with
    /// distinct messages, so the composite diagnostics can be checked.
    struct FailBothStore {
        complete_msg: String,
        failed_msg: String,
    }

    impl StatusStore for FailBothStore {
        fn write(&mut self, status: &PumpStatus) -> Result<(), String> {
            match status.state {
                PumpState::Running => Ok(()),
                PumpState::Complete => Err(self.complete_msg.clone()),
                PumpState::Failed => Err(self.failed_msg.clone()),
            }
        }
    }

    #[test]
    fn bound_error_is_utf8_safe_and_bounded() {
        // Re-audit regression: a long multibyte error is bounded without splitting a
        // UTF-8 boundary (constructing the String would panic if it did).
        let long = "€".repeat(5000); // 3 bytes each, boundary lands mid-character
        let bounded = bound_error(&long);
        assert!(
            bounded.len() <= MAX_STATUS_ERROR_LEN,
            "bounded to the total cap including the marker, got {}",
            bounded.len()
        );
        assert!(bounded.ends_with("…[truncated]"));
        // A short message is returned unchanged.
        assert_eq!(bound_error("short"), "short");
    }

    #[test]
    fn composite_diagnostics_are_distinct_and_bounded() {
        // Re-audit regression: when both the Complete write and its Failed fallback
        // fail, the settlement preserves the two DISTINCT errors, each bounded.
        let store = FailBothStore {
            complete_msg: "C".repeat(5000),
            failed_msg: "F".repeat(5000),
        };
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let sink = SaturatedSink::new();
        let err = drain_with_store(
            Cursor::new(b"{\"a\":1}\n".to_vec()),
            &transcript,
            Some(Box::new(store)),
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .expect_err("both terminal writes failing fails the drain");

        assert!(
            err.message().starts_with('C'),
            "the primary is the Complete write failure"
        );
        assert!(
            err.message().len() <= MAX_STATUS_ERROR_LEN,
            "the primary is bounded"
        );
        let fallback = err
            .fallback_error()
            .expect("the Failed fallback also failed");
        assert!(fallback.starts_with('F'), "the fallback error is distinct");
        assert!(
            fallback.len() <= MAX_STATUS_ERROR_LEN,
            "the fallback error is bounded"
        );
        assert_ne!(
            err.message(),
            fallback,
            "the Complete and Failed errors are preserved distinctly"
        );
    }

    /// A store that fails the Complete write and PANICS on the Failed fallback, so a
    /// fallback panic (not merely a failure) can be exercised.
    struct FailCompletePanicFallbackStore;

    impl StatusStore for FailCompletePanicFallbackStore {
        fn write(&mut self, status: &PumpStatus) -> Result<(), String> {
            match status.state {
                PumpState::Running => Ok(()),
                PumpState::Complete => Err("simulated Complete write failure".to_string()),
                PumpState::Failed => panic!("simulated Failed fallback panic"),
            }
        }
    }

    #[test]
    fn fallback_panic_preserves_settlement_and_worker_diagnostics_distinctly() {
        // B4: when the Complete write fails and the Failed fallback write PANICS, the
        // settlement preserves the Complete settlement failure AND the worker panic as
        // DISTINCT diagnostics — a fallback panic never erases the primary settlement
        // error — and every submission stays accounted so the balance holds.
        let latch = Arc::new(FirstFault::default());
        let mut coordinator = StatusCoordinator::spawn(
            Box::new(FailCompletePanicFallbackStore),
            Some(Arc::clone(&latch)),
        )
        .unwrap();
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        assert!(
            settlement
                .settlement_error
                .as_deref()
                .is_some_and(|e| e.contains("simulated Complete write failure")),
            "the Complete settlement failure survives the fallback panic: {:?}",
            settlement.settlement_error
        );
        assert_eq!(
            settlement.worker_error.as_deref(),
            Some(STATUS_WORKER_PANIC),
            "the fallback panic is preserved distinctly as the worker error"
        );
        assert!(
            settlement
                .fallback_error
                .as_deref()
                .is_some_and(|e| e.contains(STATUS_WORKER_PANIC)),
            "the fallback write's own failure is attributed to the distinct fallback_error, \
             not only the generic worker error: {:?}",
            settlement.fallback_error
        );
        assert!(
            latch.observed(),
            "the terminal-settlement failure latched the first fault before the fallback"
        );
        assert!(
            settlement.diagnostics.is_balanced(),
            "the balance holds across a failed Complete plus a panicking fallback: {:?}",
            settlement.diagnostics
        );
    }

    #[test]
    fn settlement_leaves_no_pending_slot_queue_or_active_write() {
        // B1/B3 quiescence: after settlement every internal slot is proven empty — no
        // pending periodic snapshot, no queued required status, no unwritten terminal,
        // and no in-flight write — so the balanced terminal diagnostics account for
        // every submission with nothing left behind.
        let (store, _writes) = RecordingStore::new();
        let mut coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();
        coordinator.submit_periodic(running_status());
        coordinator.submit_required(running_status()).unwrap();
        coordinator.submit_periodic(running_status());
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        assert!(
            coordinator.is_quiescent(),
            "no coalescing slot, required queue, terminal, or in-flight write remains \
             after settlement"
        );
        assert!(
            settlement.diagnostics.is_balanced(),
            "every submission is accounted at settlement: {:?}",
            settlement.diagnostics
        );
    }

    /// A store whose every write fails with a pathologically long error, so the
    /// bounding of the error returned to a required-status caller can be exercised.
    struct LongErrorStore;

    impl StatusStore for LongErrorStore {
        fn write(&mut self, _status: &PumpStatus) -> Result<(), String> {
            Err("x".repeat(5000))
        }
    }

    #[test]
    fn required_write_failure_returned_to_caller_is_bounded() {
        // B1/B4 bounding: a required-status write that fails with a pathological
        // 5000-char error returns a BOUNDED typed failure to the caller — the ack path
        // is bounded at the constructor, not only in the retained diagnostics.
        let coordinator = StatusCoordinator::spawn(Box::new(LongErrorStore), None).unwrap();
        let err = coordinator
            .submit_required(running_status())
            .expect_err("a failed required write is a typed failure");
        assert!(
            err.message().len() <= MAX_STATUS_ERROR_LEN,
            "the required failure returned to the caller is bounded, got {}",
            err.message().len()
        );
        assert!(
            err.message().ends_with("…[truncated]"),
            "the long required error is truncated with the marker: {}",
            err.message()
        );
    }

    #[test]
    fn persisted_document_multibyte_errors_are_bounded() {
        // B4: the actual persisted-document builder bounds a pathological multibyte
        // primary AND periodic error within the total cap without splitting a UTF-8
        // boundary (constructing the String would panic if it did).
        let long = "€".repeat(5000);
        let status = build_status(
            PumpState::Failed,
            0,
            &PumpSummary::default(),
            Some(&long),
            Some(&long),
            StatusTransportDiagnostics::default(),
        );
        let persisted_error = status.error.expect("a Failed document carries its error");
        let persisted_periodic = status
            .periodic_error
            .expect("the periodic error is retained");
        assert!(
            persisted_error.len() <= MAX_STATUS_ERROR_LEN,
            "the persisted primary error is bounded, got {}",
            persisted_error.len()
        );
        assert!(
            persisted_periodic.len() <= MAX_STATUS_ERROR_LEN,
            "the persisted periodic error is bounded, got {}",
            persisted_periodic.len()
        );
    }

    #[test]
    fn persisted_on_disk_multibyte_failed_document_is_bounded() {
        // B4: drive the REAL coordinator through a file-backed store and read the
        // persisted Failed document back FROM DISK. A pathological multibyte primary
        // error is bounded within the total cap, stays valid UTF-8 (deserialization
        // would fail if a codepoint were split), and the on-disk transport accounting
        // is balanced — proving the bound holds at the actual persisted boundary, not
        // only in the in-memory builder.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript-pump.json");
        let mut coordinator = StatusCoordinator::spawn(file_status_store(&path), None).unwrap();
        coordinator.submit_required(running_status()).unwrap();
        let long = "€".repeat(5000);
        let settlement =
            coordinator.finish(TerminalStatusSpec::failed(0, PumpSummary::default(), &long));

        // Read the document the coordinator actually wrote to disk.
        let persisted: PumpStatus = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted.state, PumpState::Failed);
        let persisted_error = persisted
            .error
            .expect("the on-disk Failed document carries its error");
        assert!(
            persisted_error.len() <= MAX_STATUS_ERROR_LEN,
            "the on-disk primary error is bounded, got {}",
            persisted_error.len()
        );
        assert!(
            persisted_error.chars().next() == Some('€'),
            "the bound preserved whole codepoints, never splitting one"
        );
        assert!(
            persisted.transport.is_balanced(),
            "the on-disk transport accounting is balanced: {:?}",
            persisted.transport
        );
        // The returned settlement agrees with what landed on disk.
        assert!(settlement.diagnostics.is_balanced());
    }

    #[test]
    fn real_path_transport_last_error_is_bounded() {
        // B1/B3: on the real coordinator path, a store that fails every write with a
        // pathological error leaves a BOUNDED `last_error` in the terminal transport
        // diagnostics, and the balance still holds.
        let mut coordinator = StatusCoordinator::spawn(Box::new(LongErrorStore), None).unwrap();
        let _ = coordinator.submit_required(running_status());
        let settlement = coordinator.finish(TerminalStatusSpec::failed(
            0,
            PumpSummary::default(),
            "primary",
        ));
        let last_error = settlement
            .diagnostics
            .last_error
            .clone()
            .expect("a failed write records a last error");
        assert!(
            last_error.len() <= MAX_STATUS_ERROR_LEN,
            "the retained transport last_error is bounded, got {}",
            last_error.len()
        );
        assert!(
            settlement.diagnostics.is_balanced(),
            "the balance holds despite the failing writes: {:?}",
            settlement.diagnostics
        );
    }

    #[test]
    fn running_document_carries_projected_transport() {
        // Re-audit regression: a Running document carries live projected transport
        // diagnostics, not zeroed defaults, and that projection is self-consistent.
        let (store, writes) = RecordingStore::new();
        let coordinator = StatusCoordinator::spawn(Box::new(store), None).unwrap();
        coordinator.submit_required(running_status()).unwrap();

        let writes = writes.lock().unwrap();
        let running = &writes[0];
        assert_eq!(running.state, PumpState::Running);
        assert_eq!(
            running.transport.submitted, 1,
            "the Running document projects the live submission count, not zeros"
        );
        assert_eq!(running.transport.written, 1, "its own write is projected");
        assert!(running.transport.is_balanced());
    }

    #[test]
    fn successful_capture_returns_balanced_transport() {
        // Re-audit regression: a successful capture returns the final balanced
        // transport accounting in its summary, symmetric with the error path.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let status = status_path_for(&transcript);
        let sink = SaturatedSink::new();
        let summary = drain(
            Cursor::new(b"{\"a\":1}\n".to_vec()),
            &transcript,
            Some(&status),
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .unwrap();
        // Exactly the initial Running and the terminal are submitted and written, so
        // the balance is terminal-inclusive and no snapshot remained pending.
        assert_eq!(summary.transport.submitted, 2, "initial Running + terminal");
        assert_eq!(summary.transport.written, 2, "both persisted");
        assert_eq!(summary.transport.coalesced, 0);
        assert_eq!(summary.transport.dropped, 0);
        assert_eq!(summary.transport.disconnected, 0);
        assert_eq!(summary.transport.write_failures, 0);
        assert!(summary.transport.is_balanced());
        assert_eq!(
            summary.provider_timing,
            ProviderTimingDiagnostics::default(),
            "ordinary provider activity must not be classified as wait"
        );
    }

    #[test]
    fn claude_api_retry_records_provider_wait_diagnostics() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let status = status_path_for(&transcript);
        let sink = SaturatedSink::new();
        let input = concat!(
            "{\"type\":\"system\",\"subtype\":\"api_retry\",\"retry_delay_ms\":10000}\n",
            "{\"type\":\"assistant\",\"message\":{}}\n"
        );

        let summary = drain(
            Cursor::new(input.as_bytes()),
            &transcript,
            Some(&status),
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .unwrap();

        assert_eq!(summary.provider_timing.retry_count, 1);
        assert_eq!(summary.provider_timing.resume_at_ms, None);
        let persisted: PumpStatus =
            serde_json::from_slice(&std::fs::read(status).unwrap()).unwrap();
        assert_eq!(persisted.schema_version, PUMP_STATUS_SCHEMA_VERSION);
        assert_eq!(persisted.provider_timing.retry_count, 1);
        assert_eq!(persisted.provider_timing.resume_at_ms, None);
    }

    #[test]
    fn provider_wait_accumulates_only_declared_interval() {
        let mut timing = ProviderTimingState {
            retry_count: 1,
            settled_wait_ms: 200,
            waiting_since_ms: Some(1_000),
            resume_at_ms: Some(1_500),
        };

        assert_eq!(timing.snapshot(1_250).wait_ms, 450);
        timing.finish_wait(2_000);

        assert_eq!(timing.settled_wait_ms, 700);
        assert_eq!(timing.snapshot(2_000).resume_at_ms, None);
    }

    #[test]
    fn legacy_pump_status_without_provider_timing_deserializes() {
        let legacy = r#"{
            "schema_version":3,
            "state":"complete",
            "started_at_ms":1,
            "updated_at_ms":2,
            "bytes":3,
            "records":1,
            "dropped_console":1,
            "transport":{
                "submitted":2,
                "written":2,
                "coalesced":0,
                "dropped":0,
                "disconnected":0,
                "write_failures":0
            }
        }"#;

        let status: PumpStatus = serde_json::from_str(legacy).unwrap();

        assert_eq!(status.provider_timing, ProviderTimingDiagnostics::default());
    }

    #[test]
    fn terminal_submission_is_counted_in_diagnostics() {
        // Audit regression: the terminal write is itself accounted as a submission
        // and its result recorded, and the persisted document projects its own write
        // so the accounting is self-consistent and balanced including the terminal.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript-pump.json");
        let mut coordinator = StatusCoordinator::spawn(file_status_store(&path), None).unwrap();
        coordinator.submit_required(running_status()).unwrap();
        let settlement =
            coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        // One required Running plus the terminal write.
        assert_eq!(settlement.diagnostics.submitted, 2, "Running + terminal");
        assert_eq!(settlement.diagnostics.written, 2, "both persisted");
        assert!(settlement.diagnostics.is_balanced());

        let persisted: PumpStatus = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted.transport.submitted, 2, "the doc counts itself");
        assert_eq!(persisted.transport.written, 2);
        assert!(persisted.transport.is_balanced());
    }

    /// A sink that records every delivered preview and always accepts.
    struct RecordingSink {
        previews: Mutex<Vec<Vec<u8>>>,
    }

    impl RecordingSink {
        fn new() -> Self {
            Self {
                previews: Mutex::new(Vec::new()),
            }
        }
    }

    impl PreviewSink for RecordingSink {
        fn deliver(&self, preview: &[u8]) -> bool {
            self.previews.lock().unwrap().push(preview.to_vec());
            true
        }
    }

    #[test]
    fn oversized_record_does_not_block_later_records() {
        // A record larger than the OS pipe capacity (64 KiB) followed by a
        // second record: every byte must persist in order and draining must
        // continue past the oversized record through EOF.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");

        let big = "x".repeat(70_350);
        let mut input = Vec::new();
        input.extend_from_slice(format!("{{\"type\":\"big\",\"data\":\"{big}\"}}\n").as_bytes());
        input.extend_from_slice(b"{\"type\":\"after\"}\n");

        let sink = RecordingSink::new();
        let summary = drain(
            Cursor::new(input.clone()),
            &path,
            None,
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .unwrap();

        let persisted = std::fs::read(&path).unwrap();
        assert_eq!(
            persisted, input,
            "every emitted byte must persist exactly and in order"
        );
        assert_eq!(summary.bytes, input.len() as u64);
        assert_eq!(
            summary.records, 2,
            "draining must continue past the oversized record"
        );
    }

    /// A sink that refuses every preview, modelling a blocked or disconnected
    /// console, but records how many it was offered.
    struct SaturatedSink {
        offered: Mutex<u64>,
    }

    impl SaturatedSink {
        fn new() -> Self {
            Self {
                offered: Mutex::new(0),
            }
        }
    }

    impl PreviewSink for SaturatedSink {
        fn deliver(&self, _preview: &[u8]) -> bool {
            *self.offered.lock().unwrap() += 1;
            false
        }
    }

    #[test]
    fn saturated_console_does_not_stop_transcript_capture() {
        // A console that refuses every preview must not stop canonical capture:
        // every byte still persists and the pump accounts for each dropped
        // preview.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");

        let mut input = Vec::new();
        for i in 0..5 {
            input.extend_from_slice(format!("{{\"type\":\"rec\",\"n\":{i}}}\n").as_bytes());
        }

        let sink = SaturatedSink::new();
        let summary = drain(
            Cursor::new(input.clone()),
            &path,
            None,
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            input,
            "a saturated console must not cost the transcript any bytes"
        );
        assert_eq!(summary.records, 5);
        assert_eq!(
            summary.dropped_console, 5,
            "every undelivered preview must be counted"
        );
        assert_eq!(*sink.offered.lock().unwrap(), 5);
    }

    #[test]
    fn pump_status_moves_atomically_through_terminal_states() {
        // On success the adjacent status reaches `complete` with the observed
        // counters, schema version, and no error; on failure it reaches
        // `failed` and names the cause. Both are written atomically, so each
        // read parses.
        let dir = tempfile::tempdir().unwrap();

        // Success path.
        let transcript = dir.path().join("transcript.jsonl");
        let status = status_path_for(&transcript);
        let mut input = Vec::new();
        input.extend_from_slice(b"{\"type\":\"a\"}\n{\"type\":\"b\"}\n");
        let sink = SaturatedSink::new();
        drain(
            Cursor::new(input),
            &transcript,
            Some(&status),
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .unwrap();

        let persisted: PumpStatus =
            serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
        assert_eq!(persisted.schema_version, PUMP_STATUS_SCHEMA_VERSION);
        assert_eq!(persisted.state, PumpState::Complete);
        assert_eq!(persisted.records, 2);
        assert_eq!(persisted.dropped_console, 2);
        assert!(persisted.bytes > 0);
        assert!(persisted.updated_at_ms >= persisted.started_at_ms);
        assert!(persisted.error.is_none());

        // Failure path: a transcript path that is a directory cannot be opened.
        let bad_transcript = dir.path().join("as-dir.jsonl");
        std::fs::create_dir(&bad_transcript).unwrap();
        let bad_status = status_path_for(&bad_transcript);
        let err = drain(
            Cursor::new(Vec::new()),
            &bad_transcript,
            Some(&bad_status),
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .unwrap_err();
        assert!(err.message().contains("create transcript"));

        let failed: PumpStatus =
            serde_json::from_slice(&std::fs::read(&bad_status).unwrap()).unwrap();
        assert_eq!(failed.state, PumpState::Failed);
        assert!(
            failed
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("create transcript"),
            "the terminal failure must name the specific pump error"
        );
    }

    #[test]
    fn oversized_console_preview_is_bounded() {
        // A record far larger than the console preview limit yields a bounded,
        // lossy preview ending in the truncation marker, while the full record
        // survives only in the canonical transcript.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");

        let limit = 128;
        let big = "y".repeat(4096);
        let mut input = Vec::new();
        input.extend_from_slice(format!("{{\"big\":\"{big}\"}}\n").as_bytes());

        let sink = RecordingSink::new();
        let config = TranscriptPumpConfig {
            console_preview_limit: limit,
            ..TranscriptPumpConfig::default()
        };
        drain(Cursor::new(input.clone()), &path, None, &sink, &config).unwrap();

        let previews = sink.previews.lock().unwrap();
        assert_eq!(previews.len(), 1);
        let preview = &previews[0];
        assert!(
            preview.ends_with(TRUNCATION_MARKER),
            "an oversized preview must carry the truncation marker"
        );
        assert!(
            preview.len() <= limit,
            "the TOTAL rendered preview (payload + marker) must stay within the limit, got {} bytes",
            preview.len()
        );

        let persisted = std::fs::read(&path).unwrap();
        assert_eq!(
            persisted, input,
            "the complete record is preserved only in the canonical transcript"
        );
        assert!(
            persisted.len() > preview.len(),
            "the transcript record must exceed its bounded preview"
        );
    }

    #[test]
    fn invalid_utf8_is_preserved_and_capture_continues() {
        // Invalid UTF-8 in the stream must not terminate capture: the original
        // bytes are preserved and later records are still captured.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");

        let mut input = Vec::new();
        input.extend_from_slice(b"{\"type\":\"bad\",\"data\":\"");
        input.extend_from_slice(&[0xff, 0xfe, 0x80, 0x00]);
        input.extend_from_slice(b"\"}\n");
        input.extend_from_slice(b"{\"type\":\"after-invalid-utf8\"}\n");

        let sink = RecordingSink::new();
        let summary = drain(
            Cursor::new(input.clone()),
            &path,
            None,
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .unwrap();

        let persisted = std::fs::read(&path).unwrap();
        assert_eq!(persisted, input, "raw bytes must be preserved unchanged");
        assert_eq!(summary.records, 2, "capture continues after invalid UTF-8");
    }

    #[test]
    fn trailing_record_without_newline_is_captured_and_counted() {
        // A final record with no trailing newline must still be preserved
        // byte-exactly, counted as a record, drive its preview/drop accounting,
        // and be reflected in the terminal status.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let status = status_path_for(&path);

        let mut input = Vec::new();
        input.extend_from_slice(b"{\"type\":\"first\"}\n");
        // No trailing newline on the last record.
        input.extend_from_slice(b"{\"type\":\"last-no-newline\"}");

        let sink = SaturatedSink::new();
        let summary = drain(
            Cursor::new(input.clone()),
            &path,
            Some(&status),
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            input,
            "the trailing record's bytes must be preserved without a synthesized newline"
        );
        assert_eq!(
            summary.records, 2,
            "the newline-less trailing record is counted"
        );
        assert_eq!(
            summary.dropped_console, 2,
            "the trailing record's preview participates in drop accounting"
        );
        assert_eq!(*sink.offered.lock().unwrap(), 2);

        let persisted: PumpStatus =
            serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
        assert_eq!(persisted.state, PumpState::Complete);
        assert_eq!(persisted.records, 2);
        assert_eq!(persisted.dropped_console, 2);
        assert_eq!(persisted.bytes, input.len() as u64);
    }

    #[test]
    fn production_console_sink_declines_and_counts_every_preview() {
        // The production sink declines every preview: `deliver` reports the loss
        // and writes no bytes. Driven through a full drain, every record counts
        // as a dropped preview and canonical capture is byte-exact, so an
        // operator reading the status sees `dropped_console == records`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut input = Vec::new();
        for i in 0..6 {
            input.extend_from_slice(format!("{{\"n\":{i}}}\n").as_bytes());
        }

        let sink = console_preview_sink();
        assert!(
            !sink.deliver(b"any preview"),
            "the production sink must decline previews so none is counted delivered"
        );

        let summary = drain(
            Cursor::new(input.clone()),
            &path,
            None,
            sink,
            &TranscriptPumpConfig::default(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read(&path).unwrap(),
            input,
            "a declined console must not cost the transcript any bytes"
        );
        assert_eq!(summary.records, 6);
        assert_eq!(
            summary.dropped_console, summary.records,
            "every declined preview must be counted; dropped_console must equal records"
        );
    }

    #[test]
    fn unwritable_status_path_is_a_typed_terminal_failure() {
        // B8: the durable status must be independently observable. When it cannot
        // be persisted (here its parent directory does not exist), the drain
        // returns a typed transcript-pump infrastructure failure rather than
        // silently discarding the write and reporting success.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let status = dir.path().join("missing-dir/transcript-pump.json");

        let sink = SaturatedSink::new();
        let err = drain(
            Cursor::new(b"{\"a\":1}\n".to_vec()),
            &transcript,
            Some(&status),
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .expect_err("an unwritable required status must fail the pump");
        assert!(
            err.message().contains("persist pump status"),
            "the failure must name the status persistence problem: {err}"
        );
    }

    /// A reader that emits one record and then returns an I/O error on the next
    /// read, modelling a mid-stream stdout failure after real progress.
    struct ErrorAfterOneRecord {
        emitted: bool,
    }

    impl Read for ErrorAfterOneRecord {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.emitted {
                return Err(std::io::Error::other("simulated stdout read failure"));
            }
            self.emitted = true;
            let data = b"{\"rec\":1}\n";
            buf[..data.len()].copy_from_slice(data);
            Ok(data.len())
        }
    }

    #[test]
    fn required_status_failure_is_typed_and_preserves_counts() {
        // B8: two facets of the required-status contract.
        // (1) A required status that cannot be persisted fails the drain with a
        //     typed transcript-pump error rather than a silent success.
        // (2) When capture ends on a non-status failure AFTER real progress, the
        //     terminal Failed status preserves the byte and record counts observed
        //     before the failure, so the durable diagnostic is truthful.
        let dir = tempfile::tempdir().unwrap();

        // (1) Required status failure is typed.
        let transcript = dir.path().join("t1.jsonl");
        let status = dir.path().join("missing-dir/transcript-pump.json");
        let sink = SaturatedSink::new();
        let err = drain(
            Cursor::new(b"{\"a\":1}\n".to_vec()),
            &transcript,
            Some(&status),
            &sink,
            &TranscriptPumpConfig::default(),
        )
        .expect_err("an unwritable required status must fail the pump");
        assert!(
            err.message().contains("persist pump status"),
            "the required-status failure must be typed and named: {err}"
        );

        // (2) A mid-stream read failure ends capture; the terminal Failed status
        //     preserves the counts observed before the failure.
        let transcript2 = dir.path().join("t2.jsonl");
        let status2 = status_path_for(&transcript2);
        let sink2 = SaturatedSink::new();
        let err2 = drain(
            ErrorAfterOneRecord { emitted: false },
            &transcript2,
            Some(&status2),
            &sink2,
            &TranscriptPumpConfig::default(),
        )
        .expect_err("a mid-stream read failure fails the drain");
        assert!(
            err2.message().contains("read coder stdout"),
            "the failure must name the read error: {err2}"
        );
        let failed: PumpStatus = serde_json::from_slice(&std::fs::read(&status2).unwrap()).unwrap();
        assert_eq!(failed.state, PumpState::Failed);
        assert_eq!(
            failed.records, 1,
            "the terminal status preserves the record observed before the failure"
        );
        assert_eq!(
            failed.bytes,
            b"{\"rec\":1}\n".len() as u64,
            "and the bytes persisted before the failure"
        );
        assert!(
            failed
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("read coder stdout")
        );
    }

    #[test]
    fn preview_is_bounded_even_when_limit_is_below_the_marker() {
        // The configured limit bounds the TOTAL rendered preview for EVERY value,
        // including limits smaller than the truncation marker: 0, 1, and one below
        // the marker length must all yield a rendered preview within the limit,
        // while the canonical transcript still holds every byte.
        for &limit in &[0usize, 1, TRUNCATION_MARKER.len() - 1] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("t.jsonl");
            let big = "z".repeat(4096);
            let input = format!("{{\"b\":\"{big}\"}}\n").into_bytes();
            let sink = RecordingSink::new();
            let config = TranscriptPumpConfig {
                console_preview_limit: limit,
                ..TranscriptPumpConfig::default()
            };
            drain(Cursor::new(input.clone()), &path, None, &sink, &config).unwrap();

            let previews = sink.previews.lock().unwrap();
            assert_eq!(previews.len(), 1);
            assert!(
                previews[0].len() <= limit,
                "limit {limit}: the rendered preview must stay within the limit, got {}",
                previews[0].len()
            );
            drop(previews);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                input,
                "the canonical transcript still holds every byte"
            );
        }
    }

    /// A transcript writer that persists a bounded budget of bytes and then errors.
    struct WriteThenFail {
        budget: usize,
    }

    impl Write for WriteThenFail {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.budget == 0 {
                return Err(std::io::Error::other("simulated disk full"));
            }
            let n = self.budget.min(buf.len());
            self.budget -= n;
            Ok(n)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn partial_write_accounts_only_persisted_bytes_before_erroring() {
        // A transcript writer that persists a bounded prefix and then fails must
        // leave the byte and record counters reflecting exactly what reached the
        // transcript, not the whole chunk it was handed.
        let counters = SharedCounters::default();
        let mut line = PreviewLine::new(DEFAULT_CONSOLE_PREVIEW_LIMIT);
        let mut diagnostic_line = DiagnosticLine::default();
        let sink = SaturatedSink::new();
        let chunk = b"{\"rec\":1}\n{\"rec\":2}\n"; // 20 bytes, two records
        let mut writer = WriteThenFail { budget: 10 }; // persists exactly the first record
        let err = persist_chunk(
            &mut writer,
            chunk,
            &mut line,
            &mut diagnostic_line,
            &sink,
            &counters,
            Path::new("t.jsonl"),
        )
        .expect_err("the writer fails once its budget is exhausted");
        assert!(err.message().contains("write transcript"), "typed: {err}");
        let snap = counters.snapshot();
        assert_eq!(snap.bytes, 10, "only the persisted prefix is counted");
        assert_eq!(
            snap.records, 1,
            "only the record whose bytes were persisted is counted"
        );
    }

    /// A preview sink that panics on delivery, modelling a renderer that unwinds.
    struct PanicOnPreview;

    impl PreviewSink for PanicOnPreview {
        fn deliver(&self, _preview: &[u8]) -> bool {
            panic!("preview sink panicked");
        }
    }

    #[test]
    fn panicking_preview_sink_counts_the_dropped_record() {
        // A preview sink that panics must not silently lose a record: the loss is
        // accounted BEFORE delivery, so the caught-panic terminal status still
        // counts the record's preview as dropped and preserves the counters.
        static SINK: PanicOnPreview = PanicOnPreview;
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let status = status_path_for(&transcript);
        let mut pump = spawn_pump(
            Cursor::new(b"{\"rec\":1}\n".to_vec()),
            transcript.clone(),
            Some(status.clone()),
            &SINK,
            TranscriptPumpConfig::default(),
        )
        .unwrap();
        let outcome = pump.wait_terminal();
        pump.join();

        assert!(
            outcome.is_err(),
            "a panicking preview sink surfaces a typed pump failure"
        );
        let persisted: PumpStatus =
            serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
        assert_eq!(persisted.state, PumpState::Failed);
        assert_eq!(
            persisted.records, 1,
            "the record whose bytes persisted is counted"
        );
        assert_eq!(
            persisted.dropped_console, 1,
            "its preview, lost to the panic, is accounted as dropped"
        );
    }

    #[test]
    fn concurrent_captures_use_independent_configs() {
        // The resolved config travels WITH each launch, not through shared process
        // state, so two captures running at once each honor their OWN preview
        // limit. Under the old process-global config, one launch could overwrite
        // the other's threshold between resolution and pump spawn.
        let dir = tempfile::tempdir().unwrap();
        let big = "q".repeat(4096);
        let input = format!("{{\"x\":\"{big}\"}}\n").into_bytes();

        let run_one = |limit: usize, name: &str| -> usize {
            let path = dir.path().join(name);
            let sink = RecordingSink::new();
            let config = TranscriptPumpConfig {
                console_preview_limit: limit,
                ..TranscriptPumpConfig::default()
            };
            drain(Cursor::new(input.clone()), &path, None, &sink, &config).unwrap();
            let previews = sink.previews.lock().unwrap();
            previews[0].len()
        };

        let (a, b) = std::thread::scope(|s| {
            let ha = s.spawn(|| run_one(64, "a.jsonl"));
            let hb = s.spawn(|| run_one(256, "b.jsonl"));
            (ha.join().unwrap(), hb.join().unwrap())
        });

        assert!(
            a <= 64,
            "capture A must honor its own 64-byte limit, got {a}"
        );
        assert!(
            b <= 256,
            "capture B must honor its own 256-byte limit, got {b}"
        );
        assert_ne!(
            a, b,
            "each concurrent capture used its own config, not a shared global"
        );
    }

    /// A reader that emits one record and then panics on the next read.
    struct PanicAfterOneRecord {
        emitted: bool,
    }

    impl Read for PanicAfterOneRecord {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.emitted {
                panic!("simulated mid-stream pump panic");
            }
            self.emitted = true;
            let data = b"{\"rec\":1}\n";
            buf[..data.len()].copy_from_slice(data);
            Ok(data.len())
        }
    }

    #[test]
    fn pump_panic_preserves_counters_and_recovers_promptly() {
        // B6: a mid-stream pump panic is caught, reported as a typed failure, and
        // its terminal status preserves the counters accumulated before the panic
        // (not zeros). Recovery is prompt — the panic path never blocks — which
        // the process-wide hook keeps true even when stderr is saturated by
        // suppressing the pump thread's blocking default hook output.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let status = status_path_for(&transcript);

        let mut pump = spawn_pump(
            PanicAfterOneRecord { emitted: false },
            transcript.clone(),
            Some(status.clone()),
            console_preview_sink(),
            TranscriptPumpConfig::default(),
        )
        .unwrap();

        let started = Instant::now();
        let outcome = pump.wait_terminal();
        let elapsed = started.elapsed();
        pump.join();

        assert!(
            outcome.is_err(),
            "a panicking pump must report a typed failure"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "panic recovery must be prompt, not blocked; took {elapsed:?}"
        );

        let persisted: PumpStatus =
            serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
        assert_eq!(persisted.state, PumpState::Failed);
        assert_eq!(
            persisted.records, 1,
            "the terminal status must preserve counters accumulated before the panic"
        );
        assert!(persisted.error.is_some());
    }

    #[test]
    fn claude_startup_retry_records_remain_pre_progress() {
        let counters = SharedCounters::default();
        assert!(counters.claude_no_progress_retry_is_safe());

        counters.observe_partial_record();
        counters.observe_record(br#"{"type":"system","subtype":"init"}"#);
        counters.finish_complete_record_boundary();
        assert!(counters.claude_no_progress_retry_is_safe());
        counters.observe_partial_record();
        counters
            .observe_record(br#"{"type":"system","subtype":"api_retry","retry_delay_ms":1000}"#);
        counters.finish_complete_record_boundary();
        assert!(counters.claude_no_progress_retry_is_safe());
    }

    #[test]
    fn claude_assistant_record_marks_progress() {
        let counters = SharedCounters::default();
        counters.observe_partial_record();
        counters.observe_record(br#"{"type":"assistant","message":{}}"#);
        assert!(counters.claude_progress_observed());
        assert!(!counters.claude_no_progress_retry_is_safe());
    }

    #[test]
    fn claude_progress_classifier_fails_closed_on_malformed_or_unknown_records() {
        for record in [
            b"not-json".as_slice(),
            br#"{"type":"system","subtype":"mystery"}"#.as_slice(),
            br#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#.as_slice(),
        ] {
            let counters = SharedCounters::default();
            counters.observe_record(record);
            assert!(counters.claude_progress_observed());
            assert!(!counters.claude_no_progress_retry_is_safe());
        }
    }

    #[test]
    fn codex_response_wait_becomes_interruptible_without_an_active_tool() {
        let counters = SharedCounters::default();
        counters.observe_record(br#"{"type":"thread.started","thread_id":"thread-1"}"#);
        counters.observe_record(br#"{"type":"turn.started"}"#);

        let diagnostic = counters
            .codex_response_idle(Duration::ZERO)
            .expect("a response wait is eligible after its deadline");
        assert_eq!(diagnostic.session_id.as_deref(), Some("thread-1"));
        assert_eq!(diagnostic.phase.as_deref(), Some("awaiting-response"));
        assert_eq!(diagnostic.last_event.as_deref(), Some("turn.started"));
    }

    #[test]
    fn codex_active_tool_is_never_interruptible() {
        let counters = SharedCounters::default();
        counters.observe_record(br#"{"type":"thread.started","thread_id":"thread-1"}"#);
        counters.observe_record(br#"{"type":"turn.started"}"#);
        counters.observe_record(
            br#"{"type":"item.started","item":{"id":"tool-1","type":"command_execution"}}"#,
        );

        assert!(counters.codex_response_idle(Duration::ZERO).is_none());
    }

    #[test]
    fn codex_partial_record_disables_response_interruption() {
        let counters = SharedCounters::default();
        counters.observe_record(br#"{"type":"thread.started","thread_id":"thread-1"}"#);
        counters.observe_record(br#"{"type":"turn.started"}"#);
        counters.observe_partial_record();

        assert!(counters.codex_response_idle(Duration::ZERO).is_none());
    }

    #[test]
    fn malformed_layered_config_disables_automatic_claude_relaunch() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project.yaml");
        std::fs::write(
            &project,
            "provider-recovery:\n  claude:\n    no-progress-timeout-seconds: 0\n",
        )
        .unwrap();

        let resolved = resolve_config_from(&project, None);

        assert_eq!(resolved.claude_no_progress_retries, 0);
        assert_eq!(resolved.read_chunk_size, DEFAULT_READ_CHUNK_SIZE);
    }

    /// A reader that emits `count` records, sleeping between each so the pump's
    /// status lifecycle can be observed advancing.
    struct PacedReader {
        remaining: usize,
        gap: Duration,
    }

    impl Read for PacedReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Ok(0);
            }
            std::thread::sleep(self.gap);
            self.remaining -= 1;
            let data = b"{\"tick\":1}\n";
            buf[..data.len()].copy_from_slice(data);
            Ok(data.len())
        }
    }

    #[test]
    fn pump_status_advances_through_running_to_terminal() {
        // Looking only at the final JSON does not prove lifecycle wiring. Drive a
        // paced reader while a poller samples the adjacent status atomically, and
        // require observing a Running state, at least one Running with an
        // advancing record count, and the terminal Complete state.
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let status = status_path_for(&transcript);
        let config = TranscriptPumpConfig {
            status_flush_interval: Duration::from_millis(15),
            ..TranscriptPumpConfig::default()
        };

        let poll_status = Arc::new(std::sync::Mutex::new(Vec::<(PumpState, u64)>::new()));
        let poll_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let poller = {
            let status = status.clone();
            let poll_status = Arc::clone(&poll_status);
            let poll_done = Arc::clone(&poll_done);
            std::thread::spawn(move || {
                while !poll_done.load(Ordering::Relaxed) {
                    if let Ok(bytes) = std::fs::read(&status) {
                        if let Ok(s) = serde_json::from_slice::<PumpStatus>(&bytes) {
                            poll_status.lock().unwrap().push((s.state, s.records));
                        }
                    }
                    std::thread::sleep(Duration::from_millis(3));
                }
            })
        };

        let sink = SaturatedSink::new();
        let summary = drain(
            PacedReader {
                remaining: 6,
                gap: Duration::from_millis(40),
            },
            &transcript,
            Some(&status),
            &sink,
            &config,
        )
        .unwrap();
        poll_done.store(true, Ordering::Relaxed);
        poller.join().unwrap();

        assert_eq!(summary.records, 6);
        let samples = poll_status.lock().unwrap();
        assert!(
            samples
                .iter()
                .any(|(state, _)| *state == PumpState::Running),
            "a Running state must be observable during capture"
        );
        assert!(
            samples
                .iter()
                .any(|(state, records)| *state == PumpState::Running
                    && *records > 0
                    && *records < 6),
            "at least one Running snapshot must show an advancing record count"
        );
        // The terminal Complete status is written synchronously before drain
        // returns, so read it directly: it must be the final atomic state.
        let terminal: PumpStatus =
            serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
        assert_eq!(terminal.state, PumpState::Complete);
        assert_eq!(terminal.records, 6);
    }
}
