/** Literal SDK scope: caller imports the pure client and owns observerLifetime.
 * Capture one actual public receipt, never a newest lookup after owner loss.
 * The trusted helper path is rendered as data; no event object is forwarded. */
const originalObserver = observerLifetime.receipt;
const requestAccounting = createCuratedRequestAccountingClient({
  observer: originalObserver,
  isCurrent: () => originalObserver !== undefined && observerLifetime.receipt === originalObserver,
  async send(capsule) {
    const response = await $.process.run([__MEZ_HELPER__, "harness-source", JSON.stringify(capsule)], { timeoutMs: 3000 });
    if (response.exitCode === 0 && typeof response.stdout === "string" && response.stdout.length <= 4096) return response.stdout;
  },
});
