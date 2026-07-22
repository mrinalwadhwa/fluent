# Reproduce console pipe pressure without hanging the suite

## Title

Drive a binary test into an unread console pipe to prove capture keeps
draining, while a stdout drain and a wait timeout keep the suite safe.

## Context

Use when a binary test must prove that an unread or saturated console
(the writer's stderr) cannot stall canonical transcript capture — the
`transcript_pump` reliability contract. Two shapes: a single record
larger than the ~64 KiB OS pipe capacity, or a sustained flood of many
records whose previews would together exceed the pipe. The test must
confirm the whole writer process completes within a deadline and every
byte lands in the transcript.

Because the sink declines every live preview in this landing, previews
never reach the pipe in these tests; the regression proves that a
sustained flood cannot saturate the console and that Fluent's own
control-plane output and post-coder Attempt transition keep flowing.
Assert prompt exit, a byte-exact transcript, `Complete` pump status,
the exact record count, and `dropped_console == records`. A design that
mirrored previews into the console would fill the pipe and hang the
writer, failing the test.

## Mechanism

- Launch the writer with `std::process::Command` (not `assert_cmd`) so
  the test owns the child's pipes. Pipe stderr and **never read it** —
  that is the pressure condition the regression is about.
- Drain **stdout** on a thread. A `--no-sandbox` writer inherits the
  coder's stdout from unrelated steps (e.g. project-model seeding), which
  can itself exceed 64 KiB and block on its own pipe; that noise is not
  the console path under test, so read it to end.
- Wait with a bounded timeout (`try_wait` loop that kills on expiry). A
  capture regression blocks the writer forever; the timeout turns that
  into a fast failure instead of a hung suite.
- Assert the writer exited success, the transcript exceeds the oversized
  record's length, and the record after it is present.

## Example

```rust
let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_fluent"))
    .args(["task", "run", "work-1", "attempt-1", "attempt-1-write-1", "--no-sandbox"])
    .env("PATH", mock_path(&bin_dir))
    .stdout(Stdio::piped())
    .stderr(Stdio::piped()) // piped and left unread on purpose
    .spawn()
    .unwrap();

let mut stdout = child.stdout.take().unwrap();
let drain = std::thread::spawn(move || {
    let mut sink = Vec::new();
    let _ = std::io::Read::read_to_end(&mut stdout, &mut sink);
});

let status = wait_with_timeout(&mut child, Duration::from_secs(60))
    .expect("writer must complete instead of blocking on an unread console");
let _ = drain.join();
assert!(status.success());
```
