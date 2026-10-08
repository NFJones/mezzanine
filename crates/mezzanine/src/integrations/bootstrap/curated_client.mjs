/** Literal curated SDK client body, rendered inside classic.SessionStart.
 * Only original public selectors cross the fixed helper; no Node APIs, vendor
 * content, credentials or configurable RPC. next(e) has already completed and
 * result remains untouched. Timer loss is telemetry loss, not vendor failure.
 * Reload stops SDK timers; this source never claims a replacement namespace. */
try {
  const identifier = value => typeof value === "string" && value.length > 0
    && value.length <= 128 && /^[A-Za-z0-9._:-]+$/.test(value);
  if (identifier(e.session_id) && ["startup", "resume", "clear", "fork"].includes(e.source)) {
    const response = await $.process.run([__MEZ_HELPER__, "harness-source", JSON.stringify({
      external_session_id: e.session_id, observer_instance: "mez-curated-client-1",
      session_boundary: e.source,
    })], { timeoutMs: 3000 });
    if (response.exitCode === 0 && typeof response.stdout === "string" && response.stdout.length <= 4096) {
      const receipt = JSON.parse(response.stdout);
      const positive = value => Number.isSafeInteger(value) && value > 0;
      const fields = ["protocol", "registered", "controls", "agent_id", "generation",
        "observer_witness", "run_id", "observer_epoch", "observer_instance",
        "external_session_id", "usage", "observer_transport", "expires_at_unix_seconds", "lease_seconds"];
      if (receipt && typeof receipt === "object" && !Array.isArray(receipt)
          && Object.keys(receipt).length === fields.length
          && Object.keys(receipt).every(key => fields.includes(key))
          && receipt.protocol === "external-agent/1" && receipt.registered === true
          && Array.isArray(receipt.controls) && receipt.controls.length === 0
          && identifier(receipt.agent_id) && positive(receipt.generation)
          && positive(receipt.run_id) && positive(receipt.observer_epoch)
          && positive(receipt.expires_at_unix_seconds) && receipt.lease_seconds === 60
          && receipt.external_session_id === e.session_id
          && receipt.observer_instance === "mez-curated-client-1"
          && receipt.usage === "unavailable-source-continuity"
          && receipt.observer_transport === "unavailable-curated-freshness"
          && typeof receipt.observer_witness === "string"
          && /^[a-f0-9]{64}$/.test(receipt.observer_witness)) {
        const original = Object.freeze({ external_session_id: receipt.external_session_id,
          generation: receipt.generation, observer_witness: receipt.observer_witness });
        let sequence = 0;
        let busy = false;
        $.clock.every(__MEZ_INTERVAL__, async () => {
          if (busy || !Number.isSafeInteger(sequence + 1)) return;
          busy = true;
          try {
            const proof = await $.process.run([__MEZ_HELPER__, "harness-source", JSON.stringify({
              operation: "curated-heartbeat", external_session_id: original.external_session_id,
              generation: original.generation, observer_witness: original.observer_witness,
              sequence: ++sequence,
            })], { timeoutMs: 3000 });
            // A callback/exit is not acknowledgment or lease renewal. Do not
            // retry vendor work, re-enroll, or let helper output reach the user.
            if (proof.exitCode !== 0 || typeof proof.stdout !== "string" || proof.stdout.length > 128) return;
            const observed = JSON.parse(proof.stdout);
            if (!observed || typeof observed !== "object" || Array.isArray(observed)
                || Object.keys(observed).length !== 3 || observed.observed !== true
                || observed.sequence !== sequence || typeof observed.changed !== "boolean") return;
          } catch { /* unavailable telemetry cannot change vendor behavior */ }
          finally { busy = false; }
        });
      }
    }
  }
} catch { /* loading/policy/transport failures remain observationally neutral */ }
