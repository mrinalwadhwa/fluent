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

// Keep the installed AgentSession lifecycle; fake only the model's messages.
let session;
const queued = [];
let modelStarted;
const modelStartedPromise = new Promise((resolve) => { modelStarted = resolve; });
let releaseModel;
const modelReleasedPromise = new Promise((resolve) => { releaseModel = resolve; });
const fakeAgent = {
  state: { model: { provider: "local-openai", id: "fixture", contextWindow: 1000 }, messages: [] },
  subscribe() { return () => {}; },
  followUp(message) { queued.push(message); },
  hasQueuedMessages() { return queued.length > 0; },
  peekQueuedMessages() { return queued; },
  abort() {},
  async prompt(messages) {
    modelStarted();
    await modelReleasedPromise;
    const followUp = queued.shift();
    assert.ok(followUp, "Pi did not queue the follow-up during model work");
    const completed = [];
    await session._handleAgentEvent({ type: "agent_start" });
    for (const [user, answerText] of [[messages[0], "first answer"], [followUp, "queued answer"]]) {
      const answer = { role: "assistant", content: [{ type: "text", text: answerText }],
        stopReason: "stop", usage: { input: 1, output: 1, cacheRead: 0, cacheWrite: 0 },
        timestamp: Date.now() };
      for (const event of [
        { type: "turn_start" },
        { type: "message_start", message: user }, { type: "message_end", message: user },
        { type: "message_start", message: answer }, { type: "message_end", message: answer },
        { type: "turn_end", message: answer, toolResults: [] },
      ]) await session._handleAgentEvent(event);
      completed.push(user, answer);
    }
    await session._handleAgentEvent({ type: "agent_end", messages: completed });
  },
  async continue() { throw new Error("unexpected extra model run"); },
};
class FixtureSession extends AgentSession {
  _installAgentToolHooks() {}
  _installAgentNextTurnRefresh() {}
  _installAgentRequestProjection() {}
  _installAgentBoundaryHooks() {}
  _installHiddenDeclarationsProjection() {}
  _installAgentForcedPromptProjection() {}
  _buildRuntime() {}
  _restoreToolsFromTranscript() {}
}
session = new FixtureSession({ agent: fakeAgent, sessionManager: manager,
  settingsManager: { getCompactionSettings: () => ({ enabled: true,
    reserveTokens: 1, keepRecentTokens: 1 }), getRetrySettings: () => ({ enabled: false, maxRetries: 0 }) },
  resourceLoader: { getPrompts: () => ({ prompts: [] }) }, cwd: process.cwd() });
session._extensionRunner = extension;
session.abort = async () => {};
session._runDefaultCompaction = async (preparation) => ({
  summary: "First turn completed.", firstKeptEntryId: preparation.firstKeptEntryId,
  tokensBefore: preparation.tokensBefore,
});
session._refreshFinalizedContext = () => {};
const activity = [];
session._eventListeners.push((event) => activity.push(event));
const compacted = await session.compact();
assert.equal(compacted.summary, "First turn completed.");
assert.ok(manager.getEntries().some((entry) => entry.type === "compaction"));
assert.ok(events.some((entry) => entry.event.type === "session_compact"));
assert.ok(activity.some((event) => event.type === "compaction_end" && !event.aborted));

// Queue through AgentSession, then let its run loop deliver the message and
// emit the real before-settle and settled callbacks.
session._checkCompaction = async () => false;
session._recordSelection = () => {};
session._flushPendingCustomMessages = () => {};
session._flushPendingBashMessages = () => {};
session._modelRuntime = { hasConfiguredAuth: () => true };
session._baseSystemPromptOptions = { selectedTools: [] };
session._preparePromptAndToolLoadout = () => undefined;
session.getActiveToolNames = () => [];
const prompt = session.prompt("start after compaction");
await Promise.race([modelStartedPromise, new Promise((_, reject) =>
  setTimeout(() => reject(new Error("Pi did not start model work")), 1000))]);
assert.equal(await session.followUp("queued follow-up"), "queued");
assert.equal(queued.length, 1);
releaseModel();
await prompt;
const admitted = events.find((entry) => entry.event.type === "input" &&
  entry.event.text === "queued follow-up");
assert.equal(admitted.event.text, "queued follow-up");
assert.equal(admitted.event.streamingBehavior, "followUp");
assert.ok(manager.getEntries().some((entry) => entry.type === "message" &&
  entry.message.role === "user" && entry.message.content[0].text === "queued follow-up"));
assert.ok(events.some((entry) => entry.event.type === "agent_before_settle"));
assert.ok(events.some((entry) => entry.event.type === "input_entry" && entry.input_sequence === 2));
assert.ok(events.some((entry) => entry.event.type === "agent_settled" && !entry.event.aborted));
for (const event of events) process.stdout.write(JSON.stringify(event) + "\n");
