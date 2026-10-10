/** Literal SDK sender: fixed argv and normalized token-free stdin only.
 * Neutral output remains unverified; freshness and expense use other owners. */
if (!lifecycleSender && observerLifetime.receipt !== undefined) {
  const originalObserver = observerLifetime.receipt;
  const lifecycleOwner = observerLifetime;
  lifecycleSender = { owner: lifecycleOwner, sender: createClaudeLifecycleSender({
    observer: originalObserver,
    isCurrent: () => lifecycleOwner.receipt === originalObserver,
    async send(envelope) {
      return $.process.run([__MEZ_HELPER__, "harness-event"], { stdin: JSON.stringify(envelope), timeoutMs: 3000 });
    },
  }) };
  await lifecycleSender.sender.observe(fact);
}
