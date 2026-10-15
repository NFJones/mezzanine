# Agent actions

## Purpose

Understand the actions shown in an agent conversation, decide what to approve,
and recover safely when work fails or is interrupted. You do not need to write
action batches yourself; the agent submits them to Mezzanine.

## Prerequisites

Read [Agent overview](../agent/overview.md) and
[Approvals and review](../safety-and-trust/approvals-and-review.md).

## Read an action result

An agent response contains a short reason and one or more actions. Mezzanine
checks which actions are available, their arguments, and their permissions
before execution. The agent can also publish a short current-work objective
for other agents to discover; that is coordination information, not permission.

Use the recorded result, not the agent's proposed command or completion prose,
to determine what happened:

| Result | Meaning and next step |
| --- | --- |
| `running` | Work is still in progress; no final outcome is available yet. |
| `blocked` | The action is waiting at a resumable approval boundary. An attached primary client must decide it; observers cannot. |
| `rejected` or `denied` | The action was not accepted or authorized. Read the reason; do not assume a retry will change policy. |
| `succeeded` | The action completed successfully. Check the reported effects and validation for the larger task. |
| `failed` | The action failed. Earlier file changes or other effects may already have occurred. |
| `cancelled`, `timed_out`, or `interrupted` | Execution did not complete normally. Inspect reported effects and current state before retrying; these labels do not by themselves prove that nothing ran. |

An invalid response that cannot be separated into identifiable actions is
reported as a malformed response error instead.

The `say` action only displays text. A command or patch printed in such a
message does not execute. A displayed `blocked` message means the agent needs
input or an external condition before continuing; it is distinct from an
action waiting in the approval interface.

## Action names you may see

| Action | What it does | What to review |
| --- | --- | --- |
| `say` | Displays progress, completion, or a blocker. | Compare completion claims with execution and validation evidence. |
| `shell_command` | Inspects files or runs commands through the effective native or pane shell mode. | Command effects, working directory, scope, network access, and approval. |
| `apply_patch` | Adds, updates, moves, or deletes file content. | Paths and proposed changes. A multi-file patch is not an all-or-nothing transaction. |
| `web_search`, `fetch_url` | Searches the web or retrieves an HTTP(S) URL. | External destinations and potentially sensitive query data; these are not local-file readers. |
| `list_agents` | Discovers peers and their current-work objectives. | Discovery defaults to the trusted project; explicit session scope widens the audience. |
| `send_message` | Sends a message to a peer or group. | Recipient, audience, and payload. Accepted delivery does not establish recipient agreement or task completion. |
| `spawn_agent`, `close_agent` | Starts delegated work or retires a caller-owned persistent child. | Task, model, scope, and resource use; see [Subagents and messaging](../agent/subagents-and-messaging.md). |
| `wait` | Parks an agent turn until a peer replies. | It is for inter-agent coordination, not a general delay or approval wait. |
| `config_change` | Changes a supported live configuration setting. | The value, persistence, and effect on other work; execution-boundary settings require direct user control. |
| `mcp_server_search`, `mcp_server_get` | Finds configured integrations and reads their available tool contracts. | Metadata inspection does not invoke an external tool. |
| `mcp_call` | Invokes a configured MCP tool. | Tool arguments, external effects, credentials, and approval; see [MCP integration](../agent/mcp-integration.md). |
| `memory_search`, `memory_store` | Reads or retains durable memory when enabled. | Whether retained information is reusable and non-secret. |
| `issue_add`, `issue_update`, `issue_query`, `issue_delete` | Manages the active project's local issues. | Project, changed state, and any deletion. |

Availability depends on the configured action set and current request.
An action listed here is not necessarily enabled in your conversation, and
listing it does not grant approval. Mezzanine rechecks live integration state,
permissions, and arguments when the agent uses it.

## HTTP execution and authenticated-search foundation

Ordinary `fetch_url` and DuckDuckGo search share bounded pooled HTTP execution
with inference, but cannot be switched into inference SSE decoding by a remote
content type. Truncation is observed locally, not trusted from response headers.
At a text cap, an incomplete final UTF-8 character is omitted; invalid interior
bytes fail. Slow progress consumers may receive only a contiguous prefix; final
results remain authoritative. Cancellation retires the scoped forwarder/body.
Actor forwarding keeps one pending acknowledgment plus the latest cumulative
snapshot, so held ingress cannot suspend network polling or its deadlines.
Terminal output drops optional progress without a tail flush; queued stale
attempts remain subject to the runtime's exact identity/revision/state guards.

The authenticated-search transport foundation has fixed HTTPS destinations for
Brave, Tavily, Exa and SerpApi. It verifies certificates, refuses redirects and
does not discover ambient proxies. Its pooled clients cannot be confused with
legacy inference/fetch clients. Credentials are separate from approved request
descriptions and attached only at send time; SerpApi uses query authentication,
not an invented authentication header. Search JSON must be complete and pass a
known-credential echo check before publication. HTTP errors expose fixed
categories/status, never raw error bodies, headers or URL-bearing source chains.
The internal search contract distinguishes invalid/authentication/forbidden,
payment or quota (402), throttling (429), local phase timeout, transport,
provider status, malformed JSON/encoding, pending (202), oversize and known-key
suppression. A 429 is not proof of quota exhaustion, and none of these categories
authorizes retry. Cancellation returns no invented completion or no-effect
guarantee; the destination may already have received the request.

This foundation **does not enable paid search**, add profiles/credential commands,
or change the default search backend or legacy routing. Backend selection and
authorization are separate work. Query keys still reach the destination URL
and may be retained there. See [Audit and diagnostics](../safety-and-trust/audit-and-diagnostics.md)
for redaction limits; do not put credentials into prompts or `fetch_url` URLs.

## File changes and safe recovery

Patch paths are normally relative to the pane working directory; parent
traversal is rejected. Under active, non-bypassed Bubblewrap, absolute paths may
target effective write scopes. Other execution modes reject absolute patch
headers and targets outside the pane working directory. These patch checks are
not general confinement of an unsandboxed shell.

The shell patch resolver fails closed if its native component walker cannot read
a symbolic link or exceeds its link-expansion bound. It reports a fixed reason
without adding target path content to that diagnostic. These errors do not retry
the reader, authorize a weaker resolution fallback, or establish the cause of an
earlier intermittent failure; inspect the current evidence before repairing work.

Confirmed earlier file changes remain applied if a later operation fails.
Review the result and changed files, then ask the agent to repair only the
remaining work using fresh file context. Replaying the original batch can
repeat effects or fail against changed content. A truncated displayed diff is
not a complete file review.

Native shell mode currently runs both shell commands and patches through fresh
shell processes, reported as `spawned_shell`. Pane mode uses the pane shell,
reported as `pane_shell`. Neither selecting native mode nor seeing a configured
sandbox name proves process-free patch execution or effective OS confinement.
Use the actual action result and [sandbox status](../safety-and-trust/sandboxing.md).

Denied approvals and user cancellations are not automatically retried.
Mezzanine may let the agent correct invalid arguments or other recoverable
errors, but recovery does not authorize it to bypass your decision.

## Related pages

- [Commands, skills, and macros](../agent/commands-skills-and-macros.md)
- [Approvals and review](../safety-and-trust/approvals-and-review.md)
- [MCP integration](../agent/mcp-integration.md)
- [Sandboxing](../safety-and-trust/sandboxing.md)
- [MAAP protocol reference](protocols/maap.md) — for provider and harness implementers

## Next step

Use [Approvals and review](../safety-and-trust/approvals-and-review.md) when an
action needs a decision, or [Troubleshooting](../operations/troubleshooting.md)
when its outcome is uncertain.
