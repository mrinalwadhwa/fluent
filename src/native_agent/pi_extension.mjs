// Keep Pi's native UI connected to one Fluent Task execution generation.
import { createConnection } from "node:net";

function sendToOwner(payload) {
  return new Promise((resolve, reject) => {
    const path = process.env.FLUENT_NATIVE_PI_SOCKET;
    if (!path) {
      reject(new Error("Fluent native Pi receiver is missing"));
      return;
    }
    const stream = createConnection({ path });
    let reply = "";
    stream.setTimeout(8000, () => stream.destroy(new Error("Fluent native Pi receiver timed out")));
    stream.on("connect", () => stream.write(JSON.stringify(payload) + "\n"));
    stream.on("data", (chunk) => {
      reply += chunk.toString("utf8");
      if (reply.length > 65536) {
        stream.destroy(new Error("Fluent native Pi receiver response is oversized"));
        return;
      }
      const end = reply.indexOf("\n");
      if (end < 0) return;
      stream.end();
      try {
        const response = JSON.parse(reply.slice(0, end));
        if (!response.ok) throw new Error(response.error || "Fluent rejected native Pi event");
        resolve(response);
      } catch (error) {
        reject(error);
      }
    });
    stream.on("error", reject);
    stream.on("end", () => {
      if (!reply.includes("\n")) reject(new Error("Fluent native Pi receiver closed without a response"));
    });
  });
}

// Export the registration function so a fixture can drive Pi's documented
// extension callbacks without opening a real model connection.
export function createFluentPiExtension(send) {
  return function register(pi) {
    const generation = process.env.FLUENT_NATIVE_PI_GENERATION;
    if (!generation) throw new Error("Fluent native Pi generation is missing");
    let assignedSession;
    let knownUserEntries = new Set();
    let pendingInputs = [];
    let broken = false;

    async function report(event, ctx, extra = {}) {
      if (broken) throw new Error("Fluent native Pi receiver has failed");
      const sessionId = ctx.sessionManager.getSessionId();
      const sessionFile = ctx.sessionManager.getSessionFile();
      if (!sessionId || (assignedSession && sessionId !== assignedSession)) {
        broken = true;
        throw new Error("Pi switched Fluent's assigned Task conversation");
      }
      try {
        return await send({ generation, session_id: sessionId, session_file: sessionFile,
          model: ctx.model?.id, event, ...extra });
      } catch (error) {
        broken = true;
        throw error;
      }
    }

    pi.on("session_start", async (event, ctx) => {
      if (assignedSession || !["startup", "resume"].includes(event.reason)) {
        broken = true;
        throw new Error(`Pi changed Fluent's Task session through ${event.reason}`);
      }
      assignedSession = ctx.sessionManager.getSessionId();
      knownUserEntries = new Set(ctx.sessionManager.getEntries()
        .filter((entry) => entry.type === "message" && entry.message.role === "user")
        .map((entry) => entry.id));
      await report(event, ctx);
    });

    pi.on("session_before_switch", () => ({ cancel: true }));
    pi.on("session_before_fork", () => ({ cancel: true }));
    pi.on("session_before_tree", () => ({ cancel: true }));
    pi.on("user_bash", () => ({ result: {
      output: "Fluent blocks direct shell commands outside admitted Task input.",
      exitCode: 1, cancelled: false, truncated: false,
    } }));

    pi.on("input", async (event, ctx) => {
      const response = await report(event, ctx);
      if (!Number.isSafeInteger(response.input_sequence) || response.input_sequence < 1) {
        broken = true;
        throw new Error("Fluent did not acknowledge Pi input admission");
      }
      pendingInputs.push(response.input_sequence);
      return { action: "continue" };
    });

    for (const kind of ["message_end", "tool_execution_start", "tool_execution_end", "turn_start", "turn_end", "agent_start", "agent_end", "agent_before_settle", "session_compact", "session_compact_failed"]) {
      pi.on(kind, async (event, ctx) => { await report(event, ctx); });
    }

    pi.on("agent_settled", async (event, ctx) => {
      const fresh = ctx.sessionManager.getEntries().filter((entry) =>
        entry.type === "message" && entry.message.role === "user" && !knownUserEntries.has(entry.id));
      if (fresh.length !== pendingInputs.length) {
        broken = true;
        throw new Error("Pi admitted input has no matching durable session entries");
      }
      for (const [index, entry] of fresh.entries()) {
        await report({ type: "input_entry", entry_id: entry.id }, ctx,
          { input_sequence: pendingInputs[index] });
        knownUserEntries.add(entry.id);
      }
      pendingInputs = [];
      await report(event, ctx);
    });
  };
}

export default createFluentPiExtension(sendToOwner);
