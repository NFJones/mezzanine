/** Ordinary offline Pi-shaped idle/exit producer. It discovers actual installed
 * default-HOME package entries and uses only genuine callback-shaped facts.
 * Private mode/phase/result files coordinate the fixture, never credentials,
 * tickets, clocks, process identities, renewal facts or parent-completed usage.
 * A shell records the real Node exit separately from these post-assert results.
 * This is not a real Pi SDK loader/provider or a spend-completeness claim. */
import assert from "node:assert/strict";
import { readdirSync, readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

assert(process.argv.length === 2, "fixture accepts no activation arguments");
const ownerKey = Symbol.for("mezzanine.pi.ordinary-owner.v1");
assert(globalThis[ownerKey] === undefined, "factory opened resources");
for (const key of ["PI_CODING_AGENT_DIR", "MEZ_HARNESS_SESSION", "MEZ_HARNESS_OBSERVER_FD", "MEZ_HARNESS_HELPER", "MEZ_EXTERNAL_LAUNCH_TOKEN"])
  assert(process.env[key] === undefined, "activation input supplied");
const home = process.env.HOME;
const mode = readFileSync(join(home, "idle-mode"), "utf8");
assert(["eof", "exit"].includes(mode));
const root = join(home, ".pi", "agent", "extensions");
const factories = [];
for (const child of readdirSync(root, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
  if (!child.isDirectory()) continue;
  const directory = join(root, child.name);
  let manifest;
  try { manifest = JSON.parse(readFileSync(join(directory, "package.json"), "utf8")); } catch { continue; }
  for (const entry of manifest.pi?.extensions ?? []) {
    assert(entry === "index.mjs", "installed package entry unavailable");
    factories.push((await import(pathToFileURL(join(directory, entry)).href)).default);
  }
}
assert(factories.length === 1, "installed package entry unavailable");
const handlers = new Map();
for (const factory of factories) factory(new Proxy({ on(type, callback) { handlers.set(type, callback); } }, {
  get(target, key) { assert(key === "on", "vendor mutation API accessed"); return target[key]; },
}));
assert(globalThis[ownerKey] === undefined, "factory opened resources");
let callbacks = 0;
const emit = event => {
  callbacks++;
  const before = JSON.stringify(event);
  assert.equal(handlers.get(event.type)?.(event, { sessionManager: { getSessionId: () => "idle-session" } }), undefined);
  assert.equal(JSON.stringify(event), before);
};
const settle = async () => {
  const owner = globalThis[ownerKey];
  assert(owner?.tail);
  for (let remaining = 32; remaining > 0; remaining--) {
    const tail = owner.tail; await tail;
    if (tail === owner.tail) return owner;
  }
  throw new Error("observation queue did not settle");
};
const publish = (name, value) => {
  const path = join(home, name);
  writeFileSync(`${path}.tmp`, value); renameSync(`${path}.tmp`, path);
};
const checkpoint = async phase => {
  publish("idle-phase", phase);
  const path = join(home, "idle-next");
  const deadline = Date.now() + 20000;
  for (;;) {
    let value;
    try { value = readFileSync(path, "utf8"); } catch { /* parent has not observed this phase */ }
    if (value === phase) { unlinkSync(path); return; }
    assert(Date.now() < deadline, "synthetic phase gate expired");
    await new Promise(resolve => setTimeout(resolve, 10));
  }
};
const lifetime = setTimeout(() => { process.exitCode = 1; }, 60000);
try {
  emit({ type: "session_start", reason: "startup", path: "PRIVATE_IDLE_CONTENT" });
  let owner = await settle();
  assert.equal(owner.lease.failed, false);
  if (mode === "exit") {
    // A real original request is admitted but never receives a finalized end.
    // It creates no expense and does not prove that vendor work cost zero.
    emit({ type: "agent_start", prompt: "PRIVATE_IDLE_CONTENT" });
    emit({ type: "turn_start", turnIndex: 0, timestamp: 1 });
    owner = await settle();
  }
  const initial = owner.client.status();
  const initialCallbacks = callbacks;
  assert.equal(initial.phase, "enrolled");
  await checkpoint("silent");
  assert.equal(callbacks, initialCallbacks, "idle required a synthetic SDK heartbeat");
  assert.equal(owner.client.status().phase, "enrolled");
  assert.equal(owner.client.status().run, initial.run);
  assert.equal(owner.client.status().epoch, initial.epoch);
  assert.equal(owner.client.status().usage, "unavailable-source-continuity");
  await checkpoint("renewed");
  if (mode === "eof") {
    const deadline = Date.now() + 5000;
    while (owner.client.status().phase !== "connection-lost") {
      assert(Date.now() < deadline, "real observer EOF was not observed");
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    assert.equal(callbacks, initialCallbacks);
    await checkpoint("disconnected");
    assert.equal(owner.client.status().phase, "connection-lost");
  }
  assert.equal(callbacks, initialCallbacks);
  publish("idle-result", JSON.stringify({ complete: true, mode, callbacks,
    run: initial.run, epoch: initial.epoch, phase: owner.client.status().phase,
    usage: owner.client.status().usage, sdk_shutdown: false }));
  process.stdout.write("PI_IDLE_OFFLINE_COMPLETE\n");
} catch {
  // Failure is explicit without private callback/transport values in output.
  publish("idle-result", JSON.stringify({ complete: false, mode }));
  process.exitCode = 1;
} finally { globalThis[ownerKey]?.client?.detach(); clearTimeout(lifetime); }
