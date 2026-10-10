/** Content-free Claude turn.step projection from public generated mod types.
 * The pinned session/turn/index tuple names one genuine main request; opaque
 * hashes exclude counters/model/time so corrected outcomes conflict. Only final
 * SDK usage is read, never stream chunks, answers, tools or transcripts. SDK
 * ModelUsage reports uncached input plus separate cache read/create counts;
 * its omitted upstream counts are SDK zeros. Provider/reasoning are not exposed.
 * No transport, installation, vendor behavior or native authority lives here. */
const identifier = value => typeof value === 'string' && /^[A-Za-z0-9._:-]{1,128}$/.test(value);
const natural = value => Number.isSafeInteger(value) && value >= 0;

/** Capture pinned main-loop identity once before hashing or downstream work. */
export function captureClaudeStep(session, event) {
  try {
    if (!identifier(session) || !event || typeof event !== 'object' || Array.isArray(event)) return;
    const turnId = event.turnId, index = event.index, agentId = event.agentId;
    if (!identifier(turnId) || !natural(index) || agentId !== undefined) return;
    return Object.freeze({ session, turnId, index });
  } catch { /* unknown/malformed identity remains unavailable */ }
}

/** Web APIs are part of the documented no-Node SDK environment. Separate hash
 * domains preserve the original request and immutable final observation IDs. */
export async function identifyClaudeStep(captured) {
  if (!captured) return;
  const hash = async domain => {
    const bytes = new TextEncoder().encode(JSON.stringify([domain, captured.session, captured.turnId, captured.index]));
    const digest = await crypto.subtle.digest('SHA-256', bytes);
    return Array.from(new Uint8Array(digest), byte => byte.toString(16).padStart(2, '0')).join('');
  };
  const request_id = await hash('claude-turn-step/1');
  const event_id = await hash('claude-turn-step-outcome/1');
  return Object.freeze({ ...captured, request_id, event_id });
}

/** Project one matching finalized result using its actual response model.
 * Missing/null/nonfinite/unsafe counts never become invented zero. The explicit
 * unavailable provider label is not an inference from the selected model. */
export function projectClaudeStepOutcome(step, result, observed_at) {
  try {
    if (!step || !result || typeof result !== 'object' || Array.isArray(result)
        || !natural(observed_at) || observed_at === 0) return;
    const turnId = result.turnId, index = result.index, usage = result.usage;
    if (turnId !== step.turnId || index !== step.index || !usage || typeof usage !== 'object' || Array.isArray(usage)) return;
    const model = usage.model, input = usage.input_tokens, output = usage.output_tokens,
      read = usage.cache_read_input_tokens, write = usage.cache_creation_input_tokens;
    if (!identifier(model) || ![input, output, read, write].every(natural)) return;
    const inclusive = input + read + write;
    if (!natural(inclusive)) return;
    return Object.freeze({ event_id: step.event_id, observed_at, provider: 'unavailable', model,
      counters: Object.freeze({ input_tokens: inclusive, output_tokens: output,
        cached_input_tokens: read, cache_write_input_tokens: write }) });
  } catch { /* final counter evidence is optional and never affects the stream */ }
}
