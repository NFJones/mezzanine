# MAAP Action Reference

Mezzanine Agent Action Protocol (MAAP) actions are the structured operations an
agent can ask Mezzanine to perform during one turn. The provider-facing surface
is generated from `src/agent/provider/schema.rs`; runtime validation then limits
which actions are actually active for the current turn, pane, configuration, and
approval policy.

This reference describes the currently defined MAAP action objects and their
intended functionality. Some actions may be absent from a specific turn's
allowed-action surface even though they are defined here.

## Action batch envelope

MAAP actions are submitted as one action batch with these top-level fields:

| Field | Type | Functionality |
| --- | --- | --- |
| `rationale` | string | Short reason the selected action or actions directly advance the user's task. |
| `thought` | string or `null` | Optional durable work note for substantive learnings, decisions, invariants, or recovery details. It is not user-facing narration. |
| `actions` | array | One or more visible or executable action objects from the current allowed-action surface. |

The batch is the transport envelope for real work. It is not a separate setup
step, and a useful executable action should be included directly when available.

## Capability families

`request_capability` uses coarse capability names to ask the controller for a
different action family when the current surface lacks the needed action. These
families map to defined actions as follows:

| Capability | Exposed actions |
| --- | --- |
| `respond_only` | `say` |
| `shell` | `shell_command`, `apply_patch` |
| `network_search` | `web_search` |
| `network_fetch` | `fetch_url` |
| `mcp` | `mcp_call` |
| `subagent` | `send_message`, `spawn_agent` |
| `config_change` | `config_change` |
| `memory` | `memory_search`, `memory_store` |
| `issues` | `issue_add`, `issue_update`, `issue_query`, `issue_delete` |

## Action summary

| Action | Functionality | Primary capability |
| --- | --- | --- |
| `say` | Show user-visible progress, final, or blocked text. | `respond_only` |
| `request_capability` | Ask the controller to expose another coarse action family. | Always available on capability-decision surfaces |
| `request_skills` | Discover reusable skill/workflow context. | Skill surfaces only; model-selected skill lookup is currently disabled in normal provider guidance |
| `call_skill` | Load a named skill returned by skill discovery. | Skill surfaces only; model-selected skill loading is currently disabled in normal provider guidance |
| `shell_command` | Run one bounded, noninteractive command in the pane shell. | `shell` |
| `apply_patch` | Mutate repository text through Mezzanine's semantic patch format. | `shell` |
| `web_search` | Search external web/current information. | `network_search` |
| `fetch_url` | Fetch one explicit HTTP(S) URL. | `network_fetch` |
| `send_message` | Send a coordination payload to another local agent. | `subagent` |
| `spawn_agent` | Start a local subagent with a bounded task prompt. | `subagent` |
| `config_change` | Persist and apply supported live Mezzanine configuration changes. | `config_change` |
| `memory_search` | Search durable prior context when direct current artifacts cannot answer a specific prior-context gap. | `memory` |
| `memory_store` | Store stable reusable durable memory. | `memory` |
| `issue_add` | Create a local project issue record. | `issues` |
| `issue_update` | Mutate fields, state, notes, or dependencies on a local project issue. | `issues` |
| `issue_query` | Query local project issue records. | `issues` |
| `issue_delete` | Delete a local project issue record. | `issues` |
| `mcp_call` | Call one currently injected MCP tool with tool-specific arguments. | `mcp` |

## Visible response actions

### `say`

Displays text to the user. Text in `say` is display-only: commands, diffs, or
patches shown here do not execute.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `say`. |
| `status` | `progress`, `final`, or `blocked` | yes | `progress` reports a sequence-point update, `final` completes the user goal, and `blocked` reports an external blocker. `final` and `blocked` must not be paired with executable actions. |
| `content_type` | text media type enum | yes | One of `text/plain; charset=utf-8`, `text/markdown; charset=utf-8`, or `text/x-diff; charset=utf-8`. |
| `text` | string | yes | User-visible body. |

Use `progress` sparingly for new learning, phase changes, validation outcomes,
or blocker changes. Use `final` only when the user goal is complete.

### `request_capability`

Requests a coarse action family when the current allowed-action surface does not
include the action needed to continue. This is controller routing, not a request
for the user to grant permission.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `request_capability`. |
| `capability` | capability enum | yes | One of `respond_only`, `shell`, `network_search`, `network_fetch`, `mcp`, `subagent`, `config_change`, `memory`, or `issues`. |
| `reason` | string | yes | Brief task-specific reason naming the concrete action or evidence needed. |

## Skill actions

The schema still defines skill discovery and loading actions, but normal
provider guidance currently disables model-selected skill lookup and loading.
Users may still explicitly invoke skills through supported command surfaces.

### `request_skills`

Defined as an exceptional workflow discovery action. It has no fields beyond
`type` and is intended only for surfaces where skill discovery is explicitly
enabled.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `request_skills`. |

### `call_skill`

Loads a named skill returned by skill discovery. Skills add context only; they
do not grant execution permissions or action capabilities.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `call_skill`. |
| `name` | string | yes | Skill name returned by `request_skills`. |
| `additional_context` | string or `null` | yes | Optional task-specific context appended to the loaded skill context. |

## Shell and file actions

### `shell_command`

Runs one bounded, noninteractive pane-shell command for local inspection,
builds, tests, formatting, validation, filesystem work, process inspection, or
git operations.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `shell_command`. |
| `summary` | string | yes | Concise user-facing description of what will run or what output will be used. The raw command should not be repeated here. |
| `command` | string | yes | Exact shell input. It should be focused, bounded, and noninteractive. |

`shell_command` is for command execution and filesystem operations that are not
ordinary text edits. `apply_patch` must not be invoked as a shell program.
Agent-authored heredocs and here-strings are disabled.

### `apply_patch`

Mutates file content using Mezzanine's semantic patch format. It is the ordinary
action for repository text edits.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `apply_patch`. |
| `patch` | string | yes | Direct patch text starting with `*** Begin Patch` and ending with `*** End Patch`. |

Patch constraints:

- Accepted file directives are `*** Add File`, `*** Update File`, and
  `*** Delete File`. `*** Update File` may be followed by `*** Move to`.
- There is no `*** Replace File` directive.
- Paths must be safe relative paths; absolute paths and `..` traversal are not
  valid.
- Update hunks should use distinctive anchors and exact old/context lines from
  current file evidence.
- Hunk lines use one leading prefix: space for context, `-` for removals, and
  `+` for additions.
- `*** End of File` marks a file with no final newline.

## Network actions

### `web_search`

Searches external HTTP(S) web/current information. It is for tasks that ask for
web search or genuinely depend on current external facts, not for local files or
generated local content.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `web_search`. |
| `query` | string | yes | Search query. |

### `fetch_url`

Fetches one explicit external HTTP(S) URL when that URL's content is needed.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `fetch_url`. |
| `url` | string | yes | Explicit `http://` or `https://` URL. |

`fetch_url` is not used for `file://` URLs, local paths, generated local data,
or as a replacement for shell or patch actions.

## Subagent actions

### `send_message`

Sends a model-readable coordination payload to another local agent.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `send_message`. |
| `recipient` | string | yes | Target agent identifier. |
| `content_type` | `text/plain; charset=utf-8` or `application/json` | yes | Payload media type. JSON payloads are encoded as compact JSON strings. |
| `payload` | string | yes | Message body. |

### `spawn_agent`

Starts a subagent for bounded local delegation.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `spawn_agent`. |
| `role` | string | yes | Subagent role/profile, such as `explorer` for read-only inspection or `worker` for implementation. |
| `task_prompt` | string | yes | Concrete delegated task prompt. |

## Configuration action

### `config_change`

Requests a supported live Mezzanine configuration mutation. This action should
be used for explicit requests to change supported settings such as theme,
approval mode, provider/model profile, reasoning, hooks, or MCP server metadata.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `config_change`. |
| `setting_path` | string | yes | Supported dotted config path. |
| `operation` | `set`, `unset`, or `reset` | yes | `set` assigns a value, `unset` removes a scalar override, and `reset` returns the field to lower-precedence/default behavior. |
| `value` | string or `null` | yes | For `set`, a JSON scalar or string array encoded as a string; for `unset` or `reset`, `null`. |

Config changes follow the active approval policy. When approved or policy
allowed, they persist to the user config target and apply immediately. Setting
`theme.active` uses set-theme behavior, including materialized theme aliases and
colors.

## Memory actions

### `memory_search`

Searches durable prior context. It is only for a specific missing prior-context
question that cannot be answered from the current prompt, action results, MCP,
shell, web, or another direct artifact.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `memory_search`. |
| `query` | string | yes | Focused durable-context query. |
| `limit` | integer or `null` | yes | Optional maximum records to return, from 1 through 20. Use small limits; `null` uses the runtime default. |

`memory_search` is not a default preflight, a setup action before direct work,
or a way to retrieve facts already present in current action results.

### `memory_store`

Stores stable reusable information in durable memory.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `memory_store`. |
| `kind` | memory kind enum | yes | One of `preference`, `fact`, `procedure`, `documentation`, `research`, or `warning`. |
| `priority` | integer or `null` | yes | Optional retrieval priority from 0 through 100. |
| `scope` | `global`, `project`, or `null` | yes | Optional durable scope hint. |
| `keywords` | string array | yes | Search anchors or aliases for retrieval. |
| `content` | string | yes | Durable memory body. |
| `expires_in_days` | integer or `null` | yes | Optional retention period in days. `null` uses `memory.default_ttl_days`. |

Memory storage is for stable information likely to help future sessions. It
should not store secrets, credentials, prompt-specific notes, current-turn
summaries, transient action results, repository state, CI state, or other cheap
to rediscover data.

## Issue actions

Local issue actions operate on Mezzanine's project issue store. They are enabled
through the `issues` capability and use local issue identifiers returned by the
store.

### `issue_add`

Creates a local project issue.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `issue_add`. |
| `kind` | `defect` or `task` | yes | Issue category. |
| `title` | string | yes | Single-line issue title. |
| `body` | string or `null` | yes | Optional issue details. |
| `notes` | string or `null` | yes | Optional mutable progress or handoff notes. |
| `depends_on` | string array | yes | Issue ids this issue depends on, or an empty array. |

### `issue_update`

Updates a local project issue.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `issue_update`. |
| `id` | string | yes | Issue id to update. |
| `kind` | `defect`, `task`, or `null` | yes | Replacement kind, or `null` to leave unchanged. |
| `state` | `open`, `resolved`, or `null` | yes | Replacement state, or `null` to leave unchanged. |
| `title` | string or `null` | yes | Replacement title, or `null` to leave unchanged. |
| `body` | string or `null` | yes | Replacement body, or `null` to leave unchanged. |
| `clear_body` | boolean | yes | Whether to clear the existing body. Cannot be true when `body` is set. |
| `notes` | string or `null` | yes | Replacement notes, or `null` to leave unchanged. |
| `clear_notes` | boolean | yes | Whether to clear existing notes. Cannot be true when `notes` is set. |
| `depends_on` | string array or `null` | yes | Replacement dependency ids, or `null` to leave unchanged. |
| `clear_depends_on` | boolean | yes | Whether to clear dependencies. Cannot be true when `depends_on` is set. |

### `issue_query`

Queries local project issues.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `issue_query`. |
| `kind` | `defect`, `task`, or `null` | yes | Optional kind filter. `null` includes both kinds. |
| `state` | `open`, `resolved`, or `null` | yes | Optional state filter. `null` defaults to open issues. |
| `text` | string or `null` | yes | Optional title/body substring filter. |
| `limit` | integer or `null` | yes | Optional maximum result count, from 1 through 200. |

### `issue_delete`

Deletes a local project issue.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `issue_delete`. |
| `id` | string | yes | Issue id to delete. |

## MCP action

### `mcp_call`

Calls one MCP tool that is currently injected into the turn-local provider
schema. Unlike most actions, each `mcp_call` schema is generated from a concrete
server/tool pair and that tool's input schema.

Fields:

| Field | Type | Required | Functionality |
| --- | --- | --- | --- |
| `type` | string enum | yes | Must be `mcp_call`. |
| `server` | string enum | yes | The exposed MCP server id for this tool. |
| `tool` | string enum | yes | The exposed MCP tool name. |
| `arguments` | object | yes | Tool-specific arguments normalized from the MCP input schema. |

MCP calls should be used directly when the user names the MCP server/tool or the
task matches the exposed tool description. Required task-local arguments should
come from the prompt, current action results, or safely gatherable context when
available.

## Runtime boundaries and validation

- The provider-facing schema may be a stable superset, but the late
  allowed-action surface is authoritative for a turn.
- Runtime policy can require approval, delay an action, or reject it even when
  the action is defined.
- `final` and `blocked` `say` actions are terminal for a batch and should not be
  mixed with executable actions.
- Local filesystem, build, test, and git work belongs in `shell_command` or
  `apply_patch`; web and MCP actions are separate integration paths.
- Persistent memory and local issue actions are gated by their corresponding
  configuration and capability surfaces.
