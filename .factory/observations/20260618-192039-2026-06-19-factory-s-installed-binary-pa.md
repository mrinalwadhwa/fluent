2026-06-19 — Factory's installed binary passes the wrong default
Pi model name to the Coder. attempt-2 of the
`20260618-194050-tester-deterministic-core` Work Item was
launched with `--write-coder pi` (no explicit `--write-model`),
and Factory printed `Model qwen3-30b-a3b` in the launch banner.
vllm-mlx is actually serving `qwen3.6-35b-a3b` (per
`curl -s http://localhost:8000/v1/models`), which is also what
`~/.pi/extensions/local-vllm.js` advertises.

The Pi run errored on its first API call with `404 status code
(no body)` because the model name does not exist on the vllm
side. The attempt-2 writer Task ended with no committed output
and the attempt failed.

Workaround: pass `--write-model qwen3.6-35b-a3b` (or whichever
model is actually being served) when launching Pi attempts.

Real fix: Factory's hardcoded default Pi model name in
`src/coder.rs` (or wherever the per-coder default lives) must
match the model name that the vllm-mlx server advertises. Two
plausible angles:
- A previous change updated vllm to serve `qwen3.6-35b-a3b` but
  Factory's default was left at `qwen3-30b-a3b`.
- The default was authored from memory or a typo; the model that
  Pi has actually been validated against is `qwen3.6-35b-a3b`
  per the `project_pi_local_writer_works.md` notes.

Suggested follow-up: a tiny Work Item that updates Factory's
default Pi model name to `qwen3.6-35b-a3b`, with a behavior
asserting the launch banner prints the correct name and a unit
test pinning the default constant.
