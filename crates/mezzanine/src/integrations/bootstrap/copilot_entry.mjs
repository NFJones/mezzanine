/** Owned Copilot user extension, using the CLI's bundled SDK only.
 * Join the existing session without custom tools, hooks, permission handlers, logging
 * or provider calls. The original classic startup hook remains the enrollment
 * owner. Extension loading/disabled/trust policy remains entirely vendor-owned.
 * A separate exact-writer pulse loop proves finite idle health, never usage.
 */
import { joinSession } from "@github/copilot-sdk/extension";
import { attachCopilotObserver, pulseCopilotObserver } from "./copilot_extension.mjs";
import { createCopilotPulseLifetime } from "./copilot_lifetime.mjs";
import { randomBytes } from "node:crypto";
import { peerHelper } from "./peer_helper.mjs";

let observer;
let lifetime;
try {
  const session = await joinSession({});
  const options = Object.freeze({ mez: process.env.MEZ, pane: process.env.MEZ_PANE,
    session: session.sessionId, peerHelper });
  observer = await attachCopilotObserver(options);
  if (observer) {
    const original = observer, nonce = randomBytes(32).toString("hex");
    lifetime = createCopilotPulseLifetime({
      send: sequence => pulseCopilotObserver(options, original, nonce, sequence),
      setInterval, clearInterval,
    });
    session.on("session.shutdown", () => lifetime.stop());
    await lifetime.ready;
  }
} catch { lifetime?.stop(); /* unavailable observation remains neutral/private */ }

// Original public selectors only, retained for this extension instance. They
// confer no source, client or accounting authority outside native revalidation.
export { observer, lifetime };
