/** Literal main-session module entry; no store, transcript or control access.
 * Genuine classic SessionStart alone can admit a producer. Module load is not
 * a conversation boundary. One owner is retained across callbacks; changed
 * vendor session IDs stop old local work and obtain a fresh local owner.
 * Same-ID callbacks coalesce. Only a sequential completed clear end/start pair
 * may use this module's captured predecessor; restart/reload is not inferred.
 * End captures its exact owner before downstream work, so delayed old events
 * cannot stop a newer binding. Native daemon provenance/retirement stays separate. */
import { createCuratedLifetime } from './curated_lifetime.mjs';
import { projectClaudeEvent, createClaudeLifecycleSender } from './claude_observer.mjs';
import { createCuratedRequestAccountingClient } from './curated_request_accounting.mjs';
import { captureClaudeStep, identifyClaudeStep, projectClaudeStepOutcome } from './claude_request.mjs';

export function register(on) {
  let observerLifetime = createCuratedLifetime();
  let boundSession;
  let startGeneration = 0;
  let pendingStart;
  // One locally captured public predecessor, consumed at start acceptance.
  // It is not persisted, discovered by newest lookup or used for initial fallback.
  let clearHandoff;
  // Runtime-only identity fence; exhaustion rejects new admission, no eviction.
  const endedSessions = new Set();
  let identitiesExhausted = false;
  let activeRequestProducers = 0;
  let lifecycleSender;

  on('classic.SessionStart', async ($, e, next) => {
    let fact;
    try { fact = projectClaudeEvent(e?.session_id, 'SessionStart', e); } catch { /* unavailable metadata */ }
    const handoff = fact?.event.reason === 'clear' && clearHandoff?.session === fact.session
      ? clearHandoff : undefined;
    // Even an unqualified main start fences delayed earlier end completion.
    const observed = fact && Number.isSafeInteger(startGeneration + 1) ? ++startGeneration : undefined;
    if (fact && observed === undefined) identitiesExhausted = true;
    if (fact) clearHandoff = undefined;
    const accepted = observed !== undefined && !identitiesExhausted && (!endedSessions.has(fact.session) || handoff)
      && ['startup', 'resume', 'clear', 'fork'].includes(fact.event.reason)
      ? observed : undefined;
    const startOwner = accepted === undefined ? undefined
      : handoff ? createCuratedLifetime() : boundSession === fact.session ? observerLifetime
        : pendingStart?.session === fact.session ? pendingStart.owner : createCuratedLifetime();
    const pending = startOwner ? { session: fact.session, owner: startOwner } : undefined;
    if (pending) pendingStart = pending;
    let result;
    try { result = await next(e); }
    catch (error) {
      if (pendingStart === pending) pendingStart = undefined;
      if (startOwner && startOwner !== observerLifetime && startOwner !== pendingStart?.owner) startOwner.stop();
      throw error;
    }
    try {
      if (accepted === undefined || accepted !== startGeneration
          || identitiesExhausted || (endedSessions.has(fact.session) && !handoff)) return result;
      if (boundSession !== fact.session || handoff) {
        lifecycleSender?.sender.stop();
        lifecycleSender = undefined;
        observerLifetime.stop();
        observerLifetime = startOwner;
        boundSession = fact.session;
      }
      // Transport sees only the pre-downstream captured main-session metadata.
      {
        const observerLifetime = startOwner;
        const previousObserver = handoff?.observer;
        const startInstance = handoff ? `mez-curated-clear-${accepted}` : 'mez-curated-client-1';
        const e = Object.freeze({ session_id: fact.session, source: fact.event.reason });
        __MEZ_SOURCE_BODY__
      }
      if (handoff && startOwner.receipt !== undefined) endedSessions.delete(fact.session);
      __MEZ_LIFECYCLE_BODY__
    } catch { /* telemetry cannot change middleware results */ }
    finally {
      if (pendingStart === pending) pendingStart = undefined;
      if (startOwner && startOwner !== observerLifetime && startOwner !== pendingStart?.owner) startOwner.stop();
    }
    return result;
  });

  on('classic.SessionEnd', async ($, e, next) => {
    let fact;
    try { fact = projectClaudeEvent(e?.session_id, 'SessionEnd', e); } catch { /* unavailable metadata */ }
    const endingOwner = fact && (fact.session === pendingStart?.session ? pendingStart.owner
      : fact.session === boundSession ? observerLifetime : undefined);
    const endingObserver = endingOwner?.receipt;
    const endingGeneration = startGeneration;
    const endingSender = endingOwner && lifecycleSender?.owner === endingOwner ? lifecycleSender.sender : undefined;
    const endingTicket = endingSender?.capture(fact);
    let result;
    try { result = await next(e); } catch (error) { endingSender?.abandon(endingTicket); throw error; }
    if (endingOwner) {
      // A delayed old same-ID end cannot fence a newer instance's lifetime.
      const ownsIdentity = boundSession !== fact.session || endingOwner === observerLifetime
        || endingOwner === pendingStart?.owner;
      if (ownsIdentity) {
        if (endedSessions.has(fact.session) || endedSessions.size < 128) endedSessions.add(fact.session);
        else identitiesExhausted = true;
      }
      try { await endingSender?.publish(endingTicket); endingSender?.stop(); } catch { /* telemetry loss */ }
      if (endingOwner === observerLifetime && fact.session === boundSession) {
        if (!identitiesExhausted && endingGeneration === startGeneration
            && fact.event.reason === 'clear' && endingObserver) {
          clearHandoff = Object.freeze({ session: fact.session, observer: endingObserver });
        } else if (fact.event.reason !== 'clear') clearHandoff = undefined;
      }
      try { endingOwner.stop(); } catch { /* local cancellation is neutral */ }
    }
    return result;
  });
  on('classic.UserPromptSubmit', async ($, e, next) => {
    const sender = lifecycleSender?.sender;
    const fact = projectClaudeEvent(boundSession, 'UserPromptSubmit', e);
    const ticket = sender?.capture(fact);
    let result;
    try { result = await next(e); } catch (error) { sender?.abandon(ticket); throw error; }
    try { await sender?.publish(ticket); } catch { /* neutral telemetry */ }
    return result;
  });
  on('classic.Notification', async ($, e, next) => {
    const sender = lifecycleSender?.sender;
    const fact = projectClaudeEvent(boundSession, 'Notification', e);
    const ticket = sender?.capture(fact);
    let result;
    try { result = await next(e); } catch (error) { sender?.abandon(ticket); throw error; }
    try { await sender?.publish(ticket); } catch { /* neutral telemetry */ }
    return result;
  });
  on('classic.Stop', async ($, e, next) => {
    const sender = lifecycleSender?.sender;
    const fact = projectClaudeEvent(boundSession, 'Stop', e);
    const ticket = sender?.capture(fact);
    let result;
    try { result = await next(e); } catch (error) { sender?.abandon(ticket); throw error; }
    try { await sender?.publish(ticket); } catch { /* neutral telemetry */ }
    return result;
  });
  on('classic.StopFailure', async ($, e, next) => {
    const sender = lifecycleSender?.sender;
    const fact = projectClaudeEvent(boundSession, 'StopFailure', e);
    const ticket = sender?.capture(fact);
    let result;
    try { result = await next(e); } catch (error) { sender?.abandon(ticket); throw error; }
    try { await sender?.publish(ticket); } catch { /* neutral telemetry */ }
    return result;
  });
  __MEZ_STEP_BODY__
}
