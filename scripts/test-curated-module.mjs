/** Literal module-entry regression host. No vendor, process, socket or store.
 * Actual source body remains exercised separately; this host focuses on module
 * ownership, main/child filtering, ordering and exact unchanged middleware. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createCuratedLifetime } from "../crates/mezzanine/src/integrations/bootstrap/curated_lifetime.mjs";
import { projectClaudeEvent } from "../crates/mezzanine/src/integrations/bootstrap/claude_observer.mjs";

const template = readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/curated_module.mjs", import.meta.url), "utf8");
const source = readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/curated_client.mjs", import.meta.url), "utf8");
const body = source.replace(/__MEZ_[A-Z]+__/g, marker => ({ __MEZ_OWNED__: "true", __MEZ_LIFETIME__: "observerLifetime",
  __MEZ_INTERVAL__: "10000", __MEZ_HELPER__: JSON.stringify("/owned/mez"), __MEZ_INSTANCE__: JSON.stringify("mez-curated-client-1"),
  __MEZ_REPLACING__: "false", __MEZ_PREDECESSOR__: "undefined" })[marker]);
const code = template.replace(/^import .*;\n/gm, "").replace("export function register", "function register")
  .replace("__MEZ_SOURCE_BODY__", body);
const register = new Function("createCuratedLifetime", "projectClaudeEvent", `${code}\nreturn register;`)(createCuratedLifetime, projectClaudeEvent);

const start = (session = "session-a", extra = {}) => ({ hook_event_name: "SessionStart", session_id: session, source: "startup", ...extra });
const end = (session = "session-a", extra = {}) => ({ hook_event_name: "SessionEnd", session_id: session, reason: "clear", ...extra });

/** Minimal metadata-only SDK host records fixed helper calls and cancellable
 * timers; every returned receipt matches the requested native-independent slot. */
function fixture() {
  const hooks = new Map();
  register((name, callback) => { assert(!hooks.has(name)); hooks.set(name, callback); });
  const calls = [];
  const timers = [];
  const sdk = { process: { run: async argv => {
    const capsule = JSON.parse(argv[2]); calls.push(capsule);
    const receipt = capsule.operation ? { observed: true, changed: true, sequence: capsule.sequence }
      : { protocol: "external-agent/1", registered: true, controls: [], agent_id: "external-a", generation: 1,
        observer_witness: "a".repeat(64), run_id: 1, observer_epoch: 1, observer_instance: capsule.observer_instance,
        external_session_id: capsule.external_session_id, usage: "unavailable-source-continuity",
        observer_transport: "unavailable-curated-freshness", expires_at_unix_seconds: 123, lease_seconds: 60 };
    return { exitCode: 0, stdout: JSON.stringify(receipt) };
  } }, clock: { every: (_delay, callback) => { const timer = { callback, cancelled: false, cancel() { this.cancelled = true; } }; timers.push(timer); return timer; } } };
  const invoke = (event, next = async () => result) => hooks.get(`classic.${event.hook_event_name}`)(sdk, event, next);
  const result = Object.freeze({ unchanged: true });
  return { hooks, calls, timers, sdk, invoke, result };
}

/** One retained owner coalesces duplicate main starts, and exact matching end
 * stops its timer without treating Stop/tool/child observations as retirement. */
test("curated module owns one main session and filters child or foreign end", async () => {
  const f = fixture();
  assert.deepEqual([...f.hooks.keys()], ["classic.SessionStart", "classic.SessionEnd"]);
  assert.equal(await f.invoke(start()), f.result);
  assert.equal(await f.invoke(start()), f.result);
  assert.equal(f.calls.length, 1);
  await f.invoke(start("child", { agent_id: "child-a" }));
  await f.invoke(end("session-a", { agentId: "child-a" }));
  await f.invoke(end("foreign"));
  assert.equal(f.timers[0].cancelled, false);
  assert.equal(await f.invoke(end()), f.result);
  assert.equal(f.timers[0].cancelled, true);
  await f.timers[0].callback();
  assert.equal(f.calls.length, 1);
});

/** A delayed old end captures its own owner before next; it cannot stop the
 * newly accepted session's timer or cause old queued callbacks to emit proofs. */
test("curated module fences delayed old end across a new session", async () => {
  const f = fixture();
  await f.invoke(start());
  let finish;
  const pending = f.invoke(end(), () => new Promise(resolve => { finish = resolve; }));
  await f.invoke(start("session-b"));
  assert.equal(f.timers[0].cancelled, true);
  finish(f.result);
  assert.equal(await pending, f.result);
  assert.equal(f.timers[1].cancelled, false);
  await f.timers[0].callback();
  await f.timers[1].callback();
  assert.equal(f.calls.at(-1).external_session_id, "session-b");
});

/** Matching end while the first start is still downstream-pending must close
 * that exact provisional owner; a late start result cannot launch a helper or
 * timer for an already-ended logical session. */
test("curated module fences matching end before pending start completes", async () => {
  const f = fixture();
  let finish;
  const pending = f.invoke(start(), () => new Promise(resolve => { finish = resolve; }));
  assert.equal(await f.invoke(end()), f.result);
  finish(f.result);
  assert.equal(await pending, f.result);
  assert.equal(f.calls.length, 0);
  assert.equal(f.timers.length, 0);
});

/** Same-ID identity remains fenced even before the first start has bound. A
 * duplicate arriving after its end cannot replace the stopped provisional owner
 * and the first callback's later completion cannot permit an implicit restart. */
test("curated module rejects duplicate restart of an ended pending identity", async () => {
  const f = fixture();
  let finish;
  const pending = f.invoke(start(), () => new Promise(resolve => { finish = resolve; }));
  await f.invoke(end());
  await f.invoke(start());
  finish(f.result);
  await pending;
  await f.invoke(start());
  assert.equal(f.calls.length, 0);
  assert.equal(f.timers.length, 0);
});

/** Two same-ID pending starts share one provisional owner. Cleanup from the
 * older completed callback must not stop that owner while the newer callback
 * still owns acceptance, or duplicate coalescing would lose valid telemetry. */
test("curated module preserves newer coalesced pending owner during old cleanup", async () => {
  const f = fixture();
  let finishOld;
  let finishNew;
  const old = f.invoke(start(), () => new Promise(resolve => { finishOld = resolve; }));
  const fresh = f.invoke(start(), () => new Promise(resolve => { finishNew = resolve; }));
  finishOld(f.result);
  await old;
  finishNew(f.result);
  await fresh;
  assert.equal(f.calls.length, 1);
  assert.equal(f.timers.length, 1);
  assert.equal(f.timers[0].cancelled, false);
});

/** Closing many distinct main identities retains a finite fence without
 * evicting an old ended session to permit restart. Exhaustion withholds new
 * telemetry, never vendor behavior or an existing native authorization check. */
test("curated module fails closed at ended-identity capacity without eviction", async () => {
  const f = fixture();
  for (let n = 0; n <= 128; n++) {
    await f.invoke(start(`session-${n}`));
    await f.invoke(end(`session-${n}`));
  }
  const count = f.calls.length;
  assert.equal(count, 129);
  await f.invoke(start("session-0"));
  await f.invoke(start("session-new"));
  assert.equal(f.calls.length, count);
  assert(f.timers.every(timer => timer.cancelled));
});

/** Older downstream failure also cleans only its own provisional responsibility,
 * preserving the shared same-ID owner still reserved by a newer callback. */
test("curated module preserves coalesced pending owner after older failure", async () => {
  const f = fixture();
  let rejectOld;
  let finishNew;
  const error = new Error("older failure");
  const old = f.invoke(start(), () => new Promise((_resolve, reject) => { rejectOld = reject; }));
  const fresh = f.invoke(start(), () => new Promise(resolve => { finishNew = resolve; }));
  rejectOld(error);
  await assert.rejects(old, value => value === error);
  finishNew(f.result);
  await fresh;
  assert.equal(f.calls.length, 1);
  assert.equal(f.timers[0].cancelled, false);
});

/** A helper already in flight may settle, but matching end closes local owner
 * before its reply; no late receipt can schedule a timer after that boundary. */
test("curated module fences end while source helper delivery is pending", async () => {
  const f = fixture();
  const run = f.sdk.process.run;
  let finish;
  f.sdk.process.run = argv => new Promise(resolve => { finish = () => run(argv).then(resolve); });
  const pending = f.invoke(start());
  await Promise.resolve();
  assert.equal(typeof finish, "function");
  await f.invoke(end());
  finish();
  assert.equal(await pending, f.result);
  assert.equal(f.calls.length, 1);
  assert.equal(f.timers.length, 0);
});

/** Projection is frozen before downstream work. Unknown/compact/child events
 * call next unchanged but cannot admit; stop failures preserve original error. */
test("curated module captures inert metadata and preserves negative paths", async () => {
  const f = fixture();
  const e = start();
  assert.equal(await f.invoke(e, async original => {
    assert.equal(original, e); e.session_id = "foreign"; e.source = "compact"; return f.result;
  }), f.result);
  assert.equal(f.calls[0].external_session_id, "session-a");
  assert.equal(f.calls[0].session_boundary, "startup");
  for (const event of [start("session-b", { source: "compact" }), start("session-b", { agentId: "child" }),
    start("session-b", { source: "unknown" }), end("session-a", { reason: "unknown" })]) await f.invoke(event);
  assert.equal(f.calls.length, 1);
  assert.equal(f.timers[0].cancelled, false);
  const error = new Error("end failure");
  await assert.rejects(f.invoke(end(), async () => { throw error; }), value => value === error);
  assert.equal(f.timers[0].cancelled, false);
  await f.invoke(end());
  await f.invoke(start());
  assert.equal(f.calls.length, 1, "same-ID end/restart cannot rebind a stopped owner");
});

/** Acceptance ordering is captured before downstream waits. An older start
 * must not rebind/stop a newer successful main session when it finally returns. */
test("curated module fences delayed starts and preserves vendor failure", async () => {
  const f = fixture();
  let finish;
  const old = f.invoke(start(), () => new Promise(resolve => { finish = resolve; }));
  await f.invoke(start("session-b"));
  finish(f.result);
  assert.equal(await old, f.result);
  assert.equal(f.calls.length, 1);
  assert.equal(f.calls[0].external_session_id, "session-b");
  const error = new Error("vendor failure");
  await assert.rejects(f.invoke(start("session-c"), async () => { throw error; }), value => value === error);
  assert.equal(f.calls.length, 1);
  assert.equal(f.timers[0].cancelled, false);
});
