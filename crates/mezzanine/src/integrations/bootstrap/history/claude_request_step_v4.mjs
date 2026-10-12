/** Literal generator middleware: one delegate, unchanged chunks/final/error.
 * Reserve before any asynchronous projection. Helpers cannot change a request
 * or infer a newest session; only the captured original owner can report it. */
on('turn.step', async function* ($, e, next) {
  const stepOwner = observerLifetime;
  const session = boundSession;
  let reserved = false;
  let step;
  let accounting;
  let ticket;
  try {
    const captured = captureClaudeStep(session, e);
    if (captured && activeRequestProducers < 16 && stepOwner.receipt !== undefined) {
      activeRequestProducers++;
      reserved = true;
      step = await identifyClaudeStep(captured);
      if (observerLifetime === stepOwner && boundSession === session && stepOwner.receipt !== undefined) {
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
    try {
      if (ticket) {
        const report = projectClaudeStepOutcome(step, result, ticket.admitted_at);
        if (report) await accounting.endRequest(ticket, report);
      }
    } catch { /* counter/transport failure preserves the exact final result */ }
    return result;
  } finally {
    try { if (ticket) accounting.abandonRequest(ticket); accounting?.stop(); } catch { /* bounded local cleanup */ }
    if (reserved) activeRequestProducers--;
  }
});
