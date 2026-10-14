/** Offline ordinary Pi-shaped producer for finite retained-report recovery.
 * Discovers only the bare-bootstrap installed default package in temporary
 * HOME. There is one source-shaped SDK request and finalized callback; the
 * production default timer alone retries after two real pre-byte ACK losses.
 * Parent files contain only fixed phase gates, never accounting/enrollment
 * authority, callbacks, tickets, counters, UTC or synthetic acknowledgments.
 * This is not a real vendor loader/provider run or durable restart qualifier. */
import assert from "node:assert/strict";
import { readdirSync, readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

assert.equal(process.argv.length, 2);
const home = process.env.HOME;
for (const key of ["PI_CODING_AGENT_DIR", "MEZ_HARNESS_SESSION", "MEZ_HARNESS_OBSERVER_FD", "MEZ_HARNESS_HELPER", "MEZ_EXTERNAL_LAUNCH_TOKEN"])
  assert(process.env[key] === undefined, "fixture received an activation input");
const factories = [];
for (const child of readdirSync(join(home, ".pi", "agent", "extensions"), { withFileTypes: true })) {
  if (!child.isDirectory()) continue;
  const directory = join(home, ".pi", "agent", "extensions", child.name);
  let manifest;
  try { manifest = JSON.parse(readFileSync(join(directory, "package.json"), "utf8")); } catch { continue; }
  for (const entry of manifest.pi?.extensions ?? []) {
    assert.equal(entry, "index.mjs");
    factories.push((await import(pathToFileURL(join(directory, entry)).href)).default);
  }
}
assert.equal(factories.length, 1, "ordinary installed factory not discovered");
const handlers = new Map();
factories[0](new Proxy({ on(type, callback) { handlers.set(type, callback); } }, {
  get(target, key) { assert.equal(key, "on", "vendor mutation API accessed"); return target[key]; },
}));
const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
assert.equal(globalThis[ownerKey], undefined, "loading opened resources");
const publish = phase => {
  const path = join(home, "pending-phase");
  writeFileSync(`${path}.tmp`, phase); renameSync(`${path}.tmp`, path);
};
const checkpoint = async phase => {
  publish(phase); const path = join(home, "pending-next"); const deadline = Date.now() + 15000;
  for (;;) {
    let value;
    try { value = readFileSync(path, "utf8"); } catch { /* scalar gate not observed */ }
    if (value === phase) { unlinkSync(path); return; }
    assert(Date.now() < deadline, "fixed phase gate expired");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
};
let starts = 0; let finals = 0;
const emit = event => {
  if (event.type === "turn_start") starts++;
  if (event.type === "turn_end") finals++;
  const before = JSON.stringify(event);
  assert.equal(handlers.get(event.type)?.(event, { sessionManager: { getSessionId: () => "pending-session" } }), undefined);
  assert.equal(JSON.stringify(event), before);
};
const drain = async () => {
  const owner = globalThis[ownerKey];
  for (let remaining = 32; remaining > 0; remaining--) {
    const tail = owner.tail; await tail;
    if (tail === owner.tail) return;
  }
  throw new Error("bounded observer queue did not settle");
};
const lifetime = setTimeout(() => { process.exitCode = 1; }, 45000);
try {
  emit({ type: "session_start", reason: "startup" }); await drain();
  const owner = globalThis[ownerKey]; const lease = owner.lease; const client = owner.client;
  assert.equal(lease.failed, false); const initial = client.status();
  emit({ type: "agent_start" }); await drain();
  emit({ type: "turn_start", turnIndex: 0 }); await drain();
  const message = { role: "assistant", provider: "pi-pending-provider", model: "selected-model", responseModel: "actual-model",
    stopReason: "error", usage: { input: 999999, output: 7, cacheRead: 2, cacheWrite: 3, reasoning: 4 },
    content: "PRIVATE_PENDING_CONTENT", errorMessage: "PRIVATE_PENDING_CONTENT" };
  emit({ type: "message_end", message }); message.usage.input = 5;
  emit({ type: "turn_end", turnIndex: 0, messageEntryId: "pending-entry", message });
  // Capture the original end operation's tail BEFORE it appends its truthful
  // gap operation. This makes the first unavailable result visible before
  // timer recovery, even if gap delivery or later transport waits in the queue.
  const originalTail = owner.tail; await originalTail;
  assert.equal(lease.failed, false);
  assert.equal(lease.usageCoverage, "unavailable-durable-acknowledgment");
  assert.equal(lease.lastUsageGap, "durable-acknowledgment");
  const slot = [...owner.usageRecent.values()][0].slot;
  const ticket = slot.ticket; const report = slot.report; const projection = slot.projection;
  assert.equal(Object.isFrozen(ticket), true); assert.equal(Object.isFrozen(report), true);
  publish("pending");
  const deadline = Date.now() + 10000;
  while (!slot.durable) {
    assert.equal(lease.failed, false);
    assert(Date.now() < deadline, "finite default scheduler recovery unavailable");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  await drain();
  assert.equal(owner.lease === lease && owner.client === client && slot.client === client, true);
  assert.equal(slot.ticket === ticket && slot.report === report && slot.projection === projection, true);
  assert.equal(starts, 1); assert.equal(finals, 1);
  assert.equal(lease.failed, false); assert.equal(lease.cancelled, false);
  assert.equal(lease.usageCoverage, "finalized-assistant-external-reported");
  assert.equal(lease.lastUsageGap, "durable-acknowledgment");
  assert.equal(client.status().run, initial.run); assert.equal(client.status().epoch, initial.epoch);
  await checkpoint("recovered");
  emit({ type: "agent_before_settle", outcome: "completed" }); emit({ type: "agent_settled" }); await drain();
  emit({ type: "session_shutdown", reason: "quit" }); await drain();
  assert.equal(starts, 1); assert.equal(finals, 1);
  publish("complete");
  process.stdout.write("PI_NATIVE_PENDING_COMPLETE\n");
} catch {
  // Fixed stages only; never print callback content, tickets or private wire.
  publish("unavailable"); process.exitCode = 1;
} finally { globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime); }
