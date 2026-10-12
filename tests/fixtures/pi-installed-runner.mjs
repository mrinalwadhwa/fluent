import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import { createFluentPiExtension } from "../../src/native_agent/pi_extension.mjs";

process.env.FLUENT_NATIVE_PI_GENERATION = "fixture-generation";
const { ExtensionRunner } = await import(pathToFileURL(process.argv[2]).href);
const entries = [];
const manager = {
  getSessionId: () => "assigned-session",
  getSessionFile: () => "/private/task/session.jsonl",
  getEntries: () => entries,
};

function runner(send) {
  const handlers = new Map();
  createFluentPiExtension(send)({ on(kind, callback) {
    handlers.set(kind, [callback]);
  } });
  const instance = new ExtensionRunner([{ path: "fluent", handlers }], {}, process.cwd(), manager, {});
  const errors = [];
  let shutdowns = 0;
  instance.onError((error) => errors.push(error));
  instance.shutdownHandler = () => shutdowns++;
  return { instance, errors, get shutdowns() { return shutdowns; } };
}

const missing = runner(async (event) => {
  if (event.event.type === "session_start") return { ok: true };
  throw new Error("owner unavailable");
});
await missing.instance.emit({ type: "session_start", reason: "startup" });
assert.deepEqual(await missing.instance.emitInput("late", [], "interactive", "steer"),
  { action: "handled" });
assert.ok(missing.shutdowns > 0);
assert.equal(missing.errors.length, 0);

const shutdownFault = runner(async (event) => {
  if (event.event.type === "session_start") return { ok: true };
  throw new Error("receiver write failed");
});
shutdownFault.instance.shutdownHandler = () => { throw new Error("shutdown failed"); };
await shutdownFault.instance.emit({ type: "session_start", reason: "startup" });
assert.deepEqual(await shutdownFault.instance.emitInput("must stay closed", [], "interactive", "steer"),
  { action: "handled" });
assert.equal(shutdownFault.errors.length, 0);

const rejected = runner(async (event) => ({ ok: true,
  input_sequence: event.event.type === "input" ? undefined : null }));
await rejected.instance.emit({ type: "session_start", reason: "startup" });
assert.deepEqual(await rejected.instance.emitInput("unacknowledged", [], "interactive", "followUp"),
  { action: "handled" });
assert.ok(rejected.shutdowns > 0);

const switched = runner(async () => ({ ok: true, input_sequence: 1 }));
await switched.instance.emit({ type: "session_start", reason: "startup" });
assert.equal((await switched.instance.emit({ type: "session_before_switch" }))?.cancel, true);
assert.equal((await switched.instance.emit({ type: "session_before_fork" }))?.cancel, true);
assert.equal((await switched.instance.emit({ type: "session_before_tree" }))?.cancel, true);
await switched.instance.emit({ type: "session_start", reason: "reload" });
assert.ok(switched.shutdowns > 0);
assert.deepEqual(await switched.instance.emitInput("after reload", [], "interactive", "steer"),
  { action: "handled" });

const settled = runner(async () => ({ ok: true, input_sequence: 1 }));
await settled.instance.emit({ type: "session_start", reason: "startup" });
await settled.instance.emit({ type: "session_shutdown" });
assert.equal(settled.shutdowns, 0);

let sequence = 0;
const lifecycleEvents = [];
const lifecycle = runner(async (message) => {
  lifecycleEvents.push(message.event);
  return { ok: true, input_sequence: message.event.type === "input" ? ++sequence : undefined };
});
await lifecycle.instance.emit({ type: "session_start", reason: "startup" });
assert.deepEqual(await lifecycle.instance.emitInput("steer", [], "interactive", "steer"),
  { action: "continue" });
assert.deepEqual(await lifecycle.instance.emitInput("follow", [], "interactive", "followUp"),
  { action: "continue" });
entries.push({ type: "message", id: "first", message: { role: "user" } },
  { type: "message", id: "second", message: { role: "user" } });
await lifecycle.instance.emit({ type: "session_compact" });
await lifecycle.instance.emit({ type: "session_compact_failed" });
await lifecycle.instance.emit({ type: "agent_before_settle", outcome: "aborted" });
await lifecycle.instance.emit({ type: "agent_settled", aborted: true });
assert.deepEqual(lifecycleEvents.filter((event) => event.type === "input")
  .map((event) => event.streamingBehavior), ["steer", "followUp"]);
assert.ok(lifecycleEvents.some((event) => event.type === "session_compact"));
assert.ok(lifecycleEvents.some((event) => event.type === "session_compact_failed"));
assert.ok(lifecycleEvents.some((event) => event.type === "agent_settled" && event.aborted));

const ended = runner(async (event) => {
  if (event.event.type === "session_shutdown") throw new Error("owner unavailable");
  return { ok: true, input_sequence: 1 };
});
await ended.instance.emit({ type: "session_start", reason: "startup" });
await ended.instance.emit({ type: "session_shutdown" });
assert.ok(ended.shutdowns > 0);

// The installed runner swallows errors from ordinary callbacks. A local
// failure before the socket send must still close input admission, even when
// the shutdown handler itself fails.
const failedManager = runner(async () => ({ ok: true, input_sequence: 1 }));
await failedManager.instance.emit({ type: "session_start", reason: "startup" });
manager.getSessionFile = () => { throw new Error("session file disappeared"); };
let failedShutdowns = 0;
failedManager.instance.shutdownHandler = () => { failedShutdowns++; throw new Error("shutdown failed"); };
await failedManager.instance.emit({ type: "turn_start" });
assert.equal(failedShutdowns, 1, "a pre-send callback failure must stop the owned pane");
assert.deepEqual(await failedManager.instance.emitInput("never model this", [], "interactive", "steer"),
  { action: "handled" });
manager.getSessionFile = () => "/private/task/session.jsonl";
