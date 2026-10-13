/** Offline ordinary Pi-shaped finalized-callback retry qualification.
 * Discovers only the normally installed HOME package. No entry/helper/token,
 * source tuple, ticket, amount, SDK callback or provider replay comes from the
 * parent; private scalar gates merely order this process's own fake callbacks. */
import assert from "node:assert/strict";
import { readdirSync, readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

assert.equal(process.argv.length, 2);
const home = process.env.HOME;
for (const key of ["PI_CODING_AGENT_DIR", "MEZ_HARNESS_SESSION", "MEZ_HARNESS_OBSERVER_FD", "MEZ_HARNESS_HELPER", "MEZ_EXTERNAL_LAUNCH_TOKEN"])
  assert(process.env[key] === undefined, "fixture received an activation input");
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
assert.equal(factories.length, 1);
const handlers = new Map();
factories[0](new Proxy({ on(type, callback) { handlers.set(type, callback); } }, {
  get(target, key) { assert.equal(key, "on", "vendor mutation API accessed"); return target[key]; },
}));
const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
assert.equal(globalThis[ownerKey], undefined, "loading opened resources");
const publish = phase => {
  const path = join(home, "retry-phase");
  writeFileSync(`${path}.tmp`, phase); renameSync(`${path}.tmp`, path);
};
const checkpoint = async phase => {
  publish(phase);
  const path = join(home, "retry-next"); const deadline = Date.now() + 15000;
  for (;;) {
    let value;
    try { value = readFileSync(path, "utf8"); } catch { /* parent has not inspected this stage */ }
    if (value === phase) { unlinkSync(path); return; }
    assert(Date.now() < deadline, "synthetic phase gate expired");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
};
let starts = 0; let finalized = 0;
const emit = event => {
  if (event.type === "turn_start") starts++;
  if (event.type === "turn_end") finalized++;
  const before = JSON.stringify(event);
  assert.equal(handlers.get(event.type)?.(event, { sessionManager: { getSessionId: () => "native-retry-session" } }), undefined);
  assert.equal(JSON.stringify(event), before);
};
const settle = async () => {
  const owner = globalThis[ownerKey]; assert(owner?.tail);
  for (let remaining = 32; remaining > 0; remaining--) {
    const tail = owner.tail; await tail;
    if (tail === owner.tail) return owner;
  }
  throw new Error("ordinary callback queue did not settle");
};
const assistant = () => ({ role: "assistant", provider: "pi-offline-provider", model: "selected-model",
  responseModel: "actual-model", stopReason: "error", content: "PRIVATE_RETRY_CONTENT", errorMessage: "PRIVATE_RETRY_CONTENT",
  usage: { input: 5, output: 7, cacheRead: 2, cacheWrite: 3, reasoning: 4 } });
const end = (message, index) => emit({ type: "turn_end", turnIndex: index, messageEntryId: `native-retry-entry-${index}`, message });
const lifetime = setTimeout(() => { process.exitCode = 1; }, 45000);
try {
  emit({ type: "session_start", reason: "startup" });
  let owner = await settle(); assert.equal(owner.lease.failed, false);
  const initial = owner.client.status();
  emit({ type: "agent_start" }); await settle(); await checkpoint("registered");
  emit({ type: "turn_start", turnIndex: 0 }); await settle();
  const original = assistant(); emit({ type: "message_end", message: original }); end(original, 0);
  owner = await settle();
  assert.equal(starts, 1); assert.equal(finalized, 1);
  assert.equal(owner.lease.failed, false);
  assert.equal(owner.lease.usageCoverage, "unavailable-durable-acknowledgment");
  assert.equal(owner.client.status().phase, "enrolled");
  assert.equal(owner.client.status().usage, "unavailable-source-continuity");
  assert.equal(owner.client.status().run, initial.run); assert.equal(owner.client.status().epoch, initial.epoch);
  await checkpoint("exhausted");
  // Only this real later finalized callback triggers the retained report retry.
  // No extra SDK request/start, private ticket access or client method call.
  end(assistant(), 0); owner = await settle();
  assert.equal(starts, 1); assert.equal(finalized, 2);
  assert.equal(owner.lease.failed, false);
  assert.equal(owner.lease.usageCoverage, "finalized-assistant-external-reported");
  assert.equal(owner.client.status().usage, "request-bound-external-reported");
  assert.equal(owner.client.status().run, initial.run); assert.equal(owner.client.status().epoch, initial.epoch);
  await checkpoint("replayed");
  end(assistant(), 0); await settle(); await checkpoint("known");
  emit({ type: "agent_start" }); emit({ type: "turn_start", turnIndex: 1 }); await settle();
  const next = assistant(); next.stopReason = "stop";
  emit({ type: "message_end", message: next }); end(next, 1); owner = await settle();
  assert.equal(starts, 2); assert.equal(finalized, 4); assert.equal(owner.lease.failed, false);
  await checkpoint("subsequent");
  emit({ type: "agent_before_settle", outcome: "completed" }); emit({ type: "agent_settled" }); await settle();
  emit({ type: "session_shutdown", reason: "quit" }); await globalThis[ownerKey].tail;
  assert.equal(globalThis[ownerKey].closed, true);
  publish("complete"); process.stdout.write("PI_NATIVE_RETRY_COMPLETE\n");
} catch {
  // Only fixed stage/result metadata leaves the child on a failed assertion.
  publish("unavailable"); process.exitCode = 1;
} finally { globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime); }
