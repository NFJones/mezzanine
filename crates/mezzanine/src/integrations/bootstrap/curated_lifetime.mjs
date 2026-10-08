/** One-slot curated lifetime with no imports, transport, process or storage I/O.
 * Only a caller-supplied timer's fixed cancellation method may be invoked.
 * Exact object tickets fence overlapping and stale completion. Only immutable
 * public selectors are retained; neither tickets nor receipts grant native
 * authority. Stop is permanent and precedes cancellation, so cancellation
 * failure/queued callbacks cannot restore an observer or authorize more helpers.
 * The caller separately owns module reuse, handoff persistence and SDK teardown. */
const identifier = value => typeof value === "string" && value.length > 0
  && value.length <= 128 && /^[A-Za-z0-9._:-]+$/.test(value);
const positive = value => Number.isSafeInteger(value) && value > 0;
const fields = ["external_session_id", "generation", "observer_witness", "run_id", "observer_epoch", "agent_id"];

/** Projects only a matching bounded public receipt, never raw callback data. */
function project(session, value) {
  try {
    if (!value || typeof value !== "object" || Array.isArray(value)
        || Object.keys(value).length !== fields.length || Object.keys(value).some(key => !fields.includes(key))) return;
    const projected = { external_session_id: value.external_session_id, generation: value.generation,
      observer_witness: value.observer_witness, run_id: value.run_id,
      observer_epoch: value.observer_epoch, agent_id: value.agent_id };
    if (projected.external_session_id !== session || !identifier(projected.agent_id)
        || !positive(projected.generation) || !positive(projected.run_id) || !positive(projected.observer_epoch)
        || typeof projected.observer_witness !== "string" || projected.observer_witness.length !== 64
        || !/^[a-f0-9]{64}$/.test(projected.observer_witness)) return;
    return Object.freeze(projected);
  } catch { /* unavailable data never changes lifetime ownership */ }
}

/** Cancels only the exact attached SDK timer; throws/rejections are neutral. */
function cancel(timer) {
  try { timer?.cancel()?.catch?.(() => {}); } catch { /* telemetry loss */ }
}

/** Creates a caller-retained owner with one pending admission and one timer.
 * Begin/release do not retry work; an adapter must choose any later attempt.
 * A stopped owner cannot be rebound, and the facade exposes no mutable fields. */
export function createCuratedLifetime() {
  let generation = 0;
  let pending;
  let active;
  let receipt;
  let timer;
  let closedTicket;
  let closedTimer;
  let stopped = false;
  const current = ticket => !stopped && active === ticket && receipt !== undefined;
  return Object.freeze({
    begin(session) {
      if (stopped || pending || active || !identifier(session) || !Number.isSafeInteger(generation + 1)) return;
      pending = Object.freeze({ session, generation: ++generation });
      return pending;
    },
    publish(ticket, value) {
      if (stopped || pending !== ticket || active || !ticket) return false;
      const projected = project(ticket.session, value);
      if (!projected || stopped || pending !== ticket || active) return false;
      receipt = projected;
      active = ticket;
      pending = undefined;
      return true;
    },
    release(ticket) {
      if (!ticket || pending !== ticket) return false;
      pending = undefined;
      return true;
    },
    current,
    attachTimer(ticket, value) {
      // Unknown/copied/released tickets are inert, even their resource getters.
      // One exact stopped ticket may clean up its own late creation result.
      if (!ticket || (ticket !== active && ticket !== closedTicket)) return false;
      if (stopped) {
        if (value !== closedTimer) { closedTimer = value; cancel(value); }
        return false;
      }
      if (current(ticket) && timer === value && value) return true;
      let cancellable = false;
      try { cancellable = typeof value?.cancel === "function"; } catch { /* unavailable resource */ }
      if (!cancellable || !current(ticket) || timer) {
        if (stopped && value !== closedTimer) { closedTimer = value; cancel(value); }
        else if (!stopped && value !== timer) cancel(value);
        return false;
      }
      timer = value;
      return true;
    },
    stop() {
      if (stopped) return;
      stopped = true;
      closedTicket = active ?? pending;
      pending = active = receipt = undefined;
      const owned = timer;
      timer = undefined;
      closedTimer = owned;
      cancel(owned);
    },
    get receipt() { return receipt; },
  });
}
