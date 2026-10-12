import assert from "node:assert/strict";
import { mkdtemp, realpath, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { createFluentPiExtension } from "../../src/native_agent/pi_extension.mjs";

const runnerUrl = pathToFileURL(await realpath(process.argv[2]));
const { ExtensionRunner } = await import(runnerUrl.href);
const { createAgentSession } = await import(new URL("../sdk.js", runnerUrl).href);
const { SessionManager } = await import(new URL("../session-manager.js", runnerUrl).href);
const { SettingsManager } = await import(new URL("../settings-manager.js", runnerUrl).href);
const { ModelRuntime } = await import(new URL("../model-runtime.js", runnerUrl).href);
const { createFauxCore, fauxAssistantMessage } = await import(
  new URL("../../../../pi-ai/dist/index.js", runnerUrl).href);

const root = await mkdtemp(join(tmpdir(), "fluent-pi-runtime-"));
try {
  const manager = SessionManager.inMemory();
  for (const [user, answer] of [["first turn", "first answer"],
    ["second turn", "second answer"]]) {
    manager.appendMessage({ role: "user", content: user });
    manager.appendMessage({ role: "assistant", content: [{ type: "text", text: answer }],
      stopReason: "stop", timestamp: Date.now() });
  }
  const settingsManager = SettingsManager.inMemory({ compaction: { enabled: true,
    reserveTokens: 1, keepRecentTokens: 1 } });
  const faux = createFauxCore({});
  const model = faux.getModel();
  const runtime = await ModelRuntime.create({ modelsPath: null, refreshOnCreate: false });
  // Supply responses only where the installed Agent calls its model runtime.
  runtime.streamSimple = (...args) => faux.streamSimple(...args);
  runtime.hasConfiguredAuth = () => true;
  runtime.getAuth = async () => ({ auth: { apiKey: "fixture" } });
  let modelStarted;
  const modelStartedPromise = new Promise((resolve) => { modelStarted = resolve; });
  let releaseModel;
  const modelReleasedPromise = new Promise((resolve) => { releaseModel = resolve; });
  faux.setResponses([
    fauxAssistantMessage("First turn completed."),
    fauxAssistantMessage("Second turn context."),
    async () => { modelStarted(); await modelReleasedPromise;
      return fauxAssistantMessage("first answer"); },
    fauxAssistantMessage("queued answer"),
  ]);
  const { session } = await createAgentSession({ cwd: root, agentDir: root,
    sessionManager: manager, settingsManager, modelRuntime: runtime, model,
    noTools: "all" });

  process.env.FLUENT_NATIVE_PI_GENERATION = "compact-generation";
  const handlers = new Map();
  const events = [];
  let sequence = 0;
  createFluentPiExtension(async (message) => {
    events.push({ ...message, session_file: "/private/task/session.jsonl" });
    return { ok: true, input_sequence: message.event.type === "input" ? ++sequence : undefined };
  })({ on(kind, callback) { handlers.set(kind, [callback]); } });
  const extension = new ExtensionRunner([{ path: "fluent", handlers }], {}, root, manager, {});
  extension.shutdownHandler = () => { throw new Error("unexpected shutdown"); };
  extension.onError((error) => { throw error; });
  session._extensionRunner = extension;
  session._extensionRunnerRef.current = extension;
  await extension.emit({ type: "session_start", reason: "startup" });

  const activity = [];
  session.subscribe((event) => activity.push(event));
  const compacted = await session.compact();
  assert.ok(compacted.summary.includes("First turn completed."));
  assert.ok(manager.getEntries().some((entry) => entry.type === "compaction"));
  assert.ok(events.some((entry) => entry.event.type === "session_compact"));
  assert.ok(activity.some((event) => event.type === "compaction_end" && !event.aborted));

  const prompt = session.prompt("start after compaction");
  let timeout;
  await Promise.race([modelStartedPromise, new Promise((_, reject) => {
    timeout = setTimeout(() => reject(new Error("Pi did not start model work")), 5000);
  })]).finally(() => clearTimeout(timeout));
  assert.equal(await session.followUp("queued follow-up"), "queued");
  assert.equal(session.agent.hasQueuedMessages(), true);
  releaseModel();
  await prompt;
  assert.equal(faux.state.callCount, 4, "installed Agent did not run the queued turn");
  assert.equal(session.agent.hasQueuedMessages(), false);
  const admitted = events.find((entry) => entry.event.type === "input" &&
    entry.event.text === "queued follow-up");
  assert.equal(admitted.event.streamingBehavior, "followUp");
  assert.ok(manager.getEntries().some((entry) => entry.type === "message" &&
    entry.message.role === "user" && entry.message.content[0].text === "queued follow-up"));
  assert.ok(events.some((entry) => entry.event.type === "agent_before_settle"));
  assert.ok(events.some((entry) => entry.event.type === "input_entry" && entry.input_sequence === 2));
  assert.ok(events.some((entry) => entry.event.type === "agent_settled" && !entry.event.aborted));
  assert.equal(events.filter((entry) => entry.event.type === "turn_end").length, 2);
  assert.ok(events.findIndex((entry) => entry.event.type === "input_entry" &&
    entry.input_sequence === 2) < events.findIndex((entry) => entry.event.type === "agent_settled"));
  for (const event of events) process.stdout.write(JSON.stringify(event) + "\n");
} finally {
  await rm(root, { recursive: true, force: true });
}
