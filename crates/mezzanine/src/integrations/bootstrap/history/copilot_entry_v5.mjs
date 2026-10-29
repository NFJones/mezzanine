/** Owned Copilot user extension, using the CLI's bundled SDK only.
 * Join the existing session without custom tools, hooks, permission handlers, logging
 * or provider calls. The original classic startup hook remains the enrollment
 * owner. Extension loading/disabled/trust policy remains entirely vendor-owned.
 * A separate exact-writer pulse loop proves finite idle health, never usage.
 */
import { joinSession } from "@github/copilot-sdk/extension";
import { attachCopilotObserver, pulseCopilotObserver } from "./copilot_extension.mjs";
import { createCopilotPulseLifetime, createCopilotStartupAttachment } from "./copilot_lifetime.mjs";
import { randomBytes } from "node:crypto";
import { peerHelper } from "./peer_helper.mjs";

let observer;
let lifetime;
let startup;
let stopped = false;
try {
  const session = await joinSession({});
  const options = Object.freeze({ mez: process.env.MEZ, pane: process.env.MEZ_PANE,
    session: session.sessionId, peerHelper });
  // Register teardown before attachment: shutdown during the bounded startup
  // window must not leave a late successful receipt running a pulse lifetime.
  session.on("session.shutdown", () => {
    stopped = true;
    startup?.stop();
    lifetime?.stop();
  });
  if (!stopped) {
    startup = createCopilotStartupAttachment({
      attempt: () => attachCopilotObserver(options), now: () => performance.now(),
      setTimeout, clearTimeout,
    });
    observer = await startup.ready;
    if (observer && !stopped) {
      const original = observer, nonce = randomBytes(32).toString("hex");
      lifetime = createCopilotPulseLifetime({
        send: sequence => pulseCopilotObserver(options, original, nonce, sequence),
        setInterval, clearInterval,
      });
      await lifetime.ready;
    }
  }
} catch { startup?.stop(); lifetime?.stop(); /* unavailable observation remains neutral/private */ }

// Original public selectors only, retained for this extension instance. They
// confer no source, client or accounting authority outside native revalidation.
export { observer, lifetime };
