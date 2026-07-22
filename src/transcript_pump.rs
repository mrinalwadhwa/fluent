//! Drain coder stdout into the canonical transcript exactly.
//!
//! The pump reads raw fixed-size byte chunks and writes them, unmodified, to
//! the transcript file. It never decodes the stream as UTF-8 and never splits a
//! record before persisting it, so invalid bytes and records larger than the
//! enclosing pipe capacity are captured losslessly. Record boundaries are
//! detected incrementally only to drive a bounded, best-effort console preview
//! and to count records; they never gate the canonical byte path.

use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Schema version of the adjacent `transcript-pump.json` status document. Bump
/// this when the persisted shape changes so readers can detect the format.
///
/// v2 adds `periodic_error`: a best-effort periodic status write may fail without
/// failing capture, and that last failure is retained on the terminal status.
pub const PUMP_STATUS_SCHEMA_VERSION: u32 = 2;

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
#[derive(Debug, Clone)]
pub struct TranscriptPumpError {
    message: String,
}

impl TranscriptPumpError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for TranscriptPumpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "transcript pump failure: {}", self.message)
    }
}

impl std::error::Error for TranscriptPumpError {}

/// What a completed drain observed: total bytes persisted, records seen, and
/// previews a saturated or disconnected console could not accept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PumpSummary {
    pub bytes: u64,
    pub records: u64,
    pub dropped_console: u64,
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
    drain_with_counters(
        reader,
        transcript_path,
        status_path,
        preview,
        config,
        &counters,
    )
}

fn drain_with_counters(
    reader: impl Read,
    transcript_path: &Path,
    status_path: Option<&Path>,
    preview: &dyn PreviewSink,
    config: &TranscriptPumpConfig,
    counters: &SharedCounters,
) -> Result<PumpSummary, TranscriptPumpError> {
    let started = now_ms();
    let mut writer = status_path
        .map(|path| StatusWriter::spawn(path.to_path_buf()))
        .transpose()?;

    let result = capture(
        reader,
        transcript_path,
        status_path,
        preview,
        config,
        counters,
        started,
        writer.as_ref(),
    );

    // Settle the periodic writer BEFORE the terminal write, so the terminal status
    // serializes after all queued periodic work and no late snapshot can overwrite
    // it. Its `Drop` also settles it on unwind, so a pump panic cannot leave a
    // queued periodic `Running` to land after the panic handler writes `Failed`.
    // Retain its last persistence failure for the terminal diagnostic.
    let periodic_error = writer.as_mut().and_then(StatusWriter::shutdown);
    let summary = counters.snapshot();

    match result {
        Ok(()) => {
            // The terminal Complete status is required and typed: if it cannot be
            // persisted the capture is not independently observable, which is a
            // terminal infrastructure failure. A retained periodic error rides
            // along as a non-fatal diagnostic.
            persist_status_sync(
                status_path,
                PumpState::Complete,
                started,
                &summary,
                None,
                periodic_error.as_deref(),
            )?;
            Ok(summary)
        }
        Err(err) => {
            // Record the failure terminally, best-effort — it must not mask the
            // original error. The retained periodic error is kept in its own field
            // rather than folded into the primary cause.
            let _ = persist_status_sync(
                status_path,
                PumpState::Failed,
                started,
                &summary,
                Some(err.message()),
                periodic_error.as_deref(),
            );
            Err(err)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn capture(
    reader: impl Read,
    transcript_path: &Path,
    status_path: Option<&Path>,
    preview: &dyn PreviewSink,
    config: &TranscriptPumpConfig,
    counters: &SharedCounters,
    started: u64,
    writer: Option<&StatusWriter>,
) -> Result<(), TranscriptPumpError> {
    let mut file = std::fs::File::create(transcript_path).map_err(|err| {
        TranscriptPumpError::new(format!(
            "create transcript at {}: {err}",
            transcript_path.display()
        ))
    })?;
    // The initial Running status is required and typed.
    persist_status_sync(
        status_path,
        PumpState::Running,
        started,
        &counters.snapshot(),
        None,
        None,
    )?;

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
            // Periodic snapshots go through the coalescing writer, never blocking
            // canonical capture on a slow status filesystem.
            if let Some(writer) = writer {
                writer.submit(build_status(
                    PumpState::Running,
                    started,
                    &counters.snapshot(),
                    None,
                    None,
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

fn build_status(
    state: PumpState,
    started_at_ms: u64,
    summary: &PumpSummary,
    error: Option<&str>,
    periodic_error: Option<&str>,
) -> PumpStatus {
    PumpStatus {
        schema_version: PUMP_STATUS_SCHEMA_VERSION,
        state,
        started_at_ms,
        updated_at_ms: now_ms(),
        bytes: summary.bytes,
        records: summary.records,
        dropped_console: summary.dropped_console,
        error: error.map(str::to_string),
        periodic_error: periodic_error.map(str::to_string),
    }
}

/// Serialize and atomically persist a status document, returning a message on
/// failure so the caller can decide whether the failure is terminal.
fn persist_status_to(path: &Path, status: &PumpStatus) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(status).map_err(|err| err.to_string())?;
    crate::atomic_write::atomic_write(path, &bytes).map_err(|err| err.to_string())
}

/// Persist a required status synchronously. A persistence failure is a typed
/// terminal infrastructure error, because the durable diagnostic must be
/// independently observable.
fn persist_status_sync(
    status_path: Option<&Path>,
    state: PumpState,
    started_at_ms: u64,
    summary: &PumpSummary,
    error: Option<&str>,
    periodic_error: Option<&str>,
) -> Result<(), TranscriptPumpError> {
    let Some(path) = status_path else {
        return Ok(());
    };
    let status = build_status(state, started_at_ms, summary, error, periodic_error);
    persist_status_to(path, &status).map_err(|err| {
        TranscriptPumpError::new(format!("persist pump status at {}: {err}", path.display()))
    })
}

/// A background writer for periodic `Running` snapshots. The drain thread submits
/// snapshots without blocking; the writer coalesces (a full one-slot queue drops
/// the older snapshot in favor of the periodic cadence) and performs the atomic
/// writes off the canonical drain thread, so a slow status filesystem never
/// backpressures stdout capture. Its last persistence failure is retained.
struct StatusWriter {
    /// `None` once the writer has been shut down. Dropping the sender ends the
    /// worker loop.
    sender: Option<SyncSender<PumpStatus>>,
    join: Option<JoinHandle<Option<String>>>,
}

impl StatusWriter {
    fn spawn(path: PathBuf) -> Result<Self, TranscriptPumpError> {
        let (sender, receiver) = sync_channel::<PumpStatus>(1);
        let join = std::thread::Builder::new()
            .name("transcript-pump-status".to_string())
            .spawn(move || {
                let mut last_error = None;
                for status in receiver {
                    if let Err(err) = persist_status_to(&path, &status) {
                        last_error = Some(err);
                    }
                }
                last_error
            })
            .map_err(|err| {
                TranscriptPumpError::new(format!("spawn transcript pump status writer: {err}"))
            })?;
        Ok(Self {
            sender: Some(sender),
            join: Some(join),
        })
    }

    fn submit(&self, status: PumpStatus) {
        // Never block the drain thread: if the writer is mid-write, drop this
        // snapshot; the next periodic tick carries fresher counters.
        if let Some(sender) = &self.sender {
            let _ = sender.try_send(status);
        }
    }

    /// Close the queue, flush all pending periodic writes, and return the last
    /// persistence failure the writer observed, if any. Idempotent: a second call
    /// (for example from `Drop` after an explicit shutdown) is a no-op.
    fn shutdown(&mut self) -> Option<String> {
        // Drop the sender first so the worker's receive loop ends, then join it.
        self.sender = None;
        self.join.take().and_then(|join| join.join().ok().flatten())
    }
}

impl Drop for StatusWriter {
    fn drop(&mut self) {
        // On unwind (a pump panic) the writer is dropped here before the panic
        // reaches the pump's catch handler. Settling it now guarantees no queued
        // periodic `Running` snapshot can land after the handler writes `Failed`.
        let _ = self.shutdown();
    }
}

/// Install, once per process, a panic hook that suppresses the default hook's
/// blocking stderr write for transcript-pump threads. The pump's panic is caught
/// and reported through durable status instead, so a saturated stderr can never
/// block panic recovery. Non-pump panics keep the previous hook's behavior. This
/// is a single process-wide install, not a racy per-thread swap of the hook.
fn ensure_pump_panic_hook() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let is_pump = std::thread::current()
                .name()
                .is_some_and(|name| name == "transcript-pump");
            if !is_pump {
                previous(info);
            }
        }));
    });
}

/// A running pump on its own thread. The supervisor polls `try_terminal` while
/// the coder is alive and calls `wait_terminal` once the coder exits, so a pump
/// failure is observed promptly rather than only after the coder finishes.
pub struct PumpHandle {
    terminal: Receiver<Result<PumpSummary, TranscriptPumpError>>,
    join: Option<JoinHandle<()>>,
}

impl PumpHandle {
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

/// Spawn a pump on its own thread. A panic inside the drain is caught and
/// reported as a typed failure and a persisted `Failed` status that preserves the
/// counters accumulated before the panic, so a crashed pump can never silently
/// stop capture while the coder keeps running. The pump thread is named so a
/// process-wide hook can keep its panic off the blocking default stderr path.
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
    ensure_pump_panic_hook();
    let (tx, rx) = sync_channel(1);
    let counters = Arc::new(SharedCounters::default());
    let counters_for_panic = Arc::clone(&counters);
    let join = std::thread::Builder::new()
        .name("transcript-pump".to_string())
        .spawn(move || {
            let started = now_ms();
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
                drain_with_counters(
                    reader,
                    &transcript_path,
                    status_path.as_deref(),
                    preview,
                    &config,
                    &counters,
                )
            }))
            .unwrap_or_else(|_| {
                let err = TranscriptPumpError::new("transcript pump panicked");
                // Preserve the counters accumulated before the panic. The drain's
                // status writer was already settled during unwind, so this Failed
                // write is the last word — no queued Running can overwrite it.
                let summary = counters_for_panic.snapshot();
                let _ = persist_status_sync(
                    status_path.as_deref(),
                    PumpState::Failed,
                    started,
                    &summary,
                    Some(err.message()),
                    None,
                );
                Err(err)
            });
            let _ = tx.send(outcome);
        })
        .map_err(|err| {
            TranscriptPumpError::new(format!("spawn transcript pump thread: {err}"))
        })?;
    Ok(PumpHandle {
        terminal: rx,
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
