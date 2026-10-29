/** Owned Copilot user extension, using the CLI's bundled SDK only.
 * Join the existing session without custom tools, hooks, permission handlers, logging
 * or provider calls. The original classic startup hook remains the enrollment
 * owner. Extension loading/disabled/trust policy remains entirely vendor-owned.
 * Attachment alone supplies neither continuous health nor accounted usage.
 */
import { joinSession } from "@github/copilot-sdk/extension";
import { attachCopilotObserver } from "./copilot_extension.mjs";
import { peerHelper } from "./peer_helper.mjs";

let observer;
try {
  const session = await joinSession({});
  observer = await attachCopilotObserver({ mez: process.env.MEZ, pane: process.env.MEZ_PANE,
    session: session.sessionId, peerHelper });
} catch { /* unavailable observation must remain neutral and private */ }

// Original public selectors only, retained for this extension instance. They
// confer no source, client or accounting authority outside native revalidation.
export { observer };
