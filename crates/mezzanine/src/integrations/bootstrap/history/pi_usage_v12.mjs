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

/** Bounded process-owned recent metadata survives extension reload only to
 * reject replay. Original client-local tickets never acquire successor rights.
 * Eviction drops local retry evidence, not durable daemon deduplication. */
export function createPiUsageObserver(owned, enqueue, recent, now = () => Math.floor(Date.now() / 1000), onGap = () => {}) {
  let active;
  let bindings = new WeakMap();
  let aliases = new Set();
  const lose = reason => {
    owned.usageCoverage = reason;
    const fact = reason.slice("unavailable-".length);
    onGap(fact);
    if (owned.lastUsageGap !== fact) {
      owned.lastUsageGap = fact;
      enqueue(async () => {
        if (owned.client && !owned.cancelled && !owned.failed) {
          const receipt = await owned.client.piObservation({ type: "usage_gap", reason: fact });
          if (!receipt?.delivered) owned.failed = true;
        }
      });
    }
  };
  const abandon = slot => {
    if (slot) enqueue(async () => { if (slot.ticket) slot.client?.abandonRequest(slot.ticket); });
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
  const retain = (id, record) => {
    if (!recent.has(id) && recent.size >= 128) {
      const first = recent.keys().next().value;
      const old = recent.get(first);
      if (!old.slot.durable && old.slot.ticket) old.slot.client?.abandonRequest(old.slot.ticket);
      recent.delete(first);
    }
    recent.set(id, record);
  };
  const send = slot => enqueue(async () => {
    if (!slot.ticket || !slot.client || owned.cancelled || owned.failed) { lose("unavailable-request-readiness"); return; }
    if (!slot.report) {
      const observed_at = now();
      if (!natural(observed_at) || observed_at === 0 || observed_at < slot.ticket.admitted_at) {
        lose("unavailable-report-clock"); slot.client.abandonRequest(slot.ticket); return;
      }
      slot.report = Object.freeze({ ...slot.projection, observed_at });
    }
    const receipt = await slot.client.endRequest(slot.ticket, slot.report);
    slot.durable = receipt?.delivered === true && receipt?.durable === true
      && typeof receipt.applied === "boolean" && natural(receipt.revision) && receipt.revision > 0;
    if (slot.durable) owned.usageCoverage = "finalized-assistant-external-reported";
    else lose("unavailable-durable-acknowledgment");
  });
  return Object.freeze({
    /** Pi resets turnIndex at each genuine agent_start. Release only an
     * unfinished original request here; an unmatched late turn_end cannot
     * reset a currently active request or forge a new reporting boundary. */
    runStart() {
      finishUnreported();
    },
    turnStart(event) {
      if (event?.type !== "turn_start" || !natural(event.turnIndex)) return;
      if (active && !active.finalized) {
        // A duplicate start cannot mint a fresh marker. Overlapping genuine
        // turns lose coverage rather than pair an old outcome with a new ticket.
        if (active.turnIndex === event.turnIndex) return;
        lose("unavailable-overlapping-turn"); abandon(active);
      }
      bindings = new WeakMap();
      aliases = new Set();
      const slot = { owner: owned, turnIndex: event.turnIndex, marker: digest(["mez-pi-current-request/1", randomUUID()]), finalized: false };
      active = slot;
      enqueue(async () => {
        slot.client = owned.client;
        if (!slot.client || owned.cancelled || owned.failed) { lose("unavailable-request-readiness"); return; }
        // Reserve one slot before begin, not after finalization. Forgotten
        // undurable reports remain unknown-delivery, never proven non-commit.
        if (recent.size >= 128) {
          const first = recent.keys().next().value;
          const old = recent.get(first);
          if (!old.slot.durable && old.slot.ticket) old.slot.client?.abandonRequest(old.slot.ticket);
          recent.delete(first);
        }
        slot.ticket = await slot.client.beginRequest(slot.marker);
        if (!slot.ticket) lose("unavailable-request-readiness");
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
      slot.finalized = true;
      slot.projection ??= projection;
      retain(projection.event_id, { slot, fingerprint });
      if (!slot.durable) send(slot);
    },
    close() { finishUnreported(); },
  });
}
