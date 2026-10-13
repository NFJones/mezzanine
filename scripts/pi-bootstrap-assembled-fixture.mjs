/** Ordinary Pi-shaped offline process: discover installed default-root package
 * entries, load their real default factories and emit finalized source-shaped
 * callbacks. No entry/helper/root argument, fd3, token or client injection.
 * Private marker gates coordinate assertions only; they confer no authority.
 * This is not a real Pi loader/provider qualification or transcript importer. */
import assert from "node:assert/strict";
import { readdirSync, readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

assert.equal(process.argv.length, 2, "fixture accepts no activation arguments");
for (const key of ["PI_CODING_AGENT_DIR", "MEZ_HARNESS_SESSION", "MEZ_HARNESS_OBSERVER_FD", "MEZ_HARNESS_HELPER", "MEZ_EXTERNAL_LAUNCH_TOKEN"])
  assert.equal(process.env[key], undefined, "unexpected activation environment");
const root = join(process.env.HOME, ".pi", "agent", "extensions");
const factories = [];
for (const child of readdirSync(root, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
  if (!child.isDirectory()) continue;
  const directory = join(root, child.name);
  let manifest;
  try { manifest = JSON.parse(readFileSync(join(directory, "package.json"), "utf8")); } catch { continue; }
  if (!Array.isArray(manifest.pi?.extensions)) continue;
  for (const entry of manifest.pi.extensions) {
    assert.equal(entry, "index.mjs", "private siblings must not be standalone entries");
    factories.push((await import(pathToFileURL(join(directory, entry)).href)).default);
  }
}
assert.equal(factories.length, 1, "actual installed entry was not discovered");
const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
const load = () => {
  const handlers = new Map();
  for (const factory of factories) factory(new Proxy({ on(type, callback) { handlers.set(type, callback); } }, {
    get(target, key) { assert.equal(key, "on", "observation accessed a vendor mutation API"); return target[key]; },
  }));
  return event => {
    const before = JSON.stringify(event);
    const context = { sessionManager: { getSessionId: () => "assembled-session" } };
    assert.equal(handlers.get(event.type)?.(event, context), undefined);
    assert.equal(JSON.stringify(event), before, "callback modified vendor event");
  };
};
const emit = load();
const duplicate = load();
assert.equal(globalThis[ownerKey], undefined, "factory opened resources before startup");
const routed = typeof process.env.MEZ === "string";
const lifetime = setTimeout(() => { process.exitCode = 1; }, 25000);
const checkpoint = async phase => {
  if (!routed) return;
  const path = join(process.env.HOME, "assembled-phase");
  writeFileSync(`${path}.tmp`, phase); renameSync(`${path}.tmp`, path);
  const next = join(process.env.HOME, "assembled-next");
  const deadline = Date.now() + 10000;
  for (;;) {
    let value;
    try { value = readFileSync(next, "utf8"); } catch { /* parent has not observed this phase */ }
    if (value === phase) { unlinkSync(next); break; }
    assert(Date.now() < deadline, "synthetic vendor phase gate timed out");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
};
const settle = async () => {
  if (!routed) return;
  const owner = globalThis[ownerKey];
  assert(owner?.tail, "installed entry did not activate ordinary callbacks");
  await owner.tail;
  assert.equal(owner.lease?.failed ?? false, false, "ordinary telemetry unavailable");
};
try {
  emit({ type: "session_start", reason: "startup", path: "PRIVATE_CALLBACK_CONTENT" });
  duplicate({ type: "session_start", reason: "startup" });
  await settle();
  if (routed) assert.equal(globalThis[ownerKey].client.status().usage, "unavailable-source-continuity");
  await checkpoint("registered");
  emit({ type: "agent_start", prompt: "PRIVATE_CALLBACK_CONTENT" });
  emit({ type: "ui_prompt_start", reason: "ui_prompt", kind: "input", prompt: "PRIVATE_CALLBACK_CONTENT" });
  await settle(); await checkpoint("waiting");
  emit({ type: "ui_prompt_end", reason: "ui_prompt", kind: "input", answer: "PRIVATE_CALLBACK_CONTENT" });
  for (let index = 0; index < 3; index++) {
    emit({ type: "turn_start", turnIndex: index, timestamp: 1 }); await settle();
    const message = { role: "assistant", provider: "pi-offline-provider", model: "selected-model", responseModel: "actual-model",
      responseId: `PRIVATE_UPSTREAM_RESPONSE_${index}`, stopReason: ["stop", "error", "aborted"][index], timestamp: 1,
      content: [{ type: "text", text: "PRIVATE_CALLBACK_CONTENT" }], errorMessage: "PRIVATE_CALLBACK_CONTENT",
      usage: { input: 999999, output: 7, cacheRead: 2, cacheWrite: 3, reasoning: 4, cacheWrite1h: 1 } };
    emit({ type: "message_end", message });
    message.usage.input = 5;
    const event = { type: "turn_end", turnIndex: index, messageEntryId: `persisted-entry-${index}`, message,
      toolResults: [{ role: "toolResult", usage: { input: 999999 }, content: "PRIVATE_CALLBACK_CONTENT" }] };
    emit(event); await settle(); emit(event); await settle();
    if (routed) {
      assert.equal(globalThis[ownerKey].lease.usageCoverage, "finalized-assistant-external-reported");
      assert.equal(globalThis[ownerKey].client.status().usage, "request-bound-external-reported");
    }
  }
  await checkpoint("accounted");
  emit({ type: "turn_start", turnIndex: 3 }); await settle();
  const missing = { role: "assistant", provider: "pi-offline-provider", model: "actual-model", stopReason: "stop",
    usage: { output: 7, cacheRead: 2, cacheWrite: 3 }, content: "PRIVATE_CALLBACK_CONTENT" };
  emit({ type: "message_end", message: missing });
  emit({ type: "turn_end", turnIndex: 3, messageEntryId: "persisted-missing", message: missing });
  await settle();
  if (routed) assert.equal(globalThis[ownerKey].lease.usageCoverage, "unavailable-finalized-counters");
  await checkpoint("gap");
  emit({ type: "agent_before_settle", outcome: "completed", message: "PRIVATE_CALLBACK_CONTENT" });
  emit({ type: "agent_settled" }); await settle();
  emit({ type: "session_shutdown", reason: "quit" });
  if (routed) await globalThis[ownerKey].tail;
  writeFileSync(join(process.env.HOME, "assembled-completed"), "complete");
  process.stdout.write("PI_OFFLINE_COMPLETE\n");
} finally {
  globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime);
}
