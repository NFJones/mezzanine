/** Content-free request accounting beside lifecycle transport.
 * Dependencies are injected; loading opens no sockets, timers or processes.
 * Adapters must begin before a genuine new request and retain its exact ticket.
 * Historical outcomes cannot obtain a ticket implicitly. At most 128 live
 * tickets and 16 queued transport operations are retained; pressure/loss is
 * explicit, never fabricated zero. Retry resends only one immutable report.
 * Tickets are client-instance-local; they never transfer successor credentials.
 */
const hex = value => typeof value === "string" && /^[a-f0-9]{64}$/.test(value);
const natural = value => Number.isSafeInteger(value) && value >= 0;
const positive = value => natural(value) && value > 0;
/** Bounded UTF-8 length without Node/DOM globals. Lone surrogates count as
 * three replacement bytes, matching the previous Buffer encoding convention. */
function boundedUtf8(value) {
  if (value.length > 128) return false;
  let bytes = 0;
  for (const character of value) {
    const point = character.codePointAt(0);
    bytes += point <= 0x7f ? 1 : point <= 0x7ff ? 2 : point <= 0xffff ? 3 : 4;
    if (bytes > 128) return false;
  }
  return true;
}
const inert = value => typeof value === "string" && value.trim().length > 0
  && boundedUtf8(value) && !/[\u0000-\u001f\u007f-\u009f\u202a-\u202e\u2066-\u2069]/u.test(value);
const unavailable = reason => Object.freeze({ delivered: false, durable: false, reason });

/** Normalize only explicit known fields before retaining any caller object. */
function observation(input) {
  try {
    if (!input || typeof input !== "object" || Array.isArray(input)
        || Object.keys(input).some(key => !["event_id", "observed_at", "provider", "model", "counters"].includes(key))) return;
    const own = (object, key) => Object.hasOwn(object, key) ? object[key] : undefined;
    const event_id = own(input, "event_id"), observed_at = own(input, "observed_at");
    const provider = own(input, "provider"), model = own(input, "model");
    if (!hex(event_id) || !positive(observed_at) || !inert(provider) || !inert(model)) return;
    const source = own(input, "counters");
    const fields = ["input_tokens", "output_tokens", "reasoning_tokens", "cached_input_tokens", "cache_write_input_tokens"];
    if (!source || typeof source !== "object" || Array.isArray(source)
        || Object.keys(source).some(key => !fields.includes(key))) return;
    const input_tokens = own(source, "input_tokens"), output_tokens = own(source, "output_tokens");
    if (!natural(input_tokens) || !natural(output_tokens)) return;
    const counters = { input_tokens, output_tokens };
    for (const key of fields.slice(2)) {
      const value = own(source, key);
      if (value === undefined || value === null) continue;
      if (!natural(value)) return;
      counters[key] = value;
    }
    if ((counters.reasoning_tokens ?? 0) > counters.output_tokens
        || !Number.isSafeInteger((counters.cached_input_tokens ?? 0) + (counters.cache_write_input_tokens ?? 0))
        || (counters.cached_input_tokens ?? 0) + (counters.cache_write_input_tokens ?? 0) > counters.input_tokens) return;
    return Object.freeze({ event_id, observed_at, provider, model, counters: Object.freeze(counters) });
  } catch { /* arbitrary getters are not telemetry authority */ }
}

/** Validate actual typed readiness, including non-authoritative source continuity. */
function readiness(result, id) {
  return result && result.protocol === "external-request/1" && result.accepted === true
    && result.request_id === id && positive(result.admitted_at) && hex(result.source_witness)
    && ["project", "unattributed"].includes(result.origin)
    && Array.isArray(result.controls) && result.controls.length === 0
    && Object.keys(result).every(key => ["protocol", "accepted", "request_id", "admitted_at", "source_witness", "origin", "controls"].includes(key));
}

/** Exposes only begin/end/abandon operations, never a generic RPC selector. */
export function createRequestAccountingClient({ enqueue, exchange }) {
  const live = new Map();
  const settled = new WeakMap();
  let reported = false;
  return Object.freeze({
    accountingStatus() { return reported ? "request-bound-external-reported" : "unavailable-source-continuity"; },
    beginRequest(id) {
      if (!hex(id) || live.size >= 128) return Promise.resolve(undefined);
      return enqueue(async () => {
        if (live.size >= 128) return;
        const result = await exchange("agent/external/usage-begin", { request_id: id });
        if (!readiness(result, id)) return;
        const ticket = Object.freeze({ request_id: id, admitted_at: result.admitted_at,
          source_witness: result.source_witness, origin: result.origin });
        live.set(ticket, { ticket, report: undefined, fingerprint: undefined,
          abandoned: false, sending: false, attempted: false });
        return ticket;
      }).then(ticket => live.has(ticket) ? ticket : undefined);
    },
    endRequest(ticket, input) {
      const state = live.get(ticket) ?? settled.get(ticket);
      const report = observation(input);
      if (!state || !report || report.observed_at < ticket.admitted_at) return Promise.resolve(unavailable("request-or-counters-unavailable"));
      const fingerprint = JSON.stringify(report);
      if (state.fingerprint && state.fingerprint !== fingerprint) return Promise.resolve(unavailable("immutable-report-conflict"));
      if (state.ack) return Promise.resolve(state.ack);
      state.report = report;
      state.fingerprint = fingerprint;
      return enqueue(async () => {
        if (state.ack) return state.ack;
        if (state.abandoned || live.get(ticket) !== state) {
          return unavailable(state.attempted ? "request-delivery-unknown-abandoned" : "unreported-request-abandoned");
        }
        for (let attempt = 0; attempt < 2; attempt++) {
          // Restore only this already-issued marker. Never mint a newest marker
          // or replace source/time after a daemon/database/source change.
          const ready = await exchange("agent/external/usage-begin", { request_id: ticket.request_id });
          if (state.abandoned || live.get(ticket) !== state) {
            return unavailable(state.attempted ? "request-delivery-unknown-abandoned" : "unreported-request-abandoned");
          }
          if (!readiness(ready, ticket.request_id)) continue;
          if (ready.source_witness !== ticket.source_witness || ready.admitted_at !== ticket.admitted_at
              || ready.origin !== ticket.origin) return unavailable("request-source-changed");
          let result;
          state.sending = true;
          state.attempted = true;
          try { result = await exchange("agent/external/usage-end", { request_id: ticket.request_id, ...state.report }); }
          finally { state.sending = false; }
          if (result?.accepted === true && result.durable === true && typeof result.applied === "boolean"
              && positive(result.revision) && result.coverage?.source === "external-reported"
              && typeof result.coverage.reasoning_known === "boolean") {
            state.ack = Object.freeze({ delivered: true, durable: true, applied: result.applied, revision: result.revision });
            reported = true;
            live.delete(ticket);
            settled.set(ticket, state);
            return state.ack;
          }
        }
        return unavailable("durable-acknowledgment-unavailable");
      }).then(result => result?.durable === true ? result : unavailable(result?.reason ?? "telemetry-unavailable"));
    },
    abandonRequest(ticket) {
      const state = live.get(ticket);
      if (!state) return unavailable("request-unavailable");
      // A sent/in-flight report may already be committed: never claim loss,
      // undo it, erase its acknowledgment, or resurrect a terminal unsent ticket.
      if (state.sending) return unavailable("request-report-in-flight");
      state.abandoned = true;
      live.delete(ticket);
      return unavailable(state.attempted ? "request-delivery-unknown-abandoned" : "unreported-request-abandoned");
    },
  });
}
