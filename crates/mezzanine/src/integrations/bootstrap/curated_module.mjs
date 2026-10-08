/** Literal main-session module entry; no store, transcript or control access.
 * Genuine classic SessionStart alone can admit a producer. Module load is not
 * a conversation boundary. One owner is retained across callbacks; changed
 * vendor session IDs stop old local work and obtain a fresh local owner.
 * Same-ID callbacks coalesce; same-ID restart/reload/persistence is not inferred.
 * End captures its exact owner before downstream work, so delayed old events
 * cannot stop a newer binding. Native daemon provenance/retirement stays separate. */
import { createCuratedLifetime } from './curated_lifetime.mjs';
import { projectClaudeEvent } from './claude_observer.mjs';

export function register(on) {
  let observerLifetime = createCuratedLifetime();
  let boundSession;
  let startGeneration = 0;
  let pendingStart;
  // Runtime-only identity fence; exhaustion rejects new admission, no eviction.
  const endedSessions = new Set();
  let identitiesExhausted = false;

  on('classic.SessionStart', async ($, e, next) => {
    let fact;
    try { fact = projectClaudeEvent(e?.session_id, 'SessionStart', e); } catch { /* unavailable metadata */ }
    const accepted = fact && !identitiesExhausted && !endedSessions.has(fact.session)
      && ['startup', 'resume', 'clear', 'fork'].includes(fact.event.reason)
      && Number.isSafeInteger(startGeneration + 1) ? ++startGeneration : undefined;
    const startOwner = accepted === undefined ? undefined
      : boundSession === fact.session ? observerLifetime
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
          || identitiesExhausted || endedSessions.has(fact.session)) return result;
      if (boundSession !== fact.session) {
        observerLifetime.stop();
        observerLifetime = startOwner;
        boundSession = fact.session;
      }
      // Transport sees only the pre-downstream captured main-session metadata.
      {
        const observerLifetime = startOwner;
        const e = Object.freeze({ session_id: fact.session, source: fact.event.reason });
        __MEZ_SOURCE_BODY__
      }
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
    const endingOwner = fact && (fact.session === boundSession ? observerLifetime
      : fact.session === pendingStart?.session ? pendingStart.owner : undefined);
    const result = await next(e);
    if (endingOwner) {
      if (endedSessions.has(fact.session) || endedSessions.size < 128) endedSessions.add(fact.session);
      else identitiesExhausted = true;
      try { endingOwner.stop(); } catch { /* local cancellation is neutral */ }
    }
    return result;
  });
}
