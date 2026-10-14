/** Content-free ordinary Pi request accounting, separate from lifecycle control.
 * Public Pi source 7bd01fb92485c06f92ecfff7ccf3197148bc16af emits turn_start
 * before request work, message_end before extension-modified persistence, and
 * turn_end with persisted messageEntryId. Assistant object identity binds those
 * callbacks without reading transcript/content or using turnIndex as an ID.
 * Native begin owns actual admission cwd/time; callbacks never block provider
 * work. Only finalized counters are copied. Reporting UTC is frozen once after
 * readiness (not AssistantMessage.timestamp, which is request start).
 * Pending/deferred and unattributable auxiliary counters are not charged. */
import { createHash, randomUUID } from "node:crypto";

const natural = value => Number.isSafeInteger(value) && value >= 0;
const inert = value => typeof value === "string" && value.trim().length > 0
  && Buffer.byteLength(value) <= 128 && !/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(value);
const digest = value => createHash("sha256").update(JSON.stringify(value)).digest("hex");
const finalized = new Set(["stop", "length", "toolUse", "error", "aborted"]);
/** Optional provider-native response identity is only a correlation witness,
 * never the expense ID. Hashing prevents raw upstream IDs entering metadata. */
const alias = message => inert(message?.provider) && inert(message?.responseId)
  ? digest(["mez-pi-response-correlation/1", message.provider, message.responseId]) : undefined;

/** Pi input excludes cache read/write partitions; inclusive native input adds
 * each exactly once. Required missing partitions cannot be guessed as zero.
 * Reasoning/output and cacheWrite1h/cacheWrite are subsets, never added again. */
export function normalizePiUsage(usage) {
  try {
    if (!usage || typeof usage !== "object" || Array.isArray(usage)) return;
    const counts = ["input", "output", "cacheRead", "cacheWrite"].map(key => Object.hasOwn(usage, key) ? usage[key] : undefined);
    if (!counts.every(natural)) return;
    const [input, output, cacheRead, cacheWrite] = counts;
    const inclusive = input + cacheRead + cacheWrite;
    if (!natural(inclusive) || !natural(inclusive + output)) return;
    const counters = { input_tokens: inclusive, output_tokens: output,
      cached_input_tokens: cacheRead, cache_write_input_tokens: cacheWrite };
    const reasoning = Object.hasOwn(usage, "reasoning") ? usage.reasoning : undefined;
    const cacheWrite1h = Object.hasOwn(usage, "cacheWrite1h") ? usage.cacheWrite1h : undefined;
    if (reasoning !== undefined) {
      if (!natural(reasoning) || reasoning > output) return;
      counters.reasoning_tokens = reasoning;
    }
    if (cacheWrite1h !== undefined && (!natural(cacheWrite1h) || cacheWrite1h > cacheWrite)) return;
    return Object.freeze(counters);
  } catch { /* unavailable counters never change vendor behavior */ }
}

/** Projects only current callback metadata, never message/tool/error content.
 * Stable persisted session/entry identity excludes timestamps, costs and model
 * choices; concrete responseModel takes precedence over the requested model. */
export function projectPiUsage(session, event) {
  try {
    const message = event?.message;
    const entry = event?.messageEntryId;
    const provider = message?.provider;
    const responseModel = message?.responseModel;
    if (!inert(session) || event?.type !== "turn_end" || !natural(event.turnIndex)
        || !inert(entry) || message?.role !== "assistant"
        || !finalized.has(message.stopReason) || !inert(provider)) return;
    const model = responseModel === undefined ? message.model : responseModel;
    const counters = normalizePiUsage(message.usage);
    if (!inert(model) || !counters) return;
    return Object.freeze({ event_id: digest(["mez-pi-finalized-usage/1", session, entry]),
      provider, model, counters });
  } catch { /* arbitrary callback objects are not reporting authority */ }
}

/** One process-owned map reserves active plus finalized slots before admission.
 * Only durable historical dedup evidence may be pruned. Unknown-delivery slots
 * survive exhaustion/teardown/reload without transferring original tickets;
 * 128 such obligations may refuse telemetry for the rest of the process life.
 * Recovery is telemetry-only: one timer and one queued/running retry across the
 * observer, with initial plus three calls shared by callbacks and timer. The
 * 5s monotonic horizon bounds dispatch, not transport completion. No durable
 * outbox, vendor request replay or successor authority is provided. Scheduler
 * and current-owner injection are private adapter/offline-test dependencies. */
export function createPiUsageObserver(owned, enqueue, recent, now = () => Math.floor(Date.now() / 1000), onGap = () => {}, options = {}) {
  let active;
  let bindings = new WeakMap();
  let aliases = new Set();
  let accepting = true;
  let timer;
  let timerGeneration = 0;
  let recoveryBusy = false;
  const offsets = [250, 1000, 4000];
  const scheduler = options.retryScheduler ?? { now: () => performance.now(), setTimeout, clearTimeout };
  const healthy = () => !owned.failed && !owned.cancelled && (options.isCurrent?.() ?? true);
  const clearTimer = () => {
    timerGeneration++;
    if (timer !== undefined) {
      try { scheduler.clearTimeout(timer); } catch { /* cancellation remains fenced */ }
      timer = undefined;
    }
  };
  const cancelRetries = () => {
    accepting = false;
    clearTimer();
    for (const { slot } of recent.values()) if (slot.owner === owned) slot.retryClosed = true;
  };
  const clock = () => {
    try {
      const value = scheduler.now();
      if (Number.isFinite(value) && value >= 0) return value;
    } catch { /* unavailable scheduling cannot affect vendor flow */ }
    cancelRetries();
  };
  const remove = slot => {
    if (recent.get(slot.key)?.slot === slot) recent.delete(slot.key);
  };
  const eligible = (slot, at) => healthy() && slot.owner === owned && !slot.retryClosed
    && !slot.durable && slot.initialDone && slot.ticket && slot.report && !slot.sending
    && slot.retries < offsets.length && at <= slot.retryStarted + 5000;
  const due = slot => Math.max(slot.retryStarted + offsets[slot.retries],
    slot.lastRetryCompleted === undefined ? 0 : slot.lastRetryCompleted + offsets[slot.retries] - offsets[slot.retries - 1]);
  const lose = reason => {
    owned.usageCoverage = reason;
    const fact = reason.slice("unavailable-".length);
    onGap(fact);
    if (owned.lastUsageGap !== fact) {
      owned.lastUsageGap = fact;
      enqueue(async () => {
        if (owned.client && !owned.cancelled && !owned.failed) {
          const receipt = await owned.client.piObservation({ type: "usage_gap", reason: fact });
          if (!receipt?.delivered) { owned.failed = true; cancelRetries(); }
        }
      });
    }
  };
  /** Async results can outlive synchronous EOS/lease replacement. They may
   * settle only the original slot, never create a NEW diagnostic under an old
   * or successor owner. In contrast, lose() called synchronously at an actual
   * unfinished close captures its existing disclosure BEFORE retirement; that
   * already-queued fact must retain original loss/abandon/retire ordering. */
  const diagnose = (slot, reason) => {
    if (healthy() && !slot.retryClosed) lose(reason);
  };
  const abandon = slot => {
    if (slot && !slot.projection) {
      slot.retryClosed = true;
      enqueue(async () => {
        if (slot.ticket) slot.client?.abandonRequest(slot.ticket);
        remove(slot);
      });
    }
  };
  /** A genuine run/settlement/teardown boundary ends an unfinished request.
   * Disclose missing finalized counters first; abandonment never proves that
   * a provider incurred no expense or that an unacknowledged report did not commit. */
  const finishUnreported = () => {
    if (active && !active.finalized) {
      lose("unavailable-finalized-counters");
      abandon(active);
    }
    active = undefined; bindings = new WeakMap(); aliases = new Set();
  };
  const reserve = slot => {
    if (recent.size >= 128) {
      for (const [key, record] of recent) {
        if (record.slot.durable) { recent.delete(key); break; }
      }
    }
    if (recent.size >= 128) { lose("unavailable-request-readiness"); return false; }
    slot.key = slot; // Object-key reservations cannot alias finalized digest keys.
    recent.set(slot.key, { slot });
    return true;
  };
  /** Frozen incurred observation delivery is shared by initial/recovery work.
   * An in-flight real ACK may settle its slot after close, but cannot revive
   * owner status. Gap diagnostics remain sticky even after healthy recovery. */
  const deliver = async slot => {
    if (!slot.ticket || !slot.client || owned.cancelled || owned.failed) { diagnose(slot, "unavailable-request-readiness"); return; }
    if (!slot.report) {
      const observed_at = now();
      if (!natural(observed_at) || observed_at === 0 || observed_at < slot.ticket.admitted_at) {
        diagnose(slot, "unavailable-report-clock"); slot.client.abandonRequest(slot.ticket); return;
      }
      slot.report = Object.freeze({ ...slot.projection, observed_at });
    }
    slot.sending = true;
    let receipt;
    try { receipt = await slot.client.endRequest(slot.ticket, slot.report); }
    finally { slot.sending = false; }
    slot.durable = receipt?.delivered === true && receipt?.durable === true
      && typeof receipt.applied === "boolean" && natural(receipt.revision) && receipt.revision > 0;
    if (slot.durable) {
      if (healthy() && !slot.retryClosed) owned.usageCoverage = "finalized-assistant-external-reported";
    } else {
      diagnose(slot, "unavailable-durable-acknowledgment");
      if (healthy() && !slot.retryClosed) slot.retryStarted ??= clock();
      if (receipt?.reason === "request-source-changed") slot.retryClosed = true;
    }
  };
  /** Arm only the earliest eligible original slot; never enqueue 128 retries
   * into the callback queue of 32. Generation fences captured stale callbacks.
   * Delayed dispatch uses remaining backoff spacing rather than catch-up bursts. */
  const arm = () => {
    clearTimer();
    if (!healthy()) { cancelRetries(); return; }
    if (recoveryBusy) return;
    const at = clock();
    if (at === undefined) return;
    let next;
    for (const { slot } of recent.values()) {
      if (!eligible(slot, at)) continue;
      const when = slot.callbackPending ? at : due(slot);
      if (when <= slot.retryStarted + 5000 && (!next || when < next.when)) next = { slot, when };
    }
    if (!next) return;
    const generation = timerGeneration;
    try {
      timer = scheduler.setTimeout(() => {
        if (generation !== timerGeneration) return;
        timer = undefined;
        dispatch(next.slot);
      }, Math.max(0, next.when - at));
      timer?.unref?.();
    } catch { cancelRetries(); }
  };
  /** Every trigger consumes the same finite budget before queueing. Recheck
   * original owner/slot/horizon inside the queued closure, not just at timer
   * firing. Moving the record to the tail makes equal-deadline selection fair. */
  const dispatch = slot => {
    const at = clock();
    if (at === undefined || !eligible(slot, at)) { arm(); return; }
    if (recoveryBusy) { slot.callbackPending = true; return; }
    clearTimer();
    recoveryBusy = true;
    slot.callbackPending = false;
    slot.retries++;
    const record = recent.get(slot.key);
    if (record?.slot === slot) { recent.delete(slot.key); recent.set(slot.key, record); }
    const queued = enqueue(async () => {
      try {
        const current = clock();
        if (current !== undefined && healthy() && !slot.retryClosed && !slot.durable
            && slot.owner === owned && recent.get(slot.key)?.slot === slot
            && current <= slot.retryStarted + 5000) {
          await deliver(slot);
          slot.lastRetryCompleted = clock();
        }
      } finally { recoveryBusy = false; arm(); }
    });
    if (queued === false) { recoveryBusy = false; cancelRetries(); }
  };
  const send = slot => {
    if (slot.initialQueued) { if (slot.initialDone && !slot.durable) dispatch(slot); return; }
    slot.initialQueued = true;
    enqueue(async () => {
      try { await deliver(slot); }
      finally { slot.initialDone = true; arm(); }
    });
  };
  return Object.freeze({
    /** Pi resets turnIndex at each genuine agent_start. Release only an
     * unfinished original request here; an unmatched late turn_end cannot
     * reset a currently active request or forge a new reporting boundary. */
    runStart() {
      finishUnreported();
      if (healthy()) accepting = true;
    },
    turnStart(event) {
      if (!accepting || !healthy() || event?.type !== "turn_start" || !natural(event.turnIndex)) return;
      if (active && !active.finalized) {
        // A duplicate start cannot mint a fresh marker. Overlapping genuine
        // turns lose coverage rather than pair an old outcome with a new ticket.
        if (active.turnIndex === event.turnIndex) return;
        lose("unavailable-overlapping-turn"); abandon(active);
      }
      bindings = new WeakMap();
      aliases = new Set();
      const slot = { owner: owned, turnIndex: event.turnIndex, marker: digest(["mez-pi-current-request/1", randomUUID()]), finalized: false, retries: 0 };
      if (!reserve(slot)) { active = undefined; return; }
      active = slot;
      enqueue(async () => {
        slot.client = owned.client;
        if (!slot.client || owned.cancelled || owned.failed) { diagnose(slot, "unavailable-request-readiness"); return; }
        // Reservation is already process-visible before this async admission.
        slot.ticket = await slot.client.beginRequest(slot.marker);
        if (!slot.ticket) diagnose(slot, "unavailable-request-readiness");
      });
    },
    messageIdentity(event) {
      if (!active || active.finalized || !["message_start", "message_update"].includes(event?.type)
          || event.message?.role !== "assistant") return;
      const value = alias(event.message);
      if (value && aliases.size < 8) aliases.add(value);
      else if (value && !aliases.has(value)) lose("unavailable-response-correlation");
    },
    messageEnd(event) {
      const message = event?.message;
      if (event?.type === "message_end" && active && !active.finalized && !active.messageBound
          && message?.role === "assistant" && typeof message === "object") {
        bindings.set(message, active);
        active.messageBound = true;
        const value = alias(message);
        if (value && aliases.size < 8) aliases.add(value);
      }
    },
    turnEnd(event) {
      const projection = projectPiUsage(owned.session, event);
      const previous = projection && recent.get(projection.event_id);
      const slot = (event?.message && bindings.get(event.message))
        ?? (previous?.slot.owner === owned ? previous.slot : undefined)
        ?? (active && !active.finalized && aliases.size === 1 && aliases.has(alias(event?.message)) ? active : undefined);
      if (!slot || slot.turnIndex !== event.turnIndex) { lose("unavailable-unmatched-finalized-event"); return; }
      if (slot === active) { bindings = new WeakMap(); aliases = new Set(); }
      if (!projection) { slot.finalized = true; lose("unavailable-finalized-counters"); abandon(slot); return; }
      const fingerprint = JSON.stringify(projection);
      if (previous && (previous.slot !== slot || previous.fingerprint !== fingerprint)) {
        slot.finalized = true; lose("unavailable-immutable-observation-conflict"); abandon(slot); return;
      }
      if (slot.projection && JSON.stringify(slot.projection) !== fingerprint) { lose("unavailable-immutable-observation-conflict"); return; }
      slot.finalized = true;
      slot.projection ??= projection;
      if (slot.key !== projection.event_id) {
        if (recent.get(slot.key)?.slot !== slot) return;
        recent.delete(slot.key);
        slot.key = projection.event_id;
        recent.set(slot.key, { slot, fingerprint });
      }
      if (!slot.durable) send(slot);
    },
    cancelRetries,
    close() { finishUnreported(); cancelRetries(); },
  });
}
