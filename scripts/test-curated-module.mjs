/** Literal module-entry regression host. No vendor, process, socket or store.
 * Actual source body remains exercised separately; this host focuses on module
 * ownership, main/child filtering, ordering and exact unchanged middleware. */
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { createCuratedLifetime } from "../crates/mezzanine/src/integrations/bootstrap/curated_lifetime.mjs";
import { projectClaudeEvent, createClaudeLifecycleSender } from "../crates/mezzanine/src/integrations/bootstrap/claude_observer.mjs";
import { createCuratedRequestAccountingClient } from "../crates/mezzanine/src/integrations/bootstrap/curated_request_accounting.mjs";
import { captureClaudeStep, identifyClaudeStep, projectClaudeStepOutcome } from "../crates/mezzanine/src/integrations/bootstrap/claude_request.mjs";

const template = readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/curated_module.mjs", import.meta.url), "utf8");
const source = readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/curated_client.mjs", import.meta.url), "utf8");
const body = source.replace(/__MEZ_[A-Z]+__/g, marker => ({ __MEZ_OWNED__: "true", __MEZ_LIFETIME__: "observerLifetime",
  __MEZ_INTERVAL__: "10000", __MEZ_HELPER__: JSON.stringify("/owned/mez"), __MEZ_INSTANCE__: "startInstance",
  __MEZ_REPLACING__: "previousObserver !== undefined", __MEZ_PREDECESSOR__: "previousObserver" })[marker]);
const code = template.replace(/^import .*;\n/gm, "").replace("export function register", "function register")
  .replace("__MEZ_SOURCE_BODY__", body)
  .replace("__MEZ_LIFECYCLE_BODY__", readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/claude_lifecycle_runner.mjs", import.meta.url), "utf8").replace("__MEZ_HELPER__", JSON.stringify("/owned/mez")))
  .replace("__MEZ_STEP_BODY__", readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/claude_request_step.mjs", import.meta.url), "utf8")
    .replace("__MEZ_REQUEST_BODY__", readFileSync(new URL("../crates/mezzanine/src/integrations/bootstrap/curated_request_runner.mjs", import.meta.url), "utf8")
      .replace("__MEZ_HELPER__", JSON.stringify("/owned/mez"))));
const register = new Function("createCuratedLifetime", "projectClaudeEvent", "createCuratedRequestAccountingClient", "captureClaudeStep", "identifyClaudeStep", "projectClaudeStepOutcome", "createClaudeLifecycleSender", `${code}\nreturn register;`)(createCuratedLifetime, projectClaudeEvent, createCuratedRequestAccountingClient, captureClaudeStep, identifyClaudeStep, projectClaudeStepOutcome, createClaudeLifecycleSender);

const start = (session = "session-a", extra = {}) => ({ hook_event_name: "SessionStart", session_id: session, source: "startup", ...extra });
const end = (session = "session-a", extra = {}) => ({ hook_event_name: "SessionEnd", session_id: session, reason: "clear", ...extra });

/** Minimal metadata-only SDK host records fixed helper calls and cancellable
 * timers; every returned receipt matches the requested native-independent slot. */
function fixture() {
  const hooks = new Map();
  register((name, callback) => { assert(!hooks.has(name)); hooks.set(name, callback); });
  const calls = [];
  const lifecycle = [];
  const timers = [];
  const sdk = { process: { run: async (argv, options) => {
    if (argv[1] === "harness-event") { lifecycle.push(JSON.parse(options.stdin)); return { exitCode: 0, stdout: "{}\n" }; }
    const capsule = JSON.parse(argv[2]); calls.push(capsule);
    const receipt = capsule.operation === "curated-usage-begin"
      ? { protocol: "external-request/1", accepted: true, request_id: capsule.request_id,
        admitted_at: 100, source_witness: "c".repeat(64), origin: "unattributed", controls: [] }
      : capsule.operation === "curated-usage-end"
      ? { accepted: true, durable: true, applied: false, revision: 1,
        coverage: { source: "external-reported", reasoning_known: false } }
      : capsule.operation ? { observed: true, changed: true, sequence: capsule.sequence }
      : { protocol: "external-agent/1", registered: true, controls: [], agent_id: "external-a", generation: 1,
        observer_witness: "a".repeat(64), run_id: 1, observer_epoch: 1, observer_instance: capsule.observer_instance,
        external_session_id: capsule.external_session_id, usage: "unavailable-source-continuity",
        observer_transport: "unavailable-curated-freshness", expires_at_unix_seconds: 123, lease_seconds: 60 };
    return { exitCode: 0, stdout: JSON.stringify(receipt) };
  } }, clock: { every: (_delay, callback) => { const timer = { callback, cancelled: false, cancel() { this.cancelled = true; } }; timers.push(timer); return timer; } } };
  const invoke = (event, next = async () => result) => hooks.get(`classic.${event.hook_event_name}`)(sdk, event, next);
  const result = Object.freeze({ unchanged: true });
  return { hooks, calls, lifecycle, timers, sdk, invoke, result };
}

/** One retained owner coalesces duplicate main starts, and exact matching end
 * stops its timer without treating Stop/tool/child observations as retirement. */
test("curated module owns one main session and filters child or foreign end", async () => {
  const f = fixture();
  assert.deepEqual([...f.hooks.keys()], ["classic.SessionStart", "classic.SessionEnd", "classic.UserPromptSubmit", "classic.Notification", "classic.Stop", "classic.StopFailure", "turn.step"]);
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

/** The literal assembled entry cannot reuse a negatively acknowledged owner
 * for same-ID admission or manufacture a clear predecessor from its lost
 * receipt. Closing telemetry withholds proof/lifecycle helpers but leaves main
 * vendor callback results and a separately admitted changed-ID owner intact. */
test("curated module withholds same-ID helpers after negative proof", async () => {
  const f = fixture();
  const run = f.sdk.process.run;
  f.sdk.process.run = (argv, options) => argv[1] === "harness-source"
    && JSON.parse(argv[2]).operation === "curated-heartbeat"
    ? Promise.resolve({ exitCode: 0, stdout: '{"observed":false,"terminal":true}' }) : run(argv, options);
  assert.equal(await f.invoke(start()), f.result);
  const old = f.timers[0];
  await old.callback();
  assert.equal(old.cancelled, true);
  const calls = f.calls.length;
  const lifecycle = f.lifecycle.length;
  await old.callback();
  assert.equal(await f.invoke(start()), f.result);
  assert.equal(await f.invoke({ hook_event_name: "UserPromptSubmit", session_id: "session-a" }), f.result);
  assert.equal(await f.invoke(end()), f.result);
  assert.equal(await f.invoke(start("session-a", { source: "clear" })), f.result);
  assert.equal(f.calls.length, calls);
  assert.equal(f.lifecycle.length, lifecycle);
  assert.equal(f.timers.length, 1);
  assert.equal(await f.invoke(start("session-b")), f.result);
  assert.equal(f.timers.length, 2);
  assert.equal(f.timers[1].cancelled, false);
  await old.callback();
  assert.equal(f.calls.length, calls + 1);
});

/** A sequential same-ID clear may rotate only using the public predecessor
 * captured from this exact loaded module before its end stops the old lifetime.
 * Duplicate starts coalesce, the old timer stays inert and every middleware
 * result remains unchanged. No daemon restart or unpaired resume is inferred. */
test("curated module pairs one completed same-ID clear with its captured predecessor", async () => {
  const f = fixture();
  const run = f.sdk.process.run;
  f.sdk.process.run = async (argv, options) => {
    const value = await run(argv, options);
    if (argv[1] === "harness-source") {
      const capsule = JSON.parse(argv[2]);
      if (capsule.predecessor_generation !== undefined) {
        assert.equal(capsule.predecessor_generation, 1);
        assert.notEqual(capsule.observer_instance, "mez-curated-client-1");
        const receipt = JSON.parse(value.stdout);
        receipt.generation = 2; receipt.observer_epoch = 2; receipt.observer_witness = "b".repeat(64);
        value.stdout = JSON.stringify(receipt);
      }
    }
    return value;
  };
  await f.invoke(start());
  assert.equal(await f.invoke(end()), f.result);
  assert.equal(f.timers[0].cancelled, true);
  const lifecycleCount = f.lifecycle.length;
  assert.equal(await f.invoke(start("session-a", { source: "clear" })), f.result);
  assert.equal(f.calls.length, 2);
  assert.equal(f.calls[1].session_boundary, "clear");
  assert.equal(f.calls[1].predecessor_generation, 1);
  assert.equal(f.timers.length, 2);
  assert.equal(f.timers[1].cancelled, false);
  assert.equal(f.lifecycle.length, lifecycleCount + 1);
  assert.equal(f.lifecycle.at(-1).generation, 2);
  await f.invoke(start("session-a", { source: "clear" }));
  assert.equal(f.calls.length, 2);
  await f.timers[0].callback();
  assert.equal(f.calls.length, 2);
});

/** A delayed end from the original same-ID owner cannot stop or fence a newer
 * clear successor. A second legitimate clear must consume only that successor's
 * public predecessor, with a distinct instance and permanently inert old timer. */
test("curated module retains only current clear predecessor after a delayed old end", async () => {
  const f = fixture();
  const run = f.sdk.process.run;
  f.sdk.process.run = async (argv, options) => {
    const value = await run(argv, options);
    if (argv[1] === "harness-source") {
      const capsule = JSON.parse(argv[2]);
      if (capsule.predecessor_generation !== undefined) {
        const generation = capsule.predecessor_generation + 1;
        const receipt = JSON.parse(value.stdout);
        receipt.generation = generation; receipt.observer_epoch = generation;
        receipt.observer_witness = generation.toString(16).padStart(64, "0");
        value.stdout = JSON.stringify(receipt);
      }
    }
    return value;
  };
  await f.invoke(start());
  let finishOld;
  const oldEnd = f.invoke(end(), () => new Promise(resolve => { finishOld = resolve; }));
  await f.invoke(end());
  await f.invoke(start("session-a", { source: "clear" }));
  finishOld(f.result); await oldEnd;
  assert.equal(f.calls.length, 2);
  assert.equal(f.timers[1].cancelled, false);
  await f.invoke(end());
  await f.invoke(start("session-a", { source: "clear" }));
  assert.equal(f.calls.length, 3);
  assert.equal(f.calls[2].predecessor_generation, 2);
  assert.notEqual(f.calls[2].observer_instance, f.calls[1].observer_instance);
  assert.equal(f.timers[1].cancelled, true);
  assert.equal(f.timers[2].cancelled, false);
});

/** Missing/mismatched predecessor or failed downstream completion must never
 * fall back to a fresh namespace. The observed next start consumes the one-slot
 * pair even when it cannot enroll; callback errors and return values stay exact. */
test("curated module refuses unpaired or failed same-ID clear without fallback", async () => {
  for (const reason of ["startup", "resume", "fork", "compact"]) {
    const f = fixture();
    await f.invoke(start()); await f.invoke(end());
    await f.invoke(start("session-a", { source: reason }));
    await f.invoke(start("session-a", { source: "clear" }));
    assert.equal(f.calls.length, 1);
    assert.equal(f.timers[0].cancelled, true);
  }
  const f = fixture();
  await f.invoke(start()); await f.invoke(end("session-a", { reason: "logout" }));
  await f.invoke(start("session-a", { source: "clear" }));
  assert.equal(f.calls.length, 1);
  const failed = fixture();
  await failed.invoke(start()); await failed.invoke(end());
  const error = new Error("downstream clear failed");
  await assert.rejects(failed.invoke(start("session-a", { source: "clear" }), async () => { throw error; }), value => value === error);
  await failed.invoke(start("session-a", { source: "clear" }));
  assert.equal(failed.calls.length, 1);
});

/** A native handoff may fail, expire or return an invalid public successor.
 * The consumed pair cannot be retried as initial enrollment, and no timer or
 * lifecycle sender is created from an unchanged generation/epoch receipt. */
test("curated module refuses lost or invalid successor replies without re-admission", async () => {
  for (const failure of ["unchanged", "unavailable", "throw"]) {
    const f = fixture();
    const run = f.sdk.process.run;
    f.sdk.process.run = async (argv, options) => {
      const value = await run(argv, options);
      if (argv[1] === "harness-source" && JSON.parse(argv[2]).predecessor_generation !== undefined) {
        if (failure === "throw") throw new Error("lost telemetry");
        if (failure === "unavailable") value.stdout = '{"registered":false}';
      }
      return value;
    };
    await f.invoke(start()); await f.invoke(end());
    const published = f.lifecycle.length;
    assert.equal(await f.invoke(start("session-a", { source: "clear" })), f.result);
    assert.equal(f.calls.length, 2);
    assert.equal(f.timers.length, 1);
    assert.equal(f.timers[0].cancelled, true);
    assert.equal(f.lifecycle.length, published);
    await f.invoke(start("session-a", { source: "clear" }));
    assert.equal(f.calls.length, 2);
  }
});

/** A same-ID start observed while end delivery is pending cannot later borrow
 * that older end's public receipt. A matching end during a clear's downstream
 * wait closes its provisional owner before any successor helper can launch. */
test("curated module fences overlapping clear ends and starts", async () => {
  const f = fixture();
  await f.invoke(start());
  const run = f.sdk.process.run;
  let finishEnd;
  f.sdk.process.run = (argv, options) => argv[1] === "harness-event"
    ? new Promise(resolve => { finishEnd = () => run(argv, options).then(resolve); }) : run(argv, options);
  const ending = f.invoke(end());
  await Promise.resolve(); await Promise.resolve();
  assert.equal(typeof finishEnd, "function");
  await f.invoke(start("session-a", { source: "clear" }));
  finishEnd(); await ending;
  await f.invoke(start("session-a", { source: "clear" }));
  assert.equal(f.calls.length, 1);

  const pending = fixture();
  await pending.invoke(start()); await pending.invoke(end());
  let finishStart;
  const starting = pending.invoke(start("session-a", { source: "clear" }), () => new Promise(resolve => { finishStart = resolve; }));
  await pending.invoke(end());
  finishStart(pending.result); await starting;
  assert.equal(pending.calls.length, 1);
  assert.equal(pending.timers.length, 1);
  assert.equal(pending.timers[0].cancelled, true);
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

const stepInput = (extra = {}) => ({ turnId: "turn-a", index: 0, model: "selected", messageCount: 2, ...extra });
const stepResult = (extra = {}) => ({ turnId: "turn-a", index: 0, answer: "PRIVATE", toolUses: [], stopReason: "end_turn",
  usage: { model: "response", input_tokens: 10, output_tokens: 3, cache_read_input_tokens: 2, cache_creation_input_tokens: 4 }, ...extra });

/** Both immediate ACK attempts can fail after expense already committed. A
 * later genuine main step must resend the retained original normalized report,
 * never run its delegate again, reuse new readiness, or retain answer/chunks. */
test("Claude pending outcome survives failed ACK and retries on a later main step", async () => {
  const f = fixture(); await f.invoke(start());
  const run = f.sdk.process.run;
  let outage = true, delegates = 0;
  f.sdk.process.run = async (argv, options) => {
    const response = await run(argv, options);
    return JSON.parse(argv[2]).operation === "curated-usage-end" && outage
      ? { exitCode: 0, stdout: "{}" } : response;
  };
  const original = stepInput(), final = stepResult();
  const stream = f.hooks.get("turn.step")(f.sdk, original, async function* (passed) {
    assert.equal(passed, original); delegates++; return final;
  });
  assert.equal((await stream.next()).value, final);
  const endings = () => f.calls.filter(call => call.operation === "curated-usage-end");
  assert.equal(endings().length, 2);
  const report = endings()[0];
  assert.deepEqual(endings()[1], report);
  outage = false;
  const nextInput = stepInput({ turnId: "turn-b" }), nextFinal = stepResult({ turnId: "turn-b" });
  const next = f.hooks.get("turn.step")(f.sdk, nextInput, async function* (passed) {
    assert.equal(passed, nextInput); delegates++;
    assert.equal(endings().length, 3, "retained expense retries before the new delegate");
    assert.deepEqual(endings()[2], report, "source/time/origin/report identity stays original");
    return nextFinal;
  });
  assert.equal((await next.next()).value, nextFinal);
  assert.equal(delegates, 2, "one delegate per genuine request, never delivery retry");
  assert.equal(endings().length, 4);
  assert.notEqual(endings()[3].event_id, report.event_id);
  assert(!JSON.stringify(f.calls).includes("PRIVATE"));
});

/** Repeated callbacks for one locally retained marker cannot multiply pending
 * obligations or replace its original normalized payload. Retry still uses
 * exact original readiness; each callback delegates unchanged once. */
test("Claude pending marker replay does not duplicate obligations or replace counters", async () => {
  const f = fixture(); await f.invoke(start());
  const run = f.sdk.process.run;
  f.sdk.process.run = async (argv, options) => {
    const response = await run(argv, options);
    return JSON.parse(argv[2]).operation === "curated-usage-end" ? { exitCode: 0, stdout: "{}" } : response;
  };
  for (let index = 0; index < 20; index++) {
    const outcome = stepResult({ usage: { ...stepResult().usage, input_tokens: 10 + index } });
    const stream = f.hooks.get("turn.step")(f.sdk, stepInput(), async function* () { return outcome; });
    assert.equal((await stream.next()).value, outcome);
  }
  const endings = f.calls.filter(call => call.operation === "curated-usage-end");
  assert.equal(endings.length, 40, "only the first report plus one retry call per later step");
  assert(endings.every(call => JSON.stringify(call) === JSON.stringify(endings[0])));
  const before = f.calls.length;
  f.sdk.process.run = run;
  const distinct = f.hooks.get("turn.step")(f.sdk, stepInput({ index: 1 }), async function* () { return stepResult({ index: 1 }); });
  await distinct.next();
  assert.equal(f.calls.slice(before).filter(call => call.operation === "curated-usage-end").length, 2,
    "one original retained obligation and one distinct new report");
});

/** Simultaneous callback replay can acquire readiness before either result is
 * known, but one original marker must still own only one retained obligation.
 * Mixed active/pending pressure must refuse a new marker without evicting it. */
test("Claude concurrent replay and mixed pending active capacity conserve obligations", async () => {
  const f = fixture(); await f.invoke(start());
  const run = f.sdk.process.run;
  let outage = true;
  f.sdk.process.run = async (argv, options) => {
    const response = await run(argv, options);
    return JSON.parse(argv[2]).operation === "curated-usage-end" && outage
      ? { exitCode: 0, stdout: "{}" } : response;
  };
  const streams = [];
  for (let index = 0; index < 2; index++) {
    const stream = f.hooks.get("turn.step")(f.sdk, stepInput(), async function* () {
      yield "unchanged"; return stepResult();
    });
    await stream.next(); streams.push(stream);
  }
  for (const stream of streams) await stream.next();
  outage = false;
  const before = f.calls.length;
  const distinct = f.hooks.get("turn.step")(f.sdk, stepInput({ index: 1 }), async function* () { return stepResult({ index: 1 }); });
  await distinct.next();
  assert.equal(f.calls.slice(before).filter(call => call.operation === "curated-usage-end").length, 2,
    "one coalesced retained marker and one new report");
  outage = true;
  for (let index = 2; index < 10; index++) {
    const stream = f.hooks.get("turn.step")(f.sdk, stepInput({ index }), async function* () { return stepResult({ index }); });
    await stream.next();
  }
  const active = [];
  for (let index = 10; index < 18; index++) {
    const stream = f.hooks.get("turn.step")(f.sdk, stepInput({ index }), async function* () { yield "unchanged"; });
    await stream.next(); active.push(stream);
  }
  const prior = f.calls.length;
  const overflow = f.hooks.get("turn.step")(f.sdk, stepInput({ index: 18 }), async function* () { return "unchanged"; });
  assert.equal((await overflow.next()).value, "unchanged");
  assert.equal(f.calls.slice(prior).filter(call => call.operation === "curated-usage-begin").length, 2,
    "eight active plus eight pending leaves only original retry readiness, no new marker");
  for (const stream of active) await stream.return();
});

/** One in-flight pending retry cannot be duplicated by overlapping main steps.
 * A changed observer fences its late ACK and original marker; both delegates
 * still run once and return exact results without borrowing successor proof. */
test("Claude pending retry serializes overlapping steps and fences late replacement", async () => {
  const f = fixture(); await f.invoke(start());
  const run = f.sdk.process.run;
  let lost = 0, release, issued;
  const started = new Promise(resolve => { issued = resolve; });
  f.sdk.process.run = async (argv, options) => {
    const response = await run(argv, options);
    if (JSON.parse(argv[2]).operation === "curated-usage-end") {
      if (lost++ < 2) return { exitCode: 0, stdout: "{}" };
      if (lost === 3) { issued(); return new Promise(resolve => { release = () => resolve(response); }); }
    }
    return response;
  };
  const initial = f.hooks.get("turn.step")(f.sdk, stepInput(), async function* () { return stepResult(); });
  await initial.next();
  const slowFinal = stepResult({ index: 1 });
  const slow = f.hooks.get("turn.step")(f.sdk, stepInput({ index: 1 }), async function* () { return slowFinal; });
  const completing = slow.next();
  await started;
  const fastFinal = stepResult({ index: 2 });
  const fast = f.hooks.get("turn.step")(f.sdk, stepInput({ index: 2 }), async function* () { return fastFinal; });
  assert.equal((await fast.next()).value, fastFinal);
  const original = f.calls.find(call => call.operation === "curated-usage-end");
  assert.equal(f.calls.filter(call => call.operation === "curated-usage-end" && call.event_id === original.event_id).length, 3);
  await f.invoke(start("session-b"));
  release();
  assert.equal((await completing).value, slowFinal);
  const before = f.calls.length;
  const fresh = f.hooks.get("turn.step")(f.sdk, stepInput(), async function* () { return stepResult(); });
  await fresh.next();
  const sends = f.calls.slice(before).filter(call => call.operation === "curated-usage-end");
  assert.equal(sends.length, 1); assert.equal(sends[0].external_session_id, "session-b");
});

/** Active requests and retained outcomes share sixteen slots. Outage pressure
 * cannot evict prior expense to admit a seventeenth marker; subsequent main
 * steps retry one pending report at a time and eventually acknowledge every
 * original immutable obligation without a provider/delegate replay. */
test("Claude pending outcomes bound active plus retained work without eviction", async () => {
  const f = fixture(); await f.invoke(start());
  const run = f.sdk.process.run;
  let outage = true, delegates = 0;
  f.sdk.process.run = async (argv, options) => {
    const response = await run(argv, options);
    return JSON.parse(argv[2]).operation === "curated-usage-end" && outage
      ? { exitCode: 0, stdout: "{}" } : response;
  };
  const perform = async index => {
    const outcome = stepResult({ index });
    const stream = f.hooks.get("turn.step")(f.sdk, stepInput({ index }), async function* () {
      delegates++; return outcome;
    });
    assert.equal((await stream.next()).value, outcome);
  };
  for (let index = 0; index < 16; index++) await perform(index);
  const original = new Map(f.calls.filter(call => call.operation === "curated-usage-end")
    .map(call => [call.event_id, call]));
  assert.equal(original.size, 16);
  const begins = f.calls.filter(call => call.operation === "curated-usage-begin").length;
  const ends = f.calls.filter(call => call.operation === "curated-usage-end").length;
  await perform(16);
  assert.equal(delegates, 17);
  assert.equal(f.calls.filter(call => call.operation === "curated-usage-begin").length, begins + 2,
    "at pressure only the existing pending obligation may restore readiness");
  assert.equal(f.calls.filter(call => call.operation === "curated-usage-end").length, ends + 2);
  assert.equal(new Set(f.calls.filter(call => call.operation === "curated-usage-end").map(call => call.event_id)).size, 16);
  outage = false;
  const recovered = new Set();
  for (let index = 17; index < 33; index++) {
    const before = f.calls.length;
    await perform(index);
    const delivered = f.calls.slice(before).filter(call => call.operation === "curated-usage-end");
    assert.equal(delivered.length, 2, "one retained and one new report per main step");
    assert.deepEqual(delivered[0], original.get(delivered[0].event_id));
    recovered.add(delivered[0].event_id);
  }
  assert.equal(recovered.size, 16, "none of the incurred obligations was evicted");
});

/** Unknown/child steps never trigger pending delivery. Cancelled or missing
 * outcomes acquire no retained final report. Observer replacement, same-ID
 * clear, end and typed proof closure cannot move old expense to a new receipt. */
test("Claude pending reports remain fenced across unavailable outcomes and observer replacement", async () => {
  for (const boundary of ["new", "clear", "end", "proof"]) {
    const f = fixture(); await f.invoke(start());
    const run = f.sdk.process.run;
    f.sdk.process.run = async (argv, options) => {
      const response = await run(argv, options);
      return JSON.parse(argv[2]).operation === "curated-usage-end"
        ? { exitCode: 0, stdout: "{}" } : response;
    };
    const initial = f.hooks.get("turn.step")(f.sdk, stepInput(), async function* () { return stepResult(); });
    await initial.next();
    const before = f.calls.length;
    for (const event of [{}, stepInput({ agentId: "child" }), stepInput({ agentId: null })]) {
      const unknown = f.hooks.get("turn.step")(f.sdk, event, async function* () { return "unchanged"; });
      assert.equal((await unknown.next()).value, "unchanged");
    }
    assert.equal(f.calls.length, before);
    let cancelled = false;
    const stream = f.hooks.get("turn.step")(f.sdk, stepInput({ index: 1 }), async function* () {
      try { yield "unchanged"; } finally { cancelled = true; }
    });
    await stream.next(); await stream.return("cancelled");
    assert.equal(cancelled, true);
    const missing = f.hooks.get("turn.step")(f.sdk, stepInput({ index: 2 }), async function* () { return stepResult({ index: 2, usage: null }); });
    await missing.next();
    assert.equal(new Set(f.calls.filter(call => call.operation === "curated-usage-end").map(call => call.event_id)).size, 1);
    if (boundary === "new") await f.invoke(start("session-b"));
    else if (boundary === "clear") {
      await f.invoke(end());
      const originalRun = f.sdk.process.run;
      f.sdk.process.run = async (argv, options) => {
        const response = await originalRun(argv, options);
        if (JSON.parse(argv[2]).operation === undefined) {
          return { exitCode: 0, stdout: JSON.stringify({ ...JSON.parse(response.stdout), generation: 2,
            observer_witness: "b".repeat(64), observer_epoch: 2 }) };
        }
        return response;
      };
      await f.invoke(start("session-a", { source: "clear" }));
    }
    else if (boundary === "end") await f.invoke(end("session-a", { reason: "logout" }));
    else {
      f.sdk.process.run = (argv, options) => JSON.parse(argv[2]).operation === "curated-heartbeat"
        ? Promise.resolve({ exitCode: 0, stdout: '{"observed":false,"terminal":true}' }) : run(argv, options);
      await f.timers[0].callback();
    }
    f.sdk.process.run = run;
    const current = f.calls.length;
    const next = f.hooks.get("turn.step")(f.sdk, stepInput({ turnId: "next" }), async function* () { return stepResult({ turnId: "next" }); });
    await next.next();
    const delivered = f.calls.slice(current).filter(call => call.operation === "curated-usage-end");
    assert.equal(delivered.length, ["end", "proof"].includes(boundary) ? 0 : 1, boundary);
    assert(delivered.every(call => call.turnId === undefined && call.event_id !== f.calls.find(call => call.operation === "curated-usage-end").event_id));
  }
});

/** Restoring the same marker with changed native source evidence cannot send
 * its retained final outcome, even when the coarse time/origin still match.
 * No report mutates to a fresh ticket; reinstating original evidence allows
 * only the identical original report, followed by a genuine new request. */
test("Claude pending report refuses changed source readiness without rebinding", async () => {
  const f = fixture(); await f.invoke(start());
  const run = f.sdk.process.run;
  let outage = true, changed = false;
  f.sdk.process.run = async (argv, options) => {
    const response = await run(argv, options);
    const operation = JSON.parse(argv[2]).operation;
    if (operation === "curated-usage-end" && outage) return { exitCode: 0, stdout: "{}" };
    if (operation === "curated-usage-begin" && changed) {
      return { exitCode: 0, stdout: JSON.stringify({ ...JSON.parse(response.stdout), source_witness: "d".repeat(64) }) };
    }
    return response;
  };
  const initial = f.hooks.get("turn.step")(f.sdk, stepInput(), async function* () { return stepResult(); });
  await initial.next();
  const original = f.calls.find(call => call.operation === "curated-usage-end");
  outage = false; changed = true;
  const changedStep = f.hooks.get("turn.step")(f.sdk, stepInput({ index: 1 }), async function* () { return stepResult({ index: 1, usage: null }); });
  await changedStep.next();
  assert.equal(f.calls.filter(call => call.operation === "curated-usage-end").length, 2);
  changed = false;
  const recovered = f.hooks.get("turn.step")(f.sdk, stepInput({ index: 2 }), async function* () { return stepResult({ index: 2 }); });
  await recovered.next();
  assert.deepEqual(f.calls.filter(call => call.operation === "curated-usage-end")[2], original);
});

/** The literal generator delegates once, exposes each unchanged chunk as soon
 * as it arrives, and returns the exact final object. Only final request usage is
 * charged; timestamps and opaque IDs remain identical on callback replay. */
test("Claude request generator preserves chunks and final result with one usage source", async () => {
  const f = fixture(); await f.invoke(start());
  const chunks = [Object.freeze({ kind: "text", text: "PRIVATE" }), Object.freeze({ kind: "engine", ref: 1 })];
  const final = stepResult();
  const event = stepInput();
  let delegates = 0;
  const stream = f.hooks.get("turn.step")(f.sdk, event, async function* (passed) {
    assert.equal(passed, event); delegates++;
    assert.equal(f.calls.filter(call => call.operation === "curated-usage-begin").length, 1);
    yield chunks[0]; yield chunks[1]; return final;
  });
  assert.equal((await stream.next()).value, chunks[0]);
  assert.equal(f.calls.filter(call => call.operation === "curated-usage-end").length, 0);
  assert.equal((await stream.next()).value, chunks[1]);
  const completed = await stream.next();
  assert.equal(completed.done, true); assert.equal(completed.value, final);
  assert.equal(delegates, 1);
  const first = f.calls.find(call => call.operation === "curated-usage-end");
  assert.equal(first.observed_at, 100);
  assert.deepEqual(first.counters, { input_tokens: 16, output_tokens: 3, cached_input_tokens: 2, cache_write_input_tokens: 4 });
  assert.equal(first.model, "response"); assert.equal(first.provider, "unavailable");
  assert(!JSON.stringify(first).includes("PRIVATE"));
  const replay = f.hooks.get("turn.step")(f.sdk, stepInput(), async function* () { return final; });
  assert.equal((await replay.next()).value, final);
  const endings = f.calls.filter(call => call.operation === "curated-usage-end");
  assert.equal(endings.length, 2); assert.deepEqual(endings[0], endings[1]);
});

/** Downstream errors and consumer cancellation retain exact identity and native
 * generator cleanup. Missing readiness/counters or child identity never bills,
 * while the underlying vendor request still runs exactly once. */
test("Claude request generator preserves error cancellation and unavailable paths", async () => {
  const f = fixture(); await f.invoke(start());
  const error = new Error("vendor error");
  const failed = f.hooks.get("turn.step")(f.sdk, stepInput(), async function* () { throw error; });
  await assert.rejects(failed.next(), caught => caught === error);
  let closed = false;
  const cancelled = f.hooks.get("turn.step")(f.sdk, stepInput(), async function* () {
    try { yield "original"; return stepResult(); } finally { closed = true; }
  });
  assert.equal((await cancelled.next()).value, "original");
  assert.equal((await cancelled.return("cancelled")).value, "cancelled");
  assert.equal(closed, true);
  for (const [event, final] of [[stepInput({ agentId: "child" }), stepResult()], [stepInput({ agentId: null }), stepResult()],
    [stepInput(), stepResult({ usage: null })], [stepInput(), stepResult({ turnId: "other" })]]) {
    const stream = f.hooks.get("turn.step")(f.sdk, event, async function* () { return final; });
    assert.equal((await stream.next()).value, final);
  }
  assert.equal(f.calls.filter(call => call.operation === "curated-usage-end").length, 0);
});

/** Old-owner completion cannot borrow a newly started session's receipt. The
 * sixteen-active-request limit refuses telemetry only; the seventeenth vendor
 * delegate proceeds unchanged and does not silently evict an existing report. */
test("Claude request generator fences owner replacement and active pressure", async () => {
  const f = fixture(); await f.invoke(start());
  const streams = [];
  for (let index = 0; index < 16; index++) {
    const stream = f.hooks.get("turn.step")(f.sdk, stepInput({ index }), async function* () {
      yield "chunk"; return stepResult({ index });
    });
    assert.equal((await stream.next()).value, "chunk"); streams.push(stream);
  }
  const before = f.calls.length;
  const extra = f.hooks.get("turn.step")(f.sdk, stepInput({ index: 16 }), async function* () { return "unchanged"; });
  assert.equal((await extra.next()).value, "unchanged"); assert.equal(f.calls.length, before);
  await f.invoke(start("session-b"));
  for (const stream of streams) assert.equal((await stream.next()).done, true);
  assert.equal(f.calls.filter(call => call.operation === "curated-usage-end").length, 0);
  const current = f.hooks.get("turn.step")(f.sdk, stepInput(), async function* () { return stepResult(); });
  await current.next();
  assert.equal(f.calls.at(-1).operation, "curated-usage-end");
  assert.equal(f.calls.at(-1).external_session_id, "session-b");
});

/** Literal wrappers preserve next(original), results/errors and capture order.
 * Unknown/child/foreign payloads cannot publish, and old completion cannot undo
 * a newer prompt or borrow the replacement session's captured public witness. */
test("Claude literal lifecycle captures before next and fences stale status", async () => {
  const f = fixture(); await f.invoke(start());
  const event = (name, extra = {}) => ({ session_id: "session-a", hook_event_name: name, ...extra });
  let finish;
  const slow = f.invoke(event("Stop", { stop_hook_active: true }), () => new Promise(resolve => { finish = resolve; }));
  const prompt = event("UserPromptSubmit", { prompt: "PRIVATE" });
  assert.equal(await f.invoke(prompt, async original => { assert.equal(original, prompt); return f.result; }), f.result);
  finish(f.result); await slow;
  assert.deepEqual(f.lifecycle.map(value => value.data.state), ["ready", "running"]);
  await f.invoke(event("Notification", { notification_type: "permission_prompt", message: "PRIVATE" }));
  await f.invoke(event("Notification", { notification_type: "unknown" }));
  await f.invoke(event("Stop", { stop_hook_active: true, agent_id: "child" }));
  const error = new Error("vendor");
  await assert.rejects(f.invoke(event("StopFailure"), async () => { throw error; }), caught => caught === error);
  assert.equal(f.lifecycle.at(-1).data.state, "approval-wait");
  assert(!JSON.stringify(f.lifecycle).includes("PRIVATE"));
  let lateFinish;
  const late = f.invoke(event("StopFailure"), () => new Promise(resolve => { lateFinish = resolve; }));
  await f.invoke(start("session-b"));
  lateFinish(f.result); await late;
  assert.equal(f.lifecycle.at(-1).external_session_id, "session-b");
  assert.equal(f.lifecycle.at(-1).data.state, "ready");
});
