//! Exact Pi14/OpenCode12 transport bytes, not an executable current client.
//!
//! The base and every delta below are independently frozen compiled data. No
//! active client, receipt, path, environment or runtime input defines this old
//! authority. The whole-result digest regression binds the deployed bytes.

/// Reconstructs only the one shipped one-shot end revision from immutable data.
pub(super) fn bytes() -> Vec<u8> {
    let mut source = include_str!("persistent_client_pre_end.mjs").to_owned();
    for (old, new) in [
        (
            r#" * restore only the same original marker/report, never vendor/provider work. */"#,
            r#" * restore only the same original marker/report, never vendor/provider work.
 * End uses one bounded explicit durable publication, never logical fallback or
 * re-enrollment. Lost/invalid acknowledgments remain unavailable, not proof of
 * failure to commit. This client does not provide a durable unsent-end inbox. */"#,
        ),
        (
            r#"/** Private handle identity includes the credential; numeric generation alone"#,
            r#"/** Exact immutable historical end proof, not logical retirement or helper exit. */
function sourceEndReceipt(input, generation) {
  return input && typeof input === "object" && !Array.isArray(input)
    && Object.keys(input).length === 4
    && Object.keys(input).every(key => ["protocol", "retired", "durable", "generation"].includes(key))
    && input.protocol === "external-source-end/1" && input.retired === true
    && input.durable === true && positive(input.generation) && input.generation === generation;
}

/** Private handle identity includes the credential; numeric generation alone"#,
        ),
        (
            "  let acceptedRecovery;",
            r#"  let acceptedRecovery;
  let closing = false;
  let ending;
  let endPublication;
  let endOutcome;
  let endState = "not-requested";"#,
        ),
        (
            "    if (pending >= 16 || disposed) return Promise.resolve(unavailable());",
            "    if (pending >= 16 || disposed || closing) return Promise.resolve(unavailable());",
        ),
        (
            "  const accounting = createRequestAccountingClient({ enqueue,",
            r#"  // Runs on the existing serial owner, including directly inside a terminal Pi
  // observation. Never await an end queued BEHIND that same observation.
  const publishEnd = () => endPublication ??= (async () => {
    if (endOutcome) return endOutcome;
    closing = true;
    endState = "pending";
    const captured = handle;
    const current = attempt;
    const result = !disposed && phase === "enrolled" && captured
      && await exchange("agent/external/source-end", params(), current);
    const durable = !disposed && captured === handle && current === attempt
      && sourceEndReceipt(result, captured?.generation);
    endOutcome = Object.freeze(durable ? { delivered: true, durable: true } : unavailable());
    endState = durable ? "durable" : "unavailable";
    disposed = true;
    lose(current);
    handle = undefined;
    pendingEnrollment = undefined;
    acceptedRecovery = undefined;
    return endOutcome;
  })();
  const accounting = createRequestAccountingClient({ enqueue,"#,
        ),
        (
            r#"  return Object.freeze({ start, ...accounting,
    status() { return Object.freeze({ phase, usage: accounting.accountingStatus(),
      run: handle?.run, epoch: handle?.epoch }); },"#,
            r#"  return Object.freeze({ start: () => closing ? Promise.resolve(false) : start(), ...accounting,
    status() { return Object.freeze({ phase, usage: accounting.accountingStatus(),
      run: handle?.run, epoch: handle?.epoch, sourceEnd: endState }); },"#,
        ),
        (
            "        if (accepted && result.retired) { disposed = true; lose(attempt); }",
            r#"        if (accepted && result.retired) {
          const ended = await publishEnd();
          return { delivered: true, retired: true, durable: ended.durable === true };
        }"#,
        ),
        (
            "      if (disposed || !handle || !text(instance, 128) || instance === metadata.observer_instance) return;",
            "      if (disposed || closing || !handle || !text(instance, 128) || instance === metadata.observer_instance) return;",
        ),
        (
            r#"    end() {
      if (pending >= 16 || disposed) { disposed = true; lose(attempt); return Promise.resolve(unavailable()); }
      return enqueue(async () => {
        const result = phase === "enrolled" && await exchange("agent/external/deregister", params(), attempt);
        disposed = true;
        lose(attempt);
        return result?.retired === true ? { delivered: true } : unavailable();
      });
    },"#,
            r#"    end() {
      if (endOutcome) return Promise.resolve(endOutcome);
      if (endPublication) return endPublication;
      if (ending) return ending;
      if (disposed) {
        endState = "unavailable";
        endOutcome = Object.freeze(unavailable());
        disposed = true; lose(attempt);
        return Promise.resolve(endOutcome);
      }
      // One terminal obligation, not another ordinary queued callback. Existing
      // accepted work drains first; closing rejects any new ordinary admission.
      // This cannot grow beyond one promise per source or replenish attempts.
      ending = serial.then(publishEnd).catch(unavailable);
      closing = true;
      endState = "pending";
      return ending;
    },"#,
        ),
    ] {
        source = source.replace(old, new);
    }
    source.into_bytes()
}

#[cfg(test)]
mod tests {
    /// Bind the entire immutable reconstruction to the deployed pre-edit client,
    /// not to whichever bytes a future active transport happens to compile.
    #[test]
    fn bootstrap_one_shot_end_history_matches_deployed_bytes() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            Sha256::digest(super::bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "946a1867d00077d6ae610ef6a9974d09f016d0487d5d2e05f4aca80c2e68c4f2"
        );
    }
}
