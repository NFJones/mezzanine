/** Offline ordinary Pi-shaped discovery and multi-source producer.
 * Models explicit package entries, the project-trust decision and exact disabled
 * path overrides from the inspected upstream loader. It does not implement the
 * vendor trust database, general glob rules, SDK/provider loading or billing.
 * Private policy/phase files select synthetic vendor behavior, never native
 * source authority, credentials, request tickets, clocks or accounting results.
 */
import assert from "node:assert/strict";
import { readdirSync, readFileSync, writeFileSync, renameSync, unlinkSync, realpathSync } from "node:fs";
import { join, resolve, relative } from "node:path";
import { pathToFileURL } from "node:url";

assert.equal(process.argv.length, 2, "unexpected activation arguments");
for (const key of ["MEZ_HARNESS_SESSION", "MEZ_HARNESS_OBSERVER_FD", "MEZ_HARNESS_HELPER", "MEZ_EXTERNAL_LAUNCH_TOKEN"])
  assert(process.env[key] === undefined, "unexpected activation input");
const home = realpathSync(process.env.HOME);
/** Refuse inherited external directories before reading or importing anything
 * from them. Every loaded module belongs to the generated fixture HOME. */
const privatePath = path => {
  assert(resolve(path).startsWith(home + "/"), "discovery path outside fixture home");
  const actual = realpathSync(path);
  assert(actual.startsWith(home + "/"), "discovery link outside fixture home");
  return actual;
};
privatePath(process.cwd());
const policy = JSON.parse(readFileSync(join(home, "discovery-policy.json"), "utf8"));
assert(["observe", "outside", "disabled", "outage"].includes(policy.scenario));
assert.equal(typeof policy.projectTrusted, "boolean");
const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
const factories = [];
const entries = [];
// Project trust and disabled resources remain vendor decisions in this model.
for (const base of [...(policy.projectTrusted ? [join(process.cwd(), ".pi")] : []),
  process.env.PI_CODING_AGENT_DIR ?? join(home, ".pi", "agent")]) {
  privatePath(base);
  const settings = JSON.parse(readFileSync(privatePath(join(base, "settings.json")), "utf8"));
  const overrides = settings.extensions ?? [];
  assert(overrides.every(value => value === "-extensions/mezzanine/index.mjs"), "unsupported fixture policy");
  const extensionRoot = privatePath(join(base, "extensions"));
  for (const child of readdirSync(extensionRoot, { withFileTypes: true })) {
    if (!child.isDirectory()) continue;
    const directory = privatePath(join(extensionRoot, child.name));
    const manifest = JSON.parse(readFileSync(privatePath(join(directory, "package.json")), "utf8"));
    for (const entry of manifest.pi?.extensions ?? []) {
      assert.equal(entry, "index.mjs", "private sibling selected as an extension");
      const path = privatePath(resolve(directory, entry));
      if (overrides.includes("-" + relative(base, path).split("\\").join("/"))) continue;
      assert(!entries.includes(path), "same path discovered twice");
      entries.push(path);
      factories.push((await import(pathToFileURL(path).href)).default);
    }
  }
}
assert.equal(factories.length, policy.scenario === "disabled" ? 0 : policy.projectTrusted ? 2 : 1);
assert.equal(new Set(factories).size, factories.length, "distinct packages reused one module factory");
const handlers = new Map();
const api = new Proxy({ on(type, callback) {
  const rows = handlers.get(type) ?? []; rows.push(callback); handlers.set(type, rows);
} }, { get(target, key) { assert.equal(key, "on", "vendor mutation API accessed"); return target[key]; } });
for (const factory of factories) factory(api);
assert.equal(globalThis[ownerKey], undefined, "factory opened resources");
const emit = event => {
  const before = JSON.stringify(event);
  for (const callback of handlers.get(event.type) ?? [])
    assert.equal(callback(event, { sessionManager: { getSessionId: () => "same-session-in-both-panes" } }), undefined);
  assert.equal(JSON.stringify(event), before, "callback changed vendor data");
};
const publish = (name, value) => {
  const path = join(home, name); writeFileSync(path + ".tmp", value); renameSync(path + ".tmp", path);
};
const checkpoint = async phase => {
  if (policy.scenario !== "observe") return;
  publish("discovery-phase", phase);
  const deadline = Date.now() + 20000;
  for (;;) {
    let next;
    try { next = readFileSync(join(home, "discovery-next"), "utf8"); } catch { /* sequencing only */ }
    if (next === phase) { unlinkSync(join(home, "discovery-next")); return; }
    assert(Date.now() < deadline, "fixture phase expired");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
};
const settle = async () => {
  for (let budget = 32; budget > 0; budget--) {
    const tail = globalThis[ownerKey]?.tail;
    if (!tail) return;
    await tail;
    if (tail === globalThis[ownerKey]?.tail) return;
  }
  throw new Error("fixture queue did not settle");
};
const lifetime = setTimeout(() => { process.exitCode = 1; }, 90000);
let retired = false;
let intentionalFault = false;
try {
  emit({ type: "session_start", reason: "startup", path: "PRIVATE_DISCOVERY_CONTENT" });
  await settle();
  if (policy.scenario === "observe") {
    assert.equal(globalThis[ownerKey].lease.failed, false);
    await checkpoint("registered");
  }
  emit({ type: "agent_start", prompt: "PRIVATE_DISCOVERY_CONTENT" });
  emit({ type: "turn_start", turnIndex: 0 });
  const message = { role: "assistant", provider: "pi-offline-provider", model: "selected-model", responseModel: "actual-model",
    stopReason: "stop", usage: { input: 5, output: 7, cacheRead: 2, cacheWrite: 3, reasoning: 4, cacheWrite1h: 1 },
    content: "PRIVATE_DISCOVERY_CONTENT", errorMessage: "PRIVATE_DISCOVERY_CONTENT" };
  emit({ type: "message_end", message });
  const final = { type: "turn_end", turnIndex: 0, messageEntryId: "same-entry-in-both-panes", message };
  emit(final); await settle(); emit(final); await settle();
  if (policy.scenario === "observe") {
    assert.equal(globalThis[ownerKey].lease.failed, false);
    assert.equal(globalThis[ownerKey].usageRecent.size, 1);
    await checkpoint("accounted");
  }
  if (["outside", "disabled"].includes(policy.scenario)) assert.equal(globalThis[ownerKey], undefined);
  if (policy.scenario === "outage") assert.equal(globalThis[ownerKey].lease.failed, true);
  emit({ type: "agent_before_settle", outcome: "completed" });
  emit({ type: "agent_settled" }); await settle();
  emit({ type: "session_shutdown", reason: "quit" }); await settle();
  // Stale closures cannot reopen a source or acquire another module's lease.
  emit({ type: "agent_start" }); emit(final); await settle();
  retired = true;
  await checkpoint("retired");
  if (policy.failAfterRetirement) {
    intentionalFault = true;
    throw new Error("intentional post-retirement fixture failure");
  }
  publish("discovery-result", "complete");
  process.stdout.write("PI_DISCOVERY_COMPLETE\n");
} catch {
  publish("discovery-result", intentionalFault && retired ? "post-retirement-failure" : "failure");
  process.exitCode = 1;
} finally {
  globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime);
}
