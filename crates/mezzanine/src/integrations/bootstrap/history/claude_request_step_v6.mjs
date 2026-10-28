/** Literal generator middleware: one delegate, unchanged chunks/final/error.
 * Active requests and unacknowledged normalized outcomes share sixteen slots.
 * A later genuine main step retries at most one pending original report before
 * its own request. Only the exact still-live observer may deliver; no timers,
 * persistence, source handoff, newest lookup or provider replay is introduced. */
const pendingRequestReports = new Set();
let retryingRequestReport = false;

/** Release only local telemetry state; a lost ACK never proves non-delivery. */
function releaseRequestReport(pending) {
  pendingRequestReports.delete(pending);
  try { pending.accounting.stop(); pending.accounting.abandonRequest(pending.ticket); } catch { /* neutral cleanup */ }
}

/** Retry one immutable original obligation, rotating failures without eviction.
 * Concurrent steps cannot double-issue pending work; stale epochs are fenced
 * before helpers and never transfer tickets or outcomes to a successor owner. */
async function retryRequestReport(owner, observer) {
  for (const pending of pendingRequestReports) {
    if (pending.owner !== owner || pending.owner.receipt !== pending.observer) releaseRequestReport(pending);
  }
  if (retryingRequestReport || !observer || owner.receipt !== observer) return;
  const pending = pendingRequestReports.values().next().value;
  if (!pending) return;
  retryingRequestReport = true;
  try {
    const delivery = await pending.accounting.endRequest(pending.ticket, pending.report);
    if (delivery?.durable === true || owner.receipt !== observer) releaseRequestReport(pending);
    else if (pendingRequestReports.delete(pending)) pendingRequestReports.add(pending);
  } catch { /* retain exact normalized obligation while its owner is live */ }
  finally { retryingRequestReport = false; }
}

on('turn.step', async function* ($, e, next) {
  const stepOwner = observerLifetime;
  const stepObserver = stepOwner.receipt;
  const session = boundSession;
  let reserved = false;
  let retained = false;
  let step;
  let accounting;
  let ticket;
  try {
    const captured = captureClaudeStep(session, e);
    if (captured) await retryRequestReport(stepOwner, stepObserver);
    if (captured && activeRequestProducers + pendingRequestReports.size < 16 && stepObserver !== undefined
        && observerLifetime === stepOwner && boundSession === session && stepOwner.receipt === stepObserver) {
      activeRequestProducers++;
      reserved = true;
      step = await identifyClaudeStep(captured);
      const alreadyPending = [...pendingRequestReports].some(pending => pending.owner === stepOwner
        && pending.observer === stepObserver && pending.ticket.request_id === step.request_id);
      if (!alreadyPending && observerLifetime === stepOwner && boundSession === session && stepOwner.receipt === stepObserver) {
        const observerLifetime = stepOwner;
        __MEZ_REQUEST_BODY__
        accounting = requestAccounting;
        ticket = await accounting.beginRequest(step.request_id);
      }
    }
  } catch { /* unavailable telemetry must still delegate the vendor request */ }
  if (!ticket && reserved) { activeRequestProducers--; reserved = false; }
  try {
    const result = yield* next(e);
    let report;
    let delivery;
    try {
      if (ticket) {
        report = projectClaudeStepOutcome(step, result, ticket.admitted_at);
        if (report) delivery = await accounting.endRequest(ticket, report);
      }
    } catch { /* counter/transport failure preserves the exact final result */ }
    if (report && delivery?.durable !== true && observerLifetime === stepOwner
        && boundSession === session && stepOwner.receipt === stepObserver
        && ![...pendingRequestReports].some(pending => pending.owner === stepOwner
          && pending.observer === stepObserver && pending.ticket.request_id === ticket.request_id)) {
      pendingRequestReports.add(Object.freeze({ owner: stepOwner, observer: stepObserver, accounting, ticket, report }));
      retained = true;
      // Move the reserved slot to pending ownership synchronously, no eviction.
      if (reserved) { activeRequestProducers--; reserved = false; }
    }
    return result;
  } finally {
    try { if (!retained) { if (ticket) accounting.abandonRequest(ticket); accounting?.stop(); } } catch { /* bounded local cleanup */ }
    if (reserved) activeRequestProducers--;
  }
});
