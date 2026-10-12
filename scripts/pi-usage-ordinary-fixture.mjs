/** Offline Pi-like process loads the actual installed ordinary default entry.
 * Source-contract-shaped callbacks are synthetic, not a vendor/provider run.
 * The process itself enrolls and requests native readiness/durable accounting;
 * no supplied descriptor, token, parent completion or transcript import exists. */
import assert from "node:assert/strict";
import { pathToFileURL } from "node:url";

const lifetime = setTimeout(() => { process.exitCode = 1; }, 290000);
const { default: install } = await import(pathToFileURL(process.argv[2]).href);
const handlers = new Map();
install(new Proxy({ on(type, callback) { handlers.set(type, callback); } }, {
  get(target, key) { assert.equal(key, "on", "vendor mutation API accessed"); return target[key]; },
}));
const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
const emit = event => {
  const before = JSON.stringify(event);
  assert.equal(handlers.get(event.type)?.(event, { sessionManager: { getSessionId: () => "usage-session" } }), undefined);
  assert.equal(JSON.stringify(event), before);
};
const settle = async () => {
  const owner = globalThis[ownerKey];
  assert(owner?.tail); await owner.tail;
  assert.equal(owner.lease.failed, false);
};
try {
  emit({ type: "session_start", reason: "startup" }); await settle();
  emit({ type: "agent_start" }); await settle();
  for (let index = 0; index < 4105; index++) {
    emit({ type: "turn_start", turnIndex: index, timestamp: Date.now() });
    const message = { role: "assistant", provider: "pi-offline-provider", model: "selected-model",
      responseModel: "actual-model", stopReason: ["stop", "error", "aborted", "length", "toolUse"][index % 5],
      usage: { input: 999999, output: 7, cacheRead: 2, cacheWrite: 3, reasoning: 4, cacheWrite1h: 1 },
      content: [{ type: "text", text: "PRIVATE_CALLBACK_CONTENT" }], errorMessage: "PRIVATE_CALLBACK_CONTENT" };
    emit({ type: "message_end", message });
    // Mirrors message_end extension modification before persisted turn_end.
    message.usage.input = 5;
    const final = { type: "turn_end", turnIndex: index, messageEntryId: `entry-${index}`, message,
      toolResults: [{ role: "toolResult", usage: { input: 999999 }, content: "PRIVATE_CALLBACK_CONTENT" }] };
    emit(final); await settle();
    assert.equal(globalThis[ownerKey].lease.usageCoverage, "finalized-assistant-external-reported");
    assert.equal(globalThis[ownerKey].client.status().usage, "request-bound-external-reported");
    emit(final); await settle();
    assert(globalThis[ownerKey].usageRecent.size <= 128);
  }
  // No genuine current request or object binding: old session/history replay
  // cannot allocate another marker after the bounded recent window expires.
  emit({ type: "turn_end", turnIndex: 0, messageEntryId: "entry-0", message: {
    role: "assistant", provider: "pi-offline-provider", model: "actual-model", stopReason: "stop",
    usage: { input: 5, output: 7, cacheRead: 2, cacheWrite: 3, reasoning: 4 },
  } }); await settle();
  emit({ type: "agent_before_settle", outcome: "completed" });
  emit({ type: "agent_settled" }); await settle();
  emit({ type: "session_shutdown", reason: "quit" }); await globalThis[ownerKey].tail;
} finally { globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime); }
