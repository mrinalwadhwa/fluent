import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";
import { createFluentPiExtension } from "../../src/native_agent/pi_extension.mjs";

const runnerUrl = pathToFileURL(process.argv[2]);
const { ExtensionRunner } = await import(runnerUrl.href);
const { AgentSession } = await import(new URL("../agent-session.js", runnerUrl).href);
const { SessionManager } = await import(new URL("../session-manager.js", runnerUrl).href);
const manager = SessionManager.inMemory();
manager.appendMessage({ role: "user", content: "first turn" });
manager.appendMessage({ role: "assistant", content: [{ type: "text", text: "first answer" }],
  stopReason: "stop", timestamp: Date.now() });
manager.appendMessage({ role: "user", content: "second turn" });
manager.appendMessage({ role: "assistant", content: [{ type: "text", text: "second answer" }],
  stopReason: "stop", timestamp: Date.now() });

process.env.FLUENT_NATIVE_PI_GENERATION = "compact-generation";
const handlers = new Map();
const events = [];
let sequence = 0;
createFluentPiExtension(async (message) => {
  events.push({ ...message, session_file: "/private/task/session.jsonl" });
  return { ok: true, input_sequence: message.event.type === "input" ? ++sequence : undefined };
})({ on(kind, callback) { handlers.set(kind, [callback]); } });
const extension = new ExtensionRunner([{ path: "fluent", handlers }], {}, process.cwd(), manager, {});
extension.shutdownHandler = () => { throw new Error("unexpected shutdown"); };
extension.onError((error) => { throw error; });
await extension.emit({ type: "session_start", reason: "startup" });

// Call the installed AgentSession method at an idle boundary. Its normal
// preparation and session append run against real in-memory session history.
const session = Object.create(AgentSession.prototype);
session.agent = { state: { model: { provider: "local-openai", id: "fixture", contextWindow: 1000 } } };
session.sessionManager = manager;
session.settingsManager = { getCompactionSettings: () => ({ enabled: true,
  reserveTokens: 1, keepRecentTokens: 1 }) };
session._extensionRunner = extension;
session.abort = async () => {};
session._runDefaultCompaction = async (preparation) => ({
  summary: "First turn completed.", firstKeptEntryId: preparation.firstKeptEntryId,
  tokensBefore: preparation.tokensBefore,
});
session._refreshFinalizedContext = () => {};
session._resolveIdleWaitIfIdle = () => {};
const activity = [];
session._emit = (event) => activity.push(event);
const compacted = await session.compact();
assert.equal(compacted.summary, "First turn completed.");
assert.ok(manager.getEntries().some((entry) => entry.type === "compaction"));
assert.ok(events.some((entry) => entry.event.type === "session_compact"));
assert.ok(activity.some((event) => event.type === "compaction_end" && !event.aborted));

assert.deepEqual(await extension.emitInput("queued follow-up", [], "interactive", "followUp"),
  { action: "continue" });
const admitted = events.find((entry) => entry.event.type === "input");
assert.equal(admitted.event.streamingBehavior, "followUp");
manager.appendMessage({ role: "user", content: "queued follow-up" });
await extension.emit({ type: "agent_before_settle", outcome: "completed" });
await extension.emit({ type: "agent_settled", aborted: false });
assert.ok(events.some((entry) => entry.event.type === "input_entry" && entry.input_sequence === 1));
assert.ok(events.some((entry) => entry.event.type === "agent_settled" && !entry.event.aborted));
for (const event of events) process.stdout.write(JSON.stringify(event) + "\n");
