/** Offline ordinary Pi-shaped producer for actual native durable-ACK loss.
 * Discovers the installed default HOME package; accepts no entry/helper/root,
 * token, source tuple or request ticket from the parent. Only synthetic phase
 * names/mode cross private fixture files. All SDK events and replay metadata
 * originate in this process and the actual installed callback/client owners. */
import assert from "node:assert/strict";
import { readdirSync, readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

assert.equal(process.argv.length, 2);
const home = process.env.HOME;
const mode = readFileSync(join(home, "replay-mode"), "utf8");
assert(["connected", "registry"].includes(mode));
for (const key of ["PI_CODING_AGENT_DIR", "MEZ_HARNESS_SESSION", "MEZ_HARNESS_OBSERVER_FD", "MEZ_HARNESS_HELPER", "MEZ_EXTERNAL_LAUNCH_TOKEN"])
  assert.equal(process.env[key], undefined, "fixture received an activation input");
const root = join(home, ".pi", "agent", "extensions");
const factories = [];
for (const child of readdirSync(root, { withFileTypes: true })) {
  if (!child.isDirectory()) continue;
  const directory = join(root, child.name);
  let manifest;
  try { manifest = JSON.parse(readFileSync(join(directory, "package.json"), "utf8")); } catch { continue; }
  for (const entry of manifest.pi?.extensions ?? []) {
    assert.equal(entry, "index.mjs");
    factories.push((await import(pathToFileURL(join(directory, entry)).href)).default);
  }
}
assert.equal(factories.length, 1, "installed ordinary factory was not discovered");
const handlers = new Map();
for (const factory of factories) factory(new Proxy({ on(type, callback) { handlers.set(type, callback); } }, {
  get(target, key) { assert.equal(key, "on", "vendor mutation API accessed"); return target[key]; },
}));
const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
assert.equal(globalThis[ownerKey], undefined, "loading opened resources");
const publish = phase => {
  const path = join(home, "replay-phase");
  writeFileSync(`${path}.tmp`, phase); renameSync(`${path}.tmp`, path);
};
const checkpoint = async phase => {
  publish(phase);
  const path = join(home, "replay-next");
  const deadline = Date.now() + 15000;
  for (;;) {
    let value;
    try { value = readFileSync(path, "utf8"); } catch { /* phase not observed */ }
    if (value === phase) { unlinkSync(path); return; }
    assert(Date.now() < deadline, "synthetic phase gate expired");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
};
let starts = 0;
let finalized = 0;
const emit = event => {
  if (event.type === "turn_start") starts++;
  if (event.type === "turn_end") finalized++;
  const before = JSON.stringify(event);
  assert.equal(handlers.get(event.type)?.(event, { sessionManager: { getSessionId: () => "native-replay-session" } }), undefined);
  assert.equal(JSON.stringify(event), before);
};
const settle = async () => {
  const owner = globalThis[ownerKey];
  assert(owner?.tail);
  await owner.tail;
  return owner;
};
const finalizedRequest = async index => {
  emit({ type: "turn_start", turnIndex: index, timestamp: 1 }); await settle();
  const message = { role: "assistant", provider: "pi-offline-provider", model: "selected-model", responseModel: "actual-model",
    stopReason: index === 0 ? "error" : "stop", content: "PRIVATE_REPLAY_CONTENT", errorMessage: "PRIVATE_REPLAY_CONTENT",
    usage: { input: 999999, output: 7, cacheRead: 2, cacheWrite: 3, reasoning: 4 } };
  emit({ type: "message_end", message });
  message.usage.input = 5;
  emit({ type: "turn_end", turnIndex: index, messageEntryId: `native-entry-${index}`, message,
    toolResults: [{ usage: { input: 999999 }, content: "PRIVATE_REPLAY_CONTENT" }] });
  return settle();
};
const lifetime = setTimeout(() => { process.exitCode = 1; }, 45000);
try {
  emit({ type: "session_start", reason: "startup" });
  let owner = await settle();
  assert.equal(owner.lease.failed, false);
  const initial = owner.client.status();
  emit({ type: "agent_start", prompt: "PRIVATE_REPLAY_CONTENT" }); await settle();
  await checkpoint("registered");
  // Exactly one SDK request/finalization. The installed client alone retries its
  // original ticket/report after the parent drops an actual committed reply.
  owner = await finalizedRequest(0);
  if (owner.lease.failed || owner.lease.usageCoverage !== "finalized-assistant-external-reported") {
    publish("unavailable"); process.exitCode = 1;
  } else {
    assert.equal(starts, 1); assert.equal(finalized, 1);
    assert.equal(owner.client.status().run, initial.run);
    assert.equal(owner.client.status().epoch, initial.epoch + (mode === "registry" ? 1 : 0));
    assert.equal(owner.client.status().usage, "request-bound-external-reported");
    await checkpoint("recovered");
    owner = await finalizedRequest(1);
    assert.equal(owner.lease.failed, false);
    assert.equal(owner.lease.usageCoverage, "finalized-assistant-external-reported");
    assert.equal(starts, 2); assert.equal(finalized, 2);
    await checkpoint("subsequent");
    emit({ type: "agent_before_settle", outcome: "completed" });
    emit({ type: "agent_settled" }); await settle();
    emit({ type: "session_shutdown", reason: "quit" }); await globalThis[ownerKey].tail;
    publish("complete");
    process.stdout.write("PI_NATIVE_REPLAY_COMPLETE\n");
  }
} catch {
  // Never print callback content, private transport credentials or wire replies.
  publish("unavailable"); process.exitCode = 1;
} finally { globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime); }
