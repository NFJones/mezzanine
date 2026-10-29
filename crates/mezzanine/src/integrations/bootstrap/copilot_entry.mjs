/** Owned Copilot user extension, using the CLI's bundled SDK only.
 * Join the existing session without custom tools, hooks, permission handlers, logging
 * or provider calls. The original classic startup hook remains the enrollment
 * owner. Extension loading/disabled/trust policy remains entirely vendor-owned.
 * A separate exact-writer pulse loop proves finite idle health. Live terminal
 * metadata is queued independently and remains explicitly unattributed.
 */
import { attachCopilotObserver, pulseCopilotObserver, reportCopilotTerminal, reportCopilotUsageGap, copilotRoutingConfigured } from "./copilot_extension.mjs";
import { createCopilotPulseLifetime, createCopilotStartupAttachment } from "./copilot_lifetime.mjs";
import { createCopilotUsageReporter } from "./copilot_usage.mjs";
import { randomBytes } from "node:crypto";
import { peerHelper } from "./peer_helper.mjs";

let observer;
let lifetime;
let startup;
let usage;
let stopped = false;
// Missing/invalid ordinary routing stays neutral even when globally installed.
if (copilotRoutingConfigured(process.env.MEZ, process.env.MEZ_PANE)) {
  try {
    const { joinSession } = await import("@github/copilot-sdk/extension");
    const session = await joinSession({});
    const options = Object.freeze({ mez: process.env.MEZ, pane: process.env.MEZ_PANE,
      session: session.sessionId, peerHelper });
    const nonce = randomBytes(32).toString("hex");
    usage = createCopilotUsageReporter({
      send: report => reportCopilotTerminal(options, observer, nonce, report),
      gap: reason => reportCopilotUsageGap(options, observer, nonce, reason),
      setTimeout, clearTimeout,
    });
    // Observe live per-call events (including subagents) before any startup await.
    // The SDK callback is not a dispatch barrier and no history is imported.
    session.on("assistant.usage", event => usage.observe(event));
    // Register teardown before attachment: shutdown during the bounded startup
    // window must not leave a late successful receipt running a pulse lifetime.
    session.on("session.shutdown", () => {
      stopped = true;
      startup?.stop();
      lifetime?.stop();
      usage.stop();
    });
    if (!stopped) {
      startup = createCopilotStartupAttachment({
        attempt: () => attachCopilotObserver(options), now: () => performance.now(),
        setTimeout, clearTimeout,
      });
      observer = await startup.ready;
      if (observer && !stopped) {
        const original = observer;
        lifetime = createCopilotPulseLifetime({
          send: async sequence => {
            const reply = await pulseCopilotObserver(options, original, nonce, sequence);
            if (reply?.observed === false && reply.terminal === true) usage.stop();
            else if (!stopped) usage.acknowledgePulse(reply, sequence);
            return reply;
          },
          setInterval, clearInterval,
        });
        await lifetime.ready;
      }
    }
  } catch { startup?.stop(); lifetime?.stop(); usage?.stop(); /* unavailable observation remains neutral/private */ }
}

// Original public selectors only, retained for this extension instance. They
// confer no source, client or accounting authority outside native revalidation.
export { observer, lifetime };
