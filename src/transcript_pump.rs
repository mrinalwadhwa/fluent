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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock};
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
pub const PUMP_STATUS_SCHEMA_VERSION: u32 = 3;

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

/// Operator-facing thresholds that shape console previews and status flushes.
/// Resolved from layered configuration; see `config::resolve_transcript_pump_config`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptPumpConfig {
    /// Bytes read per stdout chunk.
    pub read_chunk_size: usize,
    /// Maximum bytes of the TOTAL rendered console preview for one record,
    /// including any truncation marker.
    pub console_preview_limit: usize,
    /// Minimum interval between periodic `Running` status flushes.
    pub status_flush_interval: Duration,
}

impl Default for TranscriptPumpConfig {
    fn default() -> Self {
        Self {
            read_chunk_size: DEFAULT_READ_CHUNK_SIZE,
            console_preview_limit: DEFAULT_CONSOLE_PREVIEW_LIMIT,
            status_flush_interval: Duration::from_millis(1000),
        }
    }
}

// Transcript-pump thresholds are resolved once per launch and threaded into the
// coder as an immutable value (see `coder::TranscriptCapture`). There is no
// process-global config: a concurrent launch cannot overwrite another capture's
// resolved thresholds between resolution and pump spawn.

/// Resolve this project's layered transcript-pump thresholds into an immutable
/// value the caller threads into its launch. A malformed or unreadable
/// configuration fails closed to the built-in defaults — every field, not just
/// the one that failed to parse — because capture correctness never depends on
/// these diagnostics knobs.
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
    resolved: Result<crate::config::ResolvedTranscriptPumpConfig, crate::config::FollowUpConfigError>,
) -> TranscriptPumpConfig {
    match resolved {
        Ok(resolved) => TranscriptPumpConfig {
            console_preview_limit: resolved.console_preview_limit.value as usize,
            status_flush_interval: Duration::from_millis(
                resolved.status_flush_interval_ms.value as u64,
            ),
            ..TranscriptPumpConfig::default()
        },
        Err(_) => TranscriptPumpConfig::default(),
    }
}

/// The adjacent status document path for a transcript: `transcript-pump.json`
/// beside the transcript file.
pub fn status_path_for(transcript_path: &Path) -> PathBuf {
    transcript_path.with_file_name("transcript-pump.json")
}

/// Cap a retained or persisted error message so a pathological error string can
/// never bloat the shared diagnostics or the status document.
const MAX_STATUS_ERROR_LEN: usize = 2000;

fn bound_error(message: &str) -> String {
    if message.len() <= MAX_STATUS_ERROR_LEN {
        return message.to_string();
    }
    let mut end = MAX_STATUS_ERROR_LEN;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…[truncated]", &message[..end])
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
    /// The final balanced status transport accounting, populated when the capture
    /// settles successfully (it is carried on the typed error otherwise).
    pub transport: StatusTransportDiagnostics,
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
            == self.written + self.coalesced + self.dropped + self.disconnected + self.write_failures
    }
}

/// Shared, panic-safe pump counters. They live behind atomics so the pump's
/// panic path can read the values accumulated before the panic instead of
/// reporting zeros.
#[derive(Default)]
struct SharedCounters {
    bytes: AtomicU64,
    records: AtomicU64,
    dropped_console: AtomicU64,
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
        PumpSummary {
            transport: StatusTransportDiagnostics::default(),
            bytes: self.bytes.load(Ordering::Relaxed),
            records: self.records.load(Ordering::Relaxed),
            dropped_console: self.dropped_console.load(Ordering::Relaxed),
        }
    }
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
    drain_with_first_fault(reader, transcript_path, store, preview, config, &counters, None)
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
    drain_with_first_fault(reader, transcript_path, store, preview, config, &counters, None)
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
    let mut last_flush = Instant::now();

    loop {
        let read = reader
            .read(&mut buf)
            .map_err(|err| TranscriptPumpError::new(format!("read coder stdout: {err}")))?;
        if read == 0 {
            break;
        }
        let chunk = &buf[..read];
        persist_chunk(&mut file, chunk, &mut line, preview, counters, transcript_path)?;

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
        deliver_preview(&mut line, preview, counters);
    }

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
    preview: &dyn PreviewSink,
    counters: &SharedCounters,
    transcript_path: &Path,
) -> Result<(), TranscriptPumpError> {
    let mut written = 0;
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
                for &byte in &chunk[written..written + n] {
                    if byte == b'\n' {
                        counters.add_record();
                        deliver_preview(line, preview, counters);
                    } else {
                        line.push(byte);
                    }
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
    Ok(())
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
            settlement_error: None,
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

/// A required status command awaiting acknowledgement. The submitter blocks on the
/// one-shot `ack` until the worker has attempted its persistence, so a terminal
/// acknowledgement can never be followed by a later Running write.
struct RequiredCommand {
    status: PumpStatus,
    ack: SyncSender<Result<(), String>>,
}

/// What the worker does next, decided atomically under one lock so required
/// statuses drain FIFO, the newest periodic drains before the terminal, and the
/// terminal write is always last.
enum Work {
    Required(RequiredCommand),
    Periodic(PumpStatus),
    Terminal(TerminalStatusSpec),
    Idle,
}

/// Which submission a write persisted, so periodic failures stay distinct from
/// required and terminal failures in the diagnostics.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteKind {
    Periodic,
    Required,
    Terminal,
}

/// Shared coordinator state. Every submission mutates it under one lock and every
/// category the submitter can decide (coalesced, dropped, disconnected) is recorded
/// there immediately, so the balance invariant holds even for submissions the
/// worker never sees.
struct CoordinatorInner {
    /// The newest pending periodic snapshot; a replaced one is counted coalesced.
    periodic: Option<PumpStatus>,
    /// Required statuses awaiting the worker, processed front-first.
    required: VecDeque<RequiredCommand>,
    /// The terminal status, set once by `finish`; the worker writes it last.
    terminal: Option<TerminalStatusSpec>,
    /// Exact accounting of every submission.
    diagnostics: StatusTransportDiagnostics,
    /// The category of the write the worker is currently attempting, set before
    /// each `store.write` and cleared once its result is recorded. If the worker
    /// unwinds *inside* `store.write`, this remains set, so reconciliation accounts
    /// the attempted write truthfully as a write failure rather than sweeping it
    /// into the disconnected bucket (which would hide that the store was reached).
    active_write: Option<WriteKind>,
    /// The last best-effort *periodic* write failure, kept distinct from required
    /// and terminal failures so a required failure never masquerades as periodic.
    periodic_error: Option<String>,
    /// A terminal (Complete) status write failure, recorded before any Failed
    /// fallback so a fallback panic cannot lose the primary Complete error.
    settlement_error: Option<String>,
    /// Terminal sealing has begun; no further periodic snapshot is written.
    sealed: bool,
    /// The worker has fully shut down; further submissions are disconnected.
    shutdown: bool,
}

struct SharedStatusState {
    inner: Mutex<CoordinatorInner>,
}

impl SharedStatusState {
    fn next_work(&self) -> Work {
        let mut inner = self.inner.lock().unwrap();
        if let Some(cmd) = inner.required.pop_front() {
            return Work::Required(cmd);
        }
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

    /// Mark that the worker is about to attempt a write of `kind`. Paired with
    /// `record_write`, which clears it. A still-set marker means the worker
    /// unwound inside `store.write`.
    fn begin_write(&self, kind: WriteKind) {
        self.inner.lock().unwrap().active_write = Some(kind);
    }

    fn record_write(&self, result: &Result<(), String>, kind: WriteKind) {
        let mut inner = self.inner.lock().unwrap();
        inner.active_write = None;
        match result {
            Ok(()) => inner.diagnostics.written += 1,
            Err(err) => {
                let bounded = bound_error(err);
                inner.diagnostics.write_failures += 1;
                inner.diagnostics.last_error = Some(bounded.clone());
                if kind == WriteKind::Periodic {
                    inner.periodic_error = Some(bounded);
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

    /// Test-only proof that settlement left nothing pending: no coalescing slot, no
    /// queued required status, no unwritten terminal, and no in-flight write.
    #[cfg(test)]
    fn is_quiescent(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.periodic.is_none()
            && inner.required.is_empty()
            && inner.terminal.is_none()
            && inner.active_write.is_none()
    }

    /// Reconcile work the worker abandoned when it disconnected or panicked before
    /// acknowledgement: acknowledge any pending required submission with an error so
    /// its submitter never hangs, clear the pending periodic, and account every
    /// still-uncategorized submission as disconnected so the balance invariant holds.
    fn reconcile_abandoned(&self) {
        let mut inner = self.inner.lock().unwrap();
        // A write that was in flight when the worker unwound is accounted truthfully
        // as a write failure carrying the panic as its error — never swept into the
        // disconnected bucket, which would falsely report the store was never
        // attempted. A periodic write also latches into `periodic_error`.
        if let Some(kind) = inner.active_write.take() {
            let bounded = bound_error(STATUS_WORKER_PANIC);
            inner.diagnostics.write_failures += 1;
            inner.diagnostics.last_error = Some(bounded.clone());
            if kind == WriteKind::Periodic {
                inner.periodic_error = Some(bounded);
            }
        }
        while let Some(cmd) = inner.required.pop_front() {
            let _ = cmd
                .ack
                .send(Err("persist pump status: status worker disconnected".to_string()));
        }
        inner.periodic = None;
        inner.shutdown = true;
        let d = &inner.diagnostics;
        let accounted =
            d.written + d.coalesced + d.dropped + d.disconnected + d.write_failures;
        if d.submitted > accounted {
            inner.diagnostics.disconnected += d.submitted - accounted;
        }
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
    fn spawn(
        mut store: Box<dyn StatusStore>,
        first_fault: Option<Arc<FirstFault>>,
    ) -> Result<Self, TranscriptPumpError> {
        let shared = Arc::new(SharedStatusState {
            inner: Mutex::new(CoordinatorInner {
                periodic: None,
                required: VecDeque::new(),
                terminal: None,
                diagnostics: StatusTransportDiagnostics::default(),
                active_write: None,
                periodic_error: None,
                settlement_error: None,
                sealed: false,
                shutdown: false,
            }),
        });
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
                    // Acknowledge and account any work the panic abandoned so no
                    // submitter hangs and the balance holds, then surface the panic
                    // while preserving any Complete and periodic errors already seen.
                    worker_shared.reconcile_abandoned();
                    let inner = worker_shared.inner.lock().unwrap();
                    StatusSettlement {
                        diagnostics: inner.diagnostics.clone(),
                        settlement_error: inner.settlement_error.clone(),
                        periodic_error: inner.periodic_error.clone(),
                        worker_error: Some(STATUS_WORKER_PANIC.to_string()),
                        ..StatusSettlement::default()
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

    /// Submit a required status and block until the worker acknowledges its
    /// persistence. A write failure or a worker that already shut down is a typed
    /// terminal infrastructure failure, because the durable diagnostic must be
    /// independently observable.
    fn submit_required(&self, status: PumpStatus) -> Result<(), TranscriptPumpError> {
        let (ack_tx, ack_rx) = sync_channel::<Result<(), String>>(1);
        {
            let mut inner = self.shared.inner.lock().unwrap();
            inner.diagnostics.submitted += 1;
            if inner.shutdown {
                inner.diagnostics.disconnected += 1;
                return Err(TranscriptPumpError::new(
                    "persist pump status: status coordinator already shut down",
                ));
            }
            inner.required.push_back(RequiredCommand {
                status,
                ack: ack_tx,
            });
        }
        self.wake();
        match ack_rx.recv() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => Err(TranscriptPumpError::new(err)),
            Err(_) => Err(TranscriptPumpError::new(
                "persist pump status: status worker disconnected",
            )),
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
        self.shared.reconcile_abandoned();
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
        // On an unwind that skipped `finish`, end the worker so no periodic write
        // lands after the caller stops using the coordinator: dropping the wake
        // sender disconnects the worker's receive, and joining it settles the thread.
        if let Some(join) = self.join.take() {
            self.wake = None;
            let _ = join.join();
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
                Work::Required(cmd) => {
                    let RequiredCommand { mut status, ack } = cmd;
                    // Stamp live projected transport diagnostics so the persisted
                    // Running document carries a self-consistent view, not zeros.
                    status.transport = shared.projected_write_diagnostics();
                    shared.begin_write(WriteKind::Required);
                    let result = store.write(&status);
                    shared.record_write(&result, WriteKind::Required);
                    let _ = ack.send(result);
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
        if wake_rx.recv().is_err() {
            // The coordinator was dropped without a terminal (an unwind that skipped
            // finish). Reconcile any abandoned work so no submitter hangs and the
            // balance holds.
            shared.reconcile_abandoned();
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
    let settlement_error = match write_accounted_terminal(shared, store, &spec) {
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
        let fallback_error = write_accounted_terminal(shared, store, &fallback)
            .err()
            .map(|err| bound_error(&err));
        StatusSettlement {
            diagnostics: shared.diagnostics(),
            settlement_error: Some(settlement_error),
            fallback_error,
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
) -> Result<(), String> {
    // Count this terminal write as a submission and project it as written so the
    // persisted document balances including itself.
    let (projected, periodic_error) = {
        let mut inner = shared.inner.lock().unwrap();
        inner.diagnostics.submitted += 1;
        inner.active_write = Some(WriteKind::Terminal);
        let mut projected = inner.diagnostics.clone();
        projected.written += 1;
        (projected, inner.periodic_error.clone())
    };
    let result = store.write(&spec.build(projected, periodic_error.as_deref()));
    shared.record_write(&result, WriteKind::Terminal);
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
    join: Option<JoinHandle<()>>,
}

impl PumpHandle {
    /// Whether the pump published its immutable first infrastructure fault. This is
    /// set before terminal status settlement, so supervision can terminate and reap
    /// the coder without waiting for a delayed or blocked terminal status write.
    pub fn first_fault_observed(&self) -> bool {
        self.first_fault.observed()
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
                    &counters,
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
        let settlement = coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

        // A, B, C periodics plus the terminal write are all submissions.
        assert_eq!(settlement.diagnostics.submitted, 4);
        assert_eq!(settlement.diagnostics.coalesced, 1, "the replaced snapshot B");
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
        let settlement = coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

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
            err.fallback_error().is_none(),
            "the Failed fallback persisted, so no fallback error"
        );

        let attempts = attempts.lock().unwrap();
        assert!(
            attempts.iter().any(|(s, ok)| *s == PumpState::Complete && !ok),
            "a Complete write was attempted and failed"
        );
        assert!(
            attempts.iter().any(|(s, ok)| *s == PumpState::Failed && *ok),
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
        let settlement = coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

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
        let settlement = coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));
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
        let settlement = coordinator.finish(TerminalStatusSpec::failed(0, PumpSummary::default(), "primary"));
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
            bounded.len() <= MAX_STATUS_ERROR_LEN + "…[truncated]".len(),
            "bounded to the cap plus the marker, got {}",
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
            err.message().len() <= MAX_STATUS_ERROR_LEN + "…[truncated]".len(),
            "the primary is bounded"
        );
        let fallback = err.fallback_error().expect("the Failed fallback also failed");
        assert!(fallback.starts_with('F'), "the fallback error is distinct");
        assert!(
            fallback.len() <= MAX_STATUS_ERROR_LEN + "…[truncated]".len(),
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
            err.message().len() <= MAX_STATUS_ERROR_LEN + "…[truncated]".len(),
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
        let settlement = coordinator.finish(TerminalStatusSpec::complete(0, PumpSummary::default()));

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
        let failed: PumpStatus =
            serde_json::from_slice(&std::fs::read(&status2).unwrap()).unwrap();
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
        let sink = SaturatedSink::new();
        let chunk = b"{\"rec\":1}\n{\"rec\":2}\n"; // 20 bytes, two records
        let mut writer = WriteThenFail { budget: 10 }; // persists exactly the first record
        let err = persist_chunk(
            &mut writer,
            chunk,
            &mut line,
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

        assert!(a <= 64, "capture A must honor its own 64-byte limit, got {a}");
        assert!(b <= 256, "capture B must honor its own 256-byte limit, got {b}");
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
