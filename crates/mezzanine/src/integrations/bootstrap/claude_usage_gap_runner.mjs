/** Literal SDK scope for one current original observer's fixed diagnostics.
 * Only the installing helper path and closed metadata capsules are executable;
 * no callback content or newest/source authority is lent to the pure sender. */
if (stepObserver !== undefined && (!usageGapSender || usageGapSender.owner !== stepOwner
    || usageGapSender.observer !== stepObserver)) {
  usageGapSender = { owner: stepOwner, observer: stepObserver, sender: createClaudeUsageGapSender({
    observer: stepObserver,
    isCurrent: () => stepOwner.receipt === stepObserver,
    async send(capsule) {
      const response = await $.process.run([__MEZ_HELPER__, "harness-source", JSON.stringify(capsule)], { timeoutMs: 3000 });
      if (response.exitCode === 0 && typeof response.stdout === "string") return response.stdout;
    },
  }) };
}
gap = usageGapSender?.owner === stepOwner ? usageGapSender.sender : undefined;
