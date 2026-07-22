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
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Schema version of the adjacent `transcript-pump.json` status document. Bump
/// this when the persisted shape changes so readers can detect the format.
pub const PUMP_STATUS_SCHEMA_VERSION: u32 = 1;

/// Built-in size, in bytes, of each read chunk pulled from coder stdout.
pub const DEFAULT_READ_CHUNK_SIZE: usize = 64 * 1024;
/// Built-in upper bound, in bytes, on a single rendered console preview before
/// the pump truncates it. The full record always lands in the transcript.
pub const DEFAULT_CONSOLE_PREVIEW_LIMIT: usize = 8 * 1024;

/// Appended to a preview whose record exceeded the console preview limit. It
/// points a reader at the canonical transcript, which alone holds every byte.
pub const TRUNCATION_MARKER: &[u8] = b"...[preview truncated; full record in transcript]";

/// Operator-facing thresholds that shape console previews and status flushes.
/// Resolved from layered configuration; see `config::resolve_transcript_pump_config`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptPumpConfig {
    /// Bytes read per stdout chunk.
    pub read_chunk_size: usize,
    /// Maximum bytes of one record rendered to the console preview.
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

/// Install the process-wide transcript-pump configuration. Coder supervision
/// reads this when spawning a pump, so the executor resolves layered thresholds
/// once per project and installs them before launching a coder.
pub fn install_config(config: TranscriptPumpConfig) {
    *process_config().lock().unwrap() = config;
}

/// The currently installed process-wide configuration, or the built-in default.
pub fn active_config() -> TranscriptPumpConfig {
    process_config().lock().unwrap().clone()
}

fn process_config() -> &'static Mutex<TranscriptPumpConfig> {
    static CONFIG: OnceLock<Mutex<TranscriptPumpConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| Mutex::new(TranscriptPumpConfig::default()))
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    let writer = status_path.map(|path| StatusWriter::spawn(path.to_path_buf()));

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

    // Flush the periodic writer before the terminal write, so the terminal status
    // serializes after all queued periodic work and cannot be overwritten by a
    // late snapshot. Retain its last persistence failure for diagnostics.
    let periodic_error = writer.and_then(StatusWriter::shutdown);
    let summary = counters.snapshot();

    match result {
        Ok(()) => {
            // The terminal Complete status is required and typed: if it cannot be
            // persisted the capture is not independently observable, which is a
            // terminal infrastructure failure.
            persist_status_sync(status_path, PumpState::Complete, started, &summary, None)?;
            Ok(summary)
        }
        Err(err) => {
            // Record the failure terminally, best-effort — it must not mask the
            // original error. A retained periodic error is folded in for context.
            let message = match periodic_error {
                Some(periodic) => format!("{}; periodic status write failed: {periodic}", err.message()),
                None => err.message().to_string(),
            };
            let _ = persist_status_sync(
                status_path,
                PumpState::Failed,
                started,
                &summary,
                Some(&message),
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
        file.write_all(chunk).map_err(|err| {
            TranscriptPumpError::new(format!(
                "write transcript at {}: {err}",
                transcript_path.display()
            ))
        })?;
        counters.add_bytes(read as u64);

        for &byte in chunk {
            if byte == b'\n' {
                counters.add_record();
                if !line.flush(preview) {
                    counters.add_dropped();
                }
            } else {
                line.push(byte);
            }
        }

        if last_flush.elapsed() >= config.status_flush_interval {
            // Periodic snapshots go through the coalescing writer, never blocking
            // canonical capture on a slow status filesystem.
            if let Some(writer) = writer {
                writer.submit(build_status(
                    PumpState::Running,
                    started,
                    &counters.snapshot(),
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
        if !line.flush(preview) {
            counters.add_dropped();
        }
    }

    file.flush().map_err(|err| {
        TranscriptPumpError::new(format!(
            "flush transcript at {}: {err}",
            transcript_path.display()
        ))
    })?;

    Ok(())
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
) -> Result<(), TranscriptPumpError> {
    let Some(path) = status_path else {
        return Ok(());
    };
    let status = build_status(state, started_at_ms, summary, error);
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
    sender: SyncSender<PumpStatus>,
    join: JoinHandle<Option<String>>,
}

impl StatusWriter {
    fn spawn(path: PathBuf) -> Self {
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
            .expect("spawn transcript pump status writer");
        Self { sender, join }
    }

    fn submit(&self, status: PumpStatus) {
        // Never block the drain thread: if the writer is mid-write, drop this
        // snapshot; the next periodic tick carries fresher counters.
        let _ = self.sender.try_send(status);
    }

    /// Close the queue, flush all pending periodic writes, and return the last
    /// persistence failure the writer observed, if any.
    fn shutdown(self) -> Option<String> {
        let StatusWriter { sender, join } = self;
        drop(sender);
        join.join().ok().flatten()
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
            Err(TryRecvError::Disconnected) => {
                Some(Err(TranscriptPumpError::new("transcript pump thread vanished")))
            }
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
) -> PumpHandle
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
                // Preserve the counters accumulated before the panic.
                let summary = counters_for_panic.snapshot();
                let _ = persist_status_sync(
                    status_path.as_deref(),
                    PumpState::Failed,
                    started,
                    &summary,
                    Some(err.message()),
                );
                Err(err)
            });
            let _ = tx.send(outcome);
        })
        .expect("spawn transcript pump thread");
    PumpHandle {
        terminal: rx,
        join: Some(join),
    }
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

    /// Offer the accumulated record as a bounded preview. Returns whether the
    /// sink delivered it, then resets for the next record.
    fn flush(&mut self, preview: &dyn PreviewSink) -> bool {
        let delivered = if self.truncated {
            let mut bounded = self.buf.clone();
            bounded.extend_from_slice(TRUNCATION_MARKER);
            preview.deliver(&bounded)
        } else {
            preview.deliver(&self.buf)
        };
        self.buf.clear();
        self.truncated = false;
        self.any = false;
        delivered
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
        assert_eq!(summary.records, 2, "draining must continue past the oversized record");
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
            failed.error.as_deref().unwrap_or_default().contains("create transcript"),
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
        drain(
            Cursor::new(input.clone()),
            &path,
            None,
            &sink, &config).unwrap();

        let previews = sink.previews.lock().unwrap();
        assert_eq!(previews.len(), 1);
        let preview = &previews[0];
        assert!(
            preview.ends_with(TRUNCATION_MARKER),
            "an oversized preview must carry the truncation marker"
        );
        assert!(
            preview.len() <= limit + TRUNCATION_MARKER.len(),
            "the preview must stay bounded, got {} bytes",
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
        );

        let started = Instant::now();
        let outcome = pump.wait_terminal();
        let elapsed = started.elapsed();
        pump.join();

        assert!(outcome.is_err(), "a panicking pump must report a typed failure");
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
            samples.iter().any(|(state, _)| *state == PumpState::Running),
            "a Running state must be observable during capture"
        );
        assert!(
            samples
                .iter()
                .any(|(state, records)| *state == PumpState::Running && *records > 0 && *records < 6),
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
