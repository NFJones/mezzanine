/** Pure lifetime regressions: no vendor, process, socket, storage or SDK I/O.
 * The caller retains this owner; tickets/public receipts never supply native
 * authority, and permanent stop fences pending admissions and queued timers. */
import assert from "node:assert/strict";
import test from "node:test";
import { createCuratedLifetime } from "../crates/mezzanine/src/integrations/bootstrap/curated_lifetime.mjs";

const receipt = () => ({ external_session_id: "session-a", generation: 7,
  observer_witness: "a".repeat(64), run_id: 1, observer_epoch: 1, agent_id: "external-a" });

/** A copied/foreign/unknown ticket must not inspect or cancel a timer, including
 * the genuine current timer. Rejection cannot mutate the current observer or
 * invoke an unrelated resource method before ownership is established. */
test("curated lifetime rejects copied and foreign timer tickets without effects", () => {
  const owner = createCuratedLifetime();
  const ticket = owner.begin("session-a");
  assert.equal(owner.publish(ticket, receipt()), true);
  let inspected = 0;
  let cancelled = 0;
  const timer = { get cancel() { inspected++; return () => { cancelled++; }; } };
  assert.equal(owner.attachTimer(ticket, timer), true);
  const baseline = inspected;
  const other = createCuratedLifetime();
  const foreign = other.begin("session-a");
  for (const invalid of [{ ...ticket }, {}, foreign, undefined]) {
    assert.equal(owner.attachTimer(invalid, timer), false);
    assert.equal(inspected, baseline);
    assert.equal(cancelled, 0);
    assert.equal(owner.current(ticket), true);
  }
  assert.equal(owner.attachTimer(ticket, timer), true);
  assert.equal(inspected, baseline);
  const empty = createCuratedLifetime();
  assert.equal(empty.attachTimer({}, timer), false);
  assert.equal(inspected, baseline);
  assert.equal(cancelled, 0);
  const pendingOwner = createCuratedLifetime();
  const pending = pendingOwner.begin("session-a");
  assert.equal(pendingOwner.attachTimer(pending, timer), false);
  assert.equal(pendingOwner.release(pending), true);
  assert.equal(pendingOwner.attachTimer(pending, timer), false);
  assert.equal(inspected, baseline);
  assert.equal(cancelled, 0);
});

/** Metadata/resource accessors can synchronously stop the owner. Completion
 * must recheck its exact ticket after projection or timer inspection, so closed
 * state cannot retain a late receipt or an uncancelled timer. */
test("curated lifetime remains stopped through reentrant projection and timer access", () => {
  const owner = createCuratedLifetime();
  const ticket = owner.begin("session-a");
  const value = receipt();
  Object.defineProperty(value, "generation", { enumerable: true, get() { owner.stop(); return 7; } });
  assert.equal(owner.publish(ticket, value), false);
  assert.equal(owner.receipt, undefined);
  const timerOwner = createCuratedLifetime();
  const timerTicket = timerOwner.begin("session-a");
  assert.equal(timerOwner.publish(timerTicket, receipt()), true);
  let cancelled = 0;
  const timer = { get cancel() { timerOwner.stop(); return () => { cancelled++; }; } };
  assert.equal(timerOwner.attachTimer(timerTicket, timer), false);
  assert.equal(cancelled, 1);
  assert.equal(timerOwner.current(timerTicket), false);
});

/** Timer cancellation throws/rejections are telemetry loss. The owner closes
 * first and retains no active receipt or timer, regardless of cancellation. */
test("curated lifetime closes before failing timer cancellation", async () => {
  for (const asynchronous of [false, true]) {
    const owner = createCuratedLifetime();
    const ticket = owner.begin("session-a");
    assert.equal(owner.publish(ticket, receipt()), true);
    let cancelled = 0;
    owner.attachTimer(ticket, { cancel() {
      cancelled++;
      assert.equal(owner.current(ticket), false);
      if (asynchronous) return Promise.reject(new Error("unavailable"));
      throw new Error("unavailable");
    } });
    owner.stop();
    await Promise.resolve();
    owner.stop();
    assert.equal(cancelled, 1);
    assert.equal(owner.receipt, undefined);
  }
});

/** One pending ticket and one immutable active identity bound resource use;
 * repeat admission cannot create a queue or overwrite a published observer. */
test("curated lifetime owns one pending admission and one public observer", () => {
  const owner = createCuratedLifetime();
  const ticket = owner.begin("session-a");
  assert(ticket);
  assert.equal(owner.begin("session-a"), undefined);
  const original = receipt();
  assert.equal(owner.publish(ticket, original), true);
  original.generation = 99;
  assert.equal(owner.receipt.generation, 7);
  assert(Object.isFrozen(owner.receipt));
  assert.equal(owner.begin("session-b"), undefined);
  assert.equal(owner.publish(ticket, receipt()), false);
  let cancelled = 0;
  assert.equal(owner.attachTimer(ticket, { cancel() { cancelled++; } }), true);
  assert.equal(owner.current(ticket), true);
  owner.stop();
  owner.stop();
  assert.equal(cancelled, 1);
  assert.equal(owner.current(ticket), false);
  assert.equal(owner.receipt, undefined);
  assert.equal(owner.begin("session-a"), undefined);
});

/** Stop before admission completion permanently prevents late publication and
 * cancels any late timer once, without acquiring a successor or reviving state. */
test("curated lifetime rejects stale tickets and late resources after stop", () => {
  const owner = createCuratedLifetime();
  const ticket = owner.begin("session-a");
  owner.stop();
  assert.equal(owner.publish(ticket, receipt()), false);
  let cancelled = 0;
  assert.equal(owner.attachTimer(ticket, { cancel() { cancelled++; } }), false);
  assert.equal(cancelled, 1);
  assert.equal(owner.current(ticket), false);
  assert.equal(owner.begin("session-a"), undefined);
});

/** Only the exact ticket closed during publication/creation may dispose its
 * late timer. Copied tickets have no effects, and repeating an already-owned
 * or late cleanup handle cannot repeat cancellation. */
test("curated lifetime keeps exact stopped-ticket cleanup bounded and idempotent", () => {
  const owner = createCuratedLifetime();
  const ticket = owner.begin("session-a");
  owner.publish(ticket, receipt());
  let cancelled = 0;
  const timer = { cancel() { cancelled++; } };
  owner.attachTimer(ticket, timer);
  owner.stop();
  assert.equal(owner.attachTimer(ticket, timer), false);
  assert.equal(cancelled, 1);
  const late = createCuratedLifetime();
  const lateTicket = late.begin("session-a");
  late.publish(lateTicket, receipt());
  late.stop();
  assert.equal(late.attachTimer({ ...lateTicket }, timer), false);
  assert.equal(cancelled, 1);
  assert.equal(late.attachTimer(lateTicket, timer), false);
  assert.equal(late.attachTimer(lateTicket, timer), false);
  assert.equal(cancelled, 2);
  assert.equal(late.receipt, undefined);
});

/** Releasing a failed attempt requires its exact ticket; a copied selector or
 * old completion cannot publish/release another admitted pending generation. */
test("curated lifetime release and receipt projection retain exact ownership", () => {
  const owner = createCuratedLifetime();
  assert.equal(owner.begin("private prompt"), undefined);
  const first = owner.begin("session-a");
  assert.equal(owner.publish({ ...first }, receipt()), false);
  assert.equal(owner.release({ ...first }), false);
  assert.equal(owner.release(first), true);
  const second = owner.begin("session-a");
  assert.notEqual(first, second);
  assert.equal(owner.publish(first, receipt()), false);
  assert.equal(owner.publish(second, { ...receipt(), external_session_id: "foreign" }), false);
  assert.equal(owner.publish(second, { ...receipt(), prompt: "private" }), false);
  assert.equal(owner.publish(second, receipt()), true);
  assert.equal(owner.release(second), false);
});
