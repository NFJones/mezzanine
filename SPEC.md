# Mezzanine Specification

- [Mezzanine Specification](#mezzanine-specification)
  - [1. Status and Scope](#1-status-and-scope)
  - [2. Normative Language](#2-normative-language)
  - [3. Terminology](#3-terminology)
  - [4. System Model](#4-system-model)
  - [5. Session, Client, and Process Lifecycle](#5-session-client-and-process-lifecycle)
  - [6. Terminal Multiplexing](#6-terminal-multiplexing)
    - [6.1 Windows](#61-windows)
    - [6.2 Panes](#62-panes)
    - [6.3 Layout](#63-layout)
    - [6.4 Frames](#64-frames)
    - [6.5 History Buffering](#65-history-buffering)
    - [6.6 Mouse Support](#66-mouse-support)
    - [6.7 Terminal Compatibility](#67-terminal-compatibility)
  - [7. Input, Commands, Copy Mode, and Notifications](#7-input-commands-copy-mode-and-notifications)
    - [7.1 Escape Sequence](#71-escape-sequence)
    - [7.2 Default Prefix Bindings](#72-default-prefix-bindings)
    - [7.3 Command Language](#73-command-language)
    - [7.4 Agent Shell Toggle](#74-agent-shell-toggle)
    - [7.5 Readline Semantics](#75-readline-semantics)
    - [7.6 Copy Mode and Paste Buffers](#76-copy-mode-and-paste-buffers)
    - [7.7 Messages, Activity, and Bell Notifications](#77-messages-activity-and-bell-notifications)
  - [8. Configuration](#8-configuration)
    - [8.1 Configuration Files](#81-configuration-files)
    - [8.2 Baseline Configuration Schema](#82-baseline-configuration-schema)
    - [8.3 Configuration Shell Capabilities](#83-configuration-shell-capabilities)
  - [9. Agent Harness](#9-agent-harness)
    - [9.1 Shell Discovery and Classification](#91-shell-discovery-and-classification)
    - [9.2 Tool Discovery and Environment Signatures](#92-tool-discovery-and-environment-signatures)
    - [9.3 Shell Integration and Command Boundaries](#93-shell-integration-and-command-boundaries)
    - [9.4 Agent Bootstrap](#94-agent-bootstrap)
    - [9.5 Agent Turn Lifecycle](#95-agent-turn-lifecycle)
    - [9.6 Context Assembly](#96-context-assembly)
    - [9.7 Model Request and Response](#97-model-request-and-response)
    - [9.8 Mezzanine Agent Action Protocol](#98-mezzanine-agent-action-protocol)
    - [9.9 Action Gating and Execution](#99-action-gating-and-execution)
    - [9.10 Observation and Iteration](#910-observation-and-iteration)
    - [9.11 Errors, Retries, and Persistence](#911-errors-retries-and-persistence)
  - [10. Agent Capabilities](#10-agent-capabilities)
    - [10.1 Baseline Capabilities](#101-baseline-capabilities)
    - [10.2 Shell-Only Local Interaction](#102-shell-only-local-interaction)
    - [10.3 Subagents](#103-subagents)
    - [10.4 Skills](#104-skills)
  - [11. Agent Shell Commands](#11-agent-shell-commands)
  - [12. Local Message Passing Protocol](#12-local-message-passing-protocol)
    - [12.1 Protocol Name and Version](#121-protocol-name-and-version)
    - [12.2 Transport](#122-transport)
    - [12.3 Framing](#123-framing)
    - [12.4 Envelope](#124-envelope)
    - [12.5 Message Types](#125-message-types)
    - [12.6 Delivery Semantics](#126-delivery-semantics)
    - [12.7 Errors](#127-errors)
    - [12.8 Payloads](#128-payloads)
  - [13. Control Endpoint](#13-control-endpoint)
  - [14. Model Context Protocol Integration](#14-model-context-protocol-integration)
  - [15. Authentication and Provider Accounts](#15-authentication-and-provider-accounts)
  - [16. Agent System Prompt Profile](#16-agent-system-prompt-profile)
    - [16.1 Prompt Construction](#161-prompt-construction)
    - [16.2 Required Prompt Content](#162-required-prompt-content)
    - [16.3 Required Prompt Prohibitions](#163-required-prompt-prohibitions)
    - [16.4 Prompt Profile Updates](#164-prompt-profile-updates)
  - [17. Permissions, Shell Sandboxing, and Change Review](#17-permissions-shell-sandboxing-and-change-review)
    - [17.1 Command Prefix Rules](#171-command-prefix-rules)
    - [17.2 Pane Protection View](#172-pane-protection-view)
    - [17.3 Blocked Approval Routing](#173-blocked-approval-routing)
    - [17.4 Approval Bypass](#174-approval-bypass)
  - [18. Security and Safety](#18-security-and-safety)
  - [19. Detach, Reattach, Snapshots, and Persistence](#19-detach-reattach-snapshots-and-persistence)
  - [20. Hooks](#20-hooks)
  - [21. Agent Memory](#21-agent-memory)
  - [22. Scheduling and Concurrency](#22-scheduling-and-concurrency)
  - [23. Provider Model Selection](#23-provider-model-selection)
  - [24. Project Instruction Discovery](#24-project-instruction-discovery)
  - [25. Terminal Compatibility Test Suite](#25-terminal-compatibility-test-suite)
  - [26. Security Audit Log](#26-security-audit-log)
  - [27. Extension and Versioning](#27-extension-and-versioning)
  - [28. References](#28-references)


## 1. Status and Scope

This document specifies Mezzanine, a terminal multiplexer designed for agentic AI
development. It describes observable system behavior, data concepts, user
interactions, configuration, agent orchestration, and interoperability
requirements.

This specification intentionally does not prescribe any implementation language,
runtime, storage engine, terminal library, or model provider SDK. A conforming
implementation MAY use any implementation strategy that satisfies the
requirements in this document.

This version assumes a Unix-like operating system with pseudoterminals,
processes, process groups, signals, environment variables, filesystem
permissions, and shell conventions compatible with POSIX practice. Mezzanine
MUST treat the user's `SHELL` environment variable as the definitive shell
discovery input when it is set, non-empty, absolute, and executable. If
`SHELL` is unset, empty, relative, non-executable, or otherwise unusable,
Mezzanine MUST fall through to `/bin/sh` when `/bin/sh` is executable. This
precedence rule defines the resolved shell path and MUST be used consistently
for pane creation, explicit pane commands, shell hooks, bootstrap, and agent
command wrappers.
Mezzanine MUST assume a Unix-like shell, GNU/BSD coreutils-style toolbox,
`sed`, `grep`, and `python3` or `python` unless runtime discovery proves
otherwise.

This document defines the baseline behavior for a conforming Mezzanine system.
Future revisions MAY add optional behavior, but such additions MUST NOT weaken
the requirements in this version.

## 2. Normative Language

The key words "MUST", "MUST NOT", "REQUIRED", "SHALL", "SHALL NOT",
"SHOULD", "SHOULD NOT", "RECOMMENDED", "MAY", and "OPTIONAL" in this
document are to be interpreted as described in RFC 2119.

## 3. Terminology

Mezzanine:
: The complete terminal multiplexer and agentic AI development system specified by
  this document.

Muxxer:
: The terminal multiplexing subsystem that presents multiple windows and panes
  within a single terminal instance.

Session:
: A persistent Mezzanine runtime state containing windows, panes, terminal
  buffers, agent sessions, configuration state, and other state needed for
  detach and reattach behavior.

Window:
: A top-level terminal workspace within a session. A window contains one or
  more panes.

Window Group:
: A user-facing collection of windows within a session. A session has one
  active window group, and the visible window bar and window-navigation
  commands operate within that active group.

Pane:
: A rectangular terminal region within a window. Each pane hosts a primary
  process and is wrapped by an agent harness.

Primary PID:
: The Unix process identifier for the process initially created from the
  resolved shell path for a pane. If that shell later executes another command
  with `exec`, the primary PID remains the same and refers to the replacement
  process.

Resolved Shell Path:
: The shell executable selected by reading `SHELL` when it is set, non-empty,
  absolute, and executable by the current user, otherwise `/bin/sh` when
  `/bin/sh` is executable. If neither candidate is usable, Mezzanine MUST fail
  the operation requiring a shell with an actionable diagnostic.

Client:
: A terminal attachment to a Mezzanine session.

Primary Client:
: The client currently allowed to send user input to panes and interactive
  Mezzanine control surfaces.

Read-only Observer:
: A client attached to a session that may observe terminal state but MUST NOT
  send pane input, approve agent actions, mutate configuration, or control the
  session.

Pending Observer:
: A client that has requested read-only observer access but has not yet been
  approved by the primary client. A pending observer MUST NOT receive session
  view data.

Control Endpoint:
: A structured local endpoint used by Mezzanine clients and agent harnesses to
  inspect and mutate Mezzanine state.

Agent Harness:
: The control layer attached to a pane that mediates agent interaction with the
  pane, manages model round trips, tracks harness-initiated work, and provides
  Mezzanine-specific agent context.

Agent:
: A model-backed actor operating through an agent harness. An agent may request
  pane-local actions, receive explicit action results, communicate with other
  local agents, and spawn additional agents subject to policy.

Agent Shell:
: A pane-associated interactive command surface for controlling an agent
  session.

Configuration Shell:
: An interactive command surface, entered through the Mezzanine escape
  sequence, for live inspection and mutation of Mezzanine configuration.

Frame:
: A visible boundary or status region associated with a window or pane that
  displays contextual information.

Local Message Passing Protocol:
: The local-only protocol used by agents to discover each other and exchange
  messages within a Mezzanine session.

## 4. System Model

Mezzanine MUST present multiple windows containing multiple resizable panes
within a single terminal instance.

Mezzanine MUST preserve session state across detachment and reattachment. While
a session is detached, Mezzanine MUST continue running active panes, windows,
agents, and background work unless explicitly configured or commanded otherwise.

Mezzanine MUST associate every pane with an agent harness. The agent harness
MUST be able to send commands to that pane through the pane's shell and observe
the terminal output for harness-initiated transactions. The default model
context MUST NOT include passive snapshots of the pane's visible terminal
buffer or bounded history.

The agent harness MUST always be available for every pane. Mezzanine MUST NOT
enter the agent shell by default when creating or attaching to a pane; the user
MUST explicitly enter it with the configured agent shell key binding or command.

Each pane MUST launch or attach to a Unix-like shell from the resolved shell
path as the initial primary process unless it is attaching to an already-live
pane process. If Mezzanine falls back to `/bin/sh`, it MUST record the
fallback in diagnostics.

The primary PID of a newly created pane MUST initially be the shell process.
Pane creation commands MAY specify an explicit command to run in the new pane.
When a pane creation command specifies an explicit command, Mezzanine MUST start
the resolved shell path and run the explicit command from within that shell by
using `exec` or shell-equivalent replacement semantics. After the replacement
succeeds, the primary PID MUST remain the same PID and MUST refer to the
replacement process. When that primary PID exits, the containing pane MUST close
according to normal pane lifecycle rules.

Mezzanine v1 MUST NOT support an automatic pane or session default command that
replaces the resolved shell path when the user did not explicitly provide a
creation command.
When no explicit command is provided, a new pane MUST present the resolved
shell path as an interactive shell.

A window containing only a shell MUST represent that shell as a single pane
rather than as a separate window-level primary process.

When Mezzanine creates a new session without an existing saved layout, it MUST
create one window containing one pane and MUST attach the primary client to that
window. The initial window MUST belong to a single default window group. That
default group MUST NOT consume a visible group-bar row until at least one
additional group exists.

The multiplexer and the agent harness MUST be separable logical subsystems. An
implementation MUST NOT require application code that uses Mezzanine concepts to
depend on a specific programming language or implementation framework.

## 5. Session, Client, and Process Lifecycle

Mezzanine MUST support resumable sessions.

Every live session registered for discovery MUST have a stable session identity
that is unique among concurrently registered live sessions.

Mezzanine MUST support attaching a primary client to a session.

Mezzanine MUST support attaching one or more read-only observers to a session.

A client MUST NOT be granted the primary role unless it is attached through an
interactive terminal.

Exactly one client MUST be primary for a session while any primary client is
attached. Mezzanine MUST NOT permit more than one primary client for a session
at a time. A request to attach as primary while another primary client is
attached MUST fail with a structured conflict unless it is an authenticated
reattachment of the same primary client identity or an explicit primary
transfer operation that atomically removes the previous primary role before
granting the new one.

A client requesting read-only access MUST enter the pending observer role
first. A pending observer MUST NOT receive terminal, frame, window, pane, agent
status, message-log, or notification payloads other than request-local status
for its own observer request.

A pending observer MUST graduate to read-only observer only after the primary
client explicitly approves that observer. A rejected pending observer MUST be
disconnected or left connected only to a request-local status surface that
exposes no session state.

After approval, a read-only observer MAY receive the raw rendered live view
visible to the primary client from the approval moment forward. The observer
stream MUST start at the live viewport and live scroll position at the approval
moment, not at a historical copy-mode or scrollback position. Mezzanine MUST
NOT apply observer-specific redaction to the approved live rendered view.

Read-only observers MUST NOT receive pane history, paste buffers, transcripts,
or terminal output from before the approval moment.

If the primary client is in copy mode, viewing scrollback, viewing a paste
buffer, or otherwise displaying pre-approval history when an observer is
approved, Mezzanine MUST show the observer the live viewport instead or a
status placeholder until the primary returns to live content. After approval,
commands or UI states that would expose pre-approval history to that observer
MUST either omit older content or display a placeholder for it.

The primary client MUST define the authoritative terminal dimensions for the
session while attached. Read-only observers MUST render the primary client's
view without changing pane pseudoterminal sizes. If a read-only observer's
terminal is smaller than the primary view, Mezzanine MUST either scale through
documented terminal-safe rendering or provide local scrolling within the
presented view. If a read-only observer's terminal is larger than the primary
view, unused space MUST NOT change the controlled pane layout.

If the primary client detaches, the most recent dimensions defined by the
primary client MUST remain authoritative until a primary client reattaches or a
new primary client is explicitly selected by an authorized operation. Read-only
observers MUST NOT change pane pseudoterminal dimensions while no primary
client is attached.

Approved read-only observers MAY continue receiving rendered updates while no
primary client is attached, but those updates MUST use the last authoritative
primary-client dimensions and MUST NOT expose pre-approval history.

If the primary client detaches and pane primary processes remain active,
Mezzanine MUST keep the session running.

If all clients detach, Mezzanine MUST keep the session running unless the user
has configured exit-on-detach behavior.

Mezzanine MUST provide a way to detach the primary client without terminating
the session.

Mezzanine MUST provide a way to list resumable sessions.

Mezzanine MUST provide a way to reattach to a resumable session.

Mezzanine MUST provide a way to terminate a session explicitly.

The executable program name for the multiplexer MUST be `mez`.

Invoking `mez` without a subcommand MUST create a new session or attach to the
default resumable session according to configuration.

Creating a session that attaches a primary client MUST require an interactive
terminal. If `mez new`, bare `mez`, or an equivalent command would create and
attach a primary client but the invoking process has no interactive terminal,
Mezzanine MUST fail with an actionable diagnostic. Noninteractive `mez`
invocations MAY inspect or mutate existing sessions through the control
endpoint when authenticated and authorized, but Mezzanine v1 MUST NOT create a
new primary-attached session from a noninteractive invocation.

The `mez` command line interface MUST provide subcommands or equivalent flags
for:

- `mez new`: Create a new session. It MUST NOT attach to or otherwise reuse an
  existing session merely because that session already owns the default control
  socket.
- `mez serve`: Start a foreground Mezzanine service for a new session without
  attaching a primary client, and bind separate local message and event sockets
  unless explicitly configured not to do so.
- `mez attach`: Attach to an existing resumable session. Without an explicit
  socket selector, it MUST resolve an attachable session through the session
  registry rather than assuming the default control socket is active. It MUST
  accept an explicit session identifier and attach to that session's registered
  control socket. It MUST accept `--observer` and `--observe` as equivalent
  flags for requesting pending observer access.
- `mez list`: List resumable sessions.
- `mez detach`: Detach the current client when invoked from inside a session.
- `mez kill-session`: Terminate a session explicitly.
- `mez snapshot`: Create, list, inspect, delete, or resume session snapshots.
- `mez config`: Inspect, validate, and edit configuration.
- `mez auth`: Start authentication, show authentication status, and log out.
- `mez mcp`: List, add, remove, enable, disable, inspect, login, logout, and
  report status for MCP servers.

CLI subcommands that mutate session state MUST use the control endpoint and
MUST be subject to the same authentication and permission rules as equivalent
interactive commands.

Human-facing CLI subcommands MUST emit plaintext output by default, including
foreground service startup status and other structured status summaries. The CLI
MUST provide an explicit `--json` option that preserves machine-readable JSON
for automation. This CLI presentation rule does not change JSON framing used by
the control protocol, message protocol, provider protocol, snapshot storage, or
audit log storage.

When `mez serve` or an equivalent foreground service command is used,
Mezzanine MUST create a session, launch the initial pane shell on a
pseudoterminal, bind the selected local control endpoint, and keep the service
running until the session is terminated or the service is otherwise shut down.
The service command MUST NOT create a primary client by itself; primary
attachment MUST happen through a verified interactive client attach flow. While
the service is running, resumable session metadata SHOULD be visible through
the session registry. Published registry metadata MUST reflect the current
primary availability, lifecycle state, authoritative terminal size, and removal
state after primary attach, detach, process-exit, and shutdown transitions.
Registry updates from multiple live service processes MUST preserve unrelated
session records.

By default, a foreground service command MUST expose local message protocol and
event sockets in addition to the control endpoint. Unless explicit message or
event socket paths are configured, Mezzanine MUST derive the message socket
path and event socket path from the selected control socket path in the same
directory. For a control socket named `<stem>.sock`, the default derived paths
MUST be `<stem>.message.sock` and `<stem>.event.sock`. For a control socket
without a trailing `.sock` suffix, the default derived paths MUST append
`.message.sock` and `.event.sock` to the full control socket filename.

When a foreground service command binds local message protocol or event socket
paths, Mezzanine MUST bind those sockets as separate services from the control
endpoint. Message protocol frames and event notifications served by those
sockets MUST use the same live session owner used by the control endpoint.
Mezzanine MAY provide an explicit control-only service mode that does not bind
the default auxiliary sockets.

The `mez` CLI MUST support scriptable control of live sessions without
requiring an interactive client. It MUST support `-S <socket-path>` to select
an explicit control socket and `-L <socket-name>` to select a named socket
under the default Mezzanine socket directory.

The default Mezzanine socket directory SHOULD be `$MEZ_TMPDIR/mez-$UID` when
`MEZ_TMPDIR` is set, otherwise `$XDG_RUNTIME_DIR/mez` when `XDG_RUNTIME_DIR` is
set, otherwise `/tmp/mez-$UID`. The default socket directory MUST be private to
the current user and MUST NOT be world-readable, world-writable, or
world-executable.

Socket directory paths MUST be absolute. If `XDG_RUNTIME_DIR` is used, it MUST
be owned by the current user and have Unix mode `0700`, consistent with the XDG
Base Directory runtime directory requirements. If `MEZ_TMPDIR` is used, the
derived Mezzanine socket directory MUST still satisfy the same leaf-directory
ownership and permission requirements.

When creating a socket directory, Mezzanine MUST use permissions no broader
than `0700` and MUST create it in a way that does not follow a symlink for the
directory being created. When reusing an existing socket directory, Mezzanine
MUST verify that the path is a directory, is not a symlink, is owned by the
current user, and grants no permissions to group or other users. If these
checks fail, Mezzanine MUST refuse to use the directory and MUST report an
actionable diagnostic.

When creating a Unix domain socket, Mezzanine MUST bind only inside an approved
socket directory. Mezzanine MUST NOT unlink or replace an existing socket path
unless it has verified that the existing path is owned by the current user and
is stale. Staleness MUST be determined by a failed authenticated connection or
an equivalent lock-protected server identity check. After binding, Mezzanine
SHOULD set the socket file permissions to `0600` when the host operating system
honors Unix socket permissions.

On CLI startup, Mezzanine SHOULD scan its generated current-user runtime socket
directory and remove stale current-user-owned Mezzanine socket files that are
not connected to a live server. This cleanup MUST preserve live same-user
servers, MUST ignore non-socket files, and MUST NOT sweep arbitrary
user-supplied explicit socket directories.

For local Unix-domain control connections, Mezzanine SHOULD verify peer user
identity with operating-system peer credential facilities when available. A
peer whose effective user identity does not match the session owner MUST be
rejected unless an explicit shared-access policy is configured.

Mezzanine MUST set a `MEZ` environment variable in pane shells. The `MEZ`
variable MUST contain enough structured or delimited information for `mez`
commands invoked inside a pane to locate the current control socket and current
session. The socket path MUST be the first field so simple shell scripts can
recover it without parsing the full value.

Mezzanine MUST set `MEZ_SESSION`, `MEZ_WINDOW`, and `MEZ_PANE` environment
variables in pane shells. These variables MUST contain the stable identities for
the containing session, window, and pane.

When `mez` is invoked inside a pane and no `-S` or `-L` flag is provided, it
MUST use `MEZ` to locate the current control endpoint. When `mez` is invoked
outside a pane and no `-S` or `-L` flag is provided, it MUST use the default
socket name and directory.

The CLI MUST support long-form aliases for common session operations, including
`new-session`, `attach-session`, `list-sessions`, `kill-session`, and
`detach-client`, in addition to any shorter aliases.

Session, window group, window, and pane targets accepted by `mez` and the
command language MUST support stable IDs. Stable session IDs SHOULD be prefixed
with `$`, stable window-group IDs SHOULD be prefixed with `g`, stable window
IDs SHOULD be prefixed with `@`, and stable pane IDs SHOULD be prefixed with
`%`.

Live session registry displays MUST also derive creation-order index aliases
for registered sessions. These aliases MUST be assigned from the oldest
registered creation timestamp to the newest, starting at `$1`, with the stable
session ID used as the deterministic tie-breaker. Index aliases are display and
selection aliases only; they MUST NOT replace the stable session ID persisted
in the registry. `mez attach <target>` MUST accept the full stable session ID,
the displayed `$N` index alias, and the equivalent bare decimal `N` alias. If a
target exactly matches a stable session ID, that exact ID MUST take precedence
over any derived alias.

Mezzanine MUST launch pane shells on pseudoterminals.

Mezzanine MUST propagate pane size changes to the pane pseudoterminal. The
propagated size MUST be the visible pane content area after Mezzanine reserves
multiplexer-owned window-frame rows, pane-frame rows, and divider cells. Layout state
MAY retain larger logical pane rectangles for split ratios, but reserved multiplexer
cells MUST NOT be exposed as drawable cells to pane primary processes.

Mezzanine MUST place pane shell processes into an appropriate process group so
job control, foreground process handling, and terminal-generated signals behave
according to Unix terminal conventions.

When a pane closes because its primary shell exits, Mezzanine SHOULD send
configured cleanup notifications to child processes only as required by normal
Unix terminal and process-group behavior.

When Mezzanine terminates a pane explicitly, it SHOULD first request graceful
termination through the pane shell or foreground process group and SHOULD allow
escalation according to a documented timeout policy.

## 6. Terminal Multiplexing

### 6.1 Windows

A session MUST contain one or more windows while it is active.

Each window MUST have a stable identity that is unique within the session.

Each window MUST contain one or more panes.

Mezzanine MUST support creation of a new window from a live session. The
default prefix binding for creating a new window MUST be
`Ctrl+A c`. Mezzanine MAY also provide a direct convenience binding through
configuration, but the generated defaults MUST NOT bind a duplicate direct key
for creating a new window.

Every active window MUST belong to exactly one window group. A session MUST have
one active group while it contains windows. Creating a new ordinary window MUST
place that window in the active group unless the command explicitly targets a
different group. Breaking a pane into a new window MUST keep the new window in
the source window's group.

Mezzanine MUST support creating a new window group from a live session. Creating
a group MUST create one landing window in that group. The default prefix
binding for choosing a window group MUST be `Ctrl+A G`. The default prefix
binding for creating a group MUST be `Ctrl+A C`. The default prefix bindings
for focusing the previous and next group MUST be `Ctrl+A (` and `Ctrl+A )`.
Mezzanine MAY also provide configured direct convenience bindings for group
operations, but generated defaults MUST use the prefix bindings for these
operations.

The visible window bar MUST list windows from the active group. Window
navigation commands such as next, previous, and last window MUST operate within
the active group. Window target resolution MAY accept global stable IDs, but
numeric and display-order targets SHOULD resolve against the active group first.

Mezzanine MUST support selecting groups by stable id, displayed index, or name.
It MUST remember the previously active group for last-group navigation. Closing
the last window in a group MUST close that group. If only one group remains,
the group bar MUST disappear.

If the active window closes and other windows remain, Mezzanine MUST select
another window as the active window. The selection rule MAY be configurable, but
it MUST be deterministic.

If the final window in a session closes, Mezzanine MUST either terminate the
session or enter a configured empty-session state.

### 6.2 Panes

Each pane MUST have a stable identity that is unique within its window and
addressable within the session.

Each pane MUST have a rectangular region within its containing window.

Mezzanine MUST support live pane splitting.

The default prefix binding for vertical splitting MUST be
`Ctrl+A %`. Mezzanine MAY also provide a direct convenience binding through
configuration, but the generated defaults MUST NOT bind a duplicate direct key
for vertical splitting.

The default prefix binding for horizontal splitting MUST be
`Ctrl+A "`. Mezzanine MAY also provide a direct convenience binding through
configuration, but the generated defaults MUST NOT bind a duplicate direct key
for horizontal splitting.

The default prefix bindings for focusing the pane above, below, left,
or right of the active pane MUST be `Ctrl+A Up`, `Ctrl+A Down`, `Ctrl+A Left`,
and `Ctrl+A Right`, respectively. Mezzanine MAY also provide direct convenience
bindings through configuration, but the generated defaults MUST NOT bind
duplicate direct keys for directional pane focus.

The default prefix binding for focusing the previous window MUST be
`Ctrl+A p`. Mezzanine MAY also provide a direct convenience binding through
configuration, but the generated defaults MUST NOT bind a duplicate direct key
for focusing the previous window.

The default prefix binding for focusing the next window MUST be
`Ctrl+A n`. Mezzanine MAY also provide a direct convenience binding through
configuration, but the generated defaults MUST NOT bind a duplicate direct key
for focusing the next window.

After a pane is split, Mezzanine MUST allocate usable regions to both resulting
panes and MUST preserve the primary process and visible buffer of the original
pane.

When the first pane is created in a window, it MUST occupy the entire usable
terminal region of that window.

When a pane is split vertically, the new pane and the spawning pane MUST each
receive approximately half of the spawning pane's columns, subject to minimum
pane size constraints and rounding.

When a pane is split horizontally, the new pane and the spawning pane MUST each
receive approximately half of the spawning pane's rows, subject to minimum pane
size constraints and rounding.

If a pane creation or split request includes an explicit size, Mezzanine MUST
reject the request before mutating layout state or spawning a process when that
size cannot be represented by the active split without producing a pane below
the minimum dimensions, changing the split's cross-axis dimension, leaving
uncovered space, or overlapping another pane.

If a pane or window creation request cannot spawn the requested pane process,
Mezzanine MUST roll back any in-memory window, pane, focus, and geometry changes
made for that request and MUST NOT leave a processless pane behind.

The spawning pane MUST retain focus after a split unless the user or
configuration requests focus to move to the new pane.

Panes MUST be resizable. When a pane is resized, Mezzanine MUST update layout
state and resynchronize affected pane pseudoterminals to their visible content
regions.

Normal-screen live pane content that was soft-wrapped by terminal autowrap MUST
be reflowed across pane width changes so temporarily obscured cells are not
lost when a pane is narrowed and later expanded. Width-changing resizes MUST
keep split latency bounded by reflowing only the visible viewport instead of
synchronously rebuilding retained history, and they MUST NOT pull retained
scrollback into the new visible viewport; older scrollback MAY remain stored in
its existing physical wrapping. If a pane-local clear or terminal full-screen
erase such as shell `Ctrl+L` has intentionally detached the live viewport from
retained scrollback, subsequent resizes MUST preserve the live viewport
position instead of repopulating it from history. Row-only height changes MUST
leave the live viewport stationary on both pane shrink and pane growth unless a
shrink would otherwise truncate the currently visible tail; only that
truncating shrink MAY bottom-anchor the visible tail, and later growth MUST
NOT pull retained scrollback back into the live viewport.

When a pane's primary PID exits, Mezzanine MUST close the containing pane.

If more than one pane remains in the window after a pane closes, Mezzanine MUST
resize the remaining panes to occupy the available window region according to
the active layout policy. Close reflow MUST collapse the removed pane's split
slot, expand adjacent remaining panes as needed, and leave no gaps or overlaps
inside the window body.

If the final pane in a window closes, Mezzanine MUST close the containing
window.

### 6.3 Layout

Mezzanine MUST maintain a layout model for each window.

The layout model MUST be able to represent pane splits, pane sizes, and active
pane selection.

The layout model MUST survive detach and reattach.

The layout model SHOULD preserve relative pane sizes across terminal resize
events when doing so is possible without producing unusable pane regions.

For every unzoomed window, the layout model MUST describe pane rectangles that
fit inside the window body, do not overlap, and cover the usable body after
split, resize, close, break, join, rotate, and layout-policy operations.

Mezzanine MUST support layout operations for selecting panes by direction,
cycling panes, returning to the last active pane, zooming the active pane,
swapping panes, rotating panes, breaking a pane into a new window, and joining
a pane into another window. The named layout policy set MUST include `tiled`,
`even-vertical`, `even-horizontal`, and `even-grid`.

Directional pane selection MUST use pane geometry, MUST prefer returning to the
pane focus came from when that pane is a valid target in the requested
direction, and MUST wrap to the first pane on the opposite edge when no pane is
available in the requested direction.

Mezzanine MUST support deterministic pane indexes within a window and window
indexes within a session.

### 6.4 Frames

Mezzanine MUST provide framing per window.

Mezzanine MUST support optional framing per pane.

Foreground attached clients MUST render visible window and pane state by
default. Built-in defaults MUST enable a window frame or status bar and MUST
render either pane frames, pane borders, or an equivalent pane status surface
that identifies split boundaries, the active pane, and current window/pane
state.

Default foreground rendering MUST include thin visible pane dividers for split
boundaries, including horizontal divider rows between stacked panes when pane
frames are enabled. Where split boundaries meet, foreground rendering MUST use
connected thin box-drawing junctions rather than ASCII fallback glyphs. Styled
attached-terminal output MUST render those dividers with a distinguishable color
or attribute and MUST visually distinguish the active pane from inactive panes.

Styled attached-terminal output SHOULD render standalone pane frame rows and the
window frame/status row with a subtle full-row fill so pane and window boundaries
remain visible even on edges that do not contain divider glyphs. That fill MUST
be less prominent than active-pane or active-window title highlighting. The
top-most pane header at the top of the attached terminal MUST remain a filled
theme surface made from spaces, label text, or status text rather than a row of
box-drawing divider glyphs.
Horizontal pane dividers and pane frame rows merged into divider rows MUST draw
their divider cells with Unicode box-drawing characters. Styled
attached-terminal output MUST NOT apply a background color to horizontal
divider fill cells or divider junction cells; those cells MUST remain
foreground-only. Pane title, working-directory, model, reasoning, status, and
other pill regions embedded in a merged divider row MAY retain their themed
foreground/background styling. When right-aligned pane-frame status content is
present, the renderer MUST reserve the rightmost available cell for horizontal
border fill whenever the pane frame is wider than one cell.
Vertical separator glyphs MUST remain foreground-only unless they are part of a
distinct interactive pill or status region rather than a divider cell.

Window frames or status bars rendered for an attached foreground client MUST be
useful as a header or footer: they MUST display the current window index and a
stable window name or identity, SHOULD include the active pane index and title or
identity, and SHOULD use a distinguishable color or attribute by default.
The built-in default attached foreground window bar MUST be a footer and MUST
render one pillbox per window. Each window pillbox MUST display text in the form
`<index> <title>`, and the focused window pillbox MUST be visually highlighted.
The window bar MUST remain visible when the active group contains only one
window.
The window status area MUST support templated single-character command
pillboxes through `#{button:<icon>|terminal|<command>}` and
`#{button:<icon>|agent|<command>}` fields. Terminal buttons MUST dispatch the
configured command through the `:` terminal command path. Agent buttons MUST
enter or resume the focused pane's agent shell and dispatch the configured
command through the pane-local `/` agent command path. Command pillboxes MUST
NOT use text labels, MUST visually change while pressed, and MUST revert to
their normal style when released. Releasing the mouse over the same pressed
command pillbox MUST run the command; releasing elsewhere MUST cancel it.
The generated default window right-status template MUST include padded button
pillboxes for new-pane, new-window, new-group, and agent-shell commands. The
default icons for those buttons MUST be `+`, `□`, `⊕`, and `λ`, respectively.
Pane-local agent status controls own routing display and toggling.
Users MUST be able to remove or replace these buttons by editing the window
right-status template.
When more than one window group exists, attached foreground clients MUST render
a top group bar above the active window. The group bar MUST use the same
pillbox conventions as the window bar, MUST display one entry per group in the
form `<index> <title>`, and MUST visually highlight the active group. When
only one group exists, the group bar MUST not be visible and MUST not reduce
the pane pseudoterminal size.
Pane and window titles rendered in frames MUST NOT be empty. Empty,
whitespace-only, or control-only terminal title updates MUST be normalized to the
default pane title. When the host exposes foreground process-group metadata,
automatically derived pane titles SHOULD reflect the active non-shell foreground
program name when no user-defined title is pinned. Explicit pane titles assigned
by a user or agent command MUST remain stable until another explicit pane-title
assignment replaces them. A window title MAY be derived from the active pane
title while the window uses a generated or default name; an explicit non-default
window name MUST remain stable until a later user or agent rename replaces it.
Mouse clicks on a rendered group bar, window frame, or status bar MUST be
treated as multiplexer UI interactions, such as opening the group/window chooser or
focusing the clicked group/window entry, and MUST NOT be forwarded to the pane
primary process unless an explicit passthrough policy is configured.

Window frames MUST be able to display the window identity.

Pane frames, when enabled, MUST be able to display the pane identity.

Frame content MUST be configurable.

Window frames MUST support the following fields:

- `session.id`: Stable session identity.
- `window.list`: Ordered window pillbox text for the foreground window bar.
- `window.id`: Stable window identity.
- `window.index`: Current window index in the session's display order.
- `window.title`: User-defined or automatically derived window title.
- `window.active`: Whether the window is active.
- `window.pane_count`: Number of panes in the window.
- `window.buttons`: Single-character command pillboxes for user-templated
  terminal and agent commands in the window status area.
- `window.actions`: Compatibility alias for `window.buttons`.
- `system.uptime`: Human-readable host system uptime for window status lines.
- `datetime.local`: Human-readable current local date and time for window status
  lines.
- `layout.name`: Current layout policy or layout name.
- `agent.active_count`: Number of running agents in the window.
- `message.unread_count`: Number of unread local agent messages scoped to the
  window.

Window frame templates MAY also use active-pane fields from the pane-frame field
set, such as `pane.index`, `pane.id`, and `pane.title`, to describe the focused
pane in a compact header or footer.

Pane frames MUST support the following fields:

- `session.id`: Stable session identity.
- `window.id`: Stable window identity.
- `window.index`: Current window index.
- `pane.id`: Stable pane identity.
- `pane.index`: Current pane index in the window's display order.
- `pane.title`: User-defined or automatically derived pane title, including the
  active foreground program name when known.
- `pane.active`: Whether the pane is active.
- `pane.size`: Pane size in terminal columns and rows.
- `pane.primary_pid`: Primary PID when the host platform exposes one.
- `pane.process_name`: Primary process name when known.
- `pane.exit_status`: Exit status after the primary process exits.
- `pane.pwd`: Current pane working directory, shortened relative to the user's
  home directory when possible and compacted to at most the last three path
  segments when deeper.
- `pane.mode`: Current pane interaction mode, such as normal, copy, resize, or
  agent.
- `agent.id`: Agent identity associated with the pane.
- `agent.name`: Human-readable display name associated with the pane agent.
- `agent.status`: Agent state, such as idle, running, waiting, or errored.
- `agent.model`: Active provider model name when visible by policy.
- `agent.reasoning`: Active reasoning profile or effort when visible by policy.
- `agent.thinking`: Provider thinking-mode state when the active provider
  supports a native thinking toggle.
- `agent.routing`: Pane-local routing enablement state.
- `agent.preset`: Active model preset name when the pane model and
  auto-sizing profile group match a configured preset, or an implementation
  placeholder such as `custom` when presets exist but the pane state does not
  match one.
- `agent.context_usage`: Last known provider input context-window usage
  percentage for the active pane-agent conversation, computed from the latest
  provider response input-token count and Mezzanine's effective model
  context-window token budget rather than cumulative conversation token usage
  or a provider-supplied percentage, and saturated at `100%` for display.
- `policy.mode`: Active approval policy as `ask`, `auto-allow`, or `full-access`.
- `observer.pending_count`: Number of read-only observer attach requests
  waiting for primary-client approval.
- `history.position`: Scrollback position when the pane is not at the live
  bottom.

Frame templates MUST use named fields. A missing field MUST render as an empty
string or an explicitly configured placeholder.

Frame renderers MUST sanitize control characters in field values before
display.

Frame renderers MUST truncate, wrap, or elide frame content according to a
documented rule when the available frame region is too small.

The default window frame template MUST display at least one `window.index` and
`window.title` or `window.id` pair for each window. It MAY also display
`window.pane_count` or active-pane identity.
The default window right-status template MUST place `pane.pwd` at the far left
of the right-status region and MUST include literal spaces between adjacent
status pills and command-button pills.

The default pane frame template, when pane framing is enabled, MUST display at
least `pane.index` plus `pane.title` or `pane.id`; the built-in default SHOULD
render pane text in a padded pill in the form ` <index> <title> ` without an
idle agent marker. The leading and trailing padding cells SHOULD visually frame
pane titles in the same family as window title pills.
When a pane is viewing scrollback rather than the live bottom, the default pane
frame MUST surface the current pane working directory as a right-aligned
`pane.pwd` pill, displayed relative to the user's home directory when possible,
and compacted to at most the last three path segments with a leading ellipsis
when deeper, so users can track the active shell location without relying on
shell prompts.
The `pane.pwd` pill MUST appear to the left of agent model, reasoning, and
status pills when those fields are visible. When a pane is viewing scrollback
rather than the live bottom, the default pane frame MUST also surface
`history.position` at the right edge of the pane frame.
The scrollback indicator MUST use a themed foreground/background color pair
while visible, and renderers MUST avoid placing its final glyph in a terminal
edge cell when doing so would be vulnerable to host-terminal autowrap
truncation.

The default window frame MUST support a configurable right-aligned status line.
The generated default configuration MUST display padded window command buttons
for `split-window -h`, `split-window`, `new-window`, `new-group`, and
`agent-shell`, followed by `system.uptime` and `datetime.local`, in that status
line with distinguishable themed color spans.

When one or more observer attach requests are pending, Mezzanine MUST surface
that state to the primary client through the pane frame, window frame, message
log, command prompt, or an equivalent pane status message. The displayed status
MUST include enough information for the primary client to identify the pending
observer and the command used to approve, reject, or inspect it.

Frames MUST NOT corrupt the byte stream delivered to pane primary processes.

Frames MUST NOT prevent the user from interacting with pane contents.

Mezzanine-owned colored UI surfaces MUST be themeable without modifying pane
application output. Theme colors apply to window frames, window pillboxes, pane
frames, active and inactive pane border glyphs, pane dividers, scroll position
indicators, pane working-directory pills, full-row frame fills, window status
items, window action pillboxes, window-group pillboxes, pane-frame agent model,
reasoning, and status pills,
command prompts, pane-local agent prompts, agent transcript
gutters, transcript labels, transcript status/error/command lines, display
overlays, status rows, and copy-selection highlighting.
Pane content emitted by hosted applications MUST continue to use the SGR
colors, attributes, and true-color values emitted by those applications.
Non-interactive Mezzanine-authored text rendered inside pane buffers or full
display overlays, including agent/user/thinking transcript text, errors,
command previews, help output, and command output presented for reference, MUST
use foreground color only and MUST NOT set a background color by default so the
underlying terminal background remains authoritative. Distinct interactive UI
elements, including status bars, prompt input rows, buttons, selector rows, and
copy selections, MAY use themed backgrounds.
Box-drawing glyphs used only as pane borders or divider cells MUST NOT receive
a background color by default. Filled backgrounds are reserved for textual frame
and status regions such as pane title/status pills and window status bars.

Every Mezzanine-owned colored UI surface MUST map to a named color slot whose
value is user-configurable by either a hex color code or a color alias. The
baseline slot names are `window_frame_fg`, `window_frame_bg`,
`window_active_fg`, `window_active_bg`, `window_inactive_fg`,
`window_inactive_bg`, `pane_frame_active_fg`, `pane_frame_active_bg`,
`pane_frame_inactive_fg`, `pane_frame_inactive_bg`, `pane_border_active_fg`,
`pane_border_active_bg`, `pane_border_inactive_fg`,
`pane_border_inactive_bg`, `pane_divider_fg`, `pane_divider_bg`,
`frame_fill_fg`, `frame_fill_bg`, `scroll_indicator_fg`,
`scroll_indicator_bg`, `pane_pwd_fg`, `pane_pwd_bg`, `window_status_uptime_fg`,
`window_status_uptime_bg`, `window_status_datetime_fg`,
`window_status_datetime_bg`, `prompt_fg`, `prompt_bg`, `agent_prompt_fg`,
`agent_prompt_bg`, `agent_transcript_user_fg`, `agent_transcript_user_bg`,
`agent_transcript_assistant_fg`, `agent_transcript_assistant_bg`,
`agent_transcript_status_fg`, `agent_transcript_status_bg`,
`agent_transcript_error_fg`, `agent_transcript_error_bg`,
`agent_transcript_command_fg`, `agent_transcript_command_bg`,
`agent_model_fg`, `agent_model_bg`,
`agent_reasoning_fg`, `agent_reasoning_bg`, `agent_status_idle_fg`,
`agent_status_idle_bg`, `agent_status_running_fg`,
`agent_status_running_bg`, `agent_status_blocked_fg`,
`agent_status_blocked_bg`, `agent_status_failed_fg`,
`agent_status_failed_bg`, `display_overlay_fg`, `display_overlay_bg`,
`copy_selection_fg`, `copy_selection_bg`, `syntax_plain_fg`,
`syntax_plain_bg`, `syntax_keyword_fg`, `syntax_keyword_bg`,
`syntax_string_fg`, `syntax_string_bg`, `syntax_comment_fg`,
`syntax_comment_bg`, `syntax_type_fg`, `syntax_type_bg`,
`syntax_function_fg`, `syntax_function_bg`, `syntax_number_fg`,
`syntax_number_bg`, `syntax_operator_fg`, and `syntax_operator_bg`.

The built-in default theme MUST be named `kanagawa`. It MUST define at least
the aliases `primary`, `secondary`, and `tertiary`, and those aliases MUST be
assigned to high-impact active window, active pane, prompt, overlay, divider,
or selection slots so changing only those aliases significantly changes the UI
theme. Built-in themes MUST assign muted and agent-thinking aliases to
theme-relative grey-equivalent colors that remain readable against the theme
surface while reading as lower emphasis than active titles, user transcript
labels, and assistant transcript labels.
Built-in themes MUST include common terminal and editor color schemes:
`deepforest`, `gruvbox_dark`, `gruvbox_light`, `solarized_dark`,
`solarized_light`, `monokai`, `dracula`, `nord`, `tokyo_night`,
`catppuccin_latte`, `catppuccin_frappe`, `catppuccin_macchiato`,
`catppuccin_mocha`, `one_half_dark`, `one_half_light`, `onedark`,
`rose_pine`, `rose_pine_moon`, `rose_pine_dawn`, `kanagawa`,
`everforest_dark`, `everforest_light`, `ayu`, `ayu_dark`, `ayu_light`,
`ayu_mirage`, `high_contrast_dark`, and `high_contrast_light`.

### 6.5 History Buffering

Each pane MUST maintain a bounded history buffer.

The default history buffer size MUST be 10000 lines per pane.

The history buffer size MUST be configurable.

The history overflow rotation count MUST be configurable and MUST default to
1000 lines.

When the history buffer exceeds its configured bound, Mezzanine MUST evict older
history before newer history in rotation batches rather than treating overflow
as a fatal condition. A normal single-line overflow SHOULD evict the configured
rotation count, capped so the buffer remains valid for small history limits.

History buffering MUST survive detach and reattach for the portion of history
that remains within the configured bound.

### 6.6 Mouse Support

Mouse input MUST be enabled by default in built-in and generated configuration.
When an attached foreground client with mouse input enabled takes ownership of a
host terminal, it MUST enable supported host-terminal mouse reporting modes,
including SGR-encoded button and drag events, and MUST restore those modes on
detach, shutdown, or error.

Mezzanine MUST support mouse-based pane resizing by dragging pane borders when
mouse input is enabled and the host terminal supports mouse reporting.

Mezzanine MUST support selecting text within a pane for copy and paste
purposes.

Mouse text selection MUST remain anchored to the pane where the selection began,
even when the pointer crosses pane borders, and MUST autoscroll scrollable pane
history at the top and bottom pane edges. Autoscroll speed SHOULD increase in
proportion to the distance between the current pointer position and the
selection's starting click position.

Right-click paste MUST paste host clipboard text when available and fall back to
the most recent internal paste buffer when host clipboard text is unavailable,
unless the pane application has captured mouse input.

Mezzanine MUST support mouse scrolling within pane history buffers.
When scroll-mode or copy-mode history scrolling reaches the live bottom and no
selection is active, Mezzanine MUST exit that mode automatically so subsequent
keyboard input goes to the pane process.

Mouse interactions with frames and pane borders MUST NOT be delivered to the
pane primary process unless explicitly configured.

Mouse interactions with a multiplexer-managed window frame or status bar MUST be
routed to a multiplexer control action rather than to copy-mode selection or pane
application mouse input.

Mouse interactions with a multiplexer-managed group bar MUST be routed to group
focus or group chooser actions rather than to copy-mode selection or pane
application mouse input.

Mouse clicks or selections inside a pane body MUST focus the targeted pane before
the multiplexer applies copy-mode selection or other pane-local mouse behavior.

Mouse interactions inside a pane body MUST be evaluated against that target
pane's own terminal mode state, not the previously focused pane's state. When an
unfocused pane has enabled mouse reporting, the first button press inside that
pane MUST focus the pane and MUST NOT forward the mouse packet to the previously
focused pane. Once the pane is focused, pane-application mouse reporting MUST
take precedence over pane-body multiplexer shortcuts such as history scrolling,
right-click paste, and mouse selection. Active multiplexer modes and multiplexer-owned
cells still take precedence: an in-progress resize drag, active copy-mode
selection, pane borders, and window frames remain Mezzanine-owned unless
explicitly configured otherwise. The active copy-mode selection exception remains
anchored to the pane where text drag selection began, so out-of-pane drag
continuations can autoscroll or extend that selection even when the pointer
crosses another pane.

When a pane application enables legacy xterm mouse tracking without SGR mouse
encoding, Mezzanine MUST still treat the pane as owning mouse input inside that
pane body. Host-terminal SGR mouse packets SHOULD be translated to the pane
application's selected mouse encoding and pane-local coordinates before delivery
to the pane pseudoterminal.

### 6.7 Terminal Compatibility

Mezzanine MUST decode and display terminal output according to existing terminal
conventions, including control sequences, cursor movement, screen clearing,
alternate screen behavior, color, style, and mouse reporting.

Terminal output rendering MUST preserve styled blank cells that are significant
to the visible viewport, including full-row background-color fills emitted by
full-screen applications.

Terminal output rendering MUST compose panes by terminal display cells rather
than Unicode scalar positions. If a mux-owned divider, frame, or overlay cell
overwrites either half of a previously rendered wide glyph, Mezzanine MUST clear
the entire wide glyph footprint before writing the mux-owned cell so adjacent
pane content cannot shift on only the affected row.

Terminal autowrap MUST follow terminal conventions: writing a glyph in the final
column MUST defer wrapping until the next printable glyph instead of scrolling
immediately.

Foreground attached clients MUST take ownership of the terminal presentation
surface while attached. They MUST enter the configured presentation mode, hide or
replace any host-terminal cursor that would otherwise leak through the drawn
multiplexer surface, clear or redraw the full visible viewport after attach and
resize, and clear the full visible viewport while restoring host-terminal modes
and cursor visibility on detach, shutdown, or error.
Differential attached-client redraws MUST preserve cells in the terminal's final
column; when a redraw writes a full-width row, it MUST NOT immediately follow
that row with an erase-to-end-of-line control sequence that could clear the
freshly written edge cell.
Foreground attached clients MUST reset coordinate-affecting host-terminal state
before each presentation frame, including origin mode, scrolling margins, and
left/right margin mode where supported, so cursor placement remains absolute to
the rendered multiplexer viewport after command dialogs, overlays, or hosted terminal
applications have changed terminal modes.

After the initial draw or another full-surface invalidation, foreground attached
clients MUST NOT clear and redraw the entire viewport on every stable-size
frame. They SHOULD update stable-size frames with row or cell diffs. A full clear
or full redraw remains valid after attach, detach, terminal resize,
frame-size change, or another invalidation that makes differential output unsafe.
While otherwise idle, attached foreground clients SHOULD continue polling the
local terminal size often enough to notice host-terminal resizes without
waiting for user input or daemon-side runtime events. They SHOULD trigger a
fresh client-local redraw only when that measured size actually changes.
After terminal or pane resize activity, attached clients SHOULD wait for the
configured `terminal.resize_debounce_ms` period before forcing one full-surface
redraw; the built-in default debounce MUST be `200` milliseconds.

Mezzanine MUST present a cursor for the active interactive surface when that
surface accepts keyboard input. Cursor style MUST be configurable. Cursor blink
behavior MUST be configurable, including whether blinking is enabled and the
blink interval or rate used by Mezzanine-rendered cursor presentation. The
configured cursor blink interval is the full visible-plus-hidden blink cycle;
the built-in default cycle MUST be `500` milliseconds.
For ordinary pane interaction, Mezzanine MUST present the cursor at the active
pane screen cursor position, which is the cell where the next printable glyph
would be applied by the terminal screen model. Presentation-frame cursor
placement MUST NOT apply a separate font-specific glyph-width heuristic that
can disagree with the screen model and shift the visible cursor away from that
next-input cell.

The default terminal compatibility profile MUST target xterm-compatible
behavior.

Terminal compatibility MUST be represented as a named profile with explicit
capabilities. The profile abstraction MUST allow future profiles to add, remove,
or refine terminal capabilities without changing unrelated multiplexer or agent
semantics.

The xterm-compatible profile MUST define behavior for at least C0 controls,
ESC, CSI, OSC, DCS string controls, SGR attributes, DEC private modes,
alternate screen buffers, application cursor and keypad modes, bracketed paste,
focus events when supported by the host terminal, mouse tracking including SGR
mouse encoding, title setting, clipboard sequences when enabled by policy, and
save/restore mode behavior.

Mezzanine MUST set the `TERM` value visible to panes to a value consistent with
the active terminal compatibility profile.

The default pane `TERM` value MUST be `screen-256color`. Mezzanine MAY also
provide `mez-256color` and `mezzanine-256color` as custom terminfo names for
users who explicitly select them.

The `mez-256color` and `mezzanine-256color` terminal descriptions MUST map to
the xterm-compatible Mezzanine terminal profile. They MUST advertise the
xterm-compatible capabilities implemented by Mezzanine, including 256-color
SGR, alternate screen entry and exit, cursor movement, keypad and cursor
application modes, bracketed paste, SGR mouse reporting, and title setting
when enabled. They MUST NOT advertise capabilities that Mezzanine does not
implement or intentionally pass through.

The Mezzanine-specific terminfo entry SHOULD be derived from the system
`xterm-256color` entry by removing unsupported capabilities and adding
Mezzanine-specific capabilities only when those capabilities are implemented.
Mezzanine MUST expose the active terminal profile and selected terminfo name in
diagnostics.

If a Mezzanine-specific terminfo entry is not available, Mezzanine MUST choose
the safest installed fallback in this order:

1. `screen-256color`
2. `screen`
3. `vt100`
4. `dumb`

Mezzanine MUST NOT fall back to `xterm-256color` or another host-terminal
identity in the default terminal profile. Mezzanine MAY use `xterm-256color`
only when the active terminal profile is an explicit direct-passthrough profile
configured by the user. Mezzanine MUST warn the user when it falls back from a
Mezzanine-specific terminfo entry and MUST make the selected fallback and
degraded capability set visible in diagnostics.

If none of the listed fallback terminfo entries is installed, Mezzanine MUST use
a documented built-in `dumb` compatibility profile, set pane `TERM` to `dumb`,
and advertise no capabilities beyond safe line-oriented terminal output. This
state MUST be visible in diagnostics and SHOULD include instructions for
installing or printing the Mezzanine-specific terminfo entry.

Mezzanine MUST NOT set pane `TERM` to `xterm`, `xterm-256color`, or another
host-terminal identity by default. A pane application sees Mezzanine as the
terminal, not the containing terminal emulator.

Mezzanine SHOULD provide a command or first-run step that installs or prints
the Mezzanine terminfo entry for the current user. If terminfo installation is
not possible, Mezzanine MUST continue with the best documented fallback and
make the degraded capability set visible through diagnostics.

Mezzanine MUST document which control sequences are interpreted by Mezzanine,
which are forwarded to the containing terminal, which are translated, and which
are ignored.

Alternate screen contents MUST be represented separately from the normal screen
and normal history buffer. Output produced while a pane is in an alternate
screen MUST NOT be appended to the normal bounded history buffer.

Default agent context assembly MUST NOT include passive terminal contents from
the visible screen, normal history, or alternate screen. When a pane is
currently in an alternate screen, default agent context MAY indicate that an
alternate screen is active, but it MUST omit the alternate-screen cell contents.

This alternate-screen rule is an explicit exception to explicit capture
features, not a permission to passively inject terminal contents into model
context. The harness MAY know that an alternate screen is active, but default
agent context and any history-inclusive observations MUST exclude the
alternate-screen cell contents.

User-facing commands MAY provide an explicit visible-screen capture mode for
the active alternate screen, but alternate-screen content MUST NOT appear in
history-inclusive capture, copy-mode history, or default agent context.

Mezzanine MUST preserve compatibility with programs that expect a multiplexed
xterm-compatible terminal: full-screen programs MUST be able to enter and leave
the alternate screen, line-oriented programs MUST receive normal pty input and
resize notifications, mouse-aware programs MUST receive supported mouse events,
and programs using bracketed paste or application cursor/keypad modes MUST see
mode transitions consistent with the active terminal profile.

When the active pane application enables bracketed paste, Mezzanine MUST mirror
that mode into the attached foreground terminal so host clipboard pastes arrive
with bracketed-paste delimiters. While receiving a host bracketed-paste payload,
Mezzanine MUST forward the payload bytes opaquely to the pane process, MUST NOT
interpret multiplexer prefixes or mouse reports embedded in the payload, and MUST
preserve the in-paste state across bounded terminal-read chunks. Pasting large
host clipboard contents into an attached pane MUST NOT crash, detach, or stall
the multiplexer.

Mezzanine SHOULD be operable inside other terminal multiplexers.

When nested inside another multiplexer, Mezzanine MUST NOT assume exclusive ownership
of terminal capabilities that are controlled by the outer environment.

Mezzanine MUST NOT emit terminal control sequences that intentionally break
nested operation unless the user has explicitly enabled such behavior.

## 7. Input, Commands, Copy Mode, and Notifications

### 7.1 Escape Sequence

Mezzanine MUST provide an escape sequence that enters a transient prefix-key
state.

The default escape sequence MUST be `Ctrl+A`.

The escape sequence MUST be configurable.

Pressing the escape sequence by itself MUST NOT enter the command prompt.

Pressing the escape sequence followed by `:` MUST enter a command prompt.

The command prompt MUST accept Mezzanine commands using a structured
multiplexer command language.

The command prompt and configuration shell MUST provide a selector for
Mezzanine command names and command arguments with enumerable values. Pressing
Tab MUST select or advance the next matching candidate for the token at the
cursor. Pressing Shift+Tab MUST select or advance the previous matching
candidate. Selector application MUST replace only the active token in the
current command segment and MUST NOT submit the command.

The command prompt and configuration shell MUST render prefix-based shadow
hints for the best matching command name or enumerable argument value without
mutating the editable buffer. Prefix shadow hints MUST only render when the
cursor is at the end of the token being completed so cursor navigation inside
multi-line input cannot shift or duplicate the visible token. Commands that
accept parameters SHOULD render a parameter placeholder after the complete
command name; each parameter placeholder MUST disappear as soon as the user
types any text for that parameter position.

Configuration changes made through the command prompt or configuration shell
MUST apply to the live session when the affected setting supports live mutation.

If a setting cannot be applied live, Mezzanine MUST report that limitation to
the user.

Terminal commands whose successful effect is immediately observable, such as
prefix forwarding, theme changes, option changes, binding changes, and source or
refresh operations, SHOULD take effect without opening a modal display overlay
that the user must dismiss. Short acknowledgement output from such commands
SHOULD be appended to the active pane transcript instead. Commands that return
reference information, selector choices, or diagnostics SHOULD continue to use
the display overlay. Display overlay scrolling MUST clamp to the available
content range; PageDown, mouse wheel scrolling, and selection navigation MUST
NOT leave the overlay scrolled past its final rendered content.

### 7.2 Default Prefix Bindings

Mezzanine MUST support a configurable prefix key table.

The default prefix bindings MUST follow the established mux command placement
where Mezzanine has an equivalent operation. Mezzanine-specific operations MUST
use bindings that do not conflict with the established mux-compatible set. The
default prefix bindings MUST include:

- `Ctrl+A Ctrl+A`: Send the prefix key to the active pane.
- `Ctrl+A :`: Enter the Mezzanine command prompt.
- `Ctrl+A ?`: List key bindings.
- `Ctrl+A d`: Detach the primary client.
- `Ctrl+A D`: Choose a client or observer to detach.
- `Ctrl+A c`: Create a new window.
- `Ctrl+A C`: Create a new window group.
- `Ctrl+A a`: Toggle the focused pane's agent shell.
- `Ctrl+A ,`: Rename the current window.
- `Ctrl+A &`: Kill the current window after confirmation.
- `Ctrl+A w`: Choose a window interactively.
- `Ctrl+A G`: Choose a window group interactively.
- `Ctrl+A (`: Focus the previous window group.
- `Ctrl+A )`: Focus the next window group.
- `Ctrl+A n`: Focus the next window.
- `Ctrl+A p`: Focus the previous window.
- `Ctrl+A l`: Focus the last active window.
- `Ctrl+A 0` through `Ctrl+A 9`: Focus the window with the given index.
- `Ctrl+A '`: Prompt for a window index to focus.
- `Ctrl+A .`: Prompt for a new index for the current window.
- `Ctrl+A %`: Split the active pane vertically.
- `Ctrl+A "`: Split the active pane horizontally.
- `Ctrl+A Up`, `Ctrl+A Down`, `Ctrl+A Left`, `Ctrl+A Right`: Focus a pane by
  direction.
- `Ctrl+A o`: Cycle to the next pane.
- `Ctrl+A ;`: Focus the last active pane.
- `Ctrl+A q`: Display pane indexes and allow indexed pane selection.
- `Ctrl+A z`: Toggle zoom for the active pane.
- `Ctrl+A Space`: Cycle through configured layouts.
- `Ctrl+A x`: Kill the active pane after confirmation.
- `Ctrl+A !`: Break the active pane into a new window.
- `Ctrl+A {`: Swap the active pane with the previous pane.
- `Ctrl+A }`: Swap the active pane with the next pane.
- `Ctrl+A PageUp`: Enter copy mode and scroll one page up.
- `Ctrl+A [`: Enter copy mode.
- `Ctrl+A ]`: Paste the most recent paste buffer.
- `Ctrl+A #`: List paste buffers.
- `Ctrl+A =`: Choose the active copy/paste buffer interactively.
- `Ctrl+A -`: Delete the most recent paste buffer when in buffer context.
- `Ctrl+A O`: Choose pending read-only observers to approve, reject, inspect,
  or revoke.
- `Ctrl+A ~`: Show Mezzanine messages.

Bindings MAY be changed by configuration or command.

Mezzanine MUST NOT silently choose different default key bindings because a
terminal, desktop environment, or nested multiplexer may intercept the defaults. If
Mezzanine detects that a default binding is unlikely to be delivered, it SHOULD
warn the user and SHOULD offer configuration guidance.

### 7.3 Command Language

Mezzanine commands MUST have a command name followed by flags and arguments.

Commands entered at the Mezzanine command prompt MUST be parsed by Mezzanine,
not by the pane shell.

Commands MAY be separated by semicolons. If one command in a semicolon-separated
sequence fails, subsequent commands in that sequence MUST NOT execute unless
the command language explicitly marks the failure as ignored.

Command arguments MUST support quoting sufficient to represent whitespace,
semicolons, quotes, and backslashes.

Commands that target a session, client, window group, window, or pane MUST
accept compact target flags when applicable:

- `-t` for target session, window, or pane.
- `-s` for source pane, window, or session when a command moves or copies state.
- `-c` for a starting directory when creating a pane or window.
- `-F` for formatted output when a command returns structured display text.

Targets MUST support stable identities and user-facing indexes. Exact identity
matches MUST take precedence over prefix or glob-like matches.

Invalid or failed commands submitted through an attached foreground command
prompt MUST NOT crash or detach the terminal. Mezzanine MUST display a single
prompt-line error and allow any key to return to the normal multiplexer surface.

The `new-window`, `new-group`, and `split-window` commands MUST accept an optional
`shell-command` argument. If `shell-command` is omitted, the new pane MUST start
the resolved shell path as an interactive shell. If `shell-command` is provided
as a single command string, Mezzanine MUST run that command from within the
resolved shell path using shell-specific `exec` replacement semantics. If
`shell-command` is provided as multiple command arguments, Mezzanine MUST
either execute the argument vector through a shell-specific `exec` form without
introducing additional shell parsing, or serialize it into a shell command
using a documented, lossless quoting algorithm before applying `exec`. The
command form actually used MUST be visible in diagnostics and audit records
when audit logging is enabled.

The command language MUST include commands equivalent to:

- `new-window`
- `rename-window`
- `kill-window`
- `select-window`
- `next-window`
- `previous-window`
- `last-window`
- `new-group`
- `rename-group`
- `kill-group`
- `select-group`
- `next-group`
- `previous-group`
- `last-group`
- `split-window`
- `kill-pane`
- `select-pane`
- `resize-pane`
- `rebalance-window`
- `swap-pane`
- `break-pane`
- `join-pane`
- `display-panes`
- `list-windows`
- `list-groups`
- `choose-group`
- `list-panes`
- `list-clients`
- `detach-client`
- `attach-session`
- `list-sessions`
- `rename-session`
- `kill-session`
- `help`
- `copy-mode`
- `copy-selection`
- `paste-clipboard`
- `paste-buffer`
- `create-buffer`
- `list-buffers`
- `choose-buffer`
- `delete-buffer`
- `show-messages`
- `show-metrics`
- `list-keys`
- `list-themes`
- `set-theme`
- `bind-key`
- `unbind-key`
- `show-options`
- `set-option`
- `source-file`
- `refresh-client`
- `refresh-provider-info`
- `agent-shell`
- `auth-login`
- `auth-status`
- `mcp-add`
- `mcp-remove`
- `snapshot-session`
- `resume-session`
- `capture-pane`
- `save-buffer`
- `clear-history`
- `search-history`
- `export-history`
- `pipe-pane`
- `mark-pane-ready`
- `list-observers`
- `choose-observer`
- `approve-observer`
- `reject-observer`
- `revoke-observer`

The terminal command language MUST remain focused on multiplexer control,
terminal display, configuration, and primary-client operations. Agent-scoped
operations that have an agent slash-command equivalent, including provider
logout, MCP listing, project trust decisions, permission profile display or
mutation, command-rule management, approval-bypass control, and approval-mode
selection, MUST NOT be duplicated as terminal commands.

The baseline commands MUST have the following semantics:

| Command | Required behavior |
| --- | --- |
| `new-window` | Create a window in the target session with one pane. It MUST accept an optional name, optional start directory, optional shell command, and a select flag. The new pane MUST follow pane creation shell semantics. |
| `rename-window` | Rename the target window. Repeating the command with the same target and name MUST be idempotent. |
| `kill-window` | Close the target window. If the window contains live pane processes, the command MUST require confirmation or an explicit force flag unless policy permits destructive window closure without prompting. |
| `select-window` | Make the target window active for the invoking primary client. Observers MUST NOT change active window state. |
| `next-window` | Select the next window in the active group, wrapping at the end. |
| `previous-window` | Select the previous window in the active group, wrapping at the beginning. |
| `last-window` | Select the previously active window in the active group for the invoking primary client. |
| `new-group` | Create a window group in the target session with one landing window and one pane. It MUST accept an optional name, optional start directory, optional shell command, and a select flag. The landing pane MUST follow pane creation shell semantics. |
| `rename-group` | Rename the target window group. Repeating the command with the same target and name MUST be idempotent. |
| `kill-group` | Close the target group and every window it owns. If any owned window contains live pane processes, the command MUST require confirmation or an explicit force flag unless policy permits destructive group closure without prompting. The final remaining group MUST NOT be killed by this command; users MUST terminate the session instead. |
| `select-group` | Make the target group active for the invoking primary client and focus that group's active or first window. Observers MUST NOT change active group state. |
| `next-group` | Select the next window group by group display order, wrapping at the end. |
| `previous-group` | Select the previous window group by group display order, wrapping at the beginning. |
| `last-group` | Select the previously active group for the invoking primary client. |
| `split-window` | Split the target pane or active pane vertically or horizontally. Unless an explicit size is provided, the new pane MUST receive half of the spawning pane's columns for a vertical split or half of its rows for a horizontal split, subject to minimum pane sizes. The new pane MUST become active by default. A detached/no-select flag such as `-d` MUST retain focus on the spawning pane. It MUST accept optional start directory, shell command, and select/no-select flags. |
| `kill-pane` | Close the target pane. If the pane primary PID is live, the command MUST require confirmation or an explicit force flag unless policy permits destructive pane closure without prompting. Remaining panes MUST resize according to the layout rules. |
| `select-pane` | Focus the target pane in the active window for the primary client. Observers MUST NOT change focus. |
| `resize-pane` | Resize the target pane by absolute size, relative delta, or directional edge movement. The resulting layout MUST satisfy minimum pane dimensions and MUST propagate the new pty size to affected panes. |
| `rebalance-window` | Reapply the active window's current layout policy to all panes in that window. The command MUST preserve the selected policy, recompute pane sizes and rectangles through the normal layout engine, and propagate any resulting pty size changes. |
| `swap-pane` | Exchange two panes without changing their primary processes, history buffers, or agent identities. |
| `break-pane` | Move the target pane into a new window while preserving its process, buffer, agent harness, and pane identity unless a new identity is required by the implementation's identity model. |
| `join-pane` | Move a source pane into a destination window or split destination pane while preserving its process, buffer, and agent harness. |
| `display-panes` | Display temporary pane identifiers suitable for interactive pane selection. It MUST NOT alter layout. |
| `list-windows` | Return the windows in the active or target group, including stable identity, group-local index, name, active state, pane count, and size. |
| `list-groups` | Return the window groups in the target session, including stable identity, index, name, active state, and owned window count. |
| `choose-group` | Present an interactive group picker with concrete `select-group` actions. |
| `list-panes` | Return panes for the target window or session, including stable identity, index, active state, title, primary PID when available, current size, and agent identity when present. |
| `list-clients` | Return attached clients and pending observers, including role, attach time, terminal size, and approval state. Pending observer details MUST be visible only to the primary client unless the primary explicitly grants broader visibility. |
| `detach-client` | Detach the target client. Detaching the primary client MUST NOT terminate the session unless exit-on-detach behavior is explicitly configured. |
| `attach-session` | Attach the invoking client to a resumable session as primary or pending observer according to the requested role and session authority rules. |
| `list-sessions` | Return resumable sessions, including identity, name, creation time, last attach time, window count, attached client count, and primary availability. |
| `rename-session` | Rename the target session. Repeating the command with the same target and name MUST be idempotent. |
| `kill-session` | Terminate the target session and all panes after confirmation or an explicit force flag unless policy permits destructive session termination without prompting. |
| `help` | Render a command guide for the Mezzanine command set in the interactive command-output overlay, with a column-aligned key binding list section at the bottom. |
| `copy-mode` | Enter pane-local copy mode for scrolling visible and historical terminal content, moving a selection cursor, selecting text, and copying text without sending input to the pane process or opening a command-output view. |
| `copy-selection` | Copy the active copy-mode selection to the active or named paste buffer and to the host clipboard when host clipboard integration is available. |
| `paste-clipboard` | Paste host clipboard text into the active pane, falling back to the most recent paste buffer when host clipboard text is unavailable. It MUST use bracketed paste when bracketed paste is enabled by the pane application. |
| `paste-buffer` | Paste the selected or named paste buffer into the active pane as bracketed paste when bracketed paste is enabled by the pane application; otherwise paste as ordinary terminal input. |
| `create-buffer` | Create a named internal paste buffer. The command MUST create an empty buffer by default, MUST NOT overwrite an existing buffer unless an explicit replace flag is supplied, and MAY set the created buffer as active when requested. |
| `list-buffers` | Return paste buffers with identity, creation time, size, preview text, and origin when known. |
| `choose-buffer` | Present an interactive buffer picker and set the chosen buffer as the active copy/paste buffer or paste it when requested. |
| `delete-buffer` | Delete the selected or named paste buffer. The command MUST fail with `not_found` for an unknown buffer. |
| `show-messages` | Display Mezzanine message log entries, including diagnostics, pending observer requests, pending approvals, and hook failures visible to the primary client. |
| `show-metrics` | Display runtime-service and async-runtime counters and histogram summaries for important measurements, including agent turn lifecycle, provider prompt/cache shape, token usage, shell transaction behavior, event batches, pane output sizes, side-effect queue activity, and current queue depth snapshots, in the primary command-output pager. |
| `list-keys` | Return effective key bindings in column-aligned form, including source configuration layer and command expansion. |
| `list-themes` | Return built-in UI themes and configured custom themes, marking the active theme and whether each entry comes from the built-in registry or configuration. |
| `set-theme` | Set the active UI theme to a built-in or configured theme name, validate the name against the effective theme registry, materialize the selected aliases and color slots, apply the change to the running client immediately, and persist the selected theme into the primary config. |
| `bind-key` | Add or replace a key binding in the live configuration or requested persistence target. It MUST validate that the binding is syntactically representable. |
| `unbind-key` | Remove a key binding from the live configuration or requested persistence target. |
| `show-options` | Return effective options for the requested scope, including source layer and whether each option is live-mutable. |
| `set-option` | Set a live-mutable option. Persisted changes MUST identify the target configuration layer. |
| `source-file` | Parse and apply a configuration file according to configuration trust and precedence rules. Untrusted project files MUST block until trust is decided. |
| `refresh-client` | Redraw the invoking client and recompute client-local display state without changing pane pty sizes unless the invoking client is the primary client and its terminal size changed. |
| `refresh-provider-info` | Refresh cached provider model and quota information for all configured providers. Ordinary pane creation, pane-frame rendering, and model-list displays MUST NOT trigger provider catalog network refreshes; they MUST use cached provider information or configured fallback models. |
| `agent-shell` | Show, hide, or toggle the agent shell for the target pane. Hiding MUST request `/stop` for any in-progress pane-local agent task before the shell is hidden. |
| `auth-login` | Start provider authentication using the configured provider profile and persist credentials only through the auth storage rules. |
| `auth-status` | Show non-secret provider authentication status and selected model profile. |
| `mcp-add` | Add an MCP server configuration after validation and permission checks. Project-scoped MCP additions MUST require project trust. |
| `mcp-remove` | Remove or disable an MCP server configuration from the requested persistence target. |
| `mcp-retry` | Clear current-session MCP blacklist state for an enabled configured server, reconnect or restart its transport, rediscover its tools, and report whether the server became available or remained unavailable. |
| `snapshot-session` | Create a structured session snapshot according to snapshot policy. The command MUST report which process state cannot be snapshotted. |
| `resume-session` | Resume from a saved live session or snapshot. Snapshot resume MUST visibly identify restarted pane primary PIDs. |
| `capture-pane` | Capture visible or historical content from a target pane. History-inclusive capture MUST exclude alternate-screen content. |
| `save-buffer` | Persist a paste buffer to a path or named store subject to file-write permissions. |
| `clear-history` | Clear the target pane's normal history buffer after confirmation unless policy permits without prompting. It MUST NOT affect the current visible screen unless explicitly requested. |
| `search-history` | Search the target pane's normal history and visible screen content, excluding alternate-screen history. |
| `export-history` | Export bounded history for the target pane subject to permission policy and configured redaction behavior. |
| `pipe-pane` | Stream pane output to a configured command or file subject to permission policy. The command MUST make active pipes visible in diagnostics and provide a way to stop them. |
| `mark-pane-ready` | Primary-only command that marks an uncertain target pane as ready for one shell interaction epoch after displaying a warning that Mezzanine could not verify a safe shell boundary. A command invocation that would apply this override MUST require an explicit risk acknowledgement; without that acknowledgement, it MUST display the current readiness state, reason, and risk without mutating pane readiness. It MUST record an audit entry when audit logging is enabled, MUST clear any pending readiness probe for that pane, and MUST be revoked automatically when Mezzanine observes command-start metadata, sends a harness-owned command, sees alternate-screen entry, observes foreground-interactive prompts, observes the pane primary PID changing, observes the pane's environment signature changing, or observes a later readiness probe failure. Observers, agents, and automation clients MUST NOT invoke this command. |
| `list-observers` | List pending, approved, rejected, and revoked observers visible to the primary client. |
| `choose-observer` | Present an interactive selector for observer requests and approved observers, allowing inspect, approve, reject, revoke, or detach actions according to observer state. |
| `approve-observer` | Primary-only command that graduates a pending observer to read-only observer. The observer's visible stream MUST begin no earlier than the approval decision. |
| `reject-observer` | Primary-only command that rejects a pending observer without exposing session state. |
| `revoke-observer` | Primary-only command that removes an approved observer's session view and prevents further session data from being sent to that client. |

Commands that perform the same logical operation as a control endpoint method
MUST enforce the same role, permission, idempotency, target resolution, and
audit requirements as that method. Interactive commands MAY omit explicit
`idempotency_key` input, but Mezzanine MUST still assign an internal
idempotency key to each non-idempotent operation before sending it to the
control endpoint.

Layout-affecting terminal commands, key bindings, control requests, and MAAP
actions MUST converge on the same runtime pane/window creation, resize, and pty
synchronization paths for equivalent operations. In particular, `new-window`,
`split-window`, non-zoom `resize-pane`, and `rebalance-window` MUST use the same
live runtime mutation semantics as `window/create`, `pane/create`, `pane/resize`,
default split key bindings, and MAAP subagent pane creation. After any applied action
that can change focus, pane geometry, window geometry, or visible frame
presentation, the attached foreground renderer MUST schedule a view refresh; if
geometry can change, it MUST invalidate stale differential frame state before
repainting.

Commands that can destroy state, broaden authority, expose credentials, expose
history, execute programs, change external integrations, or send input to pane
processes MUST either be allowed by active policy or obtain primary-client
approval before execution. Read-only observers MUST NOT execute commands that
alter session state.

Interactive foreground clients MUST render terminal command invocations that
produce human-readable display output in a full-window command-output overlay.
The overlay MUST remain visible until the user explicitly dismisses it with
Escape or `q`, and it SHOULD provide paging controls for output longer than the
terminal height. Display output that advertises selectable actions MUST be
mouse-selectable and keyboard-selectable in the overlay. Arrow keys MUST move a
visible active selection across logical actionable rows, and Enter MUST execute
the active row's advertised Mezzanine command through the normal terminal-command
path and then clear or replace the overlay with the command result. If a single
logical action has multiple visible hit ranges, keyboard navigation MUST expose
that action as one selection stop while mouse selection MAY still recognize the
distinct ranges. Selectable
command rows SHOULD render as compact single-line rows or table-like rows when
space permits so users can scan available choices quickly. Rows that advertise
more than one executable choice MUST render those choices as distinct
selectable action cells or chips; keyboard navigation MUST be able to move
between choices on the same row, and mouse selection MUST resolve the clicked
choice without relying on display text scraping. Executable choices MUST be
distinguished from descriptive metadata such as non-command `action=` status
labels. Interactive overlay choices MUST use theme-aware colors that indicate
the relative intent of the choice, including a distinct treatment for
potentially destructive or disruptive actions. Command output intended for
scripts or agent harnesses SHOULD be available in a structured format. Compact
implementation records that use `key=value:key=value` or
`key=value key=value` field syntax MUST be reformatted into human-readable
labels and values before being shown in terminal overlays or pane-local agent
command displays.

### 7.4 Agent Shell Toggle

Mezzanine MUST provide a key binding that enters the agent shell for the active
pane.

The default agent shell toggle MUST be `Ctrl+A a`.

Using the agent shell toggle when no agent shell is visible MUST show the
agent shell associated with the active pane.

When the agent shell is visible, its prompt MUST be presented at the bottom of
the associated pane rather than as a detached full-window prompt surface.

The agent shell prompt MUST be pane-scoped and non-modal. Entering agent mode
for one pane MUST NOT prevent the user from focusing, navigating, resizing, or
interacting with other panes and windows through normal Mezzanine key bindings
and mouse interactions. Panes MAY each have independent visible agent shells and
independent prompt buffers.

While a pane is in agent mode, ordinary user input targeted at that pane MUST be
captured by the pane's agent shell instead of being written directly to the pane
process. Mezzanine mux commands, pane/window navigation, copy-mode controls, and
other global control bindings MUST remain available while the prompt is visible.

Cancelling the pane-local agent prompt through baseline prompt exit keys
including `Ctrl+D` on an empty prompt MUST hide the prompt and return focus to
the pane without requiring an external process kill. A standalone Escape key
MUST NOT hide the agent shell and instead MUST clear any current draft input.
When no pane-local agent task is active, `Ctrl+C` MUST require a second
`Ctrl+C` within three seconds before hiding the prompt. The first `Ctrl+C` MUST
leave the prompt visible and display a pane-local status message that explains
the confirmation requirement. While a pane-local agent task is active,
`Ctrl+C` MUST still request interruption immediately.

Using the agent shell toggle while the agent shell is visible MUST request
`/stop` for any in-progress pane-local agent task before hiding the prompt and
returning focus to the pane.
Entering or exiting agent mode MUST move the pane's used visible terminal rows
into the pane's retained history, clear the live viewport, and move the cursor
to the home position as if the user had pressed `Ctrl+L`; it MUST NOT erase
pane logs, retained scrollback, or history buffer content.
When agent mode is shown for a pane with a live shell, Mezzanine MUST enter a
child instance of the resolved pane shell in the pane's current working
directory before sending agent-owned shell commands. Agent command side effects
such as prompt changes, aliases, shell options, and environment mutations MUST
remain scoped to that child shell unless the user explicitly applies them
outside agent mode.
The agent-mode child shell and every Mezzanine-owned non-stateful action shell
MUST inherit the pane environment except for variables that can trigger shell
startup files, prompt hooks, editor/pager prompts, or other interactive
side effects. Mezzanine MUST suppress common shell startup paths for supported
shells, including Bash profile/rc files, Zsh rc files, Fish configuration
loading, and POSIX `ENV`/Bash `BASH_ENV`-style non-interactive startup hooks.
Prompt-related inherited variables such as `PROMPT_COMMAND`, `PS1`, `PROMPT`,
and right-prompt equivalents MUST be removed or replaced with inert values for
Mezzanine-owned shells. These suppressions MUST be scoped to agent-owned shells
and MUST NOT mutate the user's parent pane shell environment.

When agent mode is hidden through the toggle, an agent slash-command exit,
keyboard prompt-exit bindings, or a control API hide request, Mezzanine MUST
first submit the equivalent of `/stop` for any running pane-local agent task.
Until that stop reaches a terminal state, ordinary user input targeted at the
pane MUST be blocked and Mezzanine MUST surface a warning that the agent shell
is stopping. Mezzanine MUST keep the agent-mode child shell alive while a
running agent turn or agent-owned shell transaction still needs it, then leave
that child shell before returning the pane to ordinary user input. Attached
primary clients MUST invalidate any differential render cache and repaint the
current terminal view after the prompt is removed so cursor placement and stale
agent prompt rows cannot survive the mode boundary.

Further entrances to the agent shell from the same pane MUST resume the same
agent session unless the user explicitly starts a new session.

Each pane-local agent session MUST have a stable UUID identity. Starting a new
conversation MUST allocate a fresh UUID, and resuming or forking a saved
conversation MUST bind the active pane to the selected or newly forked
conversation identity.

Mezzanine MUST checkpoint each active pane-local agent session's conversation
binding and pane-scoped agent preferences as the session changes. This
checkpoint MUST be keyed by the owning Mezzanine session identity and pane
identity. Restarting or creating a different Mezzanine session MUST NOT
automatically bind its panes to checkpointed conversations from another session,
and newly created panes MUST receive fresh agent session identities until the
user explicitly resumes or forks a saved conversation.

When an agent task is running, agent shell input MUST support interactive task
management: non-slash prompt input submitted while a pane-local turn is active
MUST be injected into the current turn as mid-turn steering. Slash commands
MUST be rejected with a clear diagnostic or handled according to their
documented runtime rule. When Mezzanine accepts non-slash input as mid-turn steering, it
MUST wrap the input in model-facing context that identifies it as user input
submitted during the active turn, instructs the agent to incorporate it into
the current task from the next action boundary forward, and gives the newer
user instruction precedence over earlier conflicting instructions. If a
provider request is already in flight, Mezzanine MUST retain the steering input
and deliver it in the next provider request for the same turn instead of
starting an unrelated turn or silently discarding the input.

Agent prompt submissions MUST be appended to the pane's normal terminal buffer
as user-visible, copyable text before the resulting turn is queued or started.
Non-command assistant responses, final responses, concise user-facing progress
statuses, and internal or provider errors MUST also be appended to the same
normal terminal buffer, interleaved with pane process output in observation
order. By default, shell commands selected by the model MUST be rendered into
the pane terminal buffer as bounded command previews before dispatch, while
their resulting PTY output MUST be captured for audit, transcript, and
follow-up model context but MUST NOT be rendered into the pane terminal buffer.
Semantic file, directory, search, and URL actions MUST render a single
human-readable execution line in the pane buffer in normal mode. That line MUST
identify the action kind and target, MUST use the configured agent transcript
theme colors, and MUST NOT include generated shell command text, result payloads,
file contents, URL bodies, shell prompts, or wrapper traffic. Successful
semantic actions that mutate files or paths MUST additionally render a cleaned,
bounded, diff-shaped change preview in normal mode after the hidden shell
transaction completes. File content change previews MUST use unified-diff
markers, include old and new file labels and line numbers when available, and
color additions, deletions, headers, and context with the configured agent
transcript theme colors. When a changed file path resolves to a known source
syntax, Mezzanine SHOULD additionally color source tokens using the active
theme's `syntax_*` slots, so syntax highlighting tracks built-in and custom UI
themes rather than a fixed editor palette. Directory and path-only changes MUST
render a bounded path delta using the same addition/deletion conventions.
Elevated log modes MAY render cleaned, bounded result previews for
non-mutating semantic actions.
Result and change previews MUST be generated by Mezzanine's semantic action
lowering or runtime, not by the model, and MUST NOT expose shell prompts or
wrapper traffic.
If a failed action result is being fed back to the model for automatic recovery,
normal logging MAY omit the action's captured stderr/stdout detail. If recovery
is not attempted or the recovery budget is exhausted and the turn is ending
failed, normal logging MUST render a bounded final diagnostic for each failed
action, including the compact action identity, failure status, and any captured
terminal observation preview that explains the failure. This final diagnostic
MUST remove Mezzanine-owned wrapper echo, prompt repaint text, and terminal
control sequences before applying its bounded line and byte limits so harness
traffic cannot displace the actionable failure message.
Patch hunk mismatch diagnostics returned to the model MUST include bounded
human-readable context and machine-readable recovery hints, including an
affected path, failure code, recommended next step, whether retry without fresh
context is allowed, and a suggested bounded read range when one can be
identified.
When several sibling actions fail because the same runtime loop guard
suppressed a broad action fan-out, normal logging MUST aggregate those failures
into one bounded diagnostic line instead of repeating one line per sibling
action. Trace-level structured records MAY retain per-action failure metadata.
The default user-facing flow for model-authored `shell_command` actions MUST
show both the MAAP shell action's concise summary and the bounded command
preview, then rely on the model to summarize or operate on captured command
output according to the user's request. Captured
terminal observations for follow-up model context MUST remove Mezzanine-owned
wrapper echo, terminal control sequences, and shell prompt repaint text before
applying their bounded byte limit, so ordinary command output is not displaced
by PS1 styling or harness setup traffic. While a pane has a running agent turn,
the underlying pane-shell byte stream MUST be blocked from the user-facing pane
renderer in `normal` and `debug` log levels, including shell prompt redraws and
PS1 styling emitted between model response iterations. After a hidden
Mezzanine-owned shell transaction settles, the renderer MUST retain suppression
briefly so delayed prompt repaint bytes cannot leak after transaction
bookkeeping removes the live shell transaction. Agent logging MUST be
controlled by a single pane-local log level
configured through `/log-level`. `normal` MUST be the default and MUST show
user-facing prompts, assistant text, concise progress, mode changes, approval
requests, errors, and non-duplicative agent thinking or rationale lines
so users can see that the agent is actively reasoning about the request. When an
action batch includes a model-authored rationale, Mezzanine MUST render the
non-empty value once as normal assistant transcript output prefixed as
`thinking: ` in normal mode unless the same text has already been rendered
immediately nearby. Mezzanine MUST NOT render duplicate thinking lines with
both prefixed and unprefixed forms. A rationale attached to a `say` action MUST
NOT be rendered as a separate thinking/comment line because the `say` text is
already the user-visible assistant output for that action. Model-facing action
guidance SHOULD direct action-batch intent and justification into the
model-authored batch rationale rather than a redundant progress `say` action
when executable actions in the same batch already make the progress visible.
Rendered thinking/rationale lines and non-empty model-authored batch thoughts
MUST also be retained as assistant transcript content and future model-facing
assistant context so continuation requests can preserve the model's working
thread. Batch thoughts MUST NOT be rendered in normal mode, but MUST be
eligible for `verbose`, `debug`, and `trace` logging as `thinking: ` text.
While a pane has an active agent turn, the visible pane log tail MUST include a
live foreground-only grayscale footer in the form `<state> (<duration> • esc
to interrupt)`, where `<state>` is a lowercase human-readable active turn
state such as `running`, `queued`, `waiting`, or `waiting approval`, and
`<duration>` is a human-readable elapsed time such as `5m 40s`. The animated
grayscale treatment MUST apply only to the state label; the parenthesized time
and interrupt hint MUST render as muted grey text. The animation SHOULD use the
same scan-band timing, width, and phase rules as the active pane status pill
while remaining foreground-only grayscale. When the turn completes, the live
footer MUST be replaced by `Worked for <duration>`; when the turn fails or is
interrupted, it MUST be replaced by `Failed after <duration>`.
`verbose` MUST additionally show low-level provider lifecycle/status messages
plus agent-triggered command PTY output, but MUST NOT expose raw
Mezzanine-owned shell wrapper traffic or complete MAAP payloads.
`debug` MUST show the diagnostic categories that trace shows, including MAAP
request/response/action-result structure and state transitions, but MUST redact
the full shell view: prompt strings, raw provider response text, and full
command output previews. Debug MAAP action and action-result objects MUST keep
their exact shell `command` fields visible so state-machine and dispatch bugs
can be diagnosed without enabling the full shell view. `trace` MUST show
everything debug shows plus the full shell view. Raw
Mezzanine-owned shell transaction wrapper echo, including marker variable
assignment, here-doc delimiters, status capture, cleanup commands, and their
output, MUST be hidden from the pane buffer by default and MUST be
user-enableable only through trace.
Concise user-facing progress statuses, such as queued work, active work,
readiness checks, blocked approval, provider errors, and completion without a
user-facing response, MUST remain visible by default so the agent never appears
silent. These agent-authored lines, their gutter prefix characters, and their
speaker labels MUST be visually distinct through named theme colors while
preserving their plain-text content for copy mode, history export, and terminal
observation. Agent-mode log rows and rendered transcript presentation rows MUST
wrap at the smaller of the pane terminal width or 120 display cells before they
are persisted or replayed, and hard splits MUST occur only at terminal grapheme
boundaries when an unbroken token exceeds that limit. When agent-authored
transcript text soft-wraps in the pane, Mezzanine MUST repeat the display-only
agent gutter prefix on continuation rows, and resize reflow MUST preserve that
visual gutter without treating it as agent-authored content for copy or
observation semantics. Markdown transcript presentation MUST preserve the
relevant speaker, quote, list, or code indentation on continuation rows.
Non-table markdown rows MUST wrap at the nearest whitespace boundary before the
presentation limit; if no whitespace boundary exists in the overflowing
segment, Mezzanine SHOULD leave the segment intact and rely on normal terminal
soft wrapping instead of inserting a hard split.
Markdown table rows MUST preserve their table layout until they exceed the
pane terminal width; the 120-cell cap MUST NOT force table rows to wrap on
wider terminals.
When the agent runs a shell command, Mezzanine MUST render the MAAP shell
action's concise summary and exact command preview before dispatch. The exact
command preview MUST be visible in normal mode, MUST account for the pane's
terminal width capped at 120 display cells, MUST show at most ten terminal
display lines, and when the command would occupy more than ten display lines,
MUST indent continuation rows under the `$ ` prompt and prefix the final
preview line with the total display-line count in square brackets. Command
preview rows MUST wrap at the nearest whitespace boundary before the
presentation limit; if no whitespace boundary exists in the overflowing
segment, Mezzanine SHOULD leave the segment intact and rely on normal terminal
soft wrapping instead of inserting a hard split. Full
Mezzanine-owned wrapper commands and wrapper output MUST remain trace-only.
While a model-authored shell command is running and raw shell output is hidden,
Mezzanine SHOULD render the latest non-empty cleaned command-output line as a
single transient row immediately below the command preview. New command-output
lines SHOULD replace that row in place instead of appending transcript history,
and the next durable agent transcript row SHOULD clear or overwrite it. If a
PTY read contains both a Mezzanine transaction-end marker and the parent shell's
next prompt repaint, prompt bytes after the marker MUST NOT be considered
command output for this transient row. The transient row SHOULD use the same
muted foreground treatment as agent thinking or status text.
Mezzanine MUST NOT impose a total per-turn automatic shell dispatch count cap,
because broad but finite inspection batches are ordinary agent work. Mezzanine
MUST still prevent provably duplicate file mutations from replaying after the
exact same generated mutation command has already succeeded in the current
turn. Such duplicate-success suppression MUST be reported as a successful
idempotent action result with structured metadata so the model can continue
without reapplying an already-landed edit.
Shell readiness metadata observed around Mezzanine-owned harness transactions
MUST be treated as order-sensitive hints. Stale passive prompt or command-start
markers from a completed harness transaction MUST NOT overwrite the readiness
state of a newly dispatched harness command, and a pending shell action that was
blocked by transient pane busy state MUST be requeued when later prompt metadata
shows the pane has returned to a prompt candidate. The runtime MUST also detect
a running turn that has a pending shell action but no running provider task, no
live shell transaction, and no live readiness probe. If the pane is already
ready, unknown, prompt-candidate, or degraded, the runtime MUST requeue the
stored shell action for readiness handling. If the pane still says busy or
interactive-blocked but host process metadata shows the primary pane shell is
again the foreground process, the runtime MUST treat the blocking state as
stale, move the pane back to prompt-candidate, and requeue the stored shell
action. This recovery MUST avoid spamming visible logs while preserving
trace-level state-transition evidence.
When an async provider worker claims a provider task, the runtime MUST record a
finite claim lease before removing the task from the pending-provider queue. A
claimed provider worker that does not report completion or failure before its
lease expires MUST cause the runtime to fail the affected turn with pane-local
diagnostics, clear the claim, and keep the daemon available for other panes and
agents. The claim lease MUST be longer than the provider transport timeout so
valid long-running model requests can report their own timeout or provider
failure before the runtime watchdog can fail the turn. The runtime MUST ignore
stale claim-timeout generations after a later claim, retry, completion,
failure, or user interruption has already settled the worker's ownership.
At all times, a running agent turn MUST have at least one observable runtime
progress path: pending provider dispatch, claimed provider worker, live shell
transaction or readiness probe, focused-shell hook continuation, approval/user
input wait, joined subagent dependency, or stored execution ready for provider
continuation. If runtime cleanup discovers a running turn with none of those
paths, Mezzanine MUST fail that turn with a copyable pane-local diagnostic
rather than leaving it in a permanent running state.

Provider selection, credential, transport, and provider response errors during
agent prompt execution MUST fail the affected turn and render pane-local,
copyable failure details in the pane terminal buffer. Such errors MUST NOT
terminate the session daemon, detach the primary client, or prevent other panes
and agents from continuing.

Runtime command errors encountered while the pane is in agent mode, including
invalid arguments, invalid state, conflicts, not-found cases, forbidden
operations, configuration failures, I/O failures, and unimplemented operations,
MUST be rendered as pane-local agent error output instead of escaping as client
or daemon crashes.

The pane frame SHOULD display the active agent model, reasoning profile,
approval mode, context usage, and status while a pane is in agent mode. The
default pane-frame agent model, reasoning, approval, context usage, and status
fields SHOULD render as visually separate themed pills, and these default pills
SHOULD render only the item values without field-name prefixes. The default
context-usage pill SHOULD display `agent.context_usage` between the
`policy.mode` approval pill and the `agent.status` pill. The default
context-usage pill SHOULD use its own
theme-relative neutral/warning/exhausted scale rather than reusing the current
agent state colors for normal and warning percentages. The default agent status
pill SHOULD display the current `agent.status` value and MUST use that value
for themed color and animation decisions. Root pane agents MUST use `manager`
as their human-readable display name, and the default pane frame SHOULD omit
agent name pills because root identity is assumed and subagent names are
already represented by pane titles. Explicit pane-frame templates MAY still
render `agent.name` when a user or agent asks for that field.
When a root turn is blocked only because it is waiting for joined subagents to
complete, the pane-frame status value SHOULD be `waiting`, the status pill
SHOULD display `waiting`, and the pill SHOULD use the same themed color and
subtle gradient scan as running or queued work. The default agent model,
reasoning, routing, context usage, and status pills SHOULD include one
visible padding cell on each side of their contained item text. The default
routing pill MUST render the constant text `route` and signal its
enabled or disabled state through pill color alone. The default model and
reasoning pills MUST be mouse-selectable controls when the terminal
mouse protocol is active: clicking the model pill MUST present a drop-down with
configured model presets first and concrete provider/model entries after them,
selecting a preset MUST apply that preset's pane-local model and auto-sizing
settings, and the model pill MUST continue to display the active concrete model
after a preset selection. Clicking the reasoning pill MUST present a drop-down
of reasoning levels available for the active model, hovering an item SHOULD
highlight it, and clicking an item MUST apply it to the pane's agent model
profile. The same drop-down MUST support keyboard navigation:
Up/Left MUST move to the previous item, Down/Right MUST move to the next item,
Home/End SHOULD move to the first and last items when supported, Enter MUST
apply the active item, and Escape MUST close without applying a value. The
drop-down MUST remain open after the opening click's release event, MUST close
without applying a value when Escape is pressed or the user clicks outside the
selector, and MUST NOT forward selector navigation or cancelling Escape keys to
the pane process. The status pill SHOULD use distinct theme colors for
idle/completed, running/queued, blocked, and failed/interrupted states, and the
running/queued status pill SHOULD animate a subtle gradient scan behind its
text. The running/queued scan SHOULD be derived from the active theme's running
status color using a harmonious neighboring-hue range, so the animation remains
theme-relative while avoiding abrupt reuse of unrelated pill accents.

### 7.5 Readline Semantics

The agent shell MUST obey readline-style input semantics for line editing,
history navigation, cursor movement, deletion, and submission.

The primary Mezzanine command prompt entered from the terminal UI MUST obey
readline-style input semantics for line editing, history navigation, cursor
movement, deletion, and submission. Submitted command-prompt entries MUST be
retained across command-prompt openings using the same bounded retention and
lookup semantics as agent prompt history, including Up/Down navigation and
fzf-style `Ctrl+R` incremental reverse search. While incremental reverse
search is active, the prompt MUST render in the form
`(reverse-i-search'<substring>'): <item>`, where `<substring>` is the typed
search query and `<item>` is the nearest history entry that matches the query
as a case-insensitive substring or ordered-character fuzzy match. Repeated
`Ctrl+R` MUST search backward toward older matching entries, and the prompt MUST
provide a forward search binding for cycling toward newer matching entries.
Enter and Right arrow MUST accept the displayed match into the editable buffer
without submitting it. Escape, `Ctrl+C`, Left arrow, Up arrow, and Down arrow
MUST cancel reverse search and restore the draft that was present when reverse
search started. `Ctrl+L` MUST scroll the active pane's
used visible rows into retained history and clear the live viewport without
closing the prompt. In the pane-local agent shell, standalone Escape MUST
clear the current draft without closing the prompt or forwarding bytes to the
pane. In the primary command prompt, standalone Escape MUST close the prompt
without forwarding bytes to the pane. Command-prompt history MUST be persisted
in a bounded shared command-prompt history file under the agent-session parent
directory.
Command-prompt history MUST remain separate from agent prompt history so each
prompt searches only entries from its own interaction surface.

Readline-style prompt navigation MUST include character movement, word
movement, current-row start/end movement, full-buffer start/end movement,
forward delete, backward delete, word delete, kill-to-row-start, and
kill-to-row-end for both primary command prompts and agent prompts. Prompt rows
include both explicit newline-separated rows and visible rows produced by
prompt wrapping. In multiline prompt input, Up/Down arrow keys MUST move
between prompt rows before falling back to history navigation at the top or
bottom of the editable buffer.

Readline-style prompt buffers MUST collapse large pasted text blocks to a
human-readable byte-count placeholder such as `[Pasted 1.2 KiB]` instead of
rendering the pasted payload inline. The placeholder MUST move as one logical
character for cursor navigation, Backspace, Delete, and related readline
editing commands, and deleting the placeholder MUST remove the complete pasted
payload from the editable buffer. Prompt submission MUST send the exact pasted
payload to the command or agent backend while user-visible prompt echo MAY use
the collapsed placeholder form. Bracketed-paste delimiters MUST be decoded by
the prompt surface so embedded newlines in the pasted payload do not submit the
prompt unless the user presses the normal submission key after the paste.

The agent prompt MUST support Up/Down arrow navigation through submitted prompt
history and fzf-style `Ctrl+R` incremental reverse search through that history
using the same `(reverse-i-search'<substring>'): <item>` prompt format and
case-insensitive substring or ordered-character matching as the primary command
prompt, including the same accept and cancel bindings.
`Ctrl+L`
MUST scroll the active pane's used visible rows into retained history and clear
the live viewport without closing the prompt. Prompt history MUST be shared
across all agent sessions in one bounded history file and MUST be
loaded whenever an agent prompt is shown or `/resume` binds a saved conversation
into the pane.

The agent shell MUST support inserting literal newlines into the current prompt
with `Ctrl+J`; Enter MUST remain the normal prompt submission key.

When the user submits a non-empty agent prompt, the visible prompt input MUST be
cleared in the same terminal update that accepts the submission, before any
provider response or later agent state transition is required. Additional
prompts submitted while an earlier turn is still running MUST be accepted into
the pane's agent turn sequence without corrupting the active turn state.

The configuration shell MUST obey readline-style input semantics for line
editing, history navigation, cursor movement, deletion, and submission.

Implementations MAY extend readline-style behavior with additional bindings,
but such extensions MUST NOT remove the expected baseline editing behavior.

### 7.6 Copy Mode and Paste Buffers

Mezzanine MUST provide a keyboard-driven copy mode for selecting and copying
pane history.

Copy mode MUST allow keyboard navigation through pane history.

Copy mode MAY support additional vi-style and emacs-style key tables when
configured.

Copy mode MUST support scrolling the terminal buffer, beginning a selection,
cancelling a selection, copying a selection to a paste buffer, copying and
exiting, searching forward, searching backward, repeating search, jumping to
the top and bottom of history, moving by word, paging up, paging down, and
exiting copy mode.

The default keyboard copy-mode interaction MUST render a visible cursor over
the selected pane history in the pane itself, not in a dedicated command-output
view. When copy mode is entered, that cursor MUST start at the active pane's
live terminal cursor position as rendered immediately before copy mode took
over input. The rendered terminal cursor MUST move with the copy-mode cursor;
Mezzanine MUST NOT represent keyboard copy-mode cursor movement only as a styled
or highlighted text cell. While default copy mode is active, Up, Down, Left,
Right, PageUp, PageDown, Home, End, Space, Escape, Ctrl-Up, Ctrl-Down, and
Ctrl-C MUST be handled by copy mode and MUST NOT be forwarded to the pane
process. Other key input MUST be consumed without affecting the pane process.
Left at the beginning of a line MUST move to the end of the previous line when
one exists, and Right at the end of a line MUST move to the beginning of the
following line when one exists. PageUp MUST move one viewport page up, except
that it MUST jump to the beginning of the copy buffer when less than one
viewport page remains above the current viewport. PageDown MUST move one
viewport page down, except that it MUST jump to the end of the copy buffer when
less than one viewport page remains below the current viewport. Home MUST move
the copy cursor to the beginning of the current line, and End MUST move it to
the end of the current line. Ctrl-Up MUST move the copy cursor five lines up,
and Ctrl-Down MUST move the copy cursor five lines down. Ctrl-Home MUST move
the cursor to the beginning of the copy buffer, and Ctrl-End MUST move it to the
end of the copy buffer. Modified horizontal movement, including Ctrl-Left,
Ctrl-Right, Alt-Left, and Alt-Right, MUST move by word-like segments rather than
single cells. Pressing Space with no active keyboard selection MUST begin a
selection at the cursor, and pressing Space again while a selection is active
MUST finish the selection, copy the selected text to the active paste buffer or
to the `clipboard` buffer when no buffer is active, copy to the host clipboard
when available, clear the keyboard selection, and remain in copy mode. Keyboard
copy mode MUST NOT exit when scrolling or moving down reaches the live bottom
of the buffer. Pressing Escape MUST exit keyboard copy mode. Pressing Ctrl-C
MUST be consumed without exiting copy mode or forwarding input to the pane
process.
Explicit `copy-selection` commands MAY copy the active selection to the active
or named paste buffer without changing this default Space-toggle workflow.

Mezzanine MUST provide commands to capture pane contents from the visible
screen, from the bounded history buffer, or from a configured line range.

The `capture-pane` command MUST support targeting a pane, choosing visible-only
or history-inclusive capture, and writing captured text to command output or a
paste buffer.

The `export-history` command MUST support writing selected pane history to a
user-specified path or standard output according to policy.

The `search-history` command MUST search the bounded history buffer of a target
pane and return matching lines or enter copy mode at the selected match.

The `clear-history` command MUST clear a target pane's history buffer after
confirmation or when policy permits noninteractive clearing.

Mezzanine MUST maintain paste buffers.

Mezzanine MUST support creating paste buffers, listing paste buffers, choosing a
paste buffer, pasting a selected buffer, deleting a buffer, and clearing
buffers. Creating a buffer MUST validate the name, create an empty internal
buffer by default, preserve existing buffer contents unless replacement is
explicitly requested, and report whether creation or replacement occurred.
Choosing a paste buffer MUST make it the active buffer for both copy and paste
operations until the active buffer is changed, deleted, or cleared. Choosing a
named buffer that does not exist yet MUST create an empty internal buffer with
that name so it can be used as the target for the next copy-mode selection.

Clipboard integration MAY be supported. When supported, text-selection copies
MUST write to the host clipboard on a best-effort basis while also updating the
internal paste buffer. Clipboard failures MUST NOT make the copy operation fail.

When copy-mode or mouse text selection copies Mezzanine-rendered agent
transcript lines, the copied text MUST omit Mezzanine's visual gutter prefix
characters. Assistant response text copied from agent mode MUST also remove the
display-only `agent>` label and continuation padding that Mezzanine adds for
pane readability, while preserving content indentation beyond that visual
padding.

Mouse text selection MUST interact with copy mode and paste buffers according
to the configured mouse policy. Double-clicking pane text MUST select and copy
the surrounding readline-style word segment to the `mouse` paste buffer and to
the host clipboard on a best-effort basis.

### 7.7 Messages, Activity, and Bell Notifications

Mezzanine MUST maintain a bounded message log for multiplexer notices, command
errors, agent attention requests, and significant lifecycle events.

Mezzanine MUST provide a command to show recent messages.

Recoverable foreground runtime errors MUST NOT terminate the attached `mez`
client. They MUST be rendered as a transient one-line overlay on the primary
client's window status bar. The action that causes a recoverable foreground
runtime error MUST be consumed without partial side effects after the error is
recorded. The next key, mouse event, or mux action from the primary client MUST
clear the transient error overlay as a presentational dismissal and MUST NOT be
forwarded to the active pane or mux command path. Subsequent input after the
overlay is dismissed MUST dispatch normally.

Mezzanine MUST track pane activity and bell events.

Mezzanine SHOULD support configurable options for visual bell, audible bell,
activity monitoring, silence monitoring, and window attention markers.

When a background pane emits a bell, activity event, silence event, agent
approval request, or agent completion event, Mezzanine MUST make that state
visible through the window frame, pane frame, message log, or command prompt.

When a read-only observer attach request is pending, Mezzanine MUST make that
state visible to the primary client through the same notification surfaces.
Read-only observers MUST NOT receive pending observer request details unless
the primary client approves them.

## 8. Configuration

Mezzanine MUST load user configuration from `~/.config/mezzanine`.

Mezzanine MUST support TOML, YAML, and JSON configuration formats.

Configuration parsing MUST be deterministic. If multiple configuration files
define the same setting, Mezzanine MUST apply a documented precedence rule.

Configuration MUST include key bindings, frame settings, history buffer size
and rotation count,
shell settings, agent settings, permission settings, model provider settings,
message passing settings, control endpoint settings, MCP server settings,
terminal compatibility settings, instruction discovery settings, hook settings,
snapshot settings, audit settings, and extension settings.

Mezzanine MUST support live configuration mutation through the configuration
shell.

Mezzanine MUST persist live configuration changes when the user requests
persistence.

Mezzanine MUST validate configuration before applying it.

Invalid configuration MUST produce actionable diagnostics and MUST NOT partially
apply in a way that leaves the session in an undefined state.
Persistent configuration replacement MUST be written atomically: Mezzanine MUST
write a complete validated replacement to a sibling temporary file and replace
the destination with a rename or equivalent same-filesystem operation. A failed
or interrupted replacement MUST NOT leave the primary configuration file
truncated or partially rewritten.

Mezzanine MUST support project-level configuration overlays discovered from the
current working directory and its parent directories. Overlay precedence
relative to user configuration MUST be deterministic and visible through
configuration diagnostics.
Long-running sessions MUST refresh project overlay discovery from the active
pane's current working directory before building agent context, accepting an
agent prompt, or showing pane-scoped project skills. The daemon startup
directory MUST NOT be the only source used for project overlay discovery.

### 8.1 Configuration Files

The primary user configuration directory MUST be `~/.config/mezzanine`.

Exactly one primary user configuration file MUST be selected for a running
Mezzanine instance. The supported primary filenames are:

- `config.toml`
- `config.yaml`
- `config.yml`
- `config.json`

If no primary user configuration directory exists, `mez` MUST create
`~/.config/mezzanine` before starting a session. The created directory MUST be
private to the current user and SHOULD use Unix mode `0700`.

If no primary user configuration file exists, `mez` MUST create a default
primary configuration file before starting a session. The default file MUST
represent the built-in defaults in a supported structured format. The default
filename SHOULD be `config.toml`. The created file SHOULD use Unix mode `0600`
or the nearest host equivalent. If directory or file creation fails, Mezzanine
MUST fail with an actionable diagnostic.

If more than one supported primary user configuration file exists and no
explicit primary file was selected, Mezzanine MUST fail configuration loading
with an actionable diagnostic rather than merging ambiguous primary files.

Mezzanine MUST support project overlay configuration files in TOML, YAML, and
JSON. The default discovered project overlay paths for a directory MUST be:

- `.mezzanine/config.toml`
- `.mezzanine/config.yaml`
- `.mezzanine/config.yml`
- `.mezzanine/config.json`

At most one project overlay file MUST be selected from a single directory. If
more than one supported overlay file exists in the same directory, Mezzanine
MUST fail overlay loading for that directory unless the user has configured an
explicit format precedence.

Configuration layers MUST be applied in this order, from lowest precedence to
highest precedence:

1. Built-in defaults.
2. The primary user configuration file.
3. The project root overlay, when present.
4. Parent directory overlays from the project root toward the pane current
   working directory.
5. The current directory overlay, when present.
6. Live session overrides made through the command prompt, configuration shell,
   or control endpoint.

Later layers MUST override earlier layers for scalar values. Lists MUST replace
earlier lists unless the schema for that setting explicitly defines append or
merge behavior. Maps MUST merge recursively unless the schema for that setting
explicitly defines replacement behavior.

Project overlay configuration MUST NOT contain authentication secrets.

Project overlay configuration MUST NOT be applied until the project is trusted.
When an untrusted project overlay is discovered and a primary client is
attached, Mezzanine MUST prompt the primary client to trust or reject the
project before applying the overlay. The prompt MUST show the project root,
discovered overlay files, and any overlay capabilities that can execute code or
expand authority, including hooks, MCP servers, command rules, provider
settings, and permission settings.

If the primary client trusts the project, Mezzanine MUST persist that trust
decision in the primary user configuration or a user-private trust database
under `~/.config/mezzanine`. A project trust record MUST include the canonical
project root path, the `.git` marker path when present, the time trusted, the
configuration schema version, and the Mezzanine trust-policy version. When
available, it SHOULD include VCS metadata such as repository identity or remote
URL for diagnostics.

The canonical project root path is the trust boundary. Once the primary client
trusts a project root, configuration overlay files discovered at that root or
under that root in future sessions MUST be treated as trusted by virtue of the
trusted root unless the trust record is revoked, the configured trust policy
requires renewed approval, or the resolved path escapes the trusted root
through a symlink or equivalent filesystem indirection. The overlay file paths
shown at approval time are diagnostic context only and MUST NOT be the only
trusted paths. Future sessions MUST reload the trust record before applying
project overlays.

If the primary client rejects trust, Mezzanine MUST ignore the project overlay
and continue with lower-precedence configuration layers. Ignored overlays MUST
be visible in configuration diagnostics.

If an untrusted project overlay is discovered while no primary client is
attached, Mezzanine MUST put that project overlay into a pending-trust state.
Agent turns, agent prompts, hooks, MCP configuration, command rules, provider
settings, permission settings, and other behavior that would depend on that
project overlay MUST block until a primary client attaches and approves or
rejects trust. Pane primary processes MAY continue running with already-applied
configuration while the trust decision is pending, but Mezzanine MUST NOT
silently run agent work with a lower-precedence substitute configuration when
the blocked work is scoped to the pending project.

Project trust decisions MUST be available through the configuration shell,
command language, and control endpoint. The user MUST be able to list pending,
trusted, rejected, and revoked project trust records; inspect the discovered
overlay files and capability expansion summary for a project root; trust a
project root; reject a pending project root; and revoke a previously trusted
project root. Trust and rejection decisions MUST require the primary client.

Project overlay configuration under a trusted project root MUST be treated as
trusted by virtue of that root, including overlay settings that can expand
authority. A user-selected trust policy MAY require renewed approval for
authority expansion, schema-version changes, trust-policy-version changes, or
other configured risk signals, but such renewed approval is an explicit policy
choice rather than the baseline behavior.

For configuration overlay discovery, the project root MUST be the nearest
ancestor of the pane current working directory that contains a `.git` directory
or `.git` file. If no `.git` marker exists, the pane current working directory
MUST be treated as the project root.

### 8.2 Baseline Configuration Schema

The top-level configuration object MUST support the following keys:

- `version`
- `session`
- `terminal`
- `shell`
- `keys`
- `layout`
- `frames`
- `theme`
- `themes`
- `history`
- `agents`
- `model_profiles`
- `model_presets`
- `permissions`
- `providers`
- `subagents`
- `message_protocol`
- `control`
- `mcp_servers`
- `auth`
- `instructions`
- `hooks`
- `snapshots`
- `audit`
- `extensions`

The `version` key MUST identify the configuration schema version. Mezzanine
schema version 7 is the current configuration schema version for this
specification revision. Implementations MUST reject a configuration file whose
declared schema version is greater than the newest schema version understood by
the binary.

When a primary user configuration file declares an older supported schema
version, Mezzanine MUST migrate that primary file on launch before validating or
composing runtime configuration layers. Migrations MUST advance one schema
version at a time until the file reaches the current schema version, MUST add
defaults for current settings that were absent in the older file, MUST rewrite
renamed or moved settings to their canonical current paths, and MUST remove
settings that no longer exist in the current schema. Project overlay files are
not durable user-primary configuration and MUST instead be validated against the
current schema after the primary file has been migrated.

The `session` table MUST support `detach_behavior`, `reattach_behavior`,
`empty_session_behavior`, and `restore_strategy`.

Mezzanine schema version 2 MUST NOT support `session.default_command`. The
version 1 to version 2 primary-config migration MUST remove
`session.default_command`. If a current-schema configuration layer still
contains `session.default_command`, Mezzanine MUST reject the configuration with
an actionable diagnostic. Users who want a pane to run a command instead of an
interactive shell MUST provide that command explicitly through pane or window
creation.

The `terminal` table MUST support `profile`, `term`, `true_color`, `mouse`,
`bracketed_paste`, `clipboard`, `clipboard_copy_command`,
`clipboard_paste_command`, `alternate_screen`, `focus_events`, `nested_multiplexer`,
`passthrough`, `reduced_motion`, `resize_debounce_ms`,
`render_rate_limit_fps`, `cursor_style`, `cursor_blink`, and
`cursor_blink_interval_ms`.

`terminal.reduced_motion` MUST default to false. When true, optional
frame/status animations MUST render as static UI while preserving the same
semantic status text and color category.

`terminal.render_rate_limit_fps` MUST default to 5. When nonzero, attached
foreground clients SHOULD coalesce bursty render invalidations so ordinary
output rendering is emitted no more frequently than the configured frame rate
per client, while still delivering one trailing frame after a burst. A value of
0 MUST disable render rate limiting. Initial attach frames, terminal cleanup,
unsuperseded pending partial-output flushes, and user-input handling MUST NOT
be delayed by this limit. When a newer render is waiting behind the rate gate,
stale pending bytes from an older incomplete frame SHOULD be superseded by the
newer frame rather than flushed eagerly.

The version 1 to version 2 primary-config migration MUST treat
`terminal.nested_muxxer` as a migration alias for
`terminal.nested_multiplexer`. When both keys are present, the canonical
`terminal.nested_multiplexer` setting MUST take precedence and the alias MUST
be removed before layer composition.

`terminal.clipboard_copy_command` and `terminal.clipboard_paste_command` MAY be
omitted. When present, each value MUST be either a command string parsed with
shell-like quoting rules or an array of command tokens. The copy command MUST
receive clipboard contents on stdin. The paste command MUST write clipboard
contents to stdout. If either direction is omitted, Mezzanine MUST retain the
default best-effort host clipboard command list for that direction.

The `shell` table MUST support `login`, `interactive`, `integration`,
`integration_mode`, `default_working_directory`, `env`, `tool_discovery`,
`tool_cache`, and `fallback_behavior`.

The `shell` table MUST NOT override the shell executable path. The shell path
MUST be the resolved shell path.

The `keys` table MUST support `escape`, `split_vertical`, `split_horizontal`,
`new_window`, `new_group`, `agent_shell`, `focus_up`, `focus_down`, `focus_left`,
`focus_right`, `focus_previous_window`, `focus_next_window`,
`focus_previous_group`, `focus_next_group`, and a user-defined command binding
map. The `escape` setting defines the prefix table entry point. Direct key
settings in this table are convenience accelerators and MUST NOT replace the
default prefix table.

The `layout` table MUST support `default`, `resize_policy`, `close_policy`,
`min_pane_columns`, and `min_pane_rows`.

The `frames` table MUST contain `window` and `pane` subtables. Each frame
subtable MUST support `enabled`, `position`, `template`, `style`, and
`visible_fields`. `frames.window` MUST also support `right_status` for the
configurable right-aligned window status template.

The `theme` table MUST support `active`, `aliases`, and `colors`. `theme.active`
MUST select the active named theme and MUST default to `kanagawa`.
`theme.aliases` MUST be a map from alias identifiers to `#rgb` or `#rrggbb` hex
color strings. `theme.colors` MUST be a map from the baseline UI color slots to
either a hex color string or a configured alias name. The generated default
configuration MUST include `primary`, `secondary`, and `tertiary` aliases and
MUST assign those aliases to visible high-impact slots. The generated default
configuration MUST also include a `thinking` alias and use it for
`agent_transcript_status_fg` so agent thinking text has a visible muted grey
treatment by default. The generated default configuration MUST include the
`syntax_*` color slots used for source-token highlighting in agent diff output,
and those slots SHOULD derive from the same aliases used by the rest of the
active UI theme. Mezzanine MUST expose the supported theme color slot names
through human-facing configuration guidance and model-facing configuration
schema guidance so users and agents can discover and set individual colors
without relying on source-code inspection.

The `themes` table MUST be a map keyed by theme name. Each custom theme entry
MUST support `aliases` and `colors` subtables with the same value rules as
`theme.aliases` and `theme.colors`. A custom named theme MAY omit slots and
aliases; omitted values MUST inherit from the built-in `deepforest` theme unless
the implementation documents another complete base. Selecting a built-in or
custom theme MUST require only a single setting change to `theme.active`.
`set-theme <name>` MUST switch the active UI theme for the running primary
client after validation, MUST materialize the selected theme aliases and color
slots into the live configuration layer, and MUST persist the same selected
theme table into the primary user config so future launches see the same
palette. `set-option theme.active <name>` MUST remain an equivalent lower-level
live configuration mutation for immediate rendering, but it is not required to
persist the selected theme to disk.

The `history` table MUST support `lines`, `rotate_lines`, `persist`, and
`search_mode`.
`history.lines` MUST default to `10000`.
`history.rotate_lines` MUST default to `1000`.

The `agents` table MUST support `default_provider`, `default_model_profile`,
`shell_only`, `compaction_raw_retention_percent`, `routing`,
`action_failure_retry_limit`, `implementation_pressure_after_shell_actions`,
`custom_system_prompt`, `default_personality`, `subagent_placement`,
`max_concurrent_agents`, `max_root_subagents`, `max_subagents_per_subagent`,
`max_subagent_panes_per_window`, `subagent_wait_policy`, `max_depth`,
`prompt_profile`, and `default_agent_role`.
`agents.compaction_raw_retention_percent` MUST be an integer from `1` to `100`,
MUST default to `10`, and MUST apply to manual compaction and provider
context-limit recovery rather than proactive threshold compaction.
`agents.routing` MUST be a boolean and MUST default to `false`.
`agents.action_failure_retry_limit` MUST be a positive integer and MUST default
to `5`. It bounds model self-correction attempts per identical
model-correctable failed-action signature rather than per action batch, so one
bad action in a batch cannot consume the recovery budget for a different
failed action. `apply_patch` failures are excluded from this bounded recovery
budget and MAY be retried until they succeed or some other blocker ends the
turn.
`agents.implementation_pressure_after_shell_actions` MUST be a positive integer
and MUST default to `3`. It defines the gentle shell-command inspection
pressure threshold; runtime-owned inspection pressure MUST escalate to medium at
the greater of `6` or twice the configured threshold, and to strong at the
greater of `10` or three times the configured threshold.

The `agents.auto_sizing` subtable MUST support `router_model_profile`,
`small_model_profile`, `medium_model_profile`, `large_model_profile`,
`allowed_reasoning_efforts`, and `fallback_policy`. These keys define the
routing model used to classify turn size and the target model profiles that
classification may choose. `router_model_profile` MUST default to
`auto-size-router`, `small_model_profile` MUST default to `auto-size-small`,
`medium_model_profile` MUST default to `auto-size-medium`, and
`large_model_profile` MUST default to `auto-size-large`.
`allowed_reasoning_efforts` MUST default to `["low", "medium", "high",
"xhigh"]`. `fallback_policy` MUST default to `use-default-profile`, meaning
router failures fall back to the user-selected default model profile for the
turn.

The `model_presets` table MUST be a map keyed by model preset identity. Each
preset MUST define `default_model_profile` and MAY define
`auto_sizing_router_model_profile`, `auto_sizing_small_model_profile`,
`auto_sizing_medium_model_profile`, `auto_sizing_large_model_profile`, and
`allowed_reasoning_efforts`. Missing auto-sizing profile keys MUST inherit
`default_model_profile`. Every referenced model profile MUST exist in
`model_profiles`, and every configured reasoning effort MUST use the canonical
names `low`, `medium`, `high`, or `xhigh`. Selecting a model preset from a pane
status selector MUST apply the default model profile and auto-sizing profile
group to that pane without mutating the global `agents.auto_sizing` defaults.

The `model_profiles` table MUST be a map keyed by model profile identity. Each
profile MUST define `provider` and `model`, and MAY define
`reasoning_profile`, `reasoning_effort`, `latency_preference`,
`multimodal_required`, `context_window_tokens`, `context_limit_tokens`,
`max_output_tokens`, `provider_options`, `safety_tier`, `privacy`,
`privacy_tier`, `residency`, `approval`, `approval_policy`, and
`fallback_profiles`. `context_window_tokens`, `context_limit_tokens`, and
`max_output_tokens` MUST be positive token counts when present.
`context_window_tokens` and `context_limit_tokens` MUST drive context-usage
display percentages and explicit compaction budget targets for that profile.
`max_output_tokens` MUST be sent to providers that expose a compatible output
budget control and MUST NOT be included in prompt-cache identity material.
Mezzanine MUST NOT use local fallback context-size estimates as a
prompt-submission gate or provider-request preflight; provider-reported usage
and provider context-limit errors are the authoritative context-size signals.
When both context fields are absent, Mezzanine SHOULD use built-in provider
model metadata for known default model families before falling back to a
conservative local token budget for display and explicit compaction targets.
For OpenAI profiles, `provider_options.prompt_cache_retention` MAY be set to
`in_memory` or `24h` to request the corresponding provider cache-retention
policy. Mezzanine MAY omit redundant provider-default retention fields while
preserving behavior. Mezzanine MUST send `24h` only for OpenAI model families
that support extended prompt-cache retention, and MUST NOT silently translate an
unsupported explicit `in_memory` request into extended retention.
For DeepSeek profiles, `provider_options.thinking` MAY be set to `enabled` or
`disabled` to explicitly control native thinking mode independently of
`provider_options.reasoning_effort`. When omitted, Mezzanine MAY infer DeepSeek
thinking mode from a configured reasoning profile or reasoning effort. The
DeepSeek adapter MUST keep this behavior scoped to DeepSeek request
serialization and MUST NOT emit thinking controls for providers that do not
support them.

Generated default configuration MUST include the auto-sizing model profiles
referenced by `agents.auto_sizing`: `auto-size-router` using `gpt-5.4-mini`,
`auto-size-small` using `gpt-5.3-codex`, `auto-size-medium` using `gpt-5.4`, and
`auto-size-large` using `gpt-5.5`. These default profiles MUST use the same
provider as the default profile unless the user overrides them. The router
profile SHOULD use a low or medium reasoning effort by default because it is
used only for bounded classification, while target profiles MAY define their
own default reasoning efforts that the auto-sizing decision can override for a
single turn. Generated OpenAI and DeepSeek default profiles SHOULD carry
documented context-window token counts for their selected model families so
frame context usage, `/model list`, and explicit or provider-limit compaction
use the same denominator.

The `subagents` table MUST be a map keyed by subagent profile identity. Each
profile MAY define `name`, `description`, `developer_instructions`,
`model_profile`, `permission_preset`, `mcp_servers`, `shell_env`,
`default_cooperation_mode`, `default_read_scopes`, and
`default_write_scopes`.

`agents.custom_system_prompt` MAY define additional user-owned system prompt
text appended after the built-in Mezzanine agent prompt. It MUST be treated as
provider system context, not as a user message. `agents.default_personality` MAY
name a profile from the `personalities` table and MUST fail validation if the
named profile is absent.

The `personalities` table MUST be a map keyed by user-defined personality
profile identity. Mezzanine MUST NOT define built-in personality profiles. Each
profile MAY define `name`, `system_prompt`, `instructions`, `response_style`,
`style`, `model_profile`, `planning_enabled`, `planning`,
`routing_enabled`, and `routing`. The `/personality` command MUST
select configured profiles by id, MUST offer configured profile ids through
agent prompt completion, and MAY continue to accept ad hoc response-style text
for pane-local style preferences. A selected profile's `system_prompt` or
`instructions` MUST be treated as provider system context.

The `permissions` table MUST support `approval_policy`, `trusted_directories`,
`trusted_projects`, `command_rules`, `session_command_rules`,
`global_command_rules`, `network_policy`, `destructive_action_policy`, and
`bypass_mode`.

Project configuration overlays SHOULD be created at `.mezzanine/config.toml`
with a minimal `[permissions]` table and `approval_policy = "ask"` when a
project-scoped mutation needs a file that does not yet exist.

The `providers` table MUST be a map keyed by provider identity. Each provider
entry MUST support `kind`, `auth_profile`, `base_url` when applicable,
`models`, `default_model`, and provider-specific options.
For the built-in OpenAI provider, `base_url` MUST be interpreted as an API base
URL such as `https://api.openai.com/v1`; Mezzanine MUST derive the documented
`/responses` request endpoint and `/models` catalog endpoint from that base.
For named providers whose `kind` is `openai-compatible`, `base_url` MUST be
interpreted as an OpenAI-compatible API base URL; Mezzanine MUST derive the
documented `/chat/completions` request endpoint and `/models` catalog endpoint
from that base. OpenAI-compatible providers MUST use the named provider entry as
the configuration boundary so each backend can declare its own base URL, auth
profile, model list, and default model. The compatibility adapter is limited to
non-streaming Chat Completions behavior and MUST NOT inherit OpenAI Responses
prompt-cache or reasoning-control semantics unless a later provider-specific
capability explicitly enables them.
When `providers.openai.options.organization_id` or
`providers.openai.options.project_id` is configured, Mezzanine MUST send the
documented `OpenAI-Organization` and `OpenAI-Project` headers on direct OpenAI
API-key requests. These options MUST NOT be sent to ChatGPT browser/device
credential backends.
OpenAI Responses requests SHOULD include a stable, non-secret
`prompt_cache_key` derived from Mezzanine's prompt profile, provider, model,
and cache-family identity. The key SHOULD NOT vary only because the interaction
kind, exposed action surface, MCP tool catalog, or current user prompt changed;
the provider's exact prompt-prefix hashing provides the correctness boundary
for those differences, and over-fragmenting the routing key reduces cache hit
rates.
The derived key MUST NOT include rendered prompt-prefix bytes, user prompt text,
action output, transcript content, project-file content, secrets, credentials,
or per-turn identifiers. Provider token accounting MUST preserve the difference
between an omitted cached-token counter and an explicit provider-reported zero.
OpenAI request diagnostics SHOULD record non-model-visible fingerprints for the
front-loaded instructions, response-format schema, tool schema, stable input
prefix, volatile input suffix, and complete observable cacheable prefix so cache
misses can be diagnosed without inserting diagnostic text into model context.
Static invariant agent behavior SHOULD remain in the front-loaded OpenAI
`instructions` field. Dynamic controller state such as capability decisions,
repair hints, compaction notices, and current action eligibility SHOULD be
rendered as later role-preserving model input rather than mutating the static
instructions prefix.
When an OpenAI model profile includes
`provider_options.prompt_cache_retention`, Mezzanine MUST validate that it is
either `in_memory` or `24h`. Mezzanine MUST omit explicit `in_memory` retention
only for model families where in-memory retention remains the provider default,
and MUST reject explicit `in_memory` for model families whose provider default
is extended retention and whose API no longer supports in-memory retention.
Mezzanine MUST pass `24h` through as the OpenAI Responses
`prompt_cache_retention` request field only when the selected OpenAI model
family supports extended prompt-cache retention.

The built-in OpenAI provider default model MUST be `gpt-5.5` unless the user
overrides it through provider or model-profile configuration. The built-in
OpenAI provider model list SHOULD include only coding-agent harness models:
`gpt-5.5`, `gpt-5.4`, `gpt-5.4-mini`, `gpt-5.3-codex`,
`gpt-5.3-codex-spark`, and `gpt-5.2`. When a provider configuration leaves
`models` empty, Mezzanine MUST load the provider's built-in code-defined model
list instead of treating the provider as having no selectable models.
The built-in DeepSeek provider default model MUST be `deepseek-v4-pro` unless
the user overrides it through provider or model-profile configuration. The
built-in DeepSeek provider model list SHOULD include `deepseek-v4-pro` and
`deepseek-v4-flash`, and generated DeepSeek model profiles SHOULD use a
`1000000` token context window for those V4 model families.
Named `openai-compatible` providers do not have built-in models. Users SHOULD
configure `models` and `default_model` for each compatible backend, and a live
catalog refresh MAY replace or augment those configured model ids when the
backend supports the OpenAI-compatible `/models` endpoint.
Mezzanine SHOULD attempt live provider model-catalog refresh once during daemon
startup after configuration and authentication stores are available. After
startup, live provider catalog refresh MUST be explicit through a user or
control action such as `refresh-provider-info`; pane creation, pane selection,
and model selector rendering MUST NOT independently prefetch provider catalogs.

The `message_protocol` table MUST support `enabled`, `endpoint`,
`retention_messages`, `retention_bytes`, and `allow_remote_bridges`.

The `control` table MUST support `endpoint`, `socket_path`, `tcp_bind`,
`tcp_enabled`, `auth_token_file`, and `observer_policy`. `observer_policy`
MUST configure how observer requests are presented and approved; it MUST NOT
remove Mezzanine's baseline support for pending and approved read-only
observers.

The `mcp_servers` table MUST be a map keyed by MCP server identity. Each MCP
server entry MUST support `name`, `command`, and `args` for stdio servers,
`url` for streamable HTTP servers, `env`, `env_vars`, `cwd`, `http_headers`,
`bearer_token_env`, `enabled_tools`, `disabled_tools`, `startup_timeout_sec`,
`startup_timeout_ms`, `tool_timeout_sec`, `tool_timeout_ms`, `enabled`,
server approval settings, tool approval settings, and declared external
capability metadata.

The `auth` table MUST support `auth_file`, `credential_store`, and
`default_profile`. It MUST NOT contain secret material.

The `instructions` table MUST support `global_files`, `project_filenames`,
`max_bytes`, `include_hidden_directories`, and `on_truncation`.

The `hooks` table MUST support lifecycle events, matcher groups, command hooks,
and shell hooks.

The `snapshots` table MUST support `enabled`, `path`, `on_detach`,
`on_interval_seconds`, `on_agent_turn`, and `retention_count`.

The `audit` table MUST support `enabled`, `path`, `format`, `retention_days`,
`redact_secrets`, `hash_chain`, and `required`.

The `extensions` table MUST be a map keyed by extension identity.

Implementations MAY add keys under `extensions`. Implementations MUST reject
unknown non-extension keys by default, unless the user has enabled a permissive
configuration mode.

### 8.3 Configuration Shell Capabilities

The configuration shell MUST provide commands or equivalent interactive actions
to:

- Show effective configuration.
- Show the source file and precedence layer for a setting.
- Show discovered project overlay files and their trust state.
- Trust, reject, inspect, list, and revoke trust for project roots.
- Get a setting by path.
- Set a live-mutable setting by path.
- Unset a setting by path.
- Validate configuration files without applying changes.
- Reload configuration from disk.
- Persist live changes to a chosen configuration file.
- Bind and unbind keys.
- Enable, disable, and edit frame templates.
- List available UI themes and inspect or change the active UI theme, theme
  aliases, and individual UI color slots.
- Inspect and change history limits.
- Inspect and change terminal compatibility settings.
- Start provider authentication.
- Show provider authentication status.
- Log out of a provider.
- Select provider and model profiles.
- Inspect local message protocol status.
- Inspect and change control endpoint status.
- List, approve, reject, inspect, and revoke read-only observers.
- Add, list, remove, enable, disable, and authenticate MCP servers.
- Create, list, inspect, resume, and delete session snapshots.
- List, add, remove, enable, disable, and persist command prefix rules.
- Enable, disable, or inspect approval bypass mode.
- Configure hooks.
- Inspect audit logging status.
- Export a redacted diagnostic bundle.

The configuration shell MUST validate command arguments before mutating live
state.

Configuration shell commands that would expose secrets MUST require explicit
user confirmation and MUST redact secret values by default.

## 9. Agent Harness

Each pane MUST be wrapped by an agent harness.

The model-facing agent context MUST NOT include the pane's current visible
terminal buffer, a bounded tail of terminal history, or alternate-screen
contents by default.

Default model-facing agent context MUST include only the active user request,
configured system and developer instructions, applicable project guidance,
permission and scheduler state, compacted prior task context, and explicit
action results.

Every provider request for a running agent turn MUST include the current
discovered project guidance as embedded system-prompt repository instruction
content when present. Provider continuations after actions, local messages, or
approval decisions MUST refresh the embedded project-guidance content from the
discovered instruction files and MUST NOT omit or duplicate it.

Command output, file contents, directory listings, search results, web content,
and similar external observations MUST enter model context only as results of
actions requested by the user or emitted by the model.

The agent harness MAY maintain terminal buffers for rendering, copy selection,
capture commands, search commands, and transaction observation. Maintaining that
state MUST NOT cause pane contents to be passively injected into model context.

The agent harness MUST be able to send commands to the pane and observe the
effects of those harness-initiated commands through terminal output.

For local system interaction, agents MUST interact through the pane shell. A
Mezzanine agent MUST NOT receive hidden host-side capabilities for local file
system access, local process execution, or local system mutation that bypass the
pane shell.

The previous requirement does not prohibit the harness from performing
control-plane operations such as model-provider requests, credential handling,
session persistence, local agent message passing, terminal rendering, explicit
capture commands, or transaction observation, provided those operations are
disclosed by the Mezzanine model and do not bypass the shell for local system
mutation.

The harness MUST record enough session state to resume an agent session after
the agent shell is hidden, the pane is switched away from, or the Mezzanine
client detaches and later reattaches.

### 9.1 Shell Discovery and Classification

The harness MUST discover the pane shell by resolving the shell path when a
pane is created. Shell resolution MUST use `SHELL` when it is set, non-empty,
absolute, and executable by the current user; otherwise it MUST use `/bin/sh`
when `/bin/sh` is executable. The same resolved shell path precedence MUST
apply to pane creation, explicit pane commands, shell hooks, bootstrap probes,
and agent command wrappers.

The `SHELL` environment variable is the definitive source of truth for the pane
shell executable only when it is set, non-empty, absolute, and executable by the
current user. Configuration MAY control login mode, interactivity, environment
variables, and integration behavior, but it MUST NOT replace the shell
executable path.

When the pane shell enters another interactive environment, such as `ssh`, a
container shell, a chroot, or another remote command environment, the harness
MUST re-bootstrap against the shell environment observed through the pane after
that transition is detected. The `$SHELL` value observed inside the active
interactive environment is the definitive shell identity for subsequent agent
shell actions in that environment when it is set, non-empty, absolute, and
executable. If the active environment's `$SHELL` is unset, empty, relative,
non-executable, or otherwise unusable but `/bin/sh` is executable, Mezzanine
MUST classify `/bin/sh` as the shell identity for that environment.

If the active interactive environment does not expose a usable shell, the
harness MAY continue terminal observation, but it MUST treat agent shell command
execution as unavailable or interactive-only until a usable shell is observed.
This is an accepted limitation of Mezzanine's shell-only local interaction
model. When command execution is unavailable, the agent MUST report the
limitation to the user rather than pretending that normal shell capabilities
remain available.

If `SHELL` is set but is not an absolute path, is not executable by the current
user, or is otherwise unusable, Mezzanine MUST fall through to `/bin/sh` using
the same precedence rule. If neither `SHELL` nor `/bin/sh` is usable,
Mezzanine MUST fail the shell-dependent operation with an actionable
diagnostic. An implementation MAY provide an interactive recovery flow that
asks the user to export a valid `SHELL`.

The harness MUST classify the shell by executable name and runtime probing.

The baseline shell classifications MUST include `bash`, `zsh`, `fish`,
`posix-sh`, and `unknown-unix`.

If a shell cannot be classified, the harness MUST treat it as `unknown-unix`
and MUST assume only POSIX-like shell behavior.

The default local toolbox available to agents MUST be assumed to include
Unix-like shell builtins, GNU/BSD coreutils-style commands, `sed`, `grep`, and
`python3` or `python`. The baseline command set includes `sh`, `printf`,
`test`, `pwd`, `cd`, `env`, `cat`, `sed`, `awk`, `grep`, `find`, `xargs`,
`sort`, `head`, `tail`, `wc`, `mkdir`, `cp`, `mv`, `rm`, `chmod`, `ln`, `ps`,
`kill`, `stty`, and `python3` or `python`.

When common developer tools such as `git`, `rg`, `make`, language package
managers, or compilers are useful, the agent MUST discover them by using shell
commands in the pane. The agent SHOULD prefer modern fast tooling such as `rg`
over slower baseline alternatives when discovery shows that such tooling is
available.

### 9.2 Tool Discovery and Environment Signatures

Before the first model request for a pane, the harness MUST discover available
tooling through the pane shell.

Tool discovery MUST use commands visible in pane history unless policy
explicitly allows hiding bootstrap noise from the user-facing pane while still
recording it in the agent transcript and audit log.

The discovery result MUST include tool name, resolved executable path when
available, version output when safe and reasonably fast, discovery command,
exit status, and time discovered.

The harness MUST cache tool discovery results by environment signature.

The environment signature MUST be computed from shell-observed state and MUST
include at least operating system name, machine architecture, kernel or platform
version when available, hostname or stable local host identity, container or
namespace marker when detectable, resolved shell path, shell classification,
shell version when available, `PATH`, current working directory, project root
when known, and environment-manager markers such as `VIRTUAL_ENV`,
`CONDA_PREFIX`, `NIX_PROFILES`, or similar values when present.

The environment signature MUST NOT include secret environment variable values.

If a signature field cannot be observed, the cache entry MUST record that the
field is unknown rather than substituting hidden host-side information.

Model-facing context MUST NOT copy the full environment signature. It MUST
include a fixed-width stable digest of the canonical shell-observed signature
plus only compact execution-critical facts such as current working directory,
shell class, shell path, project root when known, git-repository state, and
bounded tool availability summaries. Raw `PATH`, hostname, username, platform
banner, and per-tool resolved paths or versions MUST remain internal cache,
diagnostic, transcript, or audit data unless the model explicitly asks for them
through a shell action.

The harness MUST invalidate or bypass cached tool discovery results when the
environment signature changes, when `PATH` changes, when the pane enters a
different container or remote environment detectable from the shell, or when a
discovered executable fails with command-not-found or equivalent behavior.

After every user prompt submitted to the agent shell and before the resulting
model request, the harness MUST recompute the environment signature using the
pane shell. If the signature differs from the signature used for the previous
agent turn, the harness MUST run the bootstrap phase and tool discovery again
or continue with explicitly degraded context according to policy. Signature
changes caused by SSH, container entry, chroot entry, virtual environment
activation, directory changes, `PATH` changes, or shell replacement MUST be
treated as environment transitions.

The harness MUST NOT recompute the environment signature before every provider
request. It MUST recompute it before sending a user-defined prompt or queued
user instruction to the model, and MAY recompute it before other turn triggers
when the pane shell is known to be ready.

Environment-signature recomputation MUST be bounded, read-only, and
non-interactive. It MUST use a configured timeout. If the pane is in an
`unknown`, `busy`, `degraded`, `interactive-blocked`, full-screen,
password-prompt, host-key-prompt, or otherwise not-ready state, the harness
MUST NOT wait indefinitely and MUST NOT send probes that could be consumed by
the foreground program. In that case the harness MUST either continue with the
last known signature while marking it stale, or block the agent turn with a
user-visible request to return the pane to a shell prompt or mark it ready. This
rule prevents signature recomputation from deadlocking the agent while the pane
is not at an actionable shell boundary.

Tool discovery SHOULD check for `rg`, `git`, `python3`, `python`, `make`,
package managers relevant to the project, formatters, linters, test runners,
and language compilers or interpreters detected from project files.

### 9.3 Shell Integration and Command Boundaries

Mezzanine MUST support passive shell integration using OSC 133 semantic
markers.

When passive shell integration is enabled, prompt start, prompt end, command
output start, and command finished markers MUST use the OSC 133 `A`, `B`, `C`,
and `D` conventions.

Mezzanine MAY also recognize richer shell-integration markers such as OSC 633
`A`, `B`, `C`, `D`, `E`, and `P` when they are produced by the pane shell or a
known terminal integration layer. OSC 633 metadata MAY improve command-line and
current-directory detection, but it MUST be treated as terminal metadata rather
than as a security boundary.

For `bash`, Mezzanine SHOULD use `PS0` when available to emit command-output
start markers and `PROMPT_COMMAND` or equivalent prompt wrapping to emit prompt
and command-finished markers.

For `zsh`, Mezzanine SHOULD use `preexec` and `precmd` hooks to emit command
start and command-finished markers.

For `fish`, Mezzanine SHOULD use fish preexec and postexec event hooks when
available.

For `posix-sh` and `unknown-unix`, Mezzanine MUST NOT assume shell-level
preexec or postexec support.

Passive shell integration MUST preserve the user's visible prompt text and MUST
NOT permanently modify user shell startup files unless the user explicitly
requests persistent installation.

Passive shell integration MUST be treated as advisory. It MAY improve prompt
detection, copy-mode block selection, and command status display, but it MUST
NOT be the only mechanism used to determine completion of agent-issued
commands.

For every non-interactive shell action sent by an agent, the harness MUST use
an explicit command transaction wrapper.

The transaction wrapper MUST be generated for the classified shell. For
`bash`, `zsh`, `posix-sh`, and `unknown-unix`, the default wrapper MUST use
POSIX-compatible syntax. For `fish`, the wrapper MUST use fish-native block,
variable, and status syntax with equivalent semantics.

For POSIX-compatible shells, the wrapper MUST have the following logical form:

```sh
# history-suppression prologue, if supported by the active shell
MEZ_MARKER_TOKEN='<shell-quoted marker token>'
MEZ_TURN='<shell-quoted turn id>'
MEZ_AGENT='<shell-quoted agent id>'
MEZ_PANE='<shell-quoted pane id>'
printf '\033]133;C;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\033\\' \
    "$MEZ_MARKER_TOKEN" "$MEZ_TURN" "$MEZ_AGENT" "$MEZ_PANE"
MEZ_COMMAND_FILE="$(mktemp)"
cat > "$MEZ_COMMAND_FILE" <<'MEZ_COMMAND_<unguessable-delimiter>'
# agent command text is inserted here as a child-shell script
MEZ_COMMAND_<unguessable-delimiter>
setsid env -u MEZ_MARKER_TOKEN -u MEZ_TURN -u MEZ_AGENT -u MEZ_PANE \
    TERM=dumb PAGER=cat MANPAGER=cat GIT_PAGER=cat SYSTEMD_PAGER=cat \
    '<shell-quoted resolved shell path>' "$MEZ_COMMAND_FILE" </dev/null
MEZ_STATUS=$?
rm -f -- "$MEZ_COMMAND_FILE"
printf '\033]133;D;%s;mez_marker=%s;mez_turn=%s;mez_agent=%s;mez_pane=%s\033\\' \
    "$MEZ_STATUS" "$MEZ_MARKER_TOKEN" "$MEZ_TURN" "$MEZ_AGENT" "$MEZ_PANE"
unset MEZ_MARKER_TOKEN MEZ_TURN MEZ_AGENT MEZ_PANE MEZ_STATUS
# history-suppression cleanup, if supported by the active shell
```

`<shell-quoted resolved shell path>` in the logical wrapper denotes the shell
path resolved by Mezzanine before constructing the wrapper. It MUST be quoted
by Mezzanine using shell-specific quoting and MUST NOT be read from model
output.

The wrapper MUST emit a start marker before the command and an end marker after
the command. The start marker MUST include a high-entropy marker token, turn
identity, agent identity, and pane identity. The end marker MUST include the
same marker token, turn identity, agent identity, pane identity, and the command
exit status.

The start marker MUST use OSC 133 `C` with Mezzanine-specific parameters when
the active terminal profile permits OSC 133. The end marker MUST use OSC 133
`D` with Mezzanine-specific parameters and the exit status when the active
terminal profile permits OSC 133. If the active terminal profile does not
permit OSC 133, Mezzanine MUST use a documented in-band marker format with the
same marker-token requirements and spoofing caveats.

The marker token MUST contain at least 128 bits of entropy.

The transaction marker token is a scheduling and correlation marker only. It is
not a secret, is not a capability, and MUST NOT be treated as proof that an
action is authorized or safe. Implementations SHOULD avoid intentionally placing
the marker token in the child command's environment, arguments, temporary files,
or command text, but they MUST assume that terminal output and terminal metadata
can be observed, logged, replayed, or spoofed by programs running in the pane.
A matching marker token MAY help correlate wrapper-generated start and end
markers with a pending transaction, but a matching marker token alone MUST NOT
be the sole condition for deciding that the shell is ready for more agent input.

Mezzanine-injected shell transactions, agent-mode child shell handoff commands,
readiness probes, bootstrap commands, tool discovery commands, and focused-shell
hook wrappers SHOULD avoid polluting the user's shell history file and in-memory
shell history. For `bash`-like POSIX shells, Mezzanine SHOULD temporarily
disable shell history, set `HISTFILE=/dev/null` for the transaction or child
shell handoff, remove the current wrapper prologue history entry when supported,
and restore the previous history and `errexit` state after the transaction end
marker is emitted or the child shell exits. For `fish`, Mezzanine SHOULD enable
private-mode style suppression when supported and SHOULD remove known Mezzanine
wrapper prefixes from Fish history as a fallback. For shells without known
history controls, Mezzanine SHOULD make a best-effort attempt to avoid persisted
history while preserving the transaction marker and command-status semantics.

For non-stateful POSIX-compatible actions, the default wrapper MUST execute the
agent command in a child shell whose environment omits Mezzanine transaction
metadata. The command text SHOULD be provided to the child shell as a script on
standard input using an unguessable here-document delimiter or an equivalent
mechanism. If the command text cannot be embedded in such a script without
changing its meaning, Mezzanine MUST require approval and MUST show the
transformed command or wrapper strategy to the primary client.

The default non-interactive wrapper MUST also suppress accidental interactive
program behavior. It MUST redirect the child command shell's standard input
from a non-interactive source such as `/dev/null`, SHOULD launch the child
command in a new session without a controlling terminal when a portable helper
such as `setsid`, Python, or Perl is available, and MUST set transaction-local
environment controls that disable rich terminal behavior, common pagers,
editors, credential prompts, and package prompts. At minimum this environment
SHOULD cover `TERM` with a non-capable value such as `dumb`, `PAGER`,
`MANPAGER`, `GIT_PAGER`, `SYSTEMD_PAGER`, `LESSSECURE`,
`SYSTEMD_PAGERSECURE`, `GIT_TERMINAL_PROMPT`, `GIT_EDITOR`, `EDITOR`, `VISUAL`,
and `DEBIAN_FRONTEND`. These controls MUST be scoped to the child transaction
and MUST NOT persist into the interactive pane shell after the transaction
finishes.

The harness MUST shell-quote wrapper metadata using a shell-specific quoting
routine. Metadata quoting MUST be independent of model output.

The harness MUST insert the agent command text as complete shell input inside
the wrapper block. The harness MUST NOT attempt ad hoc token-level rewriting of
the agent command text. If a command cannot be embedded without changing its
meaning, Mezzanine MUST present the transformed command to the user and require
approval before execution.

The default non-interactive wrapper MUST execute the command text in an
isolated child shell or equivalent shell context that does not persist `cd`,
variable assignment, aliases, functions, shell options, traps, or other
shell-state mutations after the transaction completes. Commands that
intentionally mutate the interactive pane shell state MUST be classified as
stateful shell actions and MUST be executed through a separate visible
stateful-action path.

Stateful shell actions MUST require explicit approval unless the active policy
allows them. A stateful shell action MUST disclose that it may change the pane
shell's working directory, environment, aliases, functions, or options.

The transaction wrapper MUST preserve the command's exit status as the exit
status reported in the end marker.

Because pane shells are connected to pseudoterminals, stdout and stderr are a
single terminal byte stream by default. The harness MUST NOT claim separate
stdout and stderr unless the command itself redirects or tags them in a
documented way.

Non-interactive shell actions MUST NOT require interactive standard input. If a
wrapped command reads from the terminal, takes over the terminal, launches a
full-screen interface, replaces the shell process, or prevents the wrapper from
emitting an end marker, the harness MUST treat the transaction as interactive
or timed out and MUST surface that state to the user.

Every Mezzanine-owned shell transaction, including agent shell actions,
readiness probes, and bootstrap probes, MUST have a finite runtime timeout. If
an agent shell action reaches its timeout before the expected end marker is
observed, Mezzanine MUST remove the live transaction, interrupt the pane shell
when practical, mark pane readiness degraded, record a pane-local copyable
diagnostic, and record structured terminal-observation metadata including
timeout state, elapsed time, observed byte count, preview, and truncation state.
Timeout action results that are safe for bounded self-correction MUST be
returned to the model once; otherwise Mezzanine MUST fail the affected turn. If
a readiness probe times out while a shell action is pending, Mezzanine MUST
fail that pending action as not sent to the pane rather than leaving the turn
running. If a bootstrap probe times out, Mezzanine MUST clear the bootstrap
attempt and mark readiness degraded instead of retrying the hidden wrapper
indefinitely. Agent turns MUST have one turn-wide timeout that defaults to
30 minutes. Shell actions MAY request a positive per-action timeout, and omitted
values MUST inherit the remaining turn-wide timeout budget. The effective
shell-action timeout MUST be bounded by that remaining turn-wide timeout budget,
so no shell action can outlive its enclosing turn. Non-stateful shell
transactions that wait for a deferred command payload receiver MUST also use a
short implementation-defined start timeout as a handoff watchdog, not as the
normal sequencing mechanism; if the receiver start marker is not observed before
that timeout, Mezzanine MUST treat the transaction as timed out. Readiness
probes, bootstrap probes, and other internal health checks MAY use shorter
implementation-defined probe timeouts.

A marker MUST NOT be accepted as authoritative merely because its marker token
matches the currently active transaction for the pane.
For runtime-dispatched shell transactions, Mezzanine MUST observe exactly one
matching transaction start marker before accepting a matching transaction end
marker. Duplicate start markers, end markers before start markers, and marker
metadata mismatches for a live transaction MUST be treated as shell protocol
violations that fail the affected action or readiness/bootstrap operation
promptly with diagnostic context instead of leaving the turn to wait for a
timeout. Markers whose tokens do not match a live Mezzanine-owned transaction
MUST NOT settle or fail that transaction solely by their presence.

If command output emits text or terminal control sequences that resemble a
marker but do not contain the active marker token, the harness MUST treat them
as ordinary pane output.

When an agent-shell transaction is hidden from the pane, Mezzanine MUST treat
the command's output bytes as action data rather than terminal-rendering input.
Hidden transaction output MUST NOT be fed through the full pane terminal screen
parser solely to discover Mezzanine transaction markers. Instead, the harness
MUST use a bounded parser that recognizes only Mezzanine-owned transaction
marker sequences and retains only the small fragment needed to complete a split
marker across PTY reads. File contents or command output containing arbitrary
terminal control sequences MUST NOT be allowed to mutate hidden terminal state
or monopolize UI rendering while the transaction waits for its end marker.
For non-stateful agent shell actions, child stdout and stderr intended for
model-facing action results MUST cross the pane PTY as a printable, tagged
base64 transport rather than as raw terminal bytes. Mezzanine MUST decode that
transport before constructing action-result content or terminal-observation
metadata for both successful and failed actions. If bounded observation cuts a
base64 block before its end marker, Mezzanine MUST decode any complete base64
prefix it retained and annotate the result as truncated or incomplete.
Human-visible pane presentation MAY show the encoded transport when that is the
least disruptive way to keep the pane responsive, provided the model-facing
result receives decoded output and the user can inspect or decode the retained
bytes when needed.

After observing a candidate end marker for a non-interactive command, the
harness MUST perform a fresh readiness check before sending another agent
command unless the primary client explicitly overrides the check. The readiness
check MUST use a new marker token and MUST be bounded by timeout. If the
readiness check is not observed, the harness MUST treat the pane as still busy,
interactive, degraded, or timed out rather than enqueueing additional shell
input.

Mezzanine MUST document that terminal byte streams are not a cryptographic
security boundary. Transaction markers are a command-boundary and scheduling
aid; permission decisions MUST be made before command execution and MUST NOT
depend on trusting command output.

Interactive shell actions MAY use passive shell integration, prompt detection,
user-visible status, and explicit user confirmation instead of a transaction
wrapper.

The harness MUST NOT change the user's shell command text in a way that changes
its semantics without showing the transformed command to the user when approval
is required.

### 9.4 Agent Bootstrap

Before the first model request for a pane, the harness MUST perform a bootstrap
phase.

The harness MUST also perform a bootstrap phase after any environment
signature change detected before an agent turn. A bootstrap phase caused by a
signature change MUST replace the active shell classification, tool discovery
cache entry, project root, instruction discovery result, and context assembly
metadata for subsequent turns.

The bootstrap phase MUST discover the shell classification, current working
directory, terminal size, project root when possible, `.git` marker when
possible, and applicable project instruction files.

The bootstrap phase MUST use the pane shell and assumed Unix-like toolbox.

The bootstrap phase MUST NOT use hidden host-side file access for project
content.

Bootstrap commands MUST be evaluated through the active permission policy.
Default bootstrap discovery MUST use only commands classified as read-only by
the built-in prefix rules. If a bootstrap command would require broader
permission, Mezzanine MUST either request approval or continue with degraded
context according to the active approval policy.

The default bootstrap sequence SHOULD use commands equivalent to `pwd`, a
resolved-shell-path probe, shell version probes, `uname`, `hostname`, `env`
limited to non-secret names needed for the environment signature, `command -v`
probes for expected tools, and bounded directory searches for project roots and
instruction files.

Bootstrap output that is hidden from the visible pane for usability MUST still
be retained in the Mezzanine-owned transaction observation used for environment
signature parsing, agent context, transcript capture, and audit logging when
audit logging is enabled.

If bootstrap discovery fails, the harness MUST continue with degraded context
and MUST disclose the failed discovery to the agent.

A bootstrap attempt MUST be bounded and one-shot for the observed pane
readiness epoch. Completing or failing a bootstrap wrapper MUST clear the
pending bootstrap dispatch for that pane. Mezzanine MUST NOT repeatedly
dispatch the same bootstrap wrapper on subsequent ticks solely because the
hidden bootstrap output could not be parsed.

For remote or nested interactive environments, bootstrap MUST be treated as a
readiness state machine with at least `unknown`, `prompt-candidate`, `probing`,
`ready`, `busy`, `degraded`, and `interactive-blocked` states.

The harness MUST NOT send non-interactive agent commands while the state is
`unknown`, `probing`, `busy`, `degraded`, or `interactive-blocked`.

Passive shell-integration markers, including OSC 133 and OSC 633 prompt and
command markers, MAY move the pane from `unknown` or `busy` to
`prompt-candidate`. They MAY also move the pane from `interactive-blocked` to
`prompt-candidate` when host process metadata independently shows the primary
pane shell is again the foreground process. Prompt-looking text without
shell-integration markers MAY be used only as a user-visible hint and MUST NOT
by itself move the pane to `ready`.

The harness MAY send a readiness probe from `unknown`, `prompt-candidate`, or
`degraded`, after a recent harness-owned transaction whose wrapper reached a
candidate end marker, or after an explicit primary-client command such as
`mark-pane-ready`. A
readiness probe MUST be bounded by timeout, MUST be read-only, MUST avoid
terminal mode changes, MUST use a fresh marker token distinct from any previous
transaction, and SHOULD execute a no-op command such as `:` or a bounded
`printf` through the active shell.

The state MAY become `ready` only after one of the following:

- A readiness probe completes and its output is observed in the expected order.
- A harness-owned non-interactive transaction reaches a candidate end marker and
  a subsequent readiness probe completes.
- The primary client explicitly marks the pane ready after Mezzanine displays
  that the state is uncertain.

The `mark-pane-ready` override MUST be primary-client only. Before accepting
the override, Mezzanine MUST display the current readiness state, the reason
automatic readiness could not be verified, and the risk that the next
harness-sent command may be delivered to a foreground program rather than an
idle shell. Accepting the override MUST transition the pane to `ready` for one
shell interaction epoch only. The epoch MUST end when Mezzanine sends a
harness-owned command, observes command-start metadata, observes alternate
screen entry, observes a foreground-interactive prompt, observes a pane primary
PID change, observes an environment-signature change, or the readiness probe
for a later transaction fails. The override MUST be recorded in the message log
and audit log when audit logging is enabled.

The state MUST become `busy` when Mezzanine sends a non-interactive command or
observes command-start metadata for the active pane. Password prompts, host-key
prompts, multi-factor prompts, login banners awaiting user input, full-screen
terminal applications, alternate-screen applications, and foreground programs
that have not returned to the shell MUST move the state to
`interactive-blocked` until the user completes or exits that interaction.
When process metadata later proves the primary shell is foreground again,
Mezzanine MAY treat `interactive-blocked` as stale and return to
`prompt-candidate` before sending any further harness-owned command.

If a probe times out, is echoed without executing, is consumed by a foreground
program, or produces output inconsistent with the expected token, the state MUST
become `interactive-blocked` or `degraded` and Mezzanine MUST surface the
condition to the user. This prevents environment-signature recomputation and
bootstrap from deadlocking on a pane that is not at an actionable shell
boundary.

If the environment does not expose a usable shell after bounded probing, the
state MUST become `degraded` and agent command execution MUST remain unavailable
until a later signature change, user intervention, or successful readiness
probe.

### 9.5 Agent Turn Lifecycle

The agent harness MUST process agent work as ordered turns.

An agent turn MUST begin from one of the following triggers:

- User input submitted through the agent shell.
- A local message addressed to the agent.
- A scheduled or resumed task owned by the agent.
- A subagent orchestration event.
- A user-approved retry or continuation.

For each turn, the harness MUST create a turn record containing turn identity,
agent identity, pane identity, triggering event, start time, active policy,
active model profile, and parent turn identity when applicable.

The harness MUST serialize turns for a single agent unless the user or policy
explicitly enables concurrent turns for that agent.

### 9.6 Context Assembly

Before making a model request, the harness MUST assemble context from:

- The Mezzanine agent system prompt profile.
- Active user, developer, policy, and configuration instructions.
- The current pane identity, window identity, and session identity.
- The active agent conversation transcript entries retained for raw replay
  since the latest compaction boundary, plus the compacted transcript summary
  for older entries.
- Pending local messages addressed to the agent.
- Applicable global Mezzanine configuration guidance.
- Project guidance discovered during bootstrap or through later pane-shell
  commands.
- Explicit action results produced by commands, file operations, directory
  listings, searches, web retrieval, or other model-requested tools.

The harness MUST distinguish terminal bytes, user text, local messages, project
files, web content, and model output as separate context sources.

Conversation transcript replay MUST preserve the author role of each replayed
entry. Prior user messages MUST be supplied as user context, prior visible
assistant responses MUST be supplied as assistant context, and prior tool or
action results MUST be supplied only through a bounded, sanitized tool-result
projection. Compact action/audit summaries MUST NOT replace visible assistant
text when that text is needed for later references.
The active post-compaction transcript replay SHOULD NOT be pre-filtered by an
arbitrary recent-entry count. When explicit compaction or provider context-limit
recovery reduces replayed transcript context, it MUST preserve or summarize
older visible assistant and user text rather than silently dropping it before
retry. Visible assistant and user transcript entries SHOULD NOT be byte-sliced
before compaction; oversized entries SHOULD use the same compact summary path as
other oversized context blocks.
When conversation compaction runs, Mezzanine MUST retain a bounded raw tail of
recent durable transcript entries after the compacted summary. The retained raw
tail MUST cover approximately the configured
`agents.compaction_raw_retention_percent` of the active model context budget by
estimated replay word count, with at least the newest entry retained when any
durable transcript entry exists. The raw tail MUST preserve author roles and exact
visible assistant/user text so terse follow-up prompts can resolve recent
references such as numbered list items. Older entries outside the raw tail
SHOULD be represented by compact memory rather than replayed verbatim.
When compact memory is injected into later model context, Mezzanine MUST also
inject an explicit compaction notice explaining that older durable transcript
entries were summarized and that only the retained recent raw tail remains
exact.

Transcript replay MUST omit durable-storage metadata that is not useful for the
next model decision, including transcript reference handles, per-entry sequence
numbers, timestamps, agent identifiers, pane identifiers, and raw content byte
counts. Historical tool entries that do not have a bounded sanitized projection
MUST be omitted rather than replaced with placeholder text. Pending local
messages supplied to the model MUST include the message metadata needed to
identify sender, type, content type, and expiry together with a bounded copy of
the message payload.

Scheduler context supplied to the model MUST be compact when no work is queued,
running, blocked, or runnable. Explicit action-result context MUST include
functionally useful fields such as command text, status, exit code, non-default
failure or interruption flags, truncation state, and bounded output. It SHOULD
omit implementation-only observation metadata such as terminal boundary names,
opaque markers, and output byte counters unless a user-facing diagnostic needs
those details.

The harness MUST NOT include the current pane visible terminal buffer, a
bounded tail of pane history, or alternate-screen contents as default model
context. The model MUST receive pane contents only when a user or model action
explicitly requests a command result, capture result, file read, search result,
or similar observation.

The harness MUST NOT read project files directly for context assembly unless
the user has explicitly configured a non-shell integration for that purpose.
Project file content made available to the model by default MUST be obtained by
commands issued through the pane shell.

If context exceeds the configured or provider-imposed limit, the harness MUST
compact older conversation and explicit action-result context before dropping
recent user instructions or active policy instructions. Final provider request
assembly MUST NOT satisfy local context budgets by byte-slicing context block
content. When a block cannot fit, the harness MUST replace it with a compact
diagnostic summary that preserves source, label, byte count, and recovery
guidance.

### 9.7 Model Request and Response

The harness MUST send model requests only to the configured provider for the
active model profile.

The harness MUST keep permission policy, approval mode, and command-rule
enforcement runtime-owned. It MUST NOT expose raw permission preset or approval
mode labels to the model as task-planning context. If a concrete action is
denied, blocked for approval, or otherwise disallowed, the harness MUST report
that fact through the explicit action result for that action.

The model response MAY contain user-facing text, shell command proposals, local
message proposals, subagent spawn proposals, configuration change proposals,
MCP tool proposals, approval responses, or completion status.

Model interaction for one turn MAY be split into capability-decision,
action-execution, and repair requests. A capability-decision request MUST expose
only non-executing response actions, including `say` and
`request_capability`. It MUST NOT expose shell, local filesystem, network, MCP,
subagent, configuration mutation, or model-authored abort actions.

`request_capability` is a non-executing control signal from the model to the
harness. It MUST include a coarse capability name and a task-specific reason.
The harness MUST decide deterministically whether that capability may be exposed
for the current turn and MUST feed the decision back to the model as context.
The model MAY include `say` in the same capability-decision batch as one or more
`request_capability` actions; the harness MUST treat the batch as one combined
capability request. A capability-decision batch MUST reject `request_capability`
combined with executable, blocking, mutation, MCP, subagent, or configuration
actions. When any requested capability is granted, the next action-execution
request MUST expose the union of action subsets associated with granted
capabilities, the non-executing `request_capability` action, and
visible responses such as `say`. If
`request_capability` is emitted during action execution, the next
action-execution request MUST retain the already granted action surface and add
any newly granted capability action subset. When all requested capabilities are
denied during the initial capability-decision phase, the next request MUST
remain a capability-decision request and MUST NOT expose the denied executable
actions.

The baseline capability names are `respond_only`, `shell`, `network_search`,
`network_fetch`, `mcp`, `subagent`, and `config_change`.

The harness MUST parse model action proposals into Mezzanine Agent Action
Protocol version 1 objects before executing them.

Malformed model action proposals and provider-native schema failures MUST be
fed back to the model through a bounded repair request when the failure is
repairable. Repair instructions MUST be ephemeral: they MAY be sent to the
provider for correction, but MUST NOT become durable transcript or future-turn
context after a corrected response succeeds. If repair attempts are exhausted,
the turn MUST fail with an actionable diagnostic. Before persisting a terminal
controller/provider failure, the harness SHOULD make one response-only follow-up
request that exposes only `say` so the model can characterize the failure for
the user; this summary attempt MUST NOT execute tools, request capabilities,
retry work, or prevent the turn from being recorded as failed.

Recoverable action execution failures MUST be fed back to the model through a
bounded continuation when the failure is evidence the model can use to correct
the current plan. Examples include failed MCP calls, local semantic action
failures, runtime shell-dispatch or network-action loop guard failures, runtime
network request or HTTP failures, config validation failures, local message
payload validation failures, and subagent spawn validation failures. Non-zero
`shell_command` exits are ordinary command results rather than semantic-action
failures, and `apply_patch` failures are patch-context recovery rather than
bounded retry events: Mezzanine MUST preserve the failed action result and
queue model continuation without consuming failure-feedback retry budget. The
continuation context MUST include the failed action results and SHOULD include
settled successful action results from the same batch so the model can avoid
repeating work that already produced usable evidence. Correctable failures MUST
receive a configurable per-batch correction budget rather than a cumulative
per-turn budget. That budget MUST default to
`agents.action_failure_retry_limit`. Moved documents, 404s, alternate URLs,
repeated URL fetches, repeated file reads, and repeated shell commands can all
be legitimate task behavior; the runtime MUST NOT reject a repeat solely
because an identical action already occurred earlier in the turn. The model
MUST NOT repeat the same failed batch beyond the bounded correction budget.
Policy denials, user cancellations, approval rejections, and interrupted work
MUST NOT be automatically retried through this failure-feedback path.

### 9.8 Mezzanine Agent Action Protocol

The Mezzanine Agent Action Protocol version 1 is abbreviated as `maap/1`.

When the configured provider supports native structured tool calls or strict
JSON-schema output, the harness MUST request model actions using the provider's
structured mechanism.

For OpenAI Responses-compatible providers, Mezzanine MUST prefer a strict
function tool carrying one complete `maap/1` action batch, MUST force that
function tool when the provider supports forced function choice, and SHOULD keep
strict JSON-schema text output as a compatibility fallback. Function-call
arguments MUST be parsed as untrusted MAAP JSON and validated by the same
identity, schema, permission, and audit rules as any other provider-native action
batch.

For providers whose reasoning or thinking mode supports tool calls but rejects
forced function choice, Mezzanine MAY use a provider-specific MAAP strategy that
advertises the current MAAP function schema, omits forced tool selection, and
lets the provider choose the tool according to its documented thinking-mode
tool-call flow. This strategy MUST remain scoped to providers that require it
and MUST NOT weaken forced-tool behavior for providers that support it. If a
thinking-mode MAAP request returns prose or otherwise omits a structured MAAP
tool call, Mezzanine MAY retry the same provider turn once with a stricter
provider-specific fallback, such as disabling thinking mode and forcing the
MAAP function when the provider supports that combination.

Provider-native structured action schemas MUST NOT require the model to emit
runtime-owned identity or bookkeeping fields such as `protocol`, `turn_id`,
`agent_id`, `final`, or action identifiers. Mezzanine MUST stamp those fields
locally before validation, audit, transcript persistence, and action-result
generation. Compatibility parsers MAY accept those fields when they appear in
older provider output, but they MUST ignore model-provided action identifiers
and prefer locally synthesized identities.

Provider-native structured action schemas SHOULD require at least one action in
each model response and SHOULD avoid advertising no-op completion-only actions.
A provider-native final response that has user-facing text MUST use `say`; a
response that needs local filesystem inspection, discovery, process execution,
validation, or non-content path operations MUST use `shell_command`, while
file-content mutations SHOULD use `apply_patch` when the Mezzanine patch
format can represent the change. When compact provider output omits a
final-turn marker, Mezzanine MUST infer completion from the emitted actions:
visible-only actions such as `say` MAY complete the turn, while executable,
blocking, or runtime-mutating actions require continuation unless compatibility
output explicitly says otherwise.

Provider-native structured action schemas MUST distinguish MAAP action names
from pane shell commands. They MUST state that `apply_patch` is a MAAP action,
not a shell executable, and MUST NOT be invoked from a `shell_command` payload.
When the model needs a Mezzanine patch mutation, it MUST emit the
corresponding MAAP action. Local inspection, discovery, and path operations are
ordinary shell work and MUST be expressed as `shell_command` actions. The
`shell_command` schema guidance SHOULD prefer one logical shell operation per
action and SHOULD tell the model to use separate MAAP actions, rather than long
shell chains, for independent shell work that can be batched safely.

Provider-native structured action schemas SHOULD be cache-stable across normal
capability-decision, action-execution, and repair interactions when the
provider's prompt-cache behavior depends on exact tool or schema prefixes. In
that mode, Mezzanine MAY present a cache-stable collection of provider-native
tools or schemas, but the provider request MUST use provider-native selection
controls, such as forced tool choice or allowed-tool restrictions, so the active
generation surface exposes only the current allowed action set. If several MAAP
action variants are encoded inside a single function argument schema, that
selected schema MUST be narrowed to the current allowed action set rather than
advertising disallowed variants and relying on prompt text to suppress them. A
late controller instruction SHOULD still list the current allowed action types
for model context and diagnostics, but it MUST NOT be the only mechanism that
prevents disallowed actions when provider-native constraints can express the
restriction. The harness MUST validate emitted actions against the current
allowed action set before execution and MUST reject disallowed action types.
When a provider or compatibility mode cannot safely expose a broad stable tool
collection while narrowing the active generation surface, Mezzanine MAY instead
assemble provider-native schemas from the current interaction kind and allowed
action set.

Provider-native structured action schemas SHOULD reject empty user-facing text
fields and SHOULD describe `say` as conversational text only, not a substitute
for terminal execution. A non-final action batch containing only `say` and
`complete` actions MUST NOT be rejected solely because the `final` flag is
false; Mezzanine MUST display the visible text and MAY treat the turn as
complete when no executable, blocking, or runtime-mutating action remains.

When the configured provider does not support native structured actions, the
harness MUST require action proposals to appear as a single fenced JSON block
with an info string of `mezzanine-action-json`. The fenced JSON block MUST
contain exactly one `maap/1` action batch.

The internal/audit action batch MUST be a JSON object with:

- `protocol`: The string `maap/1`.
- `rationale`: A non-empty concise model-authored summary of why the complete
  listed action batch is being pursued. It MUST summarize the immediate action
  strategy, not disclose hidden chain-of-thought. It SHOULD be additive to
  recent rationale/thinking lines and MUST NOT restate the user request, global
  goal, loaded context, previous rationale, prior visible `say`, or action
  summaries. When no substantive state has changed since the previous
  rationale, the rationale SHOULD be the smallest non-duplicative execution
  delta that explains why the listed actions are next.
- `thought`: An optional longer durable model-authored work note. When present
  and non-empty, it MUST be stored in the durable assistant transcript and
  future model-facing assistant context as `thinking: ` content. It MUST NOT be
  rendered in normal-mode pane logs. It MAY be rendered in `verbose`, `debug`,
  or `trace` logs as `thinking: ` text. It SHOULD be used only for substantive
  learnings, decisions, invariants, or recovery details that would materially
  help continuation, and it MUST NOT duplicate the batch rationale, visible
  progress `say`, action summaries, recent thinking lines, secrets, hidden
  policy, or private chain-of-thought.
- `turn_id`: The active turn identity.
- `agent_id`: The proposing agent identity.
- `actions`: An array of action objects.
- `final`: A Boolean indicating whether the agent believes the turn is
  complete after the listed actions.

Provider-native compact output MUST include `rationale` and `actions`, and MUST
omit runtime-owned fields unless the provider is using a compatibility fallback
that cannot enforce the compact schema.

Each action object MUST include:

- `type`: The action type.

Each action object MAY include `rationale` when it adds action-local progress
context and does not duplicate the batch rationale, a separately visible action
summary, or response. Compact provider-native schemas MUST require the batch
`rationale` field and SHOULD omit per-action rationale fields; for auto-allow
approval, the shell `summary` MAY serve as the model-authored action-local
reason when a separate per-action rationale is absent.

Mezzanine MUST synthesize a stable turn-local action identity for every action
before producing action results or audit records. Provider-facing schemas MUST
NOT require or advertise model-supplied action identifiers. If compatibility
parsing encounters an `id` field in model output, Mezzanine MUST ignore it and
use the locally synthesized identity instead. Future action types that need to
reference another model-proposed action MUST define explicit action data for
that reference rather than relying on model-generated bookkeeping identifiers.

The baseline action types are:

- `say`: Present user-facing text with an HTTP-style content type used for
  presentation decisions.
- `request_capability`: Ask the harness controller to expose a coarse
  executable action surface for the current turn without making a user-facing
  permission request.
- `request_skills`: Reserved skill-catalog action. While model-selected skill
  actions are disabled, this action MUST NOT be exposed in provider action
  surfaces.
- `call_skill`: Reserved skill-loading action. While model-selected skill
  actions are disabled, this action MUST NOT be exposed in provider action
  surfaces.
- `shell_command`: Send shell input to the pane for local filesystem
  inspection, discovery, validation, process execution, and non-content path
  operations.
- `apply_patch`: Apply a Mezzanine patch block. The first nonblank line MUST
  be `*** Begin Patch` and the last nonblank line MUST be `*** End Patch`.
  Between those delimiters, the patch MUST contain one or more file operations:
  - add: `*** Add File: <relative-path>` followed by zero or more content lines,
    each beginning with `+`;
  - update: `*** Update File: <relative-path>`, optionally followed immediately
    by `*** Move to: <relative-path>`, then one or more hunks;
  - delete: `*** Delete File: <relative-path>` with no body.
  Each canonical update hunk MUST start with a line beginning `@@`. Text after
  the leading `@@` is an optional hunk-header anchor. Multiple ordered anchor
  fragments MAY be separated by additional `@@` markers, such as
  `@@ impl Type @@ fn method`.
  For compatibility with Mezzanine model output, implementations MAY also
  accept surrounding whitespace on patch markers and file-operation directives,
  uniformly indented patch blocks, Markdown-fenced or heredoc-wrapped patch text
  in the semantic action payload, accidental shell-style `apply_patch <<...`
  wrappers, blank hunk-body lines as empty context lines, safe `./` or
  git-diff `a/`/`b/` header path prefixes, and an omitted `@@` header on the
  first update hunk only.
  Models SHOULD still emit the canonical unwrapped `@@` hunk-header form.
  For compatibility with common diff-shaped model output, a Mezzanine update
  hunk MAY include unified-diff range metadata between the opening `@@` and a
  closing `@@`, such as `@@ -10,7 +10,8 @@` or
  `@@ -10,7 +10,8 @@ fn method`; implementations MUST NOT treat the range
  numbers as hunk-header anchors or standalone placement authority. The old-line
  number MAY be used only as a conservative disambiguation hint after the hunk
  body identifies multiple valid current-file locations. The hint MUST NOT
  override hunk-header anchors, MUST NOT override body context, and MUST be
  rejected when candidates are tied, nearly tied, or distant from the hinted old
  line.
  Hunk-header anchors MUST be matched as ordered literal substring constraints
  against the current target file; they MUST NOT replace the required hunk body
  context. Implementations MAY use distinctive Rust-like hunk-header anchors as
  structural search scopes when a bounded block can be derived conservatively;
  unresolved or ambiguous structural scopes MUST fail closed to ordinary anchor
  constraints or ambiguity diagnostics. Hunk body lines MUST begin with exactly
  one prefix character: space for context, `-` for removed text, or `+` for
  added text. A `*** End of File` line inside an update hunk marks that the final
  file has no trailing newline.
  Hunk placement MUST first attempt exact old-context matching and MAY then
  attempt Mezzanine-compatible fallback matching in deterministic order:
  trailing-whitespace-insensitive, surrounding-whitespace-insensitive, and
  limited punctuation/space-normalized matching. Normalized matching MUST be
  limited to common dash, quote, and Unicode space equivalents; it MUST NOT
  ignore case, accents, or arbitrary token differences. When a non-exact match
  is accepted, unchanged context lines MUST be preserved from the current target
  file rather than rewritten from the patch text. Implementations MAY tolerate
  blank-only current-file separator lines omitted between adjacent old hunk
  lines, including add-only insertion boundaries and replacement blocks. Skipped
  blanks before copied context lines MUST be preserved; skipped blanks before
  removed lines MAY be deleted with the removed block. The implementation MUST
  NOT skip nonblank content. Models SHOULD still include exact blank separator
  lines as explicit context instead of relying on fallback matching. If the
  accepted old-context matching mode matches more than one current-file location
  and hunk-header anchors do not disambiguate it, the action MUST fail with a
  model-correctable ambiguity diagnostic instead of choosing a location. The
  diagnostic SHOULD identify the search scope, candidate spans, useful
  hunk-header anchor state, and range-hint rejection details when available.
  When old-context matching fails, the diagnostic SHOULD conservatively report
  whether the replacement block or distinctive added lines are already present
  in the relevant target scope so the model can reconcile current file state
  instead of retrying a stale hunk.
  Raw unified diffs MUST NOT be accepted by this semantic action; agents that
  truly need a raw unified diff MUST use `shell_command` with an explicit tool
  such as `git apply`.
  Canonical paths in Mezzanine patch headers MUST be relative to the pane
  current working directory and MUST NOT be empty, absolute, contain empty path
  segments, or contain `..` traversal segments. Models SHOULD omit `./`,
  git-diff `a/`/`b/` prefixes, and `.` path segments in canonical output, but
  implementations MAY normalize those safe compatibility forms before applying
  the same safety checks. This action is the only model-facing semantic action
  for file-content mutations, including file creation, update, deletion, moves
  with content changes, append-like additions, and intentional whole-file
  replacement.
- `web_search`: Perform a web search through the Mezzanine runtime HTTP executor.
- `fetch_url`: Fetch one URL through the Mezzanine runtime HTTP executor.
- `send_message`: Send a local message through MMP.
- `spawn_agent`: Request subagent pane creation through the control endpoint.
- `config_change`: Propose a live configuration change.
- `mcp_call`: Invoke an available MCP tool.
- `complete`: Mark the turn complete.
- `abort`: Reserved for controller-owned terminal failures and legacy transcript
  parsing. It MUST NOT be exposed in provider request action schemas.

For local filesystem paths inside `shell_command` payloads, provider-facing
guidance SHOULD recommend paths relative to the repository root when the target
is inside the active repository, or relative to the pane current working
directory when no repository is active. It SHOULD also state that absolute paths
remain appropriate for targets above or outside that root, such as `/tmp` files.

A `say` action MUST include `status`, non-empty `text`, and a `content_type`
media type. The `status` value MUST be one of `progress`, `final`, or
`blocked`. `progress` indicates visible narration while the turn remains active;
it MUST NOT by itself mark the turn complete. `final` indicates that the user
goal is complete. `blocked` indicates that the model cannot continue without
user input or an external condition; it is a conversational terminal state and
MUST NOT be treated as an approval wait.

Provider-facing schemas MUST support at least `text/plain; charset=utf-8` and
`text/markdown; charset=utf-8`. Compatibility parsing MAY accept missing
`content_type` as `text/plain; charset=utf-8` and MAY normalize common aliases
such as `text/plain` and `text/markdown` to their canonical UTF-8 forms.
Provider-facing prompts and schemas MUST state that `say` content is
display-only: shell commands and Mezzanine patch blocks inside `say.text` are
not executed. They MUST direct executable terminal commands to `shell_command`
and executable `*** Begin Patch` blocks to `apply_patch`, while still allowing
`say` to display commands, patches, or diffs when the user explicitly asks to see
textual examples or explanations.

Mezzanine MUST treat `say.content_type` as presentation metadata only. The raw
`text` value MUST remain the value persisted in transcript content and copied
to paste buffers or host clipboards. When `content_type` is
`text/markdown; charset=utf-8`, Mezzanine SHOULD render markdown presentation
syntax in the pane buffer with readable terminal styling such as bold,
underline, headings, and list bullets while preserving the raw markdown for
copy operations. Markdown rendering SHOULD be parser-backed and SHOULD support
the full CommonMark syntax surface, including block quotes, lists, code blocks,
thematic breaks, links, images, raw HTML passthrough, and inline emphasis. It
SHOULD also support common GitHub-style extensions used by Mezzanine output,
including tables, task-list markers, strikethrough, footnotes, and heading
attributes. Inline code spans and markdown table alternation SHOULD use
foreground-only neutral grey styling whose lightness is selected from the
active theme surface for readability. Markdown table presentation SHOULD use
Unicode box-drawing separators while preserving raw pipe-table markdown for copy
operations. Every rendered markdown
block SHOULD be visually framed above by one synthetic markdown thematic-break
row: the copy representation of the frame row SHOULD be `***`, and the
presentation SHOULD use Unicode box-drawing divider characters across the
smaller of the pane content width or 120 display cells. Markdown presentation
rows SHOULD wrap at that same width, and continuation rows SHOULD preserve
speaker, quote, list, and code indentation. Non-table markdown rows SHOULD wrap
at the nearest whitespace boundary before that width; when no whitespace
boundary exists in an overflowing segment, Mezzanine SHOULD leave the segment
intact and rely on normal terminal soft wrapping instead of inserting a hard
split. Table rows SHOULD instead wrap only when they exceed the pane content
width so Unicode table structure remains inspectable on wide terminals.
Copy metadata for wrapped markdown presentation rows MUST preserve the original
raw markdown source line and MUST NOT insert presentation-only continuation
newlines into copied output. Selecting a rendered table that visually wrapped
MUST copy the original pipe-table markdown rather than the Unicode table
presentation.
Unknown `say` media types MUST fall back to plain text presentation while
retaining the declared media type in action traces.
Runtime-generated agent shell displays that declare
`text/markdown; charset=utf-8`, including informational slash-command output,
MUST use the same presentation renderer and raw-copy preservation semantics as
markdown `say` actions. Such command displays MUST render as standalone command
blocks rather than assistant `say` rows: they MUST NOT include an `agent>`
speaker label, and their transcript gutters SHOULD use the neutral foreground
color used for white frame text.

A `say` action MUST NOT be used to present a shell command that the model
intends Mezzanine to execute.

A `request_skills` action MUST NOT require arguments. While model-selected skill
actions are disabled, Mezzanine MUST NOT expose this action in model-facing
provider schemas or allowed-action surfaces. If exposed by an implementation
profile, it MUST return the effective skill catalog visible to the active pane
as an action result. The catalog MUST include each skill's `name`,
`description`, and source scope, and it MAY include diagnostics for skipped
invalid skill directories. It MUST NOT load full skill instruction bodies.

A `call_skill` action MUST include:

- `name`: The name of the skill to load.
- `additional_context`: Optional caller-authored context to append to the
  loaded skill body.

A `call_skill` action MUST NOT be exposed in model-facing provider schemas or
allowed-action surfaces while model-selected skill actions are disabled. If
exposed by an implementation profile, it MUST resolve the skill from the
effective skill catalog, load the complete `SKILL.md` text, and return that
text as action-result context. When `additional_context` is present and
non-empty, Mezzanine MUST append it to the returned skill text under a clearly
labeled markdown section named `Additional context`. Unknown, invalid,
unavailable, or unreadable skills MUST fail with a model-correctable action
result. Skill actions MUST NOT grant additional capabilities, permissions,
filesystem access, network access, MCP access, or subagent access.

A `shell_command` action MUST include:

- `summary`: Non-empty concise user-facing progress text describing what will
  happen or what command output will be used. It MUST NOT include the raw shell
  command unless the command itself is the user's requested user-facing output.
- `command`: The exact shell input proposed for execution.

Mezzanine MUST reject model-authored `shell_command.command` values containing
unquoted heredoc or here-string redirection tokens (`<<`, `<<-`, or `<<<`).
This restriction applies only to the model-authored command payload; Mezzanine's
own transaction wrapper MAY use shell-specific mechanisms internally. Rejection
MUST produce a repairable diagnostic that instructs the model to use
`apply_patch` for file content changes and ordinary shell commands for local
inspection, validation, directory, move, or delete operations.

Mezzanine MUST also reject model-authored `shell_command.command` values that
attempt to invoke a MAAP semantic action name as a pane shell program. The
diagnostic MUST explain that the named operation is a MAAP action, not a shell
command, and MUST instruct the model to emit that action instead. This
restriction MUST NOT prohibit using those words as ordinary quoted or unquoted
arguments to real shell tools such as `rg apply_patch`.

A `shell_command` action MAY include:

- `interactive`: Whether the command is expected to require interactive user
  input or take over the terminal. Omitted values default to `false`.
- `stateful`: Whether the command intentionally changes pane shell state.
  Omitted values default to `false`.
- `timeout_ms`: Requested timeout or `null`. Omitted values use the configured
  default timeout.

For compatibility with provider-native outputs that do not yet follow the
current schema, Mezzanine MAY synthesize a missing `shell_command.summary` from
the action's non-empty `rationale` before validation and display. An explicitly
empty or non-string `summary` MUST remain invalid.

Semantic actions MUST include only the type-specific data needed to perform the
operation. For shell-backed semantic local actions, Mezzanine MUST synthesize
the concrete pane shell command, policy-classification command, local action
identity, timeout defaults, and user-facing summary. `apply_patch` is the only
baseline shell-backed semantic local action. For mutating filesystem semantic
actions, Mezzanine MUST also synthesize the bounded user-facing change preview
described in Section 7.4, so models do not need to generate or format diffs
themselves. Mutating filesystem semantic actions that carry generated content
through pane shell input MUST encode that content in bounded physical shell
lines that remain comfortably below common PTY canonical-line limits.
Non-stateful shell-action stdout and stderr intended for model context MUST use
a tagged printable base64 transport in the reverse direction before Mezzanine
decodes them into action results. For generated shell transactions with large
command or patch payloads, Mezzanine MUST start a shell-side receiver in the
target pane before sending the encoded payload bytes. Those payload bytes MUST
be sent through the pane shell as bounded data after the transaction-start
marker, not embedded in the wrapper source that the interactive shell parses.
Runtime pane-input writers MUST surface a bounded error instead of waiting
indefinitely when the PTY stops accepting bytes.
All Mezzanine-owned PTY input, including user paste bytes, terminal-command
forwarding, agent shell transactions, readiness probes, bootstrap probes, and
semantic file-action payloads, MUST pass through a bounded per-pane input
transport. The transport MUST split input into conservative byte chunks, MUST
report partial byte progress before retrying any unsent remainder, MUST keep
unsent remainders ahead of later queued input for the same pane, MUST reject
zero-byte write progress as a bounded failure, and MUST leave enough
interleaving opportunity for PTY output reads and lifecycle events between
large-input chunks. A failed chunk MUST NOT cause previously accepted bytes to
be sent again.
The `apply_patch` semantic action MUST synthesize an explicit short timeout
instead of inheriting the general shell-command default, so malformed or
blocked patch application fails quickly enough for the model to repair the
turn. Generated `apply_patch` commands MUST NOT use shell heredoc or here-string
redirections internally. Mezzanine patch blocks MUST be the only accepted
semantic patch format. The patch helper MUST resolve symlinks fully before
making filesystem safety decisions. It MUST reject targets whose resolved path
escapes the pane current working directory or resolves to a non-regular
filesystem node, and it MUST do so before reading from or writing to that node
so special filesystem nodes cannot stall the patch helper. When a symlink
resolves to a regular file inside the pane current working directory, the
helper MAY patch the resolved target, but it MUST re-resolve the path and verify
the preimage bytes immediately before writing final bytes so races or symlink
retargeting fail safely.
Shell-backed semantic local actions MUST use the pane shell only as the
filesystem primitive needed to operate in the pane's actual environment,
including remote panes. Generated commands for built-in semantic actions MUST
stay within the baseline Unix-like shell, coreutils-style commands, `sed`,
`grep`, `find`, and explicitly discovered developer tools required by the
operation. They MUST NOT embed Python or another general-purpose script runtime
for built-in buffering, truncation, diff formatting, or result shaping.
Mezzanine MUST perform those higher-level transformations natively after the
bounded pane output returns.

`web_search` and `fetch_url` are runtime-network semantic actions. Mezzanine
MUST service them directly through its runtime HTTP executor, MUST NOT dispatch
a pane shell command for them, and MUST still evaluate and audit them as
external network activity. `fetch_url` MUST accept only `http://` and `https://`
URLs; local paths and `file://` URLs MUST be inspected with `shell_command`,
not network actions.
These actions MUST be selected only when the user asks for web or URL content;
they MUST NOT be used as a generic source of local generated data such as random
strings, timestamps, or test fixtures. Runtime network actions MUST apply a
default response-body byte bound before returning content to the model, MUST
apply a hard maximum even when an optional `max_bytes` request is larger, and
MUST bound model-facing action-result context independently from durable
transcript storage. Once the `network_fetch` capability has been granted,
`fetch_url` targets MUST be validated by normal URL parsing, permission policy,
and response-size bounds. The runtime MUST NOT impose a total per-turn network
action count cap, MUST NOT impose an additional active-context URL-provenance
requirement, and MUST NOT reject a repeat solely because the same URL was
fetched earlier in the turn.

Semantic action required fields are:

- `apply_patch`: `patch`.
- `web_search`: `query`.
- `fetch_url`: `url`.

Compatibility parsers MAY accept optional semantic action fields for patch
strip count, expected content hash, output format hints, or network response
bounds. Provider-native compact schemas SHOULD steer local discovery and
bounded file inspection toward `shell_command` rather than exposing separate
filesystem action types.

The model MUST NOT be required to declare filesystem, network, credential,
process-control, privilege, destructive, or unknown effects in the MAAP action
object. Such classifications are not authoritative when supplied by the model.
Mezzanine MUST independently classify shell effects for approval, audit, and
display, and MUST route incomplete or unknown classification through the active
approval policy.

A `send_message` action MUST include `recipient`, `content_type`, and
`payload`.

When a MAAP `send_message` action uses the common `text/plain` shorthand,
Mezzanine MUST normalize it to the canonical MMP text content type
`text/plain; charset=utf-8` before validating and delivering the message.

A compact `spawn_agent` action MUST include requested role and task prompt.
Placement, cooperation mode, read scopes, and write scopes MAY appear in
compatibility output or future expanded schemas, but provider-native compact
schemas SHOULD let Mezzanine apply runtime placement, policy inheritance, and
scope defaults.

A `config_change` action MUST include setting path, operation, and value,
unset marker, or reset marker. Provider-facing `config_change` schemas MUST
include annotated configuration path guidance derived from the implementation
config schema, including supported path patterns, dynamic identifier syntax,
scalar value types, and allowed operations. Each path-pattern annotation MUST
include the setting purpose, accepted value type, required value format,
dynamic segment meaning when applicable, and supported operations. The schema
MUST state that configuration mutation is limited to scalar paths supported by
the runtime mutation planner and that the runtime still performs full config
validation before applying the change. A `reset` operation MUST remove the
explicit value from the selected persistence layer so the effective value falls
back to a lower-precedence layer or the implementation default.
Every `config_change` action MUST follow the active approval policy using the
same approval mechanism as other privileged model actions. If the current policy
does not allow the action immediately, the action MUST enter blocked approval
state and require explicit primary-client approval with `/approve` before
application. After approval or policy-based acceptance, `config_change` actions MUST
persist to the runtime-selected user configuration target and MUST take effect
immediately in the live session. A `config_change` that sets `theme.active` MUST
use the same runtime behavior as `set-theme`, including selected-theme
validation and materialization of aliases and color slots into the persisted and
live configuration state.

An `mcp_call` action MUST include server identity, tool name, and JSON
arguments. It MUST NOT reference disabled, unavailable, or session-blacklisted
MCP servers.

The harness MUST validate every action against the `maap/1` schema before
permission evaluation. Unknown action types, missing required fields, duplicate
local action IDs after synthesis, or references to unavailable capabilities
MUST be rejected before execution.

Before recording a terminal turn failure for malformed or schema-invalid MAAP,
the harness MAY issue a bounded ephemeral repair request to the same provider.
The repair request MUST include the validation diagnostic and a bounded excerpt
of the invalid provider output, MUST require the same turn and agent identity,
and MUST NOT execute any action from the invalid response. If the provider
returns a valid corrected batch, Mezzanine MUST execute and transcript only the
corrected response against the original durable model context; the repair
instruction and invalid response MUST NOT become transcript entries or
future-turn context. If repair attempts are exhausted or the provider repair
request fails, the final malformed response MUST be surfaced and recorded as a
failed turn diagnostic.

Local action IDs MUST be treated as idempotency keys within a turn. Retrying
provider requests MUST NOT execute the same accepted local action ID twice
unless explicitly approved by policy.

The harness MUST produce an internal `maap/1` action result for every syntactically
identifiable accepted, rejected, blocked, denied, executed, failed, cancelled,
timed-out, or interrupted action. Batch-level parse or schema failures that
prevent identifying an action ID MUST be recorded as malformed response errors
in the agent transcript. Action results MUST be appended to the agent
transcript. Before the model is asked to continue from an action, Mezzanine
MUST supply assistant context for the provider response being continued from,
including rendered thinking/rationale lines, and a compact model-facing
projection of the result that preserves the action identity, action type,
status, error code/message, approval prompt when blocked, command line,
exit/timeout/signal state, truncation state, and bounded cleaned output needed
for the next decision. The model-facing projection MUST
omit nulls, empty arrays/objects, runtime-owned identity duplicates, wrapper
traffic, and audit-only fields such as matched policy rules, approval audit
objects for completed actions, pane-dispatch booleans, generated wrapper
metadata, and policy-command mirrors.

An action result MUST be a JSON object with:

- `protocol`: The string `maap/1`.
- `turn_id`: The active turn identity.
- `agent_id`: The proposing agent identity.
- `action_id`: The local action identity synthesized by Mezzanine.
- `action_type`: The action type.
- `status`: One of `rejected`, `blocked`, `denied`, `running`, `succeeded`,
  `failed`, `cancelled`, `timed_out`, or `interrupted`.
- `content`: An array of model-readable content blocks. Text content blocks
  MUST use the shape `{ "type": "text", "text": string }`.
- `structured_content`: A JSON object with action-type-specific result data,
  or `null`.
- `is_error`: A Boolean indicating whether the result represents an execution
  error or denied action that the model may be able to correct.
- `error`: A structured error object or `null`.

Exactly one of the following MUST be true for an action result:

- `status` is `succeeded` or `running`, `is_error` is false, and `error` is
  `null`.
- `status` is `blocked`, `is_error` is false, `error` is `null`, and
  `structured_content.approval` describes the pending approval request.
- `status` is `rejected`, `denied`, `failed`, `cancelled`, `timed_out`, or
  `interrupted`, `is_error` is true, and `error` is non-null.

The `error` object MUST contain:

- `code`: A stable string error code.
- `message`: A concise human-readable message.
- `data`: A JSON object with additional structured detail, or `null`.

The baseline error codes MUST include `invalid_action`, `unavailable_capability`,
`approval_required`, `approval_denied`, `policy_forbidden`,
`permission_unknown`, `command_parse_unknown`, `command_failed`,
`command_timeout`, `user_interrupted`, `transport_error`, `mcp_protocol_error`,
`mcp_tool_error`, `spawn_failed`, and `internal_error`.

For shell-backed local actions, including `shell_command` and semantic local
actions lowered by Mezzanine, `structured_content` MUST include:

- `kind`: The original MAAP action type.
- `summary`: The user-facing action summary supplied by the model or
  synthesized by Mezzanine for semantic local actions.
- `command`: The exact shell input sent or proposed for pane execution.
- `sent_to_pane`: Whether any input was sent to the pane shell.
- `stateful`: Whether the action intentionally changes pane shell state.
- `approval`: Approval state, request identity, decision, and scope when
  applicable, or `null`.
- `matched_rules`: The command prefix rules that affected the decision.
- `terminal_observation`: Bounded observed terminal text, prompt or boundary
  detection state, exit status when known, signal when known, timeout state,
  and whether output was truncated. For model follow-up context, observed text
  MUST be the cleaned command-output view after removing terminal styling,
  prompt repaint text, and Mezzanine-owned wrapper echo.

For `spawn_agent` actions, `structured_content` MUST include the created pane
identity, spawned agent identity, cooperation mode, read scopes, write scopes,
and initial delivery state when spawning succeeds. If spawning is blocked or
fails, it MUST include the requested placement and reason.

For `send_message` actions, `structured_content` MUST include recipient
identity, message identity when assigned, delivery status, and any protocol
error returned by the local message passing protocol.

For `config_change` actions, `structured_content` MUST include the setting
path, operation, validation result, applied layer, and whether persistence was
requested or completed. For reset operations, it MUST also make clear that the
explicit override was removed rather than replaced with a literal default value.

For `mcp_call` actions, `structured_content` MUST include server identity,
tool name, arguments after validation, timeout state, and the MCP result or
error. MCP protocol errors MUST be represented with `mcp_protocol_error`.
Tool execution errors returned by an MCP tool MUST be represented with
`mcp_tool_error` and MUST preserve the tool's model-readable content when
policy allows.

### 9.9 Action Gating and Execution

Before executing a model-proposed action, the harness MUST evaluate the active
permission policy.

Permission evaluation MUST NOT rely on model-supplied effect claims. For shell
commands, Mezzanine MUST combine prefix-rule classification, shell syntax
classification, scope checks, and read-only preflight probes when needed.

Actions requiring approval MUST be presented to the primary client with the
requesting agent identity, pane identity, action type, action content, and
expected effect when known.

Actions that are blocked, denied, or failed before execution MUST produce a
copyable user-visible pane-buffer diagnostic that identifies the action type,
the blocking or failure reason when known, and SHOULD omit exact shell command
text from the pane buffer unless `/log-level verbose` or `/log-level trace` is
enabled. Shell
commands accepted by policy but delayed while pane readiness is verified MUST
display their MAAP summary by default and MUST defer the planned command preview
until the command is actually dispatched. Successfully dispatched shell commands
MUST display their MAAP summary and MUST render the actual command line sent
through the pane shell in normal mode using the bounded command preview rules.

If the primary client rejects an action, the harness MUST record the rejection
and MUST return a `maap/1` action result to the agent.

Shell actions MUST be executed by sending input to the pane shell. The harness
MUST NOT execute local shell actions through a hidden host-side command runner.

For non-interactive shell actions, the harness SHOULD send a complete command
followed by the pane's configured submit sequence.

For interactive shell actions, the harness MUST make the interactive state
visible to the user and MUST allow interruption.

Local message actions MUST be sent through the local message passing protocol.

Subagent spawn actions MUST be submitted to the Mezzanine control endpoint.
The control endpoint MUST create a pane shell for the subagent in a same-group
dedicated subagent window before the subagent begins work.

Configuration change actions MUST go through the same validation path as the
configuration shell.

### 9.10 Observation and Iteration

After executing a shell action, the harness MUST observe terminal output until
one of the following occurs:

- The pane reaches terminal quiescence according to the configured idle period.
- A configured prompt detector identifies that the shell is ready.
- The command exits and its status is observable.
- The user interrupts the command.
- The action reaches its configured timeout.

The harness MUST append observed output, exit status when known, timeout state,
and user interruptions to the turn record.

If observation ends because a Mezzanine transaction times out or a pending
shell dispatch is found stranded without a live transaction, the harness MUST
settle the affected action or requeue it according to pane readiness. A turn
MUST NOT remain in `running` solely because the runtime missed prompt metadata
or a transaction-end marker.

The harness MUST summarize the observation in the corresponding `maap/1`
action result before asking the model to continue.

When an executed action fails with model-correctable evidence such as a tool
execution error, Mezzanine MAY feed the failed action result back to the model
and ask for a corrected next step within the same turn. This failure-feedback
continuation MUST be bounded per stable failed-action signature by
`agents.action_failure_retry_limit`, MUST NOT apply to non-zero
`shell_command` exits, `apply_patch` failures, user rejections, approval
denials, policy denials, command timeouts, or user cancellations, and MUST
preserve the failed action result for audit, diagnostics, and model context.
If the bounded correction attempts for each model-correctable failed action are
exhausted, Mezzanine MUST settle the turn as failed.
When one model-correctable action fails before later shell-backed sibling
actions have reached the pane shell, those inactive unsent siblings MUST NOT
make recovery unavailable. Mezzanine SHOULD abandon the unsent sibling actions,
feed back the settled failed action result, and queue the corrective provider
continuation. Active sibling work that has reached an external process,
subagent, approval, or other runtime boundary MAY still block correction until
that work settles or is explicitly cancelled.
Async runtime event handling MUST emit the provider-dispatch side effect for a
newly queued corrective continuation promptly after applying the failed action
event, rather than relying solely on a later provider-poll timer.
Final failed-turn diagnostics SHOULD distinguish unavailable recovery from
exhausted bounded recovery attempts so users can tell whether the model was
given a correction opportunity. When recovery is unavailable, the diagnostic
SHOULD include the reason, such as pending or blocked sibling action results, a
non-correctable policy/user/runtime boundary, missing model-correctable action
evidence, or a correction budget that remained unused because no correction
continuation was queued.
When the failed action is a filesystem mutation, the recovery prompt MUST tell
the model that no successful mutation has occurred from the failed action
result and MUST prohibit success claims until a later mutation action succeeds
and is verified. It MUST state that file reads after a failed mutation prove
only current file state, not that the attempted edit landed.
When the failed action is `apply_patch` and the diagnostic indicates a hunk
mismatch or patch-application failure, the recovery prompt SHOULD tell the
model not to replay the same patch, SHOULD direct it to inspect the affected
file or nearby context first with a bounded `shell_command`, SHOULD explicitly
name that shell inspection as the next likely action, SHOULD direct the model to
inspect any reported line number or anchor location, and SHOULD steer the
corrected attempt toward a smaller fresh Mezzanine patch block against the
current file contents. The hunk mismatch diagnostic itself SHOULD include a
model-facing next-step hint with the same inspect-before-retry and
narrower-patch guidance. When the diagnostic indicates an unsafe patch path, the
recovery prompt MUST include the best-known pane current working directory and
MUST tell the model to reissue the patch with Mezzanine patch header paths relative
to that directory.
When generated semantic action output contains no actionable diagnostic after
wrapper filtering, Mezzanine SHOULD synthesize a concise fallback diagnostic
that still names the likely next action instead of presenting only a generic
failure label.

The harness MAY continue the model round trip with the new observations until
the agent returns completion status, requests user input, is interrupted, or
hits a configured limit.

The harness MUST expose long-running action status to the user.

### 9.11 Errors, Retries, and Persistence

Provider errors, malformed responses, permission denials, command timeouts, and
transport failures MUST be recorded in the agent transcript.

The harness MAY retry provider requests according to provider policy and local
configuration. Retries MUST NOT repeat shell actions unless the user or policy
explicitly permits repetition.

Retryable provider transport failures, provider/controller errors that
explicitly instruct retry, rate limits, and 5xx responses MUST reach the
runtime retry scheduler before Mezzanine asks the provider to characterize the
failure for the user. A failure-summary response MUST NOT convert a retryable
provider failure into a terminal failed turn before the configured retry
attempts are exhausted.

Every completed turn MUST persist enough state to resume the conversation,
audit actions, and explain the final result after detach and reattach.

The persisted turn record MUST include redaction metadata for any secret values
that were omitted or masked.

## 10. Agent Capabilities

### 10.1 Baseline Capabilities

Agents MUST be able to receive natural-language instructions from the user.

Agents MUST be able to inspect the current pane terminal buffer.

Agents MUST be able to issue shell commands to gather context, search files,
read files, edit files, run programs, inspect version control state, build,
test, lint, format, and debug.

Agents MUST be able to explain observed code, command output, terminal state,
and errors.

Agents MUST be able to produce and apply code changes using commands available
inside the pane shell.

Agents MUST be able to run verification commands when authorized by policy.

Agents MUST support task planning when requested by the user.

Agents MUST support session continuation after the agent shell is hidden and
shown again.

Agents MUST support model-generated conversation compaction or summarization
when explicit user action or provider feedback shows that context reduction is
needed. The agent harness MUST NOT block prompt submission or provider request
assembly solely because a local fallback estimate predicts high context
pressure. Compaction MUST use the same bulk shape regardless of trigger: older
compacted context MUST be represented by one memory-style summary block at the
start of model-visible context, followed by uncompacted recent blocks retained
as a raw tail. Local context reduction MUST prefer compact summaries over
partial block truncation so the model does not reason from silently incomplete
context. Compaction MUST retain a bounded raw recent transcript tail alongside
the summary so exact recent references remain available after context reduction.
The raw tail size MUST follow `agents.compaction_raw_retention_percent`, which
defaults to retaining approximately the newest 10% of the active model context
budget by estimated replay word count.
If the provider rejects a request because the input context exceeds a
provider or model limit, Mezzanine MUST treat that failure as recoverable while
the turn remains running, MUST NOT ask the provider for a failure-summary
response with the same oversized context, and MUST locally compact or omit
recoverable active-turn context before retrying within the bounded provider
retry policy. This recovery MUST preserve the durable turn and latest user
instruction; it SHOULD prefer compacting or omitting recoverable action-result,
tool, transcript, and other explicit observation context over dropping recent
user steering wholesale. If the provider still rejects the retried request,
Mezzanine MUST continue provider context-limit recovery with successively
smaller explicit compaction budgets until the provider accepts the request,
the bounded retry policy is exhausted, or no further recoverable compaction can
change the active-turn context.

Agents MUST support optional routing model sizing. When routing
is enabled for a pane, agent, or subagent, the first provider step for each new
turn MUST be a bounded classification request to the configured auto-sizing
router model. The router decision MUST select one configured size bucket
(`small`, `medium`, or `large`) and one allowed reasoning effort. Mezzanine MUST
apply the selected model profile and reasoning effort only to that turn, and
MUST restore the normal user-selected model profile after the turn completes,
fails, is interrupted, or is cancelled. Auto-sizing MUST NOT mutate persistent
model-profile overrides.

Agents MUST support a permission or approval model that can restrict command
execution, file mutation, network use, and destructive actions.

Agents MUST support permission presets, including at least `read-only` and
`auto`.

Agents MUST support approval policy values `ask`, `auto-allow`, and
`full-access`.

User configuration MAY select any supported approval policy as the default,
including elevated defaults such as `auto-allow` and `full-access`. Restored
agent-session metadata MUST NOT narrow the configured default merely because an
older checkpoint recorded an inherited effective policy.

Agents SHOULD support code review workflows that inspect current changes and
report prioritized, actionable findings without modifying files.

Agents MUST support project instruction files. By default, Mezzanine MUST
recognize `AGENTS.md` as the project instruction filename. Provider-visible
prompting MUST embed applicable project instruction contents as active
repository instructions in the system prompt, rather than describing the files
as optional reference material or asking the model to rediscover them by name.

Mezzanine MAY support additional configured instruction filenames for
repository-specific guidance. Additional filenames MUST be treated as ordinary
project content discovered through the same trust, precedence, and untrusted
content rules as `AGENTS.md`.

Agents SHOULD support multimodal user-provided context, such as screenshots or
design images, when the configured model provider supports it.

Agents SHOULD support web search or web retrieval when configured and
authorized. Web-derived content MUST be treated as untrusted input.

Agents MUST support Model Context Protocol servers as an external integration
mechanism.

External connectors or tool protocols MUST be explicitly configured, visible to
the user, and subject to the same permission model as other agent actions.

### 10.2 Shell-Only Local Interaction

An agent MUST use the pane shell for local file reads, file writes, command
execution, process inspection, package management, version control operations,
and other local system interactions.

This shell-only rule applies to the agent's native local interaction path. MCP
servers and other explicitly configured connectors are external integrations,
not hidden local shell tools. An agent MAY request an MCP tool call only through
the configured external-integration path, and Mezzanine MUST evaluate that call
under the permission and audit rules for external integrations.

If an agent edits a file, it MUST do so by issuing shell commands or invoking
programs available through the pane shell.

The `/init` slash command is the only baseline exception to the shell-only file
mutation rule. `/init` is a Mezzanine-owned project-instruction scaffold
operation, not an agent-selected local edit. It MAY create `AGENTS.md` through
the runtime control plane using the active pane's current working directory,
provided the operation is visible in the agent shell result, reports the target
path, reports whether a scaffold was created or already existed, writes only
the default scaffold content, and does not overwrite an existing file. Any
customization or subsequent edits to that scaffold MUST go through the pane
shell or an explicitly configured external integration.

The baseline edit mechanisms available to an agent MUST include shell
redirection, `sed`, `awk`, `cp`, `mv`, and `python3` or `python`. Agents MAY
use `patch`, `git apply`, project-specific formatters, or dedicated edit tools
only after discovering them through the pane shell.

Agents SHOULD prefer deterministic, reviewable edit commands. For multi-line
or structural edits, agents SHOULD prefer `python3` or `python` scripts
executed through the pane shell when available. For search, agents SHOULD
prefer `rg` when discovered and fall back to `grep` or `find` when `rg` is not
available.

If an agent searches the local workspace, it MUST do so by issuing shell
commands or invoking programs available through the pane shell.

If a needed operation cannot be performed through the pane shell, the agent
MUST report the limitation rather than silently using a hidden local tool.

### 10.3 Subagents

Agents MUST be able to spawn other agents when authorized by policy.

A spawned agent MUST be associated with a pane and MUST have its own pane
shell.

Agents MUST spawn subagents only when the user explicitly requests
multi-agent work or the active policy explicitly authorizes autonomous
delegation.

A spawn request MUST create the new agent in a dedicated subagent window in the
same window group as the controlling pane. Subagent spawn MUST NOT focus that
window or otherwise move the primary user's active window or pane.

Each spawned subagent MUST receive a human-readable display name chosen at
random from a built-in pool of short common first names. The pool SHOULD be
large enough to keep concurrent subagent groups visually varied. The display
name MUST be unique among currently active subagents when the subagent is
spawned. The canonical agent id MUST remain the stable protocol identity, and
protocol responses and parent coordination messages that expose a display name
MUST also expose the canonical agent id.

Subagent pane titles MUST be set to the spawned subagent's display name.
Generated subagent window names MUST compactly reflect the display names of the
subagents currently hosted in that window, while remaining short enough for
normal window lists and status bars. Generated names MUST NOT include a fixed
`subagents:` prefix. A generated subagent window name for a window hosting
exactly one live subagent MUST include that subagent's display name even if the
pane title later changes. Explicit user or agent window renames MUST prevent
later generated subagent-window refreshes from overwriting the chosen name.

Subagent windows MUST use an even, self-rebalancing pane layout. When a
subagent pane is added to or removed from such a window, the remaining panes
MUST be redistributed evenly along the active even-layout axis or, for
`even-grid`, across both rows and columns.

When choosing the layout for a subagent window, Mezzanine MUST evaluate the
window's current cell size and the next pane count. It MUST prefer
`even-vertical` while every resulting pane would keep at least 40 columns and 8
rows. If another vertical split would fall below that preferred size, Mezzanine
MUST choose the usable `even-horizontal` or `even-grid` layout that best
preserves those preferred dimensions. A candidate layout is usable for adding a
pane to an existing subagent window only when every resulting pane would keep at
least 24 columns and 4 rows.

The default maximum number of subagent panes in one subagent window MUST be 4.
The `agents.max_subagent_panes_per_window` configuration setting MUST allow the
user to choose a positive per-window limit. Once all subagent windows in the
controlling pane's group have reached this limit, or no existing subagent window
can accept another pane while preserving the usable subagent pane size, Mezzanine
MUST create another dedicated subagent window in that same group for the next
subagent.

The default maximum number of direct subagents that a root pane agent may keep
active MUST be 4. The `agents.max_root_subagents` configuration setting MUST
allow the user to choose a positive direct-child limit for root pane agents.

The default maximum number of direct subagents that a spawned subagent may keep
active MUST be 2. The `agents.max_subagents_per_subagent` configuration setting
MUST allow the user to choose a positive direct-child limit for spawned
subagents.

The default maximum nested subagent delegation depth MUST be 2. Root pane
agents are depth 0, subagents spawned directly by a root pane agent are depth 1,
and subagents spawned by those children are depth 2. The `agents.max_depth`
configuration setting MUST allow the user to choose a positive maximum depth.
A subagent at the configured maximum depth MUST be allowed to finish its own
task but MUST NOT spawn additional subagents.

The legacy `subagent_placement` setting and placement fields on spawn requests
MAY be accepted as compatibility hints, but Mezzanine MUST normalize subagent
placement to dedicated same-group subagent windows and MUST NOT spawn a
subagent into the controlling pane's existing window.

Each spawned subagent pane MUST enter pane-local agent mode immediately after
creation, and its agent prompt MUST be visible inside the spawned pane before
the child turn begins executing.

Spawned agents MUST inherit applicable session, policy, configuration, and
project instruction context unless the user or configuration specifies a
different context.

Spawned agents MUST inherit the effective authorization scope of their parent
agent. Mezzanine MUST NOT use model-proposed or profile-default
`read_scopes`/`write_scopes` to create a narrower child sandbox when the parent
agent is not already scope-constrained. When the parent is a scoped subagent,
the child MUST inherit the parent's cooperation mode, read scopes, and write
scopes exactly unless the primary user explicitly approves a scope change.
Spawn-request scopes MAY be retained as task-intent metadata, but they MUST NOT
broaden or narrow the child's enforceable authority by themselves.

Spawned agents MUST be discoverable through the local message passing protocol.

The parent agent MUST receive status and final output from spawned agents
through the local message passing protocol. The controlling pane MAY receive
visible status lines so the user can monitor subagent work without switching
away from the controlling window. Spawn status lines SHOULD name the child,
pane, role, cooperation mode, and a brief summary of the delegated task. When a
spawned agent finishes, the controlling pane SHOULD log a concise completion
status naming the child and final state, but final subagent output MUST NOT be
rendered directly into the parent pane as parent-agent output. The parent agent
MUST decide whether and how to analyze, summarize, or act on child results.

The `agents.subagent_wait_policy` setting MUST default to `join`. In `join`
mode, a parent `spawn_agent` action MUST remain a running action until the
spawned child delivers its final task result, and the parent provider turn MUST
not continue or complete before joined child results are available as action
result context. A joined parent turn MUST release global scheduler capacity
while waiting so child agents can run even when the concurrency limit is low.
In `detach` mode, the parent may continue immediately after spawn creation
while status updates may route to the controlling pane and final output routes
to the parent agent through the local message passing protocol.

When a subagent completes successfully, Mezzanine MUST deliver the final result
before closing the successful subagent pane. If the subagent fails, is
interrupted, or exits unexpectedly, its pane MUST remain available for
inspection unless the user explicitly closes it.
When closing a successful subagent pane removes the last pane in that dedicated
subagent window, Mezzanine MUST close the whole subagent window and prune it
from future subagent placement candidates.

Subagent pane creation MUST be requested through the control endpoint. Subagent
discovery, task messages, progress messages, and final result messages MUST use
the local message passing protocol.

Each subagent spawn request MUST declare a cooperation mode.

The baseline cooperation modes are:

- `explore-only`: The subagent may read, search, inspect, and report, but MUST
  NOT modify files or persistent state.
- `owned-write`: When an inherited parent write scope exists, the subagent may
  modify only that inherited file or directory scope. Unscoped parents delegate
  under the normal session permission policy without a child-specific scope
  fence.
- `coordinated-write`: When an inherited parent write scope exists, changes
  outside that inherited scope MUST be returned to the parent agent for review
  and application rather than applied directly.
- `serial-write`: When an inherited parent write scope exists, the subagent may
  share that scope with another agent only while holding an explicit session
  lock for the scope.
- `unrestricted`: The subagent may write according to the parent permission
  policy without additional scope restrictions. This mode MUST require explicit
  user approval unless the session policy allows unrestricted subagent writes.

Spawned subagent panes SHOULD start in the controlling pane's current working
directory. Scope checks that resolve relative command effects MUST use the
best-known pane working directory and MUST update when shell integration later
reports a more precise working directory.

Spawn requests MAY include requested read scopes and write scopes so parent
agents can describe task intent. Enforceable subagent scopes MUST be derived
from the parent agent's current scope, not from model-emitted request fields or
profile defaults. When enforceable write scopes exist, they MUST be paths, path
prefixes, or repository-relative globs with deterministic matching.

Mezzanine MUST NOT reject a subagent spawn solely because model-emitted
requested scopes overlap active write-scope metadata. If policy implements
concurrent write coordination, it MUST coordinate inherited effective scopes and
MUST NOT create a child restriction that the parent did not already have.

Subagents MUST announce their effective cooperation mode, inherited read scopes,
and inherited write scopes through the local message passing protocol presence
data. Unscoped children MUST report empty read and write scope arrays.

If a scoped subagent discovers that its task requires work outside its inherited
write scope, it MUST request scope expansion from the parent agent or user
rather than silently editing outside scope.

Mezzanine MUST define built-in subagent roles equivalent to:

- `default`: General-purpose fallback agent.
- `worker`: Execution-focused agent for implementation, fixing, and production
  changes.
- `explorer`: Read-heavy codebase exploration agent.

Custom subagent profiles MUST support name, description, developer
instructions, model profile override, permission override, MCP server override,
shell environment override, default cooperation mode, and default read and
write scopes. Default read and write scopes MUST be treated as task metadata
unless the profile is applied to an already-scoped parent and the inherited
parent scope contains the same effective boundary.

Custom subagent profiles MAY be defined globally under `~/.config/mezzanine`
or project-locally under a project configuration directory.

Subagents MUST inherit the parent model profile, permission policy, MCP
configuration, project instructions, and live runtime overrides unless their
profile explicitly defines a stricter or more specific value.

Subagent approval requests MUST identify the originating agent, pane, and task.

If an approval request originates from an inactive pane or thread, Mezzanine
MUST surface it to the primary client without requiring the user to manually
find the pane.

In non-interactive or observer-only contexts, actions requiring fresh approval
MUST enter the blocked approval routing flow and wait for interaction from the
primary client. Read-only observers MUST NOT approve, disapprove, redirect, or
otherwise decide subagent approval requests.

### 10.4 Skills

Skills are reusable markdown workflow descriptions that agents MAY discover and
invoke when they help complete the current task. They follow the OpenAI skill
structure: each skill is a directory containing a required `SKILL.md` file with
YAML front matter followed by markdown instructions. The front matter MUST
include:

- `name`: The stable skill identifier.
- `description`: A short description of when to use the skill.

Skill names MUST contain only lowercase ASCII letters, decimal digits, and
hyphens. The directory basename MUST match the `name` field. Implementations
MUST reject or skip skill directories whose resolved name is empty, contains
path traversal, contains path separators, or does not satisfy the skill-name
grammar.

Mezzanine MAY ship built-in skills. Built-in skills MUST use the same
`SKILL.md` front matter and markdown body structure as filesystem skills, but
they MAY be stored inside the implementation rather than in a user or project
skill root. Built-in skills MUST be discoverable through the same effective
skill catalog as filesystem skills. Built-in skills MUST have the lowest
precedence: a user skill or trusted project skill with the same name overrides
the built-in entry for the affected pane.

Both user and project skill roots MUST use the same directory layout:

- User skills: `~/.config/mezzanine/skills/<skill-name>/SKILL.md`.
- Project skills:
  `<project-root>/.mezzanine/skills/<skill-name>/SKILL.md`.

Skill directories MAY contain auxiliary `scripts/`, `references/`, `assets/`,
and `agents/` subdirectories following OpenAI skill conventions, but Mezzanine
MUST NOT automatically execute scripts or load auxiliary files merely because a
skill was invoked. The invoked skill context is the `SKILL.md` text plus any
explicit additional context. Agents that need auxiliary files MUST inspect them
through ordinary allowed actions and permissions.

Project skills MUST be discoverable only after the project root is trusted
under the project trust rules in Section 8.1. User skills are configuration
material from the primary user configuration directory. When a user skill and a
trusted project skill have the same name, the trusted project skill MUST take
precedence for panes whose current working directory is inside that project.
Skill catalog order MUST be deterministic, sorted by effective skill name.
Effective source scopes MUST include `builtin`, `user`, and `project` when
those source types are present.

Mezzanine MUST provide a built-in `create-skill` skill. It MUST guide agents to
create and modify concise OpenAI-structured skills in both user and project
scopes. The built-in workflow MUST require a `SKILL.md` with `name` and
`description` front matter, a directory basename matching the skill name, and a
terse markdown body containing only the information required to satisfy the
user's requested workflow. It MUST support updates to existing skills as well
as new skill creation, and it MUST discourage auxiliary files unless the
requested workflow actually needs them. It MUST default new skills to user
scope unless the user explicitly requests a repo/project-scoped skill or says
the skill must live with the current repository.

Mezzanine MUST provide a built-in `mez-config` skill. It MUST summarize how to
use `config_change`, include the supported operation names, include the value
shape contract, and include the implementation's annotated setting-path schema
so agents can make supported live configuration changes without rediscovering
the schema from source. It MUST include supported theme color slot names and a
current effective configuration summary when explicitly loaded for a user
prompt so the agent can choose precise paths and values before proposing
configuration changes. For broad theme requests, it MUST bias agents toward
`theme.active` or compact `theme.aliases.*` palette changes before enumerating
individual `theme.colors.*` slots, and it MUST remind agents to preserve
readable diagnostic foreground/background pairs.

Mezzanine MUST provide a built-in `mez-manual` skill. It MUST summarize
Mezzanine terminal commands, pane-local agent slash commands, explicit
`$<skill-name>` invocation, and common operational workflows. The built-in
manual SHOULD derive its command lists from the same registries that implement
the commands so the skill does not drift from the runtime command surface.

Mezzanine MUST keep model-selected skill discovery and skill invocation disabled
by default. While disabled, `request_skills` and `call_skill` MUST NOT appear in
provider schemas, allowed-action surfaces, or model-facing action guidance.
Users MAY still select skills explicitly with `$<skill-name>` syntax before a
provider request is built.

The pane-local agent prompt MUST support explicit skill invocation with:

```text
$<skill-name> [additional context]
```

When a user submits that form, Mezzanine MUST resolve `<skill-name>` from the
effective skill catalog, insert the complete `SKILL.md` text into the new
turn's model context, and append any text after the skill token under a
markdown `Additional context` heading. The submitted prompt remains the latest
user instruction, so the additional text is available both as user input and as
the skill-specific semantic argument. The agent prompt selector SHOULD provide
tab completion for `$<skill-name>` from the effective skill catalog.

Skill content is instruction-like context, but it does not override higher
priority policy, system instructions, developer instructions, user
instructions, repository instructions, permission rules, or action schemas.
Project skill content is untrusted project content for security analysis.
Skill names and descriptions SHOULD NOT be embedded into the stable system
prompt. While model-selected skill actions are disabled, users SHOULD discover
available skills with `/list-skills` and explicitly select them with
`$<skill-name>` so prompt-cache stable prefixes do not churn when project or
user skill files change.

## 11. Agent Shell Commands

The agent shell MUST provide slash commands.

The agent shell prompt MUST use the same selector behavior as the Mezzanine
command prompt for slash-command names and slash-command arguments with
enumerable values. Slash-command selection MUST preserve the leading `/` and
MUST NOT affect ordinary non-slash prompt text.

The agent shell prompt MUST render prefix-based shadow hints for slash-command
names and enumerable slash-command arguments. Slash commands that accept
parameters SHOULD render a parameter placeholder until the user starts typing
that parameter. Pane-local agent prompt input MUST render with a black or white
foreground chosen from the active prompt background for readability, and
shadow-hint completion text MUST use a shaded foreground derived from the same
contrast decision. Invalid slash commands or invalid slash-command arguments
MUST produce a readable pane-local error message and MUST NOT terminate
Mezzanine or tear down the agent prompt.

Command names MAY add aliases, but the slash command names listed below MUST be
accepted unless the command is explicitly unsupported by this specification.

The baseline command capabilities are:

- `/help`: Show available commands as human-readable, aligned rows with a
  concise description for every listed command. Help output MUST omit internal
  effect-type names.
- `/permissions`: Inspect and change the active permission preset and approval
  policy.
- `/approvals`: Alias for `/permissions`.
- `/approval`: Inspect or set the session approval mode. It MUST accept `ask`,
  `auto-allow`, or `full-access`.
- `/approve`: Approve a pending pane-local agent action. It MUST accept an
  approval id, `latest`, or the only pending approval for the active pane, and
  it MUST support `once`, `session`, `project`, and `global` approval scopes.
- `/trust`: Trust a pending project overlay root. It MUST accept a project root,
  `latest`, or the only pending project trust request for the live session, and
  it MUST provide a list view for pending project trust requests.
- `/list-sessions`: Show resumable saved agent sessions in the pane buffer as a
  nested list keyed by conversation UUID, sorted by last activity with the most
  recent session first. Prompt summaries MAY be truncated to the terminal width
  and MUST NOT wrap. Conversation UUIDs SHOULD be rendered as bold actionable
  command links that execute or otherwise provide `/resume <uuid>` for the
  selected session. Internal command-link destinations MUST NOT be rendered as
  visible parenthesized URI text.
- `/list-skills`: Show the effective skills available to the active pane,
  including each skill name, source scope, and description. The display MUST
  use the same catalog that backs `$<skill-name>` prompt expansion. It SHOULD
  explain that users can type `$` and use prompt completion to select a skill,
  and SHOULD show the explicit invocation form
  `$<skill-name> [additional context]`. Discovery diagnostics for skipped skill
  entries SHOULD be included so invalid skill installations are visible without
  preventing valid skills from being used.
- `/copy-context`: Copy the assembled model request context for the active
  pane's currently running agent turn. The command MUST accept `pane`, `buffer
  [name]`, and `clipboard` targets using the same target semantics as
  `/copy-trace-log` and `/copy-patches`; the default target MUST be `pane`, and
  the default paste buffer name for `buffer` MUST be `agent-context`. The dump
  MUST include the generated system prompt and every provider-bound model
  request message with role, source, byte count, and JSON-escaped content. When
  a turn is running, it MUST assemble from the already stored turn context,
  turn record, selected model profile, and current MCP prompt summary without
  rebuilding pane context, consuming pending local messages, reading terminal
  history, resuming transcripts, or starting a new agent turn. When the active
  pane has no running turn, it MAY copy an idle next-prompt preview. If the
  running turn is missing stored request state, it MUST write a short
  diagnostic when the target is `pane` and report that no context was copied.
- `/copy-trace-log`: Copy the active pane's bounded retained agent trace log.
  The command MUST accept `pane`, `buffer [name]`, and `clipboard` targets;
  the default target MUST be `pane`. `pane` MUST append the retained trace log
  to the pane buffer, `buffer` MUST write it to the named internal paste
  buffer or to `agent-trace` when no name is supplied, and `clipboard` MUST
  write it to the `clipboard` paste buffer while attempting a best-effort host
  clipboard copy. Mezzanine MUST retain this bounded pane trace log
  independently of the current visible `/log-level` so recent diagnostics
  remain exportable even when normal logging hid trace output. The retained log
  MUST be bounded by implementation-defined line and byte caps and MUST evict
  oldest entries first.
- `/copy-patches`: Copy retained `apply_patch` payloads, observed
  success/failure status records, and available failure diagnostics for the
  active pane's current agent session. The command MUST accept `pane`, `buffer
  [name]`, and `clipboard` targets; the default target MUST be `pane`. `pane`
  MUST append the patch export to the pane buffer, `buffer` MUST write it to
  the named internal paste buffer or to `agent-patches` when no name is
  supplied, and `clipboard` MUST write it to the `clipboard` paste buffer while
  attempting a best-effort host clipboard copy. Patch exports MUST come from
  structured runtime patch records rather than rendered pane text or compacted
  transcript summaries.
- `/clear`: Clear the terminal view and start a fresh visible conversation.
- `/compact`: Ask the active model to summarize older conversation content
  outside the retained raw tail when model-backed command execution is
  available, store the model-generated summary as pane-scoped memory, and
  retain only a bounded raw recent transcript tail plus the compacted summary
  for model context. The raw tail MUST cover approximately
  `agents.compaction_raw_retention_percent` of the active model context budget
  by estimated replay size, defaulting to 10%. Non-model
  runtime command paths MAY produce an implementation summary, but an explicit
  user `/compact` MUST attempt real transcript compaction whenever active
  durable transcript entries exist, regardless of retained-tail budget. It MUST
  no-op only when there are no transcript
  entries to compact or no durable transcript entries are available.
- `/copy`: Copy the latest non-empty model-authored `say.text` emitted for
  the active pane. The command MUST accept `pane`, `buffer [name]`, and
  `clipboard` targets using the same target semantics as `/copy-trace-log` and
  `/copy-patches`; the default target MUST be `pane`, and the default paste
  buffer name for `buffer` MUST be `agent-output`.
- `/diff`: Show the working tree diff, including untracked files when a
  version-control system exposes them.
- `/exit`: Exit or hide the agent shell, stopping active pane-local agent work
  before the shell is hidden.
- `/quit`: Alias for `/exit`.
- `/init`: Generate a project instruction scaffold using the explicit
  control-plane scaffold exception defined in Section 10.2.
- `/latency`: Inspect or change the pane-local latency/cost preference. It MUST
  accept `slow`, `default`, or `fast` as explicit value arguments, and MUST
  display the active setting when invoked without arguments or with `status` or
  `show`. The command MUST apply a pane-scoped model-profile override that
  includes the selected latency preference without changing the provider, model,
  or reasoning profile. Pane-frame agent status controls MUST expose the latency
  selector only when the active provider supports a provider-visible latency
  preference.
- `/thinking`: Inspect or change the pane-local provider thinking-mode toggle
  when the active provider supports one. It MUST accept `on`, `off`, `toggle`,
  and `status`, and MUST display the active setting when invoked without
  arguments or with `status` or `show`. The command MUST apply a pane-scoped
  model-profile override without changing the provider, model, reasoning
  profile, or latency preference. Providers that do not expose a native
  thinking toggle MUST reject the command without mutating model profiles.
- `/logout`: Log out of a provider account.
- `/list-mcp`: List configured Model Context Protocol servers and tools.
- `/model`: Inspect and change the active model and reasoning settings. The
  command MUST accept `list` to list models for the active provider, and MUST
  accept a provider model name with an optional reasoning level to select that
  model for the active scope.
- `/routing`: Inspect or change automatic turn model sizing. It MUST
  accept `on`, `off`, `toggle`, and `status`. The command MUST update the
  pane-local agent preference and MUST checkpoint that preference with other
  pane-scoped agent shell preferences.
- `/personality`: Configure response style when supported.
- `/stop`: Stop background jobs owned by the agent. User-initiated stops MUST
  settle the affected turn as interrupted/cancelled rather than failed.
- `/fork`: Fork the current conversation into a new agent thread.
- `/resume`: Resume a saved conversation.
- `/new`: Start a new conversation.
- `/status`: Show session status, including model, policy, identity, writable
  roots, context usage, and cumulative provider token counters. The status
  display SHOULD be `text/markdown; charset=utf-8` and SHOULD present the main
  status data as a markdown table. If a provider omits cached-token accounting,
  the status display SHOULD show that counter as unknown rather than as zero.
  Provider token counters MUST include auxiliary routing/model-sizing provider
  requests as separate provider/model rows when token usage is reported.
- `/debug-config`: Show effective configuration, layer order, and policy
  diagnostics.
- `/statusline`: Configure agent shell status-line fields.
- `/title`: Configure terminal window or tab title fields.
- `/log-level`: Show or set the pane-local agent log level. Accepted levels
  MUST include `normal`, `verbose`, `debug`, and `trace`; implementations MAY
  provide aliases for compatibility if they resolve to those canonical levels.

Commands issued while an agent task is running MUST either be rejected with a
clear diagnostic or queued according to a documented rule.

Slash commands typed while a task is running SHOULD be queueable for the next
turn.

Commands that mutate policy, credentials, session state, or files MUST be
subject to the permission model.

The `/permissions` command MUST allow the user to inspect the active preset,
approval policy, bypass state, matched command prefix rules, and available
session/global/project rule scopes. When policy permits mutation, it MUST allow
adding, removing, and persisting command prefix rules.

The `/approval` command MUST set the approval policy for the entire live session
without changing project or user configuration by itself. After an approval
policy or effective permission-policy change, Mezzanine MUST re-evaluate
pending blocked agent approvals against the new live policy. Pending blocked
actions that are now allowed by the active policy MUST be decided and resumed
through the normal blocked-action resume path rather than remaining in
`waiting_approval`.

The `/approve` command MUST route decisions through the same approval policy,
hook, audit, persistent-rule, and blocked-action resume machinery as the
`approval/decide` control method. Pending approval requests visible in an agent
pane MUST clearly log the approval id, requested action, and the corresponding
`/approve` command before the action waits for the user.

The `/trust` command MUST route decisions through the same project trust,
configuration reload, lifecycle event, and audit machinery as the
`project/trust/decide` control method. Trust decisions made with `/trust` MUST
persist to the configured project trust database under the user's configuration
root when persistence is available, so trusted project overlays remain trusted
across sessions. Pending project trust requests MUST clearly log the project
root, overlay count, and corresponding `/trust` command before agent work is
blocked on that trust decision.

The `/list-mcp` command MUST show enabled, disabled, unavailable, and
session-blacklisted MCP servers and tools in a human-readable display format
consistent with other non-tabular list and show commands. The display MUST keep
each server keyed by configured MCP server id in headings or labels, and MUST
show server-specific status, retryability, blacklist reason, transport, and
tool state without requiring users to parse JSON-like object text.

## 12. Local Message Passing Protocol

Mezzanine MUST provide a local message passing protocol that allows agents to
discover each other and send messages to each other.

The local message passing protocol and the control endpoint MUST be separate
services within the multiplexer. The message passing protocol MUST handle agent
discovery, direct messages, group messages, presence, request-response
correlation, and task status messages. It MUST NOT be used to create panes,
resize panes, mutate layout, or perform other multiplexer control operations.

The protocol MUST be local to the Mezzanine session by default.

The protocol MUST provide each agent with a stable local identity.

The protocol MUST support agent discovery by identity, pane, window, role,
status, and declared capabilities.

The protocol MUST support direct messages between agents.

The protocol SHOULD support broadcast or group messages scoped to a session,
window, task, or configured group.

The protocol MUST preserve message ordering for messages sent from one sender
to one recipient over a single logical channel.

The protocol MUST provide delivery status sufficient for a sender to distinguish
accepted, rejected, undeliverable, and expired messages.

The protocol MUST support request-response correlation.

The protocol MUST support presence updates or heartbeats.

The protocol MUST define a versioned message envelope. At minimum, the envelope
MUST include protocol version, message identifier, sender identity, recipient
identity or scope, timestamp or monotonic ordering value, message type,
correlation identifier when applicable, payload content type, and payload.

The protocol MUST reject malformed messages with a structured error.

The protocol MUST NOT expose agent messages to a remote network endpoint unless
the user explicitly configures a bridge or integration that does so.

### 12.1 Protocol Name and Version

The local message passing protocol specified here is the Mezzanine Message
Protocol version 1, abbreviated as `mmp/1`.

Every message envelope MUST include `"protocol": "mmp/1"`.

Endpoints MUST reject messages for unsupported protocol versions with an
`error` message.

### 12.2 Transport

An implementation MUST provide at least one reliable, ordered, local transport.

On platforms with Unix domain sockets, the default transport SHOULD be a Unix
domain socket located under a user-private runtime directory.

Loopback TCP MAY be supported only when the endpoint is bound to a loopback
address, protected by an unguessable session capability, and disabled by
default for remote access.

Transport endpoints MUST be accessible only to the current user by default.

### 12.3 Framing

Messages MUST be UTF-8 JSON values framed with an ASCII header block followed
by a JSON body.

The header block MUST use the following format:

```text
Content-Length: <decimal-octet-length>\r\n
Content-Type: application/vnd.mezzanine.mmp+json; version=1\r\n
\r\n
```

`Content-Length` MUST be the number of octets in the JSON body.

Receivers MUST reject frames with missing, invalid, negative, or oversized
`Content-Length` values.

Receivers MUST ignore unknown headers.

### 12.4 Envelope

The JSON body MUST be an object with the following fields:

- `protocol`: The string `mmp/1`.
- `id`: Globally unique message identity within the session.
- `type`: Message type.
- `time`: RFC 3339 timestamp or a documented monotonic timestamp value.
- `sender`: Sender identity object.
- `recipient`: Recipient identity object or scope object.
- `correlation_id`: Message identity this message responds to, or `null`.
- `ttl_ms`: Time-to-live in milliseconds, or `null`.
- `content_type`: Media type of `payload`.
- `payload`: Message payload.

The `sender` object MUST include `agent_id` when sent by a registered agent. It
SHOULD include `pane_id`, `window_id`, `role`, and `capabilities` when known. A
`hello` message sent before registration MAY omit `agent_id` or include a
provisional client-generated identifier; the message service MUST assign the
effective agent identity in the corresponding `welcome` message.

The message service, not the sending model or shell command text, MUST assign or
validate the effective sender identity for every accepted message using the
authenticated message connection and registered agent identity. If a message
contains a `sender` object that does not match the authenticated connection, the
message service MUST reject the message with an `error` response unless a
documented trusted bridge policy explicitly rewrites the sender. Forwarders MUST
preserve the effective sender identity and MUST NOT allow ordinary agents to
spoof another agent, pane, window, role, or capability set.

The `recipient` object MUST identify one of: a specific agent, a pane, a
window, a session, a role, a capability query, or a group.

Receivers MUST preserve unknown envelope fields when forwarding messages unless
policy requires removal.

### 12.5 Message Types

The protocol MUST support the following message types:

- `hello`: Register an agent with the message service.
- `welcome`: Confirm registration and assigned identity.
- `discover`: Query known agents.
- `discover_result`: Return discovery results.
- `send`: Send application payload to a recipient.
- `deliver`: Deliver application payload to a recipient.
- `ack`: Acknowledge accepted message handling.
- `error`: Report structured protocol or delivery errors.
- `presence`: Announce agent status or capability changes.
- `heartbeat`: Prove liveness.
- `task_status`: Report agent task state.
- `task_result`: Report agent task completion.

Message types outside this list MUST use an extension namespace containing a
reverse-DNS or URI-like prefix.

### 12.6 Delivery Semantics

The protocol MUST provide at-least-once delivery to registered local recipients
while the recipient remains available and the message has not expired.

When a recipient has an open writable local transport connection, Mezzanine
MUST continue attempting to deliver pending messages on that connection without
requiring the recipient to send another request frame first. Implementations
MAY use bounded polling, event notifications, or another local scheduling
mechanism to wake writable recipient connections.

Recipients MUST treat message `id` values as idempotency keys.

A sender MUST receive `ack` when a message is accepted for delivery.

A sender MUST receive `error` when a message is rejected, undeliverable, or
expired before delivery.

For messages sent from one sender to one recipient over one logical channel,
delivery order MUST match acceptance order.

### 12.7 Errors

`error` payloads MUST include `code`, `message`, and `retryable`.

`error` payloads SHOULD include `details` when doing so does not expose
secrets.

The baseline error codes are `unsupported_protocol`, `malformed_frame`,
`invalid_envelope`, `unauthorized`, `not_found`, `expired`, `payload_too_large`,
`rate_limited`, `policy_denied`, and `internal_error`.

### 12.8 Payloads

Text payloads MUST use `content_type` of `text/plain; charset=utf-8`.

JSON payloads MUST use `content_type` of `application/json`.

Binary payloads MUST be encoded as base64 text and MUST declare
`payload_encoding` as `base64`.

Receivers MUST enforce configured payload size limits.

## 13. Control Endpoint

Mezzanine MUST expose a structured control endpoint for clients, the
configuration shell, and agent harnesses.

The control endpoint and the local message passing protocol MUST be separate
services within the multiplexer. The control endpoint MUST handle multiplexer state
inspection and mutation. Agent discovery and agent-to-agent messaging MUST use
the local message passing protocol.

The control endpoint MUST be able to operate over a Unix domain socket.

The control endpoint MAY operate over TCP.

When TCP is enabled, the default bind address MUST be loopback only.

Remote TCP access MUST be disabled by default and MUST require explicit user
configuration.

The control endpoint MUST authenticate mutating requests.

The default authentication mechanism for local control requests SHOULD be a
user-private socket path or an unguessable bearer token stored in a
user-private file.

The control endpoint MUST support version negotiation.

The control endpoint protocol specified here is Mezzanine Control Protocol
version 1, abbreviated as `mezctl/1`.

`mezctl/1` messages over stream transports MUST be framed as UTF-8 JSON values
with an ASCII header block followed by a JSON body. The header block MUST use
the following format:

```text
Content-Length: <decimal-octet-length>\r\n
Content-Type: application/vnd.mezzanine.control+json; version=1\r\n
\r\n
```

`Content-Length` MUST be the number of octets in the JSON body. Receivers MUST
reject frames with missing, invalid, negative, or oversized `Content-Length`
values. Receivers MUST ignore unknown headers unless a header is explicitly
documented as mandatory.

The JSON body MUST use JSON-RPC 2.0 request, response, and notification
objects. Requests MUST include `"jsonrpc": "2.0"`, a non-null string or
integer `id`, a string `method`, and optional object `params`. Notifications
MUST include `"jsonrpc": "2.0"` and `method`, and MUST NOT include `id`.
Responses MUST include `"jsonrpc": "2.0"` and the same `id` as the request.
A successful response MUST include `result` and MUST NOT include `error`. An
error response MUST include `error` and MUST NOT include `result`.

The `error` object MUST include integer `code`, string `message`, and optional
`data`. Error `data`, when present, SHOULD include a stable string
`mezzanine_code` and MAY include method-specific details. The baseline
application error codes are:

- `-32000`: `internal_error`
- `-32001`: `unauthorized`
- `-32002`: `forbidden`
- `-32003`: `unsupported_version`
- `-32004`: `invalid_state`
- `-32005`: `not_found`
- `-32006`: `conflict`
- `-32007`: `not_primary`
- `-32008`: `policy_denied`
- `-32009`: `approval_required`
- `-32010`: `timeout`
- `-32011`: `rate_limited`
- `-32012`: `cancelled`

Standard JSON-RPC parse, invalid request, method-not-found, invalid-params, and
internal-error codes MUST be used for protocol-level failures when applicable.

The first request on a connection MUST be `control/initialize` unless the
transport has already authenticated and negotiated a protocol version through a
documented outer mechanism. `control/initialize` params MUST include client
name, client version when known, requested protocol version, requested role
(`primary`, `observer`, `agent`, or `automation`), and authentication material
when required by the selected transport. The result MUST include selected
protocol version, server identity, granted role, capabilities, and whether
additional approval is pending. The result MUST include session identity when
the caller is bound to a session and the granted role is not
`pending_observer`. When an observer request has not been approved, the granted
role MUST be `pending_observer`, the result MUST omit session identity, and the
result MUST expose only request-local observer status.

For Unix-domain local transports, Mezzanine SHOULD authenticate using peer
credentials and the user-private socket path. For TCP transports, Mezzanine
MUST require an unguessable bearer token or stronger authentication before any
session data or mutating operation is available. Bearer tokens SHOULD be sent
in an `Authorization: Bearer <token>` header when the framing layer supports
headers; otherwise they MUST be sent only in `control/initialize` params.

Mutating methods MUST require an authenticated caller and MUST enforce role
authorization. Observer-role and pending-observer-role callers MUST NOT invoke
mutating methods except for operations that create, refresh, detach, or shut down
their own observer connection. An authenticated caller requesting observer
access MAY create or refresh its own pending observer request through
`session/attach`; that operation MUST expose no session view and MUST mutate
only observer-request metadata. Agent callers MAY invoke only the control
methods allowed by active permission policy. Primary-only methods MUST fail
with `not_primary` for every non-primary caller.

Pending-observer callers MUST NOT receive session data through read-only
methods. Before approval, a pending observer connection MAY invoke only
`control/initialize`, `session/attach` for its own observer request,
`observer/inspect` for its own observer request, `control/cancel` for its own
in-flight requests, and `control/shutdown`. Any other method from a
pending-observer caller MUST fail with `forbidden` or `approval_required` and
MUST NOT include session, window, pane, frame, history, agent, message-log, or
configuration data in the error payload.

Approved observer callers MAY receive the rendered terminal stream and
post-approval view events for their approved observer attachment. Approved
observer callers MUST NOT receive control-method responses or event replay that
include terminal output, frame content, history, paste buffers, transcripts,
agent state, configuration, MCP state, approval state, client lists, or session
metadata from before `ObserverState.visible_from_event_id` and
`ObserverState.visible_from_time`. Unless a baseline method explicitly permits
approved-observer use, approved observers MAY invoke only `control/shutdown`,
`control/cancel` for their own requests, `observer/inspect` for their own
observer state, and methods that refresh or detach their own observer
attachment. A method being read-only or naturally idempotent MUST NOT by itself
grant observer authorization.

Every non-idempotent mutating request params object MUST include
`idempotency_key`. Methods that are naturally idempotent MAY omit
`idempotency_key` only when this specification declares them idempotent.
Mezzanine MUST remember the completed result or error for each idempotency key
for the lifetime of the live session and MUST return the same response for a
repeated request with the same method, caller identity, and key. If the same key
is reused with different method or parameters, Mezzanine MUST return
`conflict`.

Method names MUST use slash-separated namespaces. Baseline namespaces are
`control/*`, `session/*`, `client/*`, `window/*`, `pane/*`, `agent/*`,
`observer/*`, `approval/*`, `config/*`, `project/*`, `snapshot/*`, and
`mcp/*`.
Notifications sent by the server SHOULD use the `event/*` namespace and MUST
NOT require a response.

The endpoint MUST support `control/shutdown` for orderly client disconnects
and SHOULD support `control/cancel` with a target request id for cancellation
of long-running requests. Cancellation MUST NOT leave a request without a
response; a cancelled request MUST eventually return a normal result or an
error with `mezzanine_code` of `cancelled` or `timeout`.

All baseline control endpoint methods MUST use JSON object params and JSON
object results. A params object with no method-specific fields MUST be `{}`.
Unless a method explicitly states that it is naturally idempotent, a mutating
method MUST require `idempotency_key`.

Targets MUST be encoded as objects rather than as unstructured display text.
Exact identities MUST take precedence over indexes or names. Ambiguous targets
MUST fail with `conflict`; missing targets MUST fail with `not_found`.

Target object alternatives MUST be mutually exclusive unless this
specification explicitly combines fields, such as `session_id` plus
`window_index`. If a target contains multiple independent identity forms, such
as both `pane_id` and `window_id` plus `pane_index`, Mezzanine MUST reject it
with `invalid-params` unless all forms resolve to the same entity and the
method explicitly permits redundant targets.

`SessionTarget` MUST use exactly one of `session_id`, `name`, or
`default = true`. `WindowTarget` MUST use exactly one of `window_id`,
`session_id` plus a window selector, `session` as a `SessionTarget` plus a
window selector, or `default_session = true` plus a window selector. A window
selector MUST use exactly one of `window_index`, `window_name`, or
`active = true`. `PaneTarget` MUST use exactly one of `pane_id`, `window_id`
plus a pane selector, `window` as a `WindowTarget` plus a pane selector, or
`session` as a `SessionTarget` plus `active = true` to select the active pane in
the active window for that session. A pane selector MUST use exactly one of
`pane_index`, `pane_title`, or `active = true`. `AgentTarget` MUST use exactly
one of `agent_id` or `pane_id`. Index values MUST be non-negative integers.

`AuthenticationMaterial` MUST include `mechanism`. `mechanism` MUST be one of
`peer_credentials`, `bearer_token`, `none`, or an extension value under an
extension namespace. For `bearer_token`, the object MUST include `token`.
Authentication material MUST be accepted only in `control/initialize` or an
explicit authentication method defined by a future protocol version. Servers
MUST NOT echo secret authentication material in responses, diagnostics, events,
or audit logs.

When `mechanism` is `none`, Mezzanine MUST treat the caller as unauthenticated
unless the transport has already established an authenticated identity through a
documented outer mechanism. An unauthenticated caller MUST NOT receive session
data, observer data for any existing request, mutating capabilities, agent
capabilities, or primary authority. `none` MAY be used only for version
negotiation, no-session capability discovery, or an outer-authenticated
connection whose authentication is represented outside the JSON payload.

`control/initialize` params MUST be an object with `client_name`,
`requested_version`, `requested_role`, and optional `client_version`,
`session_target`, `client`, and `authentication`. `requested_role` MUST be one
of `primary`, `observer`, `agent`, or `automation`. `client`, when present,
MUST be a `ClientDescriptor`. `authentication`, when present, MUST be
`AuthenticationMaterial`. The result MUST include `selected_version`, `server`,
`session`, `granted_role`, `capabilities`, `approval_pending`, and
`observer_request`.
`granted_role` MUST be one of `primary`, `pending_observer`, `observer`,
`agent`, or `automation`. `observer_request` MUST be `null` unless the result
concerns an observer attachment or pending observer request. When `granted_role`
is `pending_observer`, `session` MUST be `null`, `observer_request` MUST contain
request-local observer status, `capabilities.methods` MUST be limited to the
pending-observer allowlist, and the result MUST NOT include session, window,
pane, frame, history, agent, message-log, or configuration data. A request for
`requested_role = "primary"` MUST fail with `invalid_state` or `forbidden`
when the client descriptor does not identify an interactive terminal.

`ServerIdentity` MUST include `id`, `implementation_name`, `version`,
`protocol_versions`, `started_at`, `user_id` when available, `host` when
available, and `pid` when available. It MUST NOT include authentication
secrets.

`Capabilities` MUST include `protocol_version`, `methods`, `event_types`,
`roles`, `transports`, `limits`, and `features`. `limits` MUST include maximum
frame size, maximum request size, maximum event replay retention when replay is
supported, and maximum payload size for methods that return captured content.
`features` MUST include Booleans for at least `tcp`, `event_replay`,
`observers`, `mcp`, `snapshots`, `audit`, and `approval_bypass`.

`TerminalSize` MUST include positive integer `columns` and `rows`. A terminal
size with non-positive values MUST be rejected with `invalid-params`.

`PermissionSummary` MUST include `preset`, `approval_policy`,
`bypass_active`, `trusted_project`, `trusted_directories`, `read_scopes`,
`write_scopes`, and `command_rule_generation`. It MUST NOT include secret
configuration values.

`ActionEffects` MUST include `reads`, `writes`, `creates`, `deletes`, and
`touches` as arrays of shell-path strings, plus Boolean fields `network`,
`credentials`, `process_control`, `destructive`, `privilege_change`, and
`unknown`. When an effect is not known, `unknown` MUST be true.

The baseline control methods are:

| Method | Params | Result | Notes |
| --- | --- | --- | --- |
| `control/initialize` | `{ "client_name": string, "requested_version": integer, "requested_role": string, "client_version": string \| null, "session_target": SessionTarget \| null, "detach_primary_on_disconnect": boolean \| null, "client": ClientDescriptor \| null, "authentication": AuthenticationMaterial \| null }` | `{ "selected_version": integer, "server": ServerIdentity, "session": SessionSummary \| null, "granted_role": string, "capabilities": Capabilities, "approval_pending": boolean, "observer_request": ObserverState \| null }` | First request on a connection unless negotiated externally. Pending observers receive no session data beyond request-local status. Foreground primary attach clients set `detach_primary_on_disconnect` so an EOF clears their primary ownership; request-local clients leave it false. |
| `control/shutdown` | `{}` | `{ "closed": boolean }` | Orderly client disconnect. Naturally idempotent. |
| `control/cancel` | `{ "request_id": string }` | `{ "cancel_requested": boolean }` | May cancel only requests owned by the caller unless primary policy permits broader cancellation. |
| `session/list` | `{}` | `{ "sessions": [SessionSummary] }` | Read-only and naturally idempotent. |
| `session/get` | `{ "target": SessionTarget }` | `{ "session": SessionState }` | Read-only and naturally idempotent. |
| `session/attach` | `{ "target": SessionTarget, "role": "primary" \| "observer", "client": ClientDescriptor, "idempotency_key": string }` | `{ "client": ClientState, "approval_pending": boolean }` | Primary attachment MUST enforce the single-primary invariant. Observer attachment MUST create a pending observer and expose no view until approved. |
| `client/list` | `{ "target": SessionTarget }` | `{ "clients": [ClientState] }` | Primary-readable and naturally idempotent. Observers MAY receive only their own `ClientState` unless the primary explicitly grants broader visibility. Pending observers MUST NOT use this method. |
| `client/detach` | `{ "client_id": string, "idempotency_key": string }` | `{ "detached": boolean }` | Mutating. |
| `client/select_primary` | `{ "client_id": string, "idempotency_key": string }` | `{ "primary_client_id": string }` | Primary-only mutating method when a primary is attached; authenticated session-owner operation when no primary is attached. The target client MUST have an interactive terminal. The transfer MUST be atomic and MUST never create two primaries. |
| `observer/list` | `{ "target": SessionTarget, "state": string \| null }` | `{ "observers": [ObserverState] }` | Primary-readable and naturally idempotent. Pending observers MUST NOT use this method. |
| `observer/inspect` | `{ "observer_request_id": string }` | `{ "observer": ObserverState }` | Primary-readable and naturally idempotent. Pending and approved observers MAY inspect only their own request-local `ObserverState`, with no session data beyond their observer status. |
| `observer/approve` | `{ "observer_request_id": string, "idempotency_key": string }` | `{ "observer": ObserverState }` | Primary-only mutating method. The observer view begins at the live viewport and live scroll position at approval time. |
| `observer/reject` | `{ "observer_request_id": string, "reason": string \| null, "idempotency_key": string }` | `{ "observer": ObserverState }` | Primary-only mutating method. |
| `observer/revoke` | `{ "client_id": string, "reason": string \| null, "idempotency_key": string }` | `{ "revoked": boolean }` | Primary-only mutating method. |
| `window/list` | `{ "target": SessionTarget }` | `{ "windows": [WindowState] }` | Read-only and naturally idempotent. |
| `window/create` | `{ "target": SessionTarget, "name": string \| null, "start_directory": string \| null, "shell_command": string \| [string] \| null, "select": boolean, "idempotency_key": string }` | `{ "window": WindowState, "pane": PaneState }` | Mutating. `shell_command`, when present, MUST follow pane creation command semantics. |
| `window/rename` | `{ "target": WindowTarget, "name": string, "idempotency_key": string }` | `{ "window": WindowState }` | Naturally idempotent when the target and name are unchanged. |
| `window/select` | `{ "target": WindowTarget, "idempotency_key": string }` | `{ "active_window_id": string }` | Mutating client/session focus. |
| `window/close` | `{ "target": WindowTarget, "force": boolean, "idempotency_key": string }` | `{ "closed": boolean }` | Mutating and destructive. |
| `pane/list` | `{ "target": WindowTarget \| SessionTarget }` | `{ "panes": [PaneState] }` | Read-only and naturally idempotent. |
| `pane/create` | `{ "target": PaneTarget \| WindowTarget, "split": "vertical" \| "horizontal" \| "window", "start_directory": string \| null, "shell_command": string \| [string] \| null, "size": SizeSpec \| null, "select": boolean, "idempotency_key": string }` | `{ "pane": PaneState, "layout": LayoutState }` | Mutating. `shell_command`, when present, MUST follow pane creation command semantics. |
| `pane/select` | `{ "target": PaneTarget, "idempotency_key": string }` | `{ "active_pane_id": string }` | Mutating client/session focus. |
| `pane/resize` | `{ "target": PaneTarget, "size": SizeSpec, "idempotency_key": string }` | `{ "pane": PaneState, "layout": LayoutState }` | Mutating. |
| `pane/move` | `{ "source": PaneTarget, "destination": WindowTarget \| PaneTarget, "position": string \| null, "idempotency_key": string }` | `{ "pane": PaneState, "layout": LayoutState }` | Mutating. |
| `pane/swap` | `{ "source": PaneTarget, "destination": PaneTarget, "idempotency_key": string }` | `{ "layout": LayoutState }` | Mutating. |
| `pane/break` | `{ "target": PaneTarget, "name": string \| null, "idempotency_key": string }` | `{ "window": WindowState, "pane": PaneState }` | Mutating. |
| `pane/join` | `{ "source": PaneTarget, "destination": WindowTarget \| PaneTarget, "idempotency_key": string }` | `{ "pane": PaneState, "layout": LayoutState }` | Mutating. |
| `pane/close` | `{ "target": PaneTarget, "force": boolean, "idempotency_key": string }` | `{ "closed": boolean }` | Mutating and destructive. |
| `pane/capture` | `{ "target": PaneTarget, "range": CaptureRange, "include_history": boolean }` | `{ "content": string, "truncated": boolean, "range": CaptureRange }` | Read-only when policy allows. |
| `frame/read` | `{ "target": WindowTarget \| PaneTarget }` | `{ "fields": object, "rendered": string }` | Read-only and naturally idempotent. |
| `agent/shell/show` | `{ "target": PaneTarget, "idempotency_key": string }` | `{ "agent": AgentState, "visible": true }` | Mutating UI state. |
| `agent/shell/hide` | `{ "target": PaneTarget, "idempotency_key": string }` | `{ "agent": AgentState, "visible": false }` | Mutating UI state. MUST stop active pane-local agent work before hiding. |
| `agent/shell/command` | `{ "input": string, "idempotency_key": string }` | `{ "pane_id": string, "input": string, "kind": string, "command": string \| null, "body": string \| null, "turn": AgentTaskState \| null }` | Primary-only command submitted through the visible agent shell for the active pane. Slash commands MUST return display, mutation, or runtime-required results. Non-slash prompts MUST create or queue an agent turn when no pane-local turn is active, or become mid-turn steering for the active turn. |
| `agent/list` | `{ "target": SessionTarget }` | `{ "agents": [AgentState] }` | Read-only and naturally idempotent. |
| `agent/task/list` | `{ "target": AgentTarget \| SessionTarget }` | `{ "tasks": [AgentTaskState] }` | Read-only and naturally idempotent. |
| `agent/spawn` | `{ "parent_agent": AgentTarget, "placement": Placement, "role": string, "cooperation_mode": string, "read_scopes": [string], "write_scopes": [string], "prompt": string, "idempotency_key": string }` | `{ "agent": AgentState, "pane": PaneState }` | Mutating and permission-gated. |
| `approval/list` | `{ "target": SessionTarget, "state": string \| null }` | `{ "approvals": [ApprovalState] }` | Primary-readable and naturally idempotent. |
| `approval/decide` | `{ "approval_id": string, "decision": "approve" \| "disapprove" \| "redirect", "scope": ApprovalScope \| null, "instruction": string \| null, "idempotency_key": string }` | `{ "approval": ApprovalState }` | Primary-only mutating method. |
| `config/get` | `{ "path": string \| null, "effective": boolean }` | `{ "value": any, "layers": [ConfigLayer] }` | Read-only and naturally idempotent. |
| `config/set` | `{ "path": string, "value": any, "persist": PersistTarget \| null, "idempotency_key": string }` | `{ "applied": boolean, "diagnostics": [Diagnostic] }` | Mutating. |
| `config/unset` | `{ "path": string, "persist": PersistTarget \| null, "idempotency_key": string }` | `{ "applied": boolean, "diagnostics": [Diagnostic] }` | Mutating. |
| `config/reload` | `{ "idempotency_key": string }` | `{ "applied": boolean, "diagnostics": [Diagnostic] }` | Mutating. |
| `config/validate` | `{ "files": [string] \| null }` | `{ "valid": boolean, "diagnostics": [Diagnostic] }` | Read-only and naturally idempotent. |
| `project/trust/list` | `{ "state": string \| null }` | `{ "projects": [ProjectTrustState] }` | Primary-readable and naturally idempotent. |
| `project/trust/inspect` | `{ "project_root": string }` | `{ "project": ProjectTrustState }` | Primary-readable and naturally idempotent. |
| `project/trust/decide` | `{ "project_root": string, "decision": "trust" \| "reject", "reason": string \| null, "idempotency_key": string }` | `{ "project": ProjectTrustState, "diagnostics": [Diagnostic] }` | Primary-only mutating method. Trusting a project MUST validate and apply pending overlays as part of the decision. If validation fails, Mezzanine MUST leave those overlays unapplied and report diagnostics. |
| `project/trust/revoke` | `{ "project_root": string, "reason": string \| null, "idempotency_key": string }` | `{ "project": ProjectTrustState, "diagnostics": [Diagnostic] }` | Primary-only mutating method. Must reload effective configuration without revoked overlays. |
| `snapshot/list` | `{ "target": SessionTarget \| null }` | `{ "snapshots": [SnapshotState] }` | Read-only and naturally idempotent. |
| `snapshot/create` | `{ "target": SessionTarget, "name": string \| null, "idempotency_key": string }` | `{ "snapshot": SnapshotState }` | Mutating. |
| `snapshot/resume` | `{ "snapshot_id": string, "idempotency_key": string }` | `{ "session": SessionState }` | Mutating. |
| `snapshot/delete` | `{ "snapshot_id": string, "idempotency_key": string }` | `{ "deleted": boolean }` | Mutating. |
| `mcp/list` | `{ "target": SessionTarget \| null }` | `{ "servers": [McpServerState], "tools": [McpToolState] }` | Read-only and naturally idempotent. |
| `mcp/retry` | `{ "server_id": string, "idempotency_key": string }` | `{ "server_id": string, "retried": boolean, "previous_status": string, "status": string, "retryable_before_retry": boolean, "rediscovered": boolean, "tools": number, "reason": string \| null, "diagnostics": [Diagnostic] }` | Primary-only mutating method. Clears session blacklist state for the configured enabled server, drops stale MCP transport state, attempts rediscovery, and reports whether the retry succeeded or was blacklisted again. |

Control endpoint schemas MUST use JSON objects. Time values MUST be RFC 3339
strings with an offset. IDs MUST be opaque stable strings and MUST NOT require
clients to infer structure from prefixes. Object fields not defined by this
specification MUST be placed under an `extensions` object; receivers MUST
ignore unknown extension keys they do not understand.

`ClientDescriptor` MUST include `name` and MUST include `terminal` when the
client has an interactive terminal. It MAY include `version`, `pid`, `host`,
`user`, `purpose`, `requested_role`, `interactive`, `stdio`, and `metadata`.
`requested_role`, when present, MUST be one of `primary`, `observer`, `agent`,
or `automation` and MUST match the role requested by the enclosing method.
`interactive`, when present, MUST be a Boolean assertion by the client.
`stdio`, when present, MAY include `stdin_is_tty`, `stdout_is_tty`,
`stderr_is_tty`, `controlling_tty`, and `tty_device` or a non-secret stable hash
of the device identity. `terminal`, when present, MUST include `columns`,
`rows`, `term`, and MAY include `features` such as `mouse`, `bracketed_paste`,
and `truecolor`.

For local clients, Mezzanine MUST verify primary-client interactive eligibility
from operating-system terminal state when available, such as TTY status for the
client standard streams or peer process descriptors. For remote or TCP clients,
a client-supplied descriptor alone MUST NOT be sufficient to grant primary
authority unless the authenticated transport or configured policy explicitly
trusts that client class to assert interactive terminal state. If Mezzanine
cannot verify or trust the assertion, a primary-role request MUST fail with
`forbidden` or `invalid_state`.

`SessionSummary` MUST include `id`, `version`, `name`, `state`, `created_at`,
`last_attached_at`, `window_count`, `attached_client_count`, `has_primary`,
and `active_window_id`. `state` MUST be one of `running`, `detached`,
`empty`, `stopping`, or `failed`.

`SessionState` MUST include `id`, `version`, `name`, `state`, `created_at`,
`updated_at`, `primary_client_id`, `authoritative_size`, `active_window_id`,
`windows`, `clients`, `observers`, `config_generation`, and
`permission_summary`. `primary_client_id` MAY be null only while no primary
client is attached. `authoritative_size` MUST include `columns` and `rows` and
MUST remain the last primary-defined size while detached.

`ClientState` MUST include `id`, `version`, `role`, `requested_role`, `state`,
`attached_at`, `last_seen_at`, `descriptor`, and `terminal_size`. `role` MUST
be one of `primary`, `pending_observer`, `observer`, `agent`, or `automation`.
`state` MUST be one of `attached`, `pending`, `detached`, `revoked`, or
`failed`. `terminal_size` MUST include `columns` and `rows` when known.

`ObserverState` MUST include `id`, `version`, `client_id`, `state`,
`requested_at`, `decided_at`, `decided_by_client_id`, `visible_from_event_id`,
`visible_from_time`, `descriptor`, and `reason`. `state` MUST be one of
`pending`, `approved`, `rejected`, or `revoked`. `decided_at`,
`decided_by_client_id`, `visible_from_event_id`, `visible_from_time`, and
`reason` MAY be null when not applicable. Approved observers MUST receive no
session data earlier than `visible_from_time` and `visible_from_event_id`.

`WindowState` MUST include `id`, `version`, `session_id`, `index`, `name`,
`active`, `created_at`, `size`, `active_pane_id`, `panes`, and `layout`.
`size` MUST include `columns` and `rows`.

`PaneState` MUST include `id`, `version`, `session_id`, `window_id`, `index`,
`title`, `active`, `size`, `primary_pid`, `process_state`, `exit_status`,
`current_working_directory`, `terminal_profile`, `history_limit`,
`alternate_screen_active`, `readiness_state`, and `agent_id`.
`process_state` MUST be one of `starting`, `running`, `exited`, `closing`, or
`failed`. `readiness_state` MUST be one of `unknown`, `prompt-candidate`,
`probing`, `ready`, `busy`, `degraded`, or `interactive-blocked`.

`LayoutState` MUST include `id`, `version`, `window_id`, `root`, and
`minimum_pane_size`. `root` MUST be a recursive layout node. A layout node MUST
be either `{ "type": "pane", "pane_id": string, "size": object }` or
`{ "type": "split", "direction": "vertical" | "horizontal", "children":
[LayoutNode], "sizes": [number] }`. Split node sizes MUST describe relative
allocation among children and MUST be sufficient to reconstruct the layout.

`AgentState` MUST include `id`, `version`, `session_id`, `pane_id`, `status`,
`visible`, `conversation_id`, `model_profile`, `cooperation_mode`,
`read_scopes`, `write_scopes`, and `last_turn_id`. `status` MUST be one of
`idle`, `running`, `waiting_approval`, `blocked`, `compacting`, `failed`, or
`stopped`.

`AgentTaskState` MUST include `id`, `version`, `agent_id`, `state`,
`created_at`, `started_at`, `finished_at`, `prompt_preview`, `approval_ids`,
and `result_summary`. `state` MUST be one of `queued`, `running`,
`waiting_approval`, `cancelled`, `failed`, `interrupted`, or `completed`.

`ApprovalState` MUST include `id`, `version`, `state`, `requester`,
`action_type`, `created_at`, `decided_at`, `decided_by_client_id`, `summary`,
`effects`, `scope`, and `instruction`. `state` MUST be one of `pending`,
`approved`, `disapproved`, `redirected`, `cancelled`, or `invalidated`.
`effects` MUST be an `ActionEffects` object when the approval concerns an
agent action. `invalidated` MUST be used only when the underlying requester,
pane, session, or action no longer exists or can no longer be resumed; it MUST
NOT be used merely because time has passed.

`ConfigLayer` MUST include `id`, `layer_type`, `precedence`, `path`,
`trusted`, `applied`, `schema_version`, and `diagnostics`. `layer_type` MUST
be one of `built_in`, `user`, `project_root`, `project_parent`,
`project_current`, or `live`.

`ProjectTrustState` MUST include `id`, `version`, `project_root`, `state`,
`git_marker_path`, `trusted_at`, `rejected_at`, `revoked_at`,
`decided_by_client_id`, `trust_policy_version`, `configuration_schema_version`,
`overlay_files`, `capability_expansion_summary`, and `diagnostics`. `state`
MUST be one of `pending`, `trusted`, `rejected`, or `revoked`.
`overlay_files` MUST include every discovered project overlay path under the
project root, its format, whether it was applied, and any validation
diagnostics. `capability_expansion_summary` MUST identify overlay settings
that can execute code or expand authority, including hooks, MCP servers,
command rules, provider settings, and permission settings.

`Diagnostic` MUST include `severity`, `code`, and `message`. `severity` MUST
be one of `info`, `warning`, or `error`. It MAY include `path`, `line`,
`column`, `related`, and `help`.

`SnapshotState` MUST include `id`, `version`, `session_id`, `name`,
`created_at`, `kind`, `restorable`, `window_count`, `pane_count`,
`limitations`, and `storage_ref`. `kind` MUST be one of `live`, `manual`,
`automatic`, or `crash_recovery`.

`McpServerState` MUST include `id`, `version`, `name`, `state`, `configured`,
`blacklisted`, `transport`, `tools`, `last_checked_at`, and `diagnostics`.
`state` MUST be one of `enabled`, `disabled`, `starting`, `available`,
`unavailable`, `blacklisted`, or `failed`. Secret-bearing configuration fields
MUST NOT be returned.

`McpToolState` MUST include `id`, `version`, `server_id`, `name`, `available`,
`blacklisted`, `permission_required`, `description`, and `input_schema`.
Unavailable or blacklisted MCP tools MUST NOT be included in agent prompt tool
lists as available tools.

`SizeSpec` MUST be an object with `mode`. `mode` MUST be one of `cells`,
`percent`, `delta`, or `edge`. `cells` mode MUST include `columns` or `rows`.
`percent` mode MUST include `percent` and MAY include `axis`. `delta` mode
MUST include `direction` and `amount`. `edge` mode MUST include `edge` and
`amount`. Invalid sizes or sizes below minimum pane dimensions MUST fail with
`invalid-params` or `invalid_state`.

`Placement` MUST include `mode`. `mode` MUST be one of `new-pane` or
`new-window`. Mezzanine MAY accept legacy `new-pane` placement fields for
protocol compatibility, but subagent placement MUST be normalized to
same-group dedicated subagent windows. `new-window` placement MAY include a
name, start directory, and select flag, but the select flag MUST NOT move the
primary user's focus during subagent spawn. Mezzanine v1 MUST NOT support
spawning a subagent into the parent's existing pane because each spawned agent
MUST have its own pane shell.

`CaptureRange` MUST include `origin`, `start`, and `end`. `origin` MUST be one
of `visible`, `history`, or `combined`. `start` and `end` MUST be integer line
offsets relative to the selected origin or the strings `start` and `end`.
History capture MUST exclude alternate-screen content.

`ApprovalScope` MUST include `persistence` and MAY include `command_prefix`,
`exact_sha256`, `working_directory`, `project_root`, and
`external_integration`. `persistence` MUST be one of `once`, `session`,
`project`, or `global`. Session-scoped approvals MUST survive detach and
reattach and MUST reset after session failure or crash recovery. Project-scoped
approvals MUST persist as exact command rules in the project overlay
configuration. Approval scopes MUST NOT expire solely because time has passed.

`PersistTarget` MUST include `scope`. `scope` MUST be one of `live`, `user`,
or `project`. `project` persistence MUST include a path under the trusted
project root and MUST block until project trust is decided. `user` persistence
MUST target the primary user configuration or another configured user-private
file.

Server notifications MUST use `event/*` methods. Event params MUST include
`event_id`, `time`, `event_type`, and an `object` payload. Event params MUST
include `session_id` when the recipient is authorized to know the session
identity for that event. Events sent to pending observers MUST omit `session_id`
or set it to `null`. Events that change entity state SHOULD include `previous`
when reasonably available.
When a wall-clock event timestamp is unavailable, `time` MAY use the monotonic
form `event:<event_id>`, where `<event_id>` is the same per-connection ordered
event identifier carried by `event_id`.
Baseline event types MUST cover client attach/detach, observer request and
decision, window changes, pane changes, agent task state changes, approval
creation and decision, configuration reload or mutation, snapshot changes, and
MCP server availability changes.

The `object` payload of an event MUST be one of the state objects defined in
this section or a method-specific object documented by the event type. Observer
request events visible to the primary client MUST include `ObserverState`.
Events sent to pending observers MUST be limited to the pending observer's own
request-local status and MUST NOT include `SessionState`, `WindowState`,
`PaneState`, `AgentState`, history, frame content, or message-log content.
Event delivery MUST preserve order per connection. A client that reconnects MAY
request events after a known `event_id`; Mezzanine MAY refuse replay for events
that are no longer retained, but it MUST make the retention policy visible in
capabilities.

The control endpoint MUST distinguish read-only observer requests from primary
client requests.

A read-only observer request MUST NOT mutate windows, panes, terminal buffers,
configuration, agents, approvals unrelated to the observer request, or other
runtime state. It MAY create or update observer-request metadata needed for
primary-client approval and audit.

Only the primary client MAY approve, reject, or revoke observer access through
the control endpoint. If no primary client is attached, observer requests MUST
remain pending and MUST receive no session view until a primary client decides
them.

Agent harness requests that mutate multiplexer state MUST be subject to the active
permission policy.

The control endpoint protocol MUST be extensible and MUST reject malformed
requests with structured errors.

This specification does not require a particular server process model. An
implementation MAY host the control endpoint in any process that satisfies the
session and persistence requirements.

## 14. Model Context Protocol Integration

Mezzanine MUST support Model Context Protocol servers as explicitly configured
external integrations.

MCP support means that Mezzanine can configure, start or connect to, list, expose
to agents, permission-gate, audit, and blacklist MCP servers and tools. It does
not mean that any particular configured MCP server is required to be available,
usable, trusted, or presented to the model in a given session.

MCP servers MUST be configured under the `mcp_servers` configuration table.

Mezzanine MUST support stdio MCP servers with `command`, optional `args`,
optional `env`, optional `env_vars`, and optional `cwd`.
Stdio MCP subprocesses MUST receive a usable `PATH` for command lookup unless a
server configuration explicitly sets or passes `PATH`. Other environment
variables MUST remain limited to configured `env` values and explicitly listed
`env_vars`.

Mezzanine MUST support streamable HTTP MCP servers with `url` and optional
HTTP headers, bearer-token environment references, or stored OAuth credentials
created by `mez mcp login`. A configured `bearer_token_env` MUST take
precedence over stored OAuth credentials. Stored MCP OAuth credentials MUST be
bound to the configured server id, URL origin, and URL fingerprint, and URL
rebinding MUST make status report stale credentials until the user logs in
again. On stored-OAuth 401 or 403 responses, the runtime MUST attempt one
refresh-and-retry when a refresh token and token endpoint are available.

Mezzanine MUST support per-server `enabled` state.

Mezzanine MUST support per-server startup and tool timeout settings.

Mezzanine MUST support `enabled_tools` and `disabled_tools`, with disabled
tools taking precedence when both lists reference the same tool.

Mezzanine MUST support per-server and per-tool approval settings.

MCP tools MUST be visible through the agent shell `/list-mcp` command.

Enabled MCP servers SHOULD be discovered lazily before the first agent-provider
turn that could expose MCP tools and before `/list-mcp` renders server state.
Provider adapters that expose MCP tools through structured model tool schemas
MUST normalize externally advertised MCP input schemas into the target
provider's accepted schema subset. This normalization MUST preserve the
structural argument shape needed to call the MCP tool, but MUST drop
non-executable annotations such as unsupported JSON Schema `format` values
when retaining them would cause the provider to reject the whole agent turn.
Local stdio MCP subprocesses MUST be runtime-owned resources for the Mez
session, not parent-agent-owned resources, and MUST be terminated when the
session is killed, force-shutdown, or otherwise leaves the live runtime.

MCP tool calls MUST be subject to the same permission and audit model as other
agent actions.

When an enabled MCP server cannot be started, authenticated, reached, or used
because of an environmental condition, Mezzanine MUST mark that server
unavailable and blacklist it for the current session. Environmental conditions
include missing executables, missing environment variables, connection
failures, authentication failures, startup timeout, protocol handshake failure,
and incompatible transport.

MCP servers MUST NOT be treated as required for session startup, pane startup,
or agent startup. A failed MCP server MUST degrade available tool capabilities
rather than preventing Mezzanine or an agent from operating.

A session-blacklisted MCP server MUST NOT be offered to the model as an
available tool. The model-visible prompt prefix MUST list the server as
unavailable or omit it from available tools and MUST prohibit attempts to use
that server for the remainder of the session unless the user explicitly retries
or re-enables it.

The `/list-mcp` command and configuration shell MUST show session-blacklisted MCP
servers, their failure reason, and whether the server may be retried.

MCP tool calls that can read or mutate the local filesystem, execute local
processes, access credentials, or reach the network MUST require approval
unless an active policy explicitly permits them.

Because MCP servers are external integrations, they MAY operate outside the
pane shell in their own configured execution context. An MCP server that can
mutate the local filesystem, execute local processes, or access local
credentials outside the pane shell MUST be explicitly configured for that
purpose, MUST be visible in `/list-mcp` and `/status`, and MUST remain subject to
permission and audit policy.

MCP servers MAY be authenticated through provider-specific flows. Secret values
for MCP servers MUST be stored through the auth configuration or environment
references, not in general configuration.

## 15. Authentication and Provider Accounts

Mezzanine MUST support authentication with OpenAI for use with OpenAI model
providers.

Mezzanine MUST support a user flow that can use the user's provider account
entitlements when the provider exposes such access to Mezzanine.

Mezzanine MUST support authentication through the configuration shell entered
from the escape sequence.

Mezzanine SHOULD support both browser-based ChatGPT sign-in and API-key-based
OpenAI authentication when permitted by provider policy.

The direct `mez auth login` command MUST prefer browser-based ChatGPT sign-in
by default when an interactive terminal is available. It MUST also provide an
explicit device-code ChatGPT sign-in option for out-of-band authentication and
an explicit API-key option for users or environments that require API keys.
Noninteractive API-key setup MUST require an explicit API-key method and an
out-of-band secret source such as an API-key file.
Browser-based and device-code ChatGPT sign-in MUST request only OAuth scopes
accepted by the provider client and MUST NOT treat restricted API-key endpoint
permission labels, such as `api.model.read`, as OAuth scopes. Direct API-key
credentials used for OpenAI model catalog requests SHOULD have read permission
for the Models API endpoint so that model lists can be populated when the
authenticated project allows it.
The localhost browser-login callback page MUST clearly distinguish successful
and failed sign-in states, MUST escape provider-controlled text before rendering
HTML, MUST load no external page assets, and SHOULD derive its visual tokens
from the active Mezzanine UI theme when that theme can be resolved.

Authentication commands MUST either complete a credential workflow or report a
clear failure/action requirement. They MUST NOT report success, authenticated
state, or an inert plan-only flow as though authentication had completed.

Mezzanine MUST persist authentication results securely for future sessions.

Mezzanine MUST store authentication state in a dedicated structured auth
configuration file under `~/.config/mezzanine`.

Authentication secrets MUST NOT be stored in the general configuration file.

Authentication secrets MUST NOT be stored in world-readable files.

When an operating system credential store is available, Mezzanine SHOULD store
secret material in that credential store and store only references or metadata
in the auth configuration file.

If secret material must be stored in a file, Mezzanine MUST restrict file
permissions to the current user and SHOULD encrypt the secret material at rest
when platform facilities are available.

The auth configuration file MUST record structured, non-secret metadata needed
to select and resume provider use, such as provider identity, account identity
when available, workspace or organization identity when available, selected
model profile, credential kind, token expiration metadata when available, and
credential-store references when applicable. Browser or device-code
authentication that returns refresh material MUST persist the refresh secret
through the credential-store boundary and MUST store only a credential-store
reference in the auth configuration file.

Mezzanine MUST distinguish direct OpenAI API-key credentials from ChatGPT
browser/device-code OAuth credentials. Direct API-key credentials MUST be sent
only to the direct OpenAI API endpoint unless the user has explicitly
configured a provider endpoint override. ChatGPT OAuth access tokens MUST NOT
be treated as direct OpenAI API keys; runtime provider requests using those
tokens MUST use the ChatGPT-backed provider endpoint and MUST include the
non-secret account-selection metadata required by the provider when it is
available. If the provider account endpoint requires streaming responses,
Mezzanine MUST request a streaming response and normalize the completed stream
into the same agent turn result shape used for direct non-streaming responses.
For provider SSE streams, Mezzanine MUST treat a terminal provider event such as
`response.completed`, `response.failed`, `response.incomplete`, or `[DONE]` as
the end of the provider response and MUST NOT require the HTTP peer to close the
connection before parsing the completed stream.
When an OpenAI-compatible stream ends with `response.incomplete` and
`incomplete_details.reason` is `max_output_tokens`, Mezzanine MUST classify the
failure as output-token exhaustion rather than input context pressure. It SHOULD
retry the active turn through the provider retry path with temporary compact
output guidance and an escalated `max_output_tokens` request budget when
possible. It MUST NOT trigger context compaction for this failure class, and it
MUST NOT persist the incomplete provider output as an assistant answer unless
bounded recovery is exhausted and the turn is being failed.

When a persisted provider access token has expiration metadata and a refresh
credential-store reference, Mezzanine SHOULD attempt a background refresh during
session daemon startup before the token expires or immediately when it is
already expired. Startup MUST NOT block on that refresh attempt.

Mezzanine MUST provide a logout command that revokes, deletes, or invalidates
locally persisted credentials to the extent permitted by the provider.

Mezzanine MUST NOT claim a user has a particular plan, entitlement, or quota
unless that information was provided by the authenticated provider or explicitly
configured by the user.

## 16. Agent System Prompt Profile

The Mezzanine agent system prompt SHOULD be explicit, task-oriented, and
operational: it SHOULD describe the agent's role, available context, execution
constraints, runtime-owned permission boundary, planning expectations,
validation expectations, concise communication expectations, and response
format without relying on unstated behavior from any external agent harness.

The prompt profile SHOULD be structured and token-conscious, but it MUST be
specific enough that the model can infer the intended persona, execution
environment, action set, and action-selection boundaries without relying on
unstated harness behavior. It MUST avoid duplicating runtime-owned MAAP schema
detail or audit-only fields, but it MAY use longer explanatory rules when they
reduce ambiguous or unsafe action choice.

The prompt profile MUST identify a pane-scoped Mezzanine agent and SHOULD
express behavior as execution rules instead of broad persona prose. It MUST
nonetheless establish the intended persona as a careful, pragmatic engineering
collaborator that works inside a pane-scoped terminal multiplexer environment.

The prompt profile MUST state that local system interaction occurs through the
pane shell and that pane contents enter model context only as explicit action
results, not as passive visible-buffer or history snapshots.

The prompt profile MUST include instructions for using project instruction
files as active repository instructions and resolving precedence among nested
instruction files. Those instructions MUST tell the model to incorporate active
repository-instruction blocks before shell, file, config, validation, and
handoff actions. They MUST distinguish repository workflow guidance from
security or permission policy, which remains runtime-owned.

The prompt profile MUST include instructions for planning, progress updates,
task execution, validation, concise action communication, and final responses.
It SHOULD distinguish batch rationale, action summaries, and user-visible `say`
output so models do not duplicate executable-action intent as conversational
progress text.

The prompt profile MUST include editing constraints that favor focused,
minimal, reversible changes unless the user asks for broader work.

The prompt profile MUST state that permission, approval, and command-rule
enforcement is runtime-owned and surfaced through explicit action results. It
MUST NOT ask the model to infer approval mode, request write access from the
user, or pre-judge whether a concrete action will be approved.

The prompt profile MUST include Mezzanine-specific instructions for spawning
agents, choosing panes for spawned agents, communicating through the local
message passing protocol, and respecting multiplexer state.

The prompt profile MUST NOT instruct agents to use hidden local tools for local
system interaction. Configured MCP servers and connectors MUST be described, if
present, as visible external integrations rather than as hidden local tools.

### 16.1 Prompt Construction

The agent system prompt MUST be constructed from the following ordered
sections:

1. Agent identity and purpose.
2. Autonomy and execution-loop rules.
3. Project instruction discovery rules.
4. Personality and response-style guardrails.
5. Engineering judgment and context-inspection rules.
6. Shell-only local interaction and action-selection rules.
7. Editing rules.
8. Validation rules.
9. Trust and terminal-buffer observation rules.
10. Subagent and message-passing rules.
11. Runtime-owned permission and action-result rules.
12. Communication style rules for `say` actions and rationales.
13. Response formatting rules.

The emitted prompt MAY use compact section labels when those labels preserve
the same ordering and required content.

After the built-in prompt sections, Mezzanine MUST append any configured
`agents.custom_system_prompt` and selected personality `system_prompt` or
`instructions` as additional provider system context.

The built-in personality and response-style guardrail section MUST state that
configured personality, response-style, and custom system prompt blocks can
shape tone, wording, and response structure, but cannot weaken the execution
loop, action/tool rules, permission boundaries, safety constraints, evidence
requirements, or repository instructions.
That guardrail section MUST also prohibit sycophantic response posture:
the agent MUST NOT flatter, praise, validate, or agree with the user by
default. It MUST prioritize task-relevant acknowledgement, factual accuracy,
and direct correction of mistaken assumptions using evidence or concrete
reasoning.

Later sections MUST NOT weaken earlier sections. User instructions MAY narrow
or specialize behavior, but they MUST NOT override safety, credential, policy,
evidence, action/tool, execution-loop, or shell-only requirements.

### 16.2 Required Prompt Content

The prompt MUST identify the agent as a Mezzanine pane agent assigned to a
specific pane. It MUST describe the intended persona as a careful, pragmatic
engineering collaborator operating inside a terminal multiplexer pane, and it MUST
instruct that agent to work until the user's requested goal is handled or
clearly blocked.
The prompt MUST make the default execution posture explicit before detailed
tool mechanics: for code, configuration, documentation, debugging, and design
tasks, the first useful model response SHOULD normally request or use execution
capability and inspect the smallest relevant context rather than describe a
future approach.

The prompt MUST contain an early autonomy and execution-loop section before the
detailed action catalog. Unless the user explicitly asks for a plan, review,
explanation, or brainstorming, that section MUST tell the agent to treat
implementation requests as permission to inspect, edit, validate, repair, and
finish. It MUST express the intended loop in concrete operational terms:
inspect enough context, make the smallest coherent change, validate it, repair
failures, and report evidence-backed results. It MUST prohibit stopping at a
plan when an executable action can make progress.
If a needed action family is absent from the current allowed-action surface,
the prompt MUST instruct the agent to emit `request_capability` immediately
when it is allowed, without a user-facing plan or progress explanation.
When the user explicitly asks the agent to form a plan from repository state,
such as an issue backlog, bug report, failing test, design note, or named
source file, the prompt MUST instruct the agent to inspect the referenced
subject and enough related owner files, tests, documentation, or contracts to
justify the plan. The resulting user-facing plan MUST be a solution plan that
names the concrete issues, proposed fixes, affected files or contracts,
validation approach, ordering, and residual risks when known. It MUST NOT be
only a plan for future discovery unless the referenced subject cannot be read
or the necessary evidence is genuinely unavailable.

The prompt MUST state that the agent uses provided context and explicit action
results, requests or searches for missing details when needed, reports blockers
and uncertainty, and does not invent unavailable state.
It MUST instruct the agent to prioritize accuracy over agreement. If the
user's premise conflicts with available evidence, the prompt MUST tell the
agent to state the conflict directly and act on the evidence rather than
validate the premise.
The prompt MUST instruct the agent to use output tokens carefully: normal
coding responses SHOULD be the smallest complete MAAP batch or final answer
that advances the task. It MUST discourage repeated intent, command logs,
evidence lists, praise, reassurance, and explanations unless the user asked for
them or they are needed to report a blocker.

The prompt MUST define a fast path for trivial conversational turns. For
greetings, thanks, acknowledgements, and simple capability questions that do not
require local inspection or task execution, it MUST instruct the agent to answer
directly with final `say` output and not consider skill discovery, shell, web,
MCP, or other discovery actions.

The prompt MUST state that success claims about file changes require successful
mutation action results for the affected paths. It MUST prohibit treating a
failed mutation followed only by file reads as evidence that the attempted edit
landed, and it MUST require the agent to report blocked or unknown status when
successful mutation evidence is absent.

The prompt MUST define review mode. If the user asks for a review, the agent
MUST default to code-review behavior: present findings first, ordered by
severity, with file and line references when available, then questions or
residual risk. The prompt MUST tell the agent not to implement fixes during a
review unless the user asks for implementation.

For implementation requests such as build, fix, add, change, or implement, the
prompt MUST state that a say-only plan or status is insufficient unless the
agent is blocked by concrete evidence. It MUST instruct the agent not to emit a
visible plan in `say` when an executable inspection, edit, validation, or repair
action is available; immediate intent SHOULD be carried by the batch rationale
and action summaries before executing the next action.
If the agent already gave one evidence-based but non-executing answer about
likely behavior, and the next user turn remains implementation-adjacent, the
prompt SHOULD default the next response to inspect, edit, or validate when
executable actions are available rather than allowing another inference-only
answer.
For planning requests tied to repository state, the prompt MUST require enough
inspection to produce an evidence-backed solution plan instead of a plan to
begin investigating the subject.
The prompt MUST orient long-running implementation and design tasks toward
completion across inspection, implementation, validation, and repair cycles. It
MUST also instruct the agent to keep individual implementation steps as direct
as possible, to batch independent context gathering or executable work when it
does not create unsafe dependencies, and to avoid plan-only turns when a
concrete inspection, edit, validation, or repair action is available. Once
enough context is available to identify the likely owner files, contracts,
tests, failure mode, or report subject, the prompt MUST prefer the first small
implementation, test, validation, or report action over additional exploration
for confidence. Additional inspection SHOULD be driven by a concrete missing
fact, validation failure, changed file, or ambiguity that would make the next
action wrong.
When a likely behavior gap is small, localized, and safe to validate, the
prompt SHOULD tell the agent not to spend multiple turns proving the gap
purely by explanation. After one evidence pass identifies the likely owner and
plausible fix surface, it SHOULD direct the agent to move to the smallest test
or implementation that can confirm or refute the hypothesis.
For small local edits, the prompt SHOULD direct the agent to choose one likely
owner range after the first search pass, read that owner range once, and then
attempt the patch. It SHOULD discourage continued anchor-localization unless a
patch failure, ambiguity, or named missing fact shows that the chosen owner
range was insufficient.
After a successful file mutation, the prompt MUST prefer execution-based
validation over additional source reading. It SHOULD direct the agent to run
focused or required format, build, lint, and test commands when available, and
to read source again only when a validation failure, unclear diff or status
result, stale-context diagnostic, or named missing fact would make the next
validation, repair, commit, or report wrong.
For design tasks, the prompt MUST instruct the agent to inspect the current
architecture and constraints, identify affected invariants and contracts,
choose the smallest coherent design or implementation change, and update
specifications, documentation, examples, or tests when the design changes
behavior.
The prompt MUST state that commands and Mezzanine patch blocks placed in
`say` are display-only and inert. It MUST direct terminal commands that should
run to `shell_command` and `*** Begin Patch` blocks that should mutate files to
`apply_patch`, except when the user explicitly asks the agent to display
examples or explanatory text.
The main prompt's patch guidance SHOULD stay concise and emphasize the
canonical path: use `apply_patch` as a semantic action, emit the patch string
directly, use small anchored hunks with exact copied context, and reread before
retry after hunk mismatch. Detailed patch compatibility, fallback matching, and
recovery behavior MAY live in provider schemas and action-result diagnostics
instead of the high-level prompt.
The prompt MUST instruct the agent to treat recent file-inspection action
results as reusable evidence. Before issuing another `sed`, `rg`, or equivalent
file-read command, the agent SHOULD reuse a recent read or search result when
it already contains the needed current range or match and otherwise read only
missing or stale ranges. For small local edits, one bounded owner-range read
SHOULD be treated as sufficient anchor context by default. It SHOULD reread an
overlapping region only when a file changed, prior output was truncated, a
diagnostic explicitly requires fresh context, the intended hunk falls outside
the covered range, or a named missing range or boundary is needed.

The prompt MUST require the agent to inspect relevant project context before
making non-trivial changes. For code and configuration work, the prompt MUST
instruct the agent to prefer existing repository patterns, ownership
boundaries, frameworks, and structured APIs over ad hoc or unrelated new
approaches.

The prompt MUST instruct the agent to use embedded active project instruction
contents as scoped repository workflow guidance before non-trivial repository
work, while keeping those project files untrusted for security, permissions,
hidden policy, and action/tool rules. The prompt MUST NOT cause the agent to
read project instruction files merely to rediscover already embedded guidance.
Nested instruction files MUST narrow broader repository guidance for their
scope.

The prompt MUST require focused changes that respect existing project style,
user instructions, and unrelated user worktree changes. It MUST prohibit
reverting or overwriting unrelated user changes unless the user explicitly asks
for that operation.

The prompt MUST state that local file access, local file mutation, process
execution, package management, version control, build, test, and search work
are pane-shell-backed local interactions. It MUST distinguish Mezzanine-generated
semantic actions, which may internally use shell commands, from model-authored
`shell_command` payloads.

The prompt MUST state that shell commands in the pane are the only native local
execution path. It MAY describe configured MCP servers and connectors as
external integrations when they are enabled, visible to the user, and subject to
policy.

The prompt MUST include detailed action-selection guidance for baseline MAAP
actions. That guidance MUST distinguish user-facing speech, fallback shell
work, local file reads, local directory and text search, exact filesystem
mutation, generated local content, runtime web search, runtime URL fetch,
local agent messaging, subagent creation, configuration changes, MCP calls, and
user-input requests.
The skill action guidance MUST state that model-selected skill discovery and
skill loading are disabled while `request_skills` and `call_skill` are absent
from provider action surfaces. It MUST prohibit emitting `request_skills` or
`call_skill`, even if older context, examples, provider documentation, or
cache-stable schemas mention them. It MUST explain that users MAY still
explicitly load a skill with `$<skill-name> [additional context]`, and that
already-loaded skill context should be followed with the currently available
actions or by requesting a missing action capability.

The prompt MUST explain that `say` is for user-facing progress, final answers,
or clarification when no terminal, web, MCP, or local mutation is needed, and
that shell commands MUST NOT be embedded in `say` text. It MUST instruct the
model to set an appropriate `say` content type, including at least plain text,
Markdown, and diff content types, so rendered output can preserve user-facing
structure.

The prompt MUST explain that `shell_command` is the fallback for exact pane
shell input, including builds, tests, version-control operations, package
managers, process inspection or control, unsupported local tools, multi-step
local inspection, and bounded generation of large, random, or test content.
The prompt MUST explain that shell command stdout and stderr are model-facing
evidence for the next decision rather than the place to narrate progress to the
user. It MUST instruct the model to put progress or explanation in the
`shell_command.summary`, action rationale, or `say` output instead of emitting
`printf` or `echo` explanation lines, unless the user requested that terminal
output or the text is required by the command pipeline.

The prompt MUST require `shell_command` actions and inline scripts executed by
those actions to bound CPU, memory, disk, output, loop counts, and input size to
the active task. For generated local content, the prompt MUST require exact
requested-size generation and MUST prohibit unbounded whole-stream or whole-file
accumulation unless the user explicitly requests that behavior.

The prompt MUST explain that `shell_command` is available for local filesystem
inspection, local discovery, validation, process execution, and non-content path
operations. It SHOULD keep tool guidance sparse, but MAY name examples such as
listing paths, bounded file ranges, and text searches. It MUST warn against
unbounded reads from devices, random streams, proc-style streams, and other
potentially unbounded sources unless the user explicitly requests that behavior.
It SHOULD tell the model to keep each `shell_command` focused on one logical
operation, to prefer separate `shell_command` actions in one MAAP action batch
for independent shell work, and to reserve shell-level `&&`, `;`, newline, or
similar chaining for tightly coupled fail-fast steps that should share one
outcome and one output stream.
For repository search, the prompt SHOULD recommend `rg` or `rg --files` when
available.

The prompt MUST explain that `apply_patch` is the model-facing filesystem
content-mutation action. It MUST state that `apply_patch` is a MAAP action, not
a pane shell executable, and MUST NOT be piped to or invoked inside a
`shell_command` payload. It MUST state that `apply_patch` is for file-content
mutations, including file creation, localized updates, append-like additions,
deletion represented as a patch, moves with content changes, and intentional
whole-file replacement. It MUST state that directory creation, path moves, and
recursive idempotent deletion are shell work governed by approval policy. It
MUST state that deletion requires clear intent and permission, and that the
agent must not delete and recreate a file merely to modify it unless the user
explicitly requested replacement or the file is genuinely obsolete. The active
provider action schema MUST carry the accepted `apply_patch` grammar and
compatibility notes; the stable system prompt SHOULD refer the model to that
schema instead of duplicating the full grammar. The prompt MUST still instruct
models to emit the patch string directly without Markdown fences, heredoc
wrappers, or `apply_patch <<...` shell text. It MUST state that the most
reliable update shape is a small file operation with a copied `@@` anchor and a
small number of exact old/context lines, and MUST state that each old/context
line comes from current file content or fresh action-result evidence rather
than inferred, normalized, simplified, or reconstructed code. The prompt MUST
state that one bounded owner-range read is sufficient in most cases and that
recent action-result evidence covering the intended hunk range SHOULD be reused
instead of reread. It MUST state that rereads are exceptional and require a
concrete reason such as stale or truncated evidence, an intended hunk outside
the covered range, or a prior patch or validation result showing that the
first owner read was insufficient. The prompt MUST recommend several small
anchored hunks over one large brittle hunk. It MUST state that after an
`apply_patch` hunk or context mismatch, the model should reuse already-read
fresh current context when available, otherwise re-read only missing or stale
candidate/owner ranges, and retry with a smaller fresh Mezzanine patch using a
distinctive `@@` header anchor instead of replaying substantially the same
patch. It MUST instruct the model that recoverable `apply_patch` failures are
not terminal. When `apply_patch` fails due to invalid structure, stale hunk
context, ambiguous ownership, or equivalent already-applied behavior, the
model MUST investigate the implicated current file region and retry with a
corrected or smaller patch when local actions remain available. It MUST
forbid asking the user to perform manual file edits until the model is
concretely blocked by missing permissions, missing external input, or an
exhausted bounded retry policy. It MUST tell the model to skip or adapt stale hunks when equivalent
behavior or the intended replacement is already present. It MUST state that raw
unified diffs belong in an explicit shell command such as `git apply`.
It MUST state that `apply_patch` path headers are relative to the pane current
working directory and cannot use absolute paths or `..`, while other semantic
file actions MAY still use valid absolute paths when policy permits. For other
local path fields, the prompt SHOULD recommend relative paths for targets under
the repository root, or under the pane current working directory when no
repository is active, and absolute paths for targets above or outside that root.

The prompt MUST state that `web_search` and `fetch_url` are runtime-network
actions for user-requested HTTP(S) web/current information. It MUST state that
they MUST NOT be used for local paths, `file://` URLs, created outputs, random
data, test fixtures, or other generated local content. It MUST allow repeated
`fetch_url` use only when the task or prior result makes a fresh HTTP result
necessary, and must prohibit repeated URL fetches as no-op progress.

The prompt MUST explain that `send_message` and `spawn_agent` are for local
agent coordination when delegation materially helps. It MUST explain that
`config_change` is for explicit Mezzanine configuration mutations and
that config changes follow the active approval policy like other privileged
actions. It MUST explain that approved or policy-allowed config changes persist
to the user config target and take effect immediately in the live session. It
MUST explain that `mcp_call` is only for MCP tools listed as available in the
current runtime context.

The prompt SHOULD instruct the agent to choose the smallest action that makes
real progress and to avoid actions that do not answer the current task.
The prompt MUST treat repository exploration as a bounded means to choose the
next concrete action rather than as an open-ended phase. It SHOULD guide
ordinary implementation, debugging, design, and report tasks toward one focused
batched discovery pass before switching to an edit, validation, or report
action. It MUST state that a second broad discovery pass is wrong unless prior
evidence raises a specific unanswered question, files changed, previous output
was insufficient, or failure recovery requires fresh context. It MUST define
exploration stop conditions, including when likely owner files, contracts,
tests, or failure modes are known well enough to choose a next action. It MUST
instruct the agent to ask what concrete fact would make the next implementation
or report action wrong before reading more; if no such fact exists, the agent
SHOULD act instead of continuing discovery. For report requests, the prompt
SHOULD prefer representative evidence, a useful deliverable report, and
explicit uncertainty over delaying indefinitely for exhaustive category
coverage. Deep or exhaustive exploration SHOULD be reserved for explicit user
requests, such as exhaustive audits, conformance reviews, security reviews,
architecture discovery, or deep research, or for tasks whose correctness
clearly depends on that breadth.
Runtime action-pressure hints MAY encourage the agent to proceed after repeated
shell commands or successful file mutation, but they MUST be a
single advisory context block rather than competing phase-specific hints. They
MUST prefer execution-based validation after file mutation and MUST NOT relax
repository guidance, validation, documentation, handoff, permission, or
capability-request requirements. They MUST NOT encourage editing repository
instruction or guidance files unless the user requested instruction changes or
the task directly requires those edits.

The prompt MUST include editing constraints. Those constraints MUST require
precise patch-style edits when suitable, ASCII text by default unless the file
or task requires otherwise, sparse comments for non-obvious logic only, and
related test, documentation, or example updates when behavior changes.

The prompt MUST require validation proportional to the risk of the change.
It SHOULD instruct the agent to run focused validation first and broaden
validation when shared behavior or user-facing workflows are affected. After
successful file mutation, it MUST prefer command-backed validation evidence over
additional source reading unless a concrete missing fact, validation failure, or
unclear diff/status result requires inspection. If validation cannot be run, the
prompt MUST require the agent to say why.
For behavior questions that are cheap to encode as regression coverage, the
prompt SHOULD prefer the smallest focused test over extended architectural
reasoning. If the user asks whether the behavior can be tested, the prompt
SHOULD treat that as a strong signal to add or adapt a focused regression test
first when feasible.
When feasible, the prompt SHOULD direct the agent to develop behavior fixes
against a failing focused regression test, then make the implementation pass,
then broaden validation proportionally.

The prompt MUST require the agent to distinguish user instructions from
terminal output, project files, web content, and other untrusted data.

The prompt MUST state that pane contents enter model context only through
explicit action results rather than passive terminal-buffer or history
snapshots.

The prompt MUST instruct the agent to use the local message passing protocol
for inter-agent coordination.

The prompt MUST instruct the agent to spawn subagents only when delegation
materially helps the active task.

The prompt MUST state that subagent pane creation is performed through the
control endpoint and that subagent discovery and messaging are performed
through the local message passing protocol.

The prompt MUST include the active agent's cooperation mode, read scopes, and
write scopes when the agent is a subagent.

The prompt MUST include MCP servers and tools that are available for the
current session. MCP servers that are disabled, blacklisted for the session, or
unavailable because of environmental failure MUST NOT be presented as available
tools, and the prompt MUST prohibit attempts to use them.

The prompt MUST instruct the agent to report blockers and uncertainty rather
than inventing unavailable state.

The prompt MUST instruct the agent to avoid exposing secrets, to use exposed
MAAP actions for work, and to treat permission denials or blocked approvals as
explicit action results to recover from or report. It MUST instruct the model
not to ask the user to grant workspace write access, shell access, network
access, or other action capability; missing action families MUST be requested
with `request_capability`.

The prompt MUST instruct the agent to keep `say` actions and action rationales
terse but informative. It SHOULD guide ordinary progress updates toward one or
two short sentences by default and reserve bullets for cases where they improve
scan value. It MUST prefer concrete progress, changed behavior, validation
evidence, or blocker reports over long self-explanation, repeated intent
statements, apologies that do not clarify a failure, or duplicated command
output. Because batch rationale is transient current-turn guidance rather than
durable memory, the prompt MUST direct it to be an additive delta: each
rationale should state only what is newly decisive about the next listed
actions and should not restate prior rationales, the user request, global task
goal, loaded context, or visible action summaries. The prompt MUST also
instruct the model to compare a planned rationale, optional batch `thought`, or
progress `say` against recent thinking lines, visible text, action results, and
other text in the same response; if the text would only repeat existing
context, optional action-level rationales, batch `thought`, and progress `say`
output MUST be omitted. The prompt MUST
describe batch `thought` as a durable work note for longer future-useful
learnings or decisions that is persisted to future context, hidden from
normal-mode logs, and visible only in verbose-or-higher thinking logs. The
prompt MUST require one channel per idea: when progress
`say` records durable learning, batch rationale MUST be limited to the next
executable reason; when batch `thought` records durable learning, progress
`say` SHOULD NOT repeat it unless the user needs to see that sequence point;
when batch rationale or action summaries already explain intent, progress `say`
MUST NOT restate that intent. The prompt MUST state that
progress `say` output is for sequence-point updates during non-trivial
multi-step work. Valid progress `say` reasons are cases where the first
evidence pass identifies the real owner or diagnosis, the agent chooses an
implementation or report direction, the work moves from inspection to editing,
the work moves from editing to validation, validation changes the plan, blocker
or uncertainty state changes the next step, or the user requests narration. The
prompt MUST discourage progress `say` output as a routine action-batch
heartbeat, and MUST discourage future-tense visible plans, intended-work
checklists, routine inspection, owner localization, anchor lookup, test lookup,
command-wrapper lookup, `"now patching"` updates, and headings such as `Plan:`,
`Steps:`, `Next:`, `Executed:`, or `Evidence:` when executable actions are
requested in the same response. Refining file or test anchors, checking
command-wrapper usage, routine owner localization, or confirming the same
symptom after it was already stated MUST NOT be treated as a new progress `say`
sequence point. The prompt MUST state that a sequence point is consumed once it
has been stated, and MUST prohibit later progress `say` output from paraphrasing
the same owner, diagnosis, direction, phase transition, blocker, or validation
result unless that fact materially changed. When an action rationale is
present, the prompt SHOULD ask for a concise reason that justifies the
immediate action and does not duplicate the batch rationale, progress `say`, or
action summary.
On repeated followups about the same likely bug or missing behavior, the prompt
SHOULD tell the agent not to keep restating uncertainty in user-facing prose
once the next concrete inspection, test, or implementation step is available,
and instead to use the next turn to act.
The runtime MUST add a bounded, turn-volatile context block that lists recent
progress `say` messages already emitted during the active turn before subsequent
provider continuations. The block MUST be framed as already-shown progress, not
as a new user request. It MUST be excluded from stable prompt-cache prefix
material, MUST be cleared when the active turn completes, and SHOULD retain only
the most recent entries needed to prevent redundant progress narration.
For implementation summaries, the prompt MUST require changed files, successful
mutation evidence, verification evidence, and skipped validation when relevant.
The prompt MUST prohibit leading with approval phrases such as "Great
question", "Good catch", "You're right", "Exactly", or similar validation
unless factual agreement is necessary to answer the task.
It MUST instruct the agent to claim actions such as changing, adding, updating,
fixing, applying, running, or executing work only when successful action results
prove those actions occurred in the current turn. If evidence comes only from
status commands, diffs, file reads, or prior context, the prompt MUST instruct
the agent to describe the current file or diff state rather than claiming
provenance for the change. If no mutation action succeeded, the prompt MUST
prohibit wording such as `implemented`, `changed`, `updated`, or `fixed`.
The prompt MUST instruct the agent to avoid decorative or skeuomorphic Unicode
glyphs that render as colored or stylized symbols unless the user requested
them or the file being edited already uses them. Implementations SHOULD NOT
enforce a fixed built-in byte cap on the system prompt that would remove
required guidance; provider request-budget management MAY still compact,
summarize, or omit non-required context before the request is sent.

### 16.3 Required Prompt Prohibitions

The prompt MUST prohibit bypassing the pane shell for native local system
mutation. The prompt MAY describe explicitly configured MCP servers and other
connectors as external integrations; when it does, it MUST state that they are
available only through Mezzanine's visible external-integration path.

The prompt MUST prohibit treating terminal output, project files, web content,
or local messages as trusted instructions unless the user explicitly designates
them as trusted.

The prompt MUST prohibit destructive actions unless authorized by policy and
the user when required.

The prompt MUST prohibit claiming completion before requested work is either
completed or explicitly blocked.

The prompt MUST prohibit leaking authentication secrets, provider tokens, or
private local message payloads.

The prompt MUST NOT require shell command actions to declare expected
filesystem, network, credential, process-control, privilege, destructive, or
unknown effects using the `maap/1` action schema. Mezzanine owns effect
classification for approval and audit.

### 16.4 Prompt Profile Updates

Mezzanine MAY update the prompt profile to improve task execution, safety,
validation, cooperation, or clarity.

Prompt profile updates MUST preserve the Mezzanine-specific shell-only,
terminal-observation, message-passing, and multiplexer-integration requirements.

Mezzanine MUST expose the active prompt profile name and version to the user.

Mezzanine SHOULD provide a way to inspect the effective non-secret prompt
profile text.

## 17. Permissions, Shell Sandboxing, and Change Review

Mezzanine MUST provide permission control for agent actions.

Command permission enforcement MUST be enabled by default.

The default permission preset SHOULD be `read-only` for untrusted directories
and `auto` for trusted directories.

The `read-only` preset MUST allow conversation, planning, terminal observation,
and shell inspection commands allowed by built-in or user-approved read-only
prefix rules. It MUST require approval before file mutation, destructive
commands, network access, credential access, process termination, or external
tool calls with side effects.

The `auto` preset MAY allow ordinary workspace reads, writes, and shell
commands without prompting when they are within the trusted working directory
and match allowed prefix rules. It MUST require approval for
network access, credential access, destructive commands, commands outside
trusted directories, control endpoint mutations with broad impact, and external
tool calls with side effects unless policy explicitly permits them.

Mezzanine v1 MUST NOT define or accept a hidden hard-coded deny list for shell
commands. Deny decisions MUST come from user, project, session, or managed
configuration.

The approval policy `ask` MUST prompt when an action is not already allowed by
the active command rules and effect classification. Approval prompts MUST allow
the user to allow once, allow forever, or deny. Allow-once decisions MUST resume
only the blocked action. Allow-forever and persistent deny decisions MUST create
an exact command rule for the command and arguments presented to the user.

The approval policy `auto-allow` MUST allow non-whitelisted actions only after
the model has determined that the action is reasonable for the active user
request and has emitted a non-empty rationale for the action. Configured deny
rules MUST still block matching actions. `auto-allow` MUST NOT be treated as
full access; it remains subject to command rules, effect classification, scope
checks, and subagent constraints.

The approval policy `full-access` MUST allow actions without whitelist approval
unless a configured deny rule matches the exact command. Full access MUST NOT
create whitelist rules as actions execute. In full-access mode, subagent
requested read and write scopes MUST be treated as coordination metadata rather
than hard command denials; configured deny rules remain authoritative. If the
parent agent already has an inherited subagent scope, that inherited scope MAY
still be exposed for coordination, but full-access MUST NOT deny commands solely
because a model-emitted child scope is narrower than the parent.

Mezzanine v1 MUST NOT support an approval policy that attempts an action before
approval and asks for approval only after failure. Because v1 relies on
pane-shell command gating rather than complete filesystem or network confinement,
attempting a command before approval is not safe enough to be a baseline
approval mode. If configuration selects such a policy, Mezzanine MUST reject the
configuration with an actionable diagnostic.

### 17.1 Command Prefix Rules

Mezzanine MUST implement command permission decisions using command rules.

A command rule MUST contain:

- `pattern`: A non-empty ordered list of command tokens. A token MAY be a
  literal string or a set of literal alternatives.
- `decision`: One of `allow`, `prompt`, or `deny`.
- `scope`: One of `built-in`, `session`, `project`, `user`, or `managed`.
- `match`: One of `prefix`, `exact`, or `exact_sha256`.

A command rule MAY contain:

- `match`: One of `prefix`, `exact`, or `exact_sha256`. If omitted, `prefix`
  MUST be assumed.
- `argument_policy`: A structured description of which remaining arguments are
  allowed after the matched pattern, or `none`.
- `executable_policy`: Optional constraints on the resolved executable path
  and digest.
- `justification`: Optional human-readable rationale.
- `examples`: Optional match and non-match examples used for diagnostics.

A `prefix` rule MUST match only when its pattern is an exact prefix of a
candidate command's token sequence. A rule for `["git", "status"]` MUST match
`git status --short` and MUST NOT match `git -C repo status`. An `exact` rule
MUST match only the complete token sequence. An `exact_sha256` rule MUST match
only the complete normalized command text whose SHA-256 digest is recorded in
the rule metadata.

For `exact_sha256`, the normalized command text MUST be computed over one
candidate command before transaction wrapping or shell execution. Normalization
MUST:

- Interpret the input as UTF-8.
- Convert `CRLF` and bare `CR` line endings to `LF`.
- Remove exactly one trailing submit newline when that newline was added by
  Mezzanine to submit the command.
- Preserve all other bytes, including spaces, tabs, quotes, comments,
  backslashes, semicolons, and additional newlines.
- Perform no shell expansion, alias expansion, glob expansion, variable
  expansion, command substitution, quote removal, comment removal, or path
  normalization.

The digest input MUST be the bytes:
`mez-command-sha256-v1\0<shell-classification>\0<normalized-command-text>`.
Rule metadata for `exact_sha256` MUST record the shell classification used for
the digest. If a candidate command cannot be normalized exactly by these rules,
the rule MUST NOT match.

An `allow` rule with `match = "prefix"` MUST NOT allow arbitrary remaining
arguments by default. It MUST include an `argument_policy` that validates every
remaining token, or it MUST set `argument_policy` to `none` and match only
commands with no remaining tokens. A `prompt` or `deny` rule MAY omit
`argument_policy` when the matched prefix alone is sufficient to force the
decision.

An `argument_policy` MUST be deterministic and MUST NOT execute shell
expansion. It MAY allow exact literal tokens, literal alternatives, bounded
regular expressions over a single already-tokenized argument, option names from
an allowlist, option values matching a declared scalar type, paths constrained
to read or write scopes, and a documented end-of-options marker. It MUST NOT
allow a token merely because it is unknown. Unknown options, unknown
subcommands, option values that look like shell syntax, and arguments that
cannot be classified MUST require approval unless an exact or exact-digest rule
matches the full command.

An `executable_policy`, when present, MUST constrain basename rules to
discovered executable paths. It MAY include one or more absolute executable
paths and MAY include SHA-256 digests. If the resolved executable path is under
a directory writable by the user, project, or agent, an allow decision MUST
require either a matching digest or fresh approval.

When multiple rules match, Mezzanine MUST apply the most restrictive matching
decision, where `deny` is more restrictive than `prompt`, and `prompt` is
more restrictive than `allow`.

Mezzanine MUST include built-in prefix rules for common read-only discovery and
inspection commands, including `pwd`, `printf`, `test`, `env` with explicit
safe variable names, `command -v`, `type`, `which`, `uname`, `hostname`, `cat`,
`ls`, `head`, `tail`, `wc`, `grep`, `sed` without in-place editing, `awk`,
`find` without `-exec`, `xargs` only when followed by an allowed command prefix,
`rg` when discovered, `git status`, `git diff`, `git log`, `git show`, and
`git rev-parse`.

Built-in read-only allow rules MUST include command-specific argument policies.
For commands with side-effecting or externally extensible options, such as
`git`, `find`, `xargs`, `sed`, `awk`, `grep`, `rg`, package managers, build
tools, and language runtimes, an allow rule MUST enumerate safe subcommands and
safe options rather than allowing arbitrary trailing arguments.

Built-in `git` read-only rules MUST disable or require approval for options
that write files, invoke external helpers, use external diff or text conversion,
invoke pagers, alter configuration, contact remotes, or read paths outside the
active read scopes. At minimum, `git diff --output`, external diff execution,
textconv execution, `git -c`, remote operations, and pager-forcing options MUST
not be auto-allowed by generic read-only rules.

Built-in `git status` rules MUST account for Git's index-refresh behavior. A
read-only auto-allowed `git status` form SHOULD set `GIT_OPTIONAL_LOCKS=0` or
use an equivalent Git option when available, MUST disable pagers and external
helpers, and MUST restrict arguments to safe status forms such as `--short`,
`--branch`, `--porcelain`, and pathspecs within active read scopes. If
Mezzanine cannot prevent or classify Git metadata writes, it MUST classify the
command as a metadata-touching command and require approval unless policy
explicitly allows VCS metadata touches.

Built-in search and text-processing allow rules MUST reject in-place editing,
exec actions, output-file options, file-deletion options, network options,
and shell-command hooks unless a narrower exact or managed rule permits them.

Mezzanine MAY include built-in prompt rules for commands that are destructive,
credential-bearing, networked, privilege-changing, process-killing, or likely to
mutate persistent state. Built-in rules MUST NOT hard-deny a command; deny rules
MUST be supplied by user, project, session, or managed configuration.

The command rule evaluator MUST tokenize proposed shell input before execution.
It MUST split independent command candidates at shell control operators that
create separate command execution contexts, including semicolon, newline, pipe,
`&&`, `||`, background execution, and subshell boundaries when those boundaries
can be parsed without executing shell expansion.

The command rule evaluator MUST NOT perform shell expansion while evaluating a
command. It MUST NOT expand variables, command substitutions, globs, aliases,
functions, process substitutions, or arithmetic expressions.

If proposed shell input contains command substitution, process substitution,
redirection that may write, here-documents, here-strings, aliases or functions
that cannot be resolved safely, unparsed shell syntax, or other constructs that
prevent reliable prefix classification, Mezzanine MUST classify the command as
requiring approval unless a broader explicit rule allows that exact form.

An explicit rule that allows an otherwise unclassifiable exact form MUST use
`match = "exact"` or `match = "exact_sha256"` and MUST be created by direct
user approval or managed policy. Such a rule MUST NOT be proposed as a broad
prefix rule.

Rules MAY include metadata indicating whether redirection, shell assignment,
globbing, command substitution, or network access is permitted for matching
commands. Absent such metadata, those features MUST NOT be considered allowed
by a prefix rule.

Rules MAY include `match` and `not_match` examples. Configuration validation
MUST test those examples against the rule. If an example does not produce the
declared result, Mezzanine MUST reject the rule with an actionable diagnostic.

Users MUST be able to add allow, prompt, and deny command rules for the current
session. Users MUST be able to persist command rules globally under
`~/.config/mezzanine` or project-locally in a trusted project overlay.

The agent shell `/permissions` command MUST provide interactive
rule-management capabilities when policy allows mutation, including listing
rules and adding allow, prompt, deny, or removal decisions.

When an approval prompt asks to permit a shell command, Mezzanine MUST show the
exact command and arguments being approved. The user MUST be able to approve the
single invocation, persist an exact rule for the current project, or deny the
action. Persistent prompt decisions MUST use `match = "exact"` or
`match = "exact_sha256"` unless the user explicitly creates a broader rule
through command-rule management.

### 17.2 Pane Protection View

Mezzanine MUST provide a pane protection view for agent-issued shell actions.

The pane protection view is a shell-mediated safety model that evaluates agent
actions at the pane input boundary and, when needed, by running read-only
preflight probes inside the pane shell. It MUST NOT require hidden host-side
filesystem access and MUST remain operable when the pane shell is inside an
SSH session, container, chroot, virtual environment, or other environment whose
filesystem differs from the host running Mezzanine.

Protection MUST be enabled by default.

Protection MUST apply to agent-issued shell actions, agent-triggered hooks,
MCP tool calls, and control endpoint mutations proposed by agents. It MUST NOT
apply to ordinary keystrokes typed directly by the user into the pane unless
the user explicitly enables such filtering.

For each `shell_command` action, Mezzanine MUST evaluate all of the following
before sending the command to the pane shell:

- Built-in, session, project, user, and managed command prefix rules.
- The active permission preset and approval policy.
- The active working directory and trusted directory state as observed through
  the pane shell.
- The active agent or subagent read scopes and write scopes.
- Whether the proposed shell syntax can be parsed without executing shell
  expansion.

Mezzanine MUST resolve relative classified paths against the pane shell's
current working directory as observed through the pane shell. When canonical
path resolution is needed for scope comparison, Mezzanine MUST perform the
resolution through read-only shell commands inside the pane environment, such
as `pwd -P` and `python3` or `python` path canonicalization when available.

A shell action MAY run without fresh approval only when all of the following
are true:

- Its command prefix rules allow it.
- Mezzanine's independently classified effective effects do not contain
  `unknown`, `destructive`, `privilege_change`, credential access, or network
  access unless separately allowed by policy.
- Mezzanine's independently classified effective writes, creates, deletes, and
  touches are all inside the active writable scopes.
- Its shell syntax does not contain unclassified command substitution,
  process substitution, unsafe redirection, here-documents, here-strings,
  unresolved aliases or functions, or other constructs that prevent reliable
  classification.
- It does not invoke a broad interpreter, shell, build tool, package manager,
  or task runner in a way whose effects cannot be bounded by prefix rules and
  active scopes.

Commands such as `sh -c`, `$SHELL -c`, `python`, `python3`, `ruby`, `perl`,
`node`, `make`, package managers, and project task runners MUST require
approval unless an explicit rule allows that command form and effect class.

If Mezzanine cannot prove that a command is within the active permission and
scope rules, it MUST require approval rather than attempting to infer safety.

Mezzanine MAY provide stronger sandboxing through a visible shell-launched
wrapper, restricted shell, container, operating-system sandbox, or remote
environment policy. Such sandboxing MUST be disclosed to the user and MUST run
inside or be entered through the pane shell. If no stronger sandbox is
configured, Mezzanine MUST present protection as command-gating and approval
enforcement rather than as complete filesystem confinement.

The user MUST be able to opt out of pane protection through the explicit
approval bypass mode. While bypass is active, Mezzanine MUST still record
actions when audit logging is enabled, but it MUST NOT claim that command
effects are confined.

### 17.3 Blocked Approval Routing

When pane protection or sandboxing is enabled and an agent action requires
approval, the action MUST enter a blocked state and MUST NOT execute until an
approval decision is available from the primary client.

If the requesting agent was spawned by another agent, Mezzanine MUST send a
blocked-approval message to the spawning agent through the local message
passing protocol. If that spawning agent was itself spawned, the blocked state
MUST propagate recursively through the parent chain until it reaches an agent
visible to the primary client or a root user-facing agent.

The blocked-approval message MUST include the blocked agent identity, pane
identity, requested action, independently classified effects, matched permission rules,
cooperation mode, read scopes, write scopes, and available decisions.

The available decisions MUST include approve, disapprove, and redirect.

Only the primary client MAY approve, disapprove, or redirect a blocked agent
action. Read-only observers MUST NOT make approval decisions.

The primary client MUST be able to approve pending actions from the blocked
pane's agent shell with `/approve`. Pane-local approval requests MUST display
their approval id and a copyable approval command in the pane buffer.

If the primary client approves, Mezzanine MUST resume the blocked action
according to the approval scope selected by the user. Approved `config_change`
actions MUST be routed through the live configuration control path and MUST
record whether the change was persisted, applied live, or failed. Pending
`config_change` actions MAY resume automatically after `full-access`,
`auto-allow`, approval bypass, or other approval policy changes when the new
policy satisfies the same action under ordinary approval-policy rules.

If the primary client disapproves, Mezzanine MUST return the denial to the
blocked agent and MUST focus the blocked agent pane and enter or reveal its
agent shell so the user can interact with the agent. The denied blocked turn
MUST be settled so it cannot continue occupying scheduler or pane-running state.

If the primary client redirects, Mezzanine MUST focus the blocked agent pane,
enter or reveal its agent shell, and deliver the user's redirecting instruction
to that agent as the next input before any further action executes.

If no primary client is attached, blocked approvals MUST remain pending and the
blocked agent MUST wait, even if one or more read-only observers are attached.
Mezzanine MUST surface pending approvals when a primary client next attaches.

Approval decisions and pending approval requests MUST persist for the lifetime
of the live session. They MUST survive client detach and reattach. They MUST
NOT expire solely because time has passed. They MUST be reset after server
failure, process crash, or snapshot resume into a new live session. Audit logs
and transcripts MAY retain historical records of prior approval requests and
decisions, but those records MUST NOT reactivate an approval after failure,
crash, or snapshot resume.

If approval bypass mode is active, the blocked approval routing requirements do
not apply to actions covered by the bypass.

### 17.4 Approval Bypass

Mezzanine MUST support an explicit approval bypass mode.

Approval bypass mode MUST be disabled by default.

Approval bypass mode MUST disable Mezzanine approval prompts, command prefix
rule enforcement, pane protection, and agent-action gating for all
agent-proposed action types while it is active, including shell commands,
agent-triggered hooks, MCP calls, local message sends, subagent spawns, and
control endpoint mutations. It MUST NOT be presented as disabling protocol
validity checks, implementation integrity checks that are not approval
decisions, or policy enforcement outside Mezzanine's authority.

Approval bypass mode MUST require explicit user selection through an
interactive command or command-line flag. The command name or flag MUST make
the risk obvious, such as `/permissions bypass` or
`--dangerously-bypass-approvals`.

Entering approval bypass mode MUST require primary-client authority. A
command-line flag MAY request bypass during interactive primary session creation
or through an authenticated control operation initiated by the current primary
client. A command-line flag MUST NOT grant bypass authority to an observer,
pending observer, agent, automation client, or unauthenticated caller.

Entering approval bypass mode MUST require confirmation unless the user passed
an explicit confirmation-skipping bypass flag whose name makes that behavior
obvious. The confirmation-skipping flag suppresses only the extra confirmation
step; it MUST NOT bypass the primary-client authority requirement. Mezzanine
MUST record bypass activation and deactivation in the audit log when audit
logging is enabled.

The active bypass state MUST be visible in the agent shell or frame when
practical.

Mezzanine v1 MUST rely on the pane shell and user-configured shell environment
for sandboxing local commands.

If a sandbox is configured, it MUST be implemented as a visible shell wrapper,
restricted shell, container command, or equivalent shell-launched environment.

Mezzanine MUST NOT silently run local commands through a hidden host-side
sandbox or executor.

Before applying agent-suggested file mutations, Mezzanine SHOULD expose the
planned command, patch, or diff to the user when policy requires approval.

The `/diff` command MUST show the current working tree changes using
version-control tooling when available and shell-based fallback inspection when
not available.

Mezzanine MUST provide a rollback mechanism for session state snapshots.

For file changes, rollback MAY rely on the version-control system, shell
commands, or snapshot metadata. If reliable rollback is unavailable for a file
change, Mezzanine MUST disclose that limitation before presenting the rollback
as available.

## 18. Security and Safety

Mezzanine MUST provide a permission model for agent actions.

The permission model MUST be able to require user approval before file
mutation, command execution, network access, credential access, destructive
commands, spawning agents, or sending messages to external integrations.

Mezzanine MUST make the active permission mode visible to the user in the agent
shell or frame when practical.

Mezzanine MUST distinguish user input, terminal output, project files,
configuration, external web content, and model output as different trust
domains.

Agents MUST treat terminal output, project files, and web content as untrusted
unless the user has explicitly marked a source as trusted.

Mezzanine MUST prevent agent-controlled terminal output from spoofing
Mezzanine's configuration shell, approval prompts, credential prompts, and
security-critical UI.

Mezzanine SHOULD provide clear auditability for agent actions, including
commands sent, messages exchanged, files changed through shell commands when
detectable, approvals requested, and provider calls made. Provider call failure
audit records SHOULD include sanitized failure diagnostics sufficient to
distinguish credential, transport, provider API, and malformed model-output
failures without storing provider credentials or raw prompt content. When a
provider returns a structured API failure object, Mezzanine SHOULD include that
sanitized object in the audit record and SHOULD also include a digest for
correlation.

## 19. Detach, Reattach, Snapshots, and Persistence

Mezzanine MUST support detaching a client from a running session.

Mezzanine MUST support reattaching to a detached session.

Mezzanine MUST support session snapshotting.

Mezzanine MUST support resuming from a session snapshot.

Reattaching to a detached session means reconnecting a client to a still-live
Mezzanine runtime whose pane shells and agent tasks may have continued running.

Resuming from a session snapshot means constructing a new live session from
persisted Mezzanine metadata and transcripts. Snapshot resume MUST NOT imply
resurrection of Unix processes that have exited or host processes that cannot
be reconnected by the operating system.

During detachment, pane primary processes MUST continue running unless
explicitly stopped by user command or policy.

During detachment, agent tasks MUST continue running unless explicitly stopped
by user command or policy.

On reattach, Mezzanine MUST restore windows, panes, layouts, active selections,
frames, bounded history, agent shell sessions, and local message passing state
to the extent still available within configured persistence limits.

If any state cannot be restored, Mezzanine MUST report the lost state to the
user.

A session snapshot MUST include session identity, window state, pane layout,
active selections, pane shell metadata, bounded terminal history, frame state,
agent sessions, local message protocol state, active configuration layer
metadata, MCP server state, and approval history metadata needed for audit.

A session snapshot MUST NOT persist active pending approval requests or active
approval grants as live authority. When a snapshot is resumed into a new live
session, pending approvals and approval grants from the previous runtime MUST
be reset.

Session snapshots MUST NOT contain raw provider credentials, provider tokens,
private keys, or other authentication secrets.

Session snapshots may contain sensitive terminal history, command output, file
content excerpts, and agent transcripts because those are part of the state
being preserved. This is an accepted risk of snapshotting terminal sessions.
Mezzanine MUST disclose this risk in snapshot documentation and SHOULD store
snapshots in a user-private path with permissions no broader than `0700` for
directories and `0600` for files when the host platform supports Unix modes.

Session snapshots SHOULD be stored in a structured format under
`~/.config/mezzanine` unless the user configures another user-private path.

Mezzanine MUST support listing snapshots.

Mezzanine MUST support resuming the most recent snapshot for a session.

Mezzanine MUST support selecting a snapshot to resume.

When resuming a snapshot, Mezzanine MUST preserve the prior transcript and
append new activity rather than overwriting history.

If pane shell processes cannot be reconnected because the original live session
is gone, the host rebooted, or the processes have exited, Mezzanine MUST
restore their terminal history and mark those panes as exited. Mezzanine MAY
offer to restart a shell in those panes, but restarted panes MUST receive fresh
primary PIDs and MUST be visibly marked as restarted.

If an agent task was running at snapshot time and the original live task can be
reconnected, resume MAY continue the task. If the original live task cannot be
reconnected, resume MUST mark it as interrupted and MUST require explicit user
confirmation before retrying any non-idempotent action.

Mezzanine MUST persist agent conversation transcripts as JSON Lines or another
documented structured append-only format.

Durable agent transcripts MUST record only durable conversation facts: current
user instructions, assistant-visible responses, model-authored thinking or
rationale lines, and action results or diagnostics. They MUST NOT persist full
model request scaffolding such as system prompts, developer policy blocks,
passive terminal snapshots, prompt-injected action feedback, transcript
reference blocks, or recent transcript excerpts. Permitted scaffolding blocks
MAY be assembled into the next model request, but they MUST remain prompt-local
so transcript context cannot recursively store or multiply itself across
provider continuations.

Agent conversation persistence MUST live under the user's configuration
directory in a parent agent-session directory. Each saved agent session MUST have
its own private child directory containing the durable transcript. The parent
agent-session directory MUST contain one bounded prompt-history file shared by
all agent sessions for readline navigation.

Each saved agent session MAY also contain a separate durable presentation log
for user-visible agent pane output. Presentation log entries MUST be append-only
and structured. They SHOULD record the conversation id, presentation sequence,
creation time, pane id, optional turn id, original terminal width, rendered
display lines, style category for each display line, and copy-mode replacement
text needed to replay what the user originally saw. Implementations SHOULD
persist the exact rendered terminal byte stream for each presentation entry
when doing so is available from the renderer, and MAY fall back to style
category replay for older or partial records. Presentation logs MUST NOT be
loaded into model context and MUST remain separate from durable conversation
transcripts so visual replay state cannot affect future model decisions.
Implementations MAY keep a small active cleartext presentation-log tail for
append latency, but once that tail exceeds a bounded threshold they SHOULD
compress it into concatenated zstd frames in the same saved-session directory.
Readers of presentation logs MUST treat the concatenated zstd history and the
active cleartext tail as one ordered presentation stream.

The parent agent-session directory MUST also contain a structured active-session
metadata checkpoint. This checkpoint MUST be metadata-only: it MAY contain the
Mezzanine session id, pane id, active conversation id, visibility state, active
turn id, known transcript-entry count, log level, model-profile selection, plan
mode, and response style. It MUST NOT duplicate model request context, terminal
screens, passive pane output, provider credentials, action feedback, or
transcript excerpts. If a checkpoint records an active turn that cannot be
reconnected after restart, Mezzanine MUST restore the conversation binding but
MUST mark the turn as interrupted and require a fresh user action to retry.

The parent agent-session directory MUST contain one bounded command-prompt
history file shared by the primary Mezzanine command prompt for readline
navigation. This file MUST remain separate from the agent prompt-history file.

The user MUST be able to list, inspect, fork, resume, and delete saved agent
conversations.

The `/resume` command MUST provide an interactive picker for saved
conversations or snapshots. Agent prompt completion for `/resume` MUST include
saved conversation UUIDs from the active transcript store.

When `/resume <session-uuid>` is invoked, Mezzanine MUST load the saved
conversation transcript into subsequent model context, MUST replay saved
presentation log entries into the current pane buffer when they are available,
MUST fall back to a bounded human-readable transcript/log summary when no
presentation log exists, and MUST reload the shared prompt history into the
current pane's agent prompt.

The `/fork` command MUST clone the current conversation into a new thread with
a fresh identity while preserving the original transcript and any presentation
log associated with that transcript. It MUST bind the forked conversation to a
new agent-mode pane in the same window instead of rebinding or mutating the
source pane's active conversation. The new pane's agent prompt MUST be seeded
with the last submitted prompt before the `/fork` command when such a prompt is
available, so users can edit and rerun the fork point. Prompt history MUST
remain shared rather than being copied into the forked conversation directory.

## 20. Hooks

Mezzanine MUST support configurable lifecycle hooks.

Hooks MUST be configured as matcher groups under a `hooks` configuration table.

Mezzanine MUST support hook events equivalent to:

- `SessionStart`
- `SessionStop`
- `ClientAttach`
- `ClientDetach`
- `WindowCreate`
- `WindowClose`
- `PaneCreate`
- `PaneClose`
- `UserPromptSubmit`
- `AgentTurnStart`
- `AgentTurnStop`
- `PreShellCommand`
- `PostShellCommand`
- `PermissionRequest`
- `PermissionDecision`
- `PreMcpToolUse`
- `PostMcpToolUse`
- `SnapshotCreate`
- `SnapshotResume`

Hook handlers MUST support program invocations launched outside the pane shell.

Hook handlers MUST support shell invocations sent through the focused pane
shell.

When a hook is configured as a shell invocation, Mezzanine MUST resolve the
target shell as follows:

1. Use the pane identified by the hook event when the event has an owning pane.
2. Otherwise use the active pane of the primary client when a primary client is
   attached.
3. Otherwise, if the hook is an agent hook, wait for the owning pane shell to
   become available or fail according to the hook timeout and `on_failure`
   policy.
4. Otherwise launch the resolved shell path outside the pane shell for the hook
   invocation.

If no shell target can be resolved and the hook cannot fall back to an external
shell invocation under this rule, Mezzanine MUST treat the hook as failed under
its `on_failure` policy.

Non-agent hooks MAY invoke arbitrary programs outside the pane shell according
to user configuration and policy.

Agent hooks MUST execute through the regular agent shell action loop. If an
agent hook is configured as a shell invocation, it MUST be queued for the
owning pane shell, MUST use normal command boundaries and pane protection, and
MUST block on shell availability rather than bypassing the pane shell.

Agent hooks MUST NOT be exposed to the model as hidden local tools.

Program hooks MUST receive structured event data through standard input or a
documented environment variable.

Shell hooks MUST be visible in the focused pane when they run and MUST use the
same command boundary and permission model as agent shell commands.
When shell hooks receive event data through an environment variable, the
variable name MUST be `MEZ_HOOK_PAYLOAD` and the value MUST contain the
structured event payload for the hook invocation.

Hooks that can mutate files, execute commands, access network resources, or
read credentials MUST be subject to permission policy.

Hook failures MUST be recorded in the audit log when audit logging is enabled.

Each hook handler configuration MUST include or inherit an `on_failure` policy.
The valid `on_failure` policies are:

- `block`: Fail the triggering event before it executes, or stop the remaining
  event pipeline when the event has already executed.
- `warn`: Continue the triggering event and surface a warning to the user,
  message log, and audit log when enabled.
- `ignore`: Continue the triggering event and record the failure only in
  diagnostics and audit logs when enabled.

If `on_failure` is omitted, Mezzanine MUST apply these defaults:

- `PreShellCommand`, `PermissionRequest`, `PreMcpToolUse`, `SnapshotResume`, and
  hooks that provide or modify a permission decision default to `block`.
- `UserPromptSubmit` and `AgentTurnStart` default to `block` when the hook is
  configured to inject instructions, mutate policy, or alter the pending action;
  otherwise they default to `warn`.
- `SessionStart` defaults to `block` only when the hook is marked `required`;
  otherwise it defaults to `warn`.
- `SessionStop`, `ClientDetach`, `PostShellCommand`, `PermissionDecision`,
  `PostMcpToolUse`, `SnapshotCreate`, and lifecycle notification hooks default
  to `warn`.
- Hooks whose triggering event has already completed MUST NOT retroactively
  claim that the completed event was blocked; they MAY trigger compensating
  actions only when those actions are explicitly configured and permissioned.

If a `block` hook fails before a shell command, MCP call, subagent spawn,
configuration mutation, snapshot resume, or other permission-gated action,
Mezzanine MUST NOT execute that action. The action result MUST use a structured
hook failure error that identifies the hook event, handler identity, failure
kind, and whether retry is possible.

If a hook times out, Mezzanine MUST treat the timeout as a hook failure under
the same `on_failure` policy. Hook handlers MUST have configurable timeouts, and
the default timeout MUST be finite.

## 21. Agent Memory

Mezzanine MUST distinguish session memory from persistent memory.

Session memory is scoped to one Mezzanine session and MUST be removed when the
session is deleted.

Persistent memory MAY survive across sessions only when enabled by the user.

Persistent memory MUST be stored under `~/.config/mezzanine` or in an explicitly
configured user-private location.

Persistent memory MUST be structured. Each memory record MUST include identity,
scope, creation time, update time, source, confidence or priority, and content.

Memory scopes MUST include at least global, project, session, window, pane, and
agent.

The user MUST be able to inspect, edit, export, and delete persistent memory.

Agents MUST NOT store secrets in persistent memory.

Agents MUST NOT store sensitive terminal output, credentials, private keys,
tokens, or personal data in persistent memory unless the user explicitly
instructs the agent to do so for that specific content.

Memory content MUST have lower priority than system requirements, active user
instructions, active policy, and project instruction files.

When memory conflicts with current instructions or observed project state, the
agent MUST prefer current instructions and observed state.

## 22. Scheduling and Concurrency

Mezzanine MUST provide a scheduler for panes, agent turns, local messages, and
background tasks.

Pane primary processes MUST continue to receive terminal input and produce
output independently of agent scheduling.

Only one agent turn per agent MUST execute at a time by default.

Multiple agents MAY execute concurrently only when they are associated with
separate pane shells. Mezzanine v1 MUST NOT permit multiple independently
running agents to send shell input to the same pane shell. If an implementation
supports multiple agent conversations associated with one pane, only one such
conversation MAY have an active turn that can issue shell input at a time; other
turns MUST wait, be queued, or run only non-shell planning work according to
policy.

Subagents MUST inherit the parent approval policy unless a stricter policy is
configured for the subagent.

The default maximum number of concurrently running agents in a session MUST be
4.

The maximum number of concurrently running agents MUST be configurable.

The scheduler MUST provide fair progress among runnable agents and MUST NOT
starve an agent indefinitely while resources remain available.

Agent turns blocked on approval, redirect, trust, or user input MUST retain
their pane and agent exclusivity, but they MUST NOT consume global running-agent
capacity while they wait for the user. Approving or answering the blocked action
MUST restore running state only for that blocked turn.

Long-running shell actions MUST expose status to the user and MUST be
interruptible when the underlying terminal process can be interrupted.

Detached sessions MUST continue scheduling active agents and panes unless the
user has configured pause-on-detach behavior.

## 23. Provider Model Selection

Mezzanine MUST select models through named model profiles.

Each model profile MUST include provider identity, model identity, reasoning or
effort preference when supported, latency preference when supported, multimodal
capability requirements, and any provider-specific non-secret options.

Interactive agent shells MUST also support direct provider model selection.
`/model <model-name>` MUST resolve `<model-name>` against the active provider's
available model catalog when that catalog can be queried, and MAY synthesize a
local runtime model profile for the selected model. `/model <model-name>
<reasoning-level>` MUST set the selected model and reasoning or effort
preference together. The runtime-generated profile identity MUST be assigned by
Mezzanine and MUST NOT require the user to predefine a named model profile.

`/model list` MUST list model names from the active configured provider in one
markdown table. The display SHOULD be `text/markdown; charset=utf-8`, SHOULD
include provider, model name, reasoning levels, context limit, source, and
active-profile columns, and MUST mark the active model and active reasoning
level with a visible star indicator in their cells. It MUST NOT append a usage
example below the catalog. When provider credentials and metadata endpoints are available,
Mezzanine SHOULD use the live provider catalog and SHOULD retain the successful
catalog in a session-local cache for later model-selection UI. When a live
catalog cannot be queried, Mezzanine MAY fall back to explicitly configured
provider models, but the display MUST identify that the source is configuration
rather than provider metadata and SHOULD expose the provider error in non-secret
form.

Provider-backed browser authentication does not imply that the provider exposes
the same model-catalog endpoint as API-key authentication. Mezzanine MUST NOT
derive or call undocumented model-catalog URLs from provider response endpoints;
when a browser-authenticated provider lacks an explicit catalog endpoint,
`/model list` MUST use configured provider models instead.

When provider metadata exposes supported reasoning levels, `/model` MUST
validate the requested reasoning level against that metadata. When provider
metadata does not expose reasoning levels, Mezzanine MAY use non-secret
provider defaults or configured model-profile reasoning values and MUST NOT
claim that a reasoning level is provider-supported unless it came from provider
metadata or an explicit provider-default implementation.

The pane-frame model and reasoning selectors MUST use the same provider model
and reasoning metadata as `/model` selection. When possible, Mezzanine SHOULD
refresh the active provider catalog before opening a selector, and it MUST use a
cached live catalog when one is available. Selector choices MUST apply
pane-scoped model profile overrides, MUST preserve the current reasoning
preference when the newly selected model supports it, and MUST NOT block normal
pane interaction after the drop-down is closed.

When the active provider supports a native thinking toggle, pane-frame agent
status controls MUST expose an `agent.thinking` pill immediately after
`agent.reasoning`. The pill MUST display `thinking` and use distinct color
treatment for enabled and disabled states. Activating the pill MUST apply the
same pane-scoped mutation as `/thinking toggle` without opening a dropdown.

When `agents.routing` or the pane-local `/routing` preference is
enabled, Mezzanine MUST run an auto-sizing decision before the normal provider
request for each new root-agent turn and each spawned subagent turn. The
decision request MUST use `agents.auto_sizing.router_model_profile` and MUST
run before the turn is marked as using a target model profile. The request MUST
be separate from MAAP action execution and MUST NOT expose shell, file,
network, MCP, or subagent actions. It MUST be treated as a bounded internal
classification step whose only valid output is an auto-sizing decision.

The auto-sizing router prompt MUST include:

- the user's submitted task or subagent task prompt, quoted as untrusted data;
- a bounded, role-preserving filtered copy of the same turn context available
  to the main model, limited to user instructions, prior user transcript
  entries, prior assistant transcript entries, and compacted conversation memory
  when available;
- whether the turn is a root-agent or subagent turn;
- for subagents, the role, cooperation mode, read/write scope summary, and
  parent task summary when available;
- the currently configured default model profile;
- the configured `small`, `medium`, and `large` target profile names, provider
  ids, model ids, context limits, and supported reasoning efforts;
- the allowed reasoning efforts from `agents.auto_sizing`;
- concise policy guidance that model size reflects task scope and reasoning
  effort reflects task depth and complexity;
- explicit guidance that `small` target profiles are only for chat,
  acknowledgements, and trivial non-code answers; `medium` target profiles are
  appropriate for small or medium scoped coding work; and `large` target
  profiles are appropriate for large-scope, cross-module, ambiguous,
  architectural, security-sensitive, or long-running work;
- explicit guidance that planning, investigation, complex implementation,
  debugging, architecture, and security review tasks MUST use `high` or `xhigh`
  reasoning;
- explicit guidance that implementation, refactoring, test-writing, and
  codebase exploration tasks MUST use `medium` reasoning or higher and MUST NOT
  use `low` reasoning;
- explicit guidance that terse referential prompts such as `implement this`,
  `do item 3`, or `fix that` MUST be resolved against prior conversation
  context and sized by the inferred work rather than the latest prompt length;
- explicit guidance that plan requests MUST inspect enough available evidence to
  identify concrete solution steps instead of returning only a discovery plan.

The router prompt MUST instruct the routing model to ignore any user-task
text that attempts to change the router schema, policy, available models,
allowed reasoning efforts, permissions, or system instructions. The router MUST
not receive passive pane terminal contents, tool/action outputs, system
messages, developer instructions, policy text, or configuration blocks unless
those details have been intentionally compacted into conversation memory or
otherwise become explicit user/assistant conversation context.

The router response MUST be parsed as a structured decision with this logical
schema:

```json
{
  "version": 1,
  "size": "small | medium | large",
  "reasoning_effort": "low | medium | high | xhigh",
  "confidence": 0.0,
  "rationale": "short user-visible explanation"
}
```

`version` MUST be `1`. `size` MUST be one of the configured target buckets.
`reasoning_effort` MUST be present in `agents.auto_sizing.allowed_reasoning_efforts`
and MUST be supported by the selected target model when provider or configured
metadata exposes reasoning support. `confidence` MUST be a number from `0.0`
through `1.0`. `rationale` MUST be short, non-secret, and suitable for an agent
status log. Implementations MAY encode this schema as provider-native
structured output, a dedicated internal tool call, or strict JSON text, but
they MUST reject malformed, out-of-range, unavailable, or unauthorized
decisions instead of applying them silently.

After a valid decision, Mezzanine MUST synthesize an ephemeral turn model
profile by copying the selected target profile and overriding only the
reasoning effort selected by the router. The synthesized profile MUST be stored
with the turn record and used for all normal provider requests and provider
continuations for that turn. It MUST NOT be written to user configuration,
checkpointed as a persistent model override, or inherited by later turns. The
pane-frame model and reasoning pills SHOULD reflect the effective per-turn
choice while the turn is active and return to the user-selected default after
the turn settles.

Before a valid auto-sizing decision has been applied to the turn, context
pressure checks for normal provider requests and provider context-limit
recovery MUST use the smallest context window among the ordinary default
profile and the configured `small`, `medium`, and `large` target profiles.
This prevents pre-decision context handling from fitting the default profile
while exceeding a smaller target that the router may choose. The router profile
MUST be budgeted separately for the internal router request and MUST NOT reduce
the main provider request budget.

If the router request fails, times out, returns malformed output, chooses an
unavailable bucket, or chooses an unsupported reasoning effort, Mezzanine MUST
follow `agents.auto_sizing.fallback_policy`. The default `use-default-profile`
fallback MUST log a concise diagnostic and continue the turn with the normal
active model profile. The fallback MUST NOT fail the user turn unless the user
configured a stricter fallback policy. Router failures and decisions MUST be
recorded in the audit log and transcript metadata, but router prompts and raw
router outputs MUST NOT be replayed as ordinary assistant conversation in later
model context.

Auto-sizing MUST preserve existing override precedence. Explicit model
selection from `/model`, session/window/pane/agent/subagent model overrides,
and subagent profile model settings define the default profile that is restored
after the turn. Auto-sizing only chooses an effective model profile for the
current turn. If routing is disabled for a subagent, that subagent MUST
use the model profile selected by ordinary override and inheritance rules. If
routing is enabled for a subagent, the subagent MUST run its own router
decision using its subagent task prompt and scope metadata rather than reusing
the parent turn's router decision.

The user MUST be able to configure the default model profile.

The user MUST be able to override the model profile for a session, window,
pane, agent, or subagent.

When no explicit model profile is configured, Mezzanine SHOULD select the
provider's recommended development-assistant model available to the
authenticated user.

If the preferred model is unavailable, Mezzanine MUST report the failure and MAY
offer configured fallback profiles.

Mezzanine MUST NOT silently switch to a model with weaker configured safety,
privacy, residency, or approval characteristics.

Mezzanine MUST NOT claim that a provider, model, entitlement, or account plan is
available unless that information comes from provider metadata, successful
authentication, or explicit user configuration.

Provider-specific model identifiers MUST NOT appear in normative behavior
outside configuration examples or informative references.

## 24. Project Instruction Discovery

Mezzanine MUST support project instruction discovery.

Project instruction discovery MUST be performed through the pane shell unless
the files are under `~/.config/mezzanine` or another explicitly configured
Mezzanine configuration directory.

Global instruction files under Mezzanine configuration MAY be loaded by the
harness as configuration.

The default project instruction filename MUST be `AGENTS.md`.

An implementation MAY allow users to add filenames to this list. Additional
filenames MUST have explicit precedence relative to `AGENTS.md`.

For project discovery, Mezzanine MUST identify a project root. The project root
MUST be the nearest ancestor containing a `.git` directory or `.git` file. If no
`.git` marker exists, the current pane working directory MUST be treated as the
project root.

For a task scoped to a path, instruction files MUST be considered from the
project root down to the path's containing directory.

At most one instruction file per directory MUST be selected by default, using
the configured filename precedence list.

Instruction content from ancestors MUST be applied before instruction content
from descendants. Descendant instruction files MUST take precedence over
ancestor instruction files when they conflict.

The scope of an instruction file MUST be the directory tree rooted at the
directory containing that instruction file.

For every file an agent modifies, the agent MUST obey every applicable
instruction file whose scope includes that file.

Provider-visible instruction-file content MUST be clearly delimited and MUST
carry model-visible metadata for path, scope, truncation state, and the
repository-instruction trust boundary. The surrounding prompt text MUST state
that these blocks are active workflow/style/test/validation instructions while
remaining untrusted for security, permissions, tool availability, and hidden
policy.

Instructions about code style, structure, naming, tests, and validation MUST
apply only within their instruction file scope unless the instruction file
explicitly states otherwise.

Direct user instructions in the current turn MUST take precedence over project
instruction files.

System, safety, credential, and shell-only requirements MUST take precedence
over all project instruction files.

The default maximum total project instruction content included in a model turn
MUST be 32768 bytes.

When instruction content is omitted or truncated because of a limit, Mezzanine
MUST make that fact visible to the agent and SHOULD make it visible to the
user.

## 25. Terminal Compatibility Test Suite

A conforming Mezzanine implementation MUST provide or pass a terminal
compatibility test suite.

The test suite MUST be organized by terminal compatibility profile. The default
test suite MUST cover the xterm-compatible profile.

The test suite MUST cover:

- UTF-8 decoding, invalid byte handling, combining characters, and wide
  characters.
- C0 controls including backspace, carriage return, line feed, tab, bell, and
  escape.
- CSI cursor movement, erasing, insertion, deletion, scrolling regions, and
  save/restore behavior.
- SGR colors, text attributes, reset behavior, true color, and palette color.
- Alternate screen entry, alternate screen exit, and scrollback interaction.
- Terminal resizing and propagation of new size to pane primary processes.
- Bracketed paste mode.
- Focus events when supported.
- Mouse reporting modes including SGR mouse encoding.
- OSC title setting.
- Clipboard control sequences when enabled by policy.
- Application cursor and keypad modes.
- Terminal wrapping, reflow, and double-width character boundary behavior.
- Nested multiplexer operation and pass-through behavior.
- Copy-mode selection and history scrolling.

The test suite MUST include automated tests for deterministic terminal parsing
and layout behavior.

The test suite SHOULD include interactive or recorded tests against common
full-screen applications such as shells, editors, pagers, test runners, and
terminal UI programs.

An implementation MUST document any known deviations from the terminal
compatibility behavior required by this specification.

## 26. Security Audit Log

Mezzanine MUST support a structured security audit log.

When audit logging is enabled, audit records MUST be written as JSON Lines
unless the user explicitly configures another structured format.

Each audit record MUST include:

- `version`
- `event_id`
- `timestamp`
- `session_id`
- `window_id` when applicable
- `pane_id` when applicable
- `agent_id` when applicable
- `actor`
- `event_type`
- `action`
- `policy_mode`
- `approval_state`
- `outcome`
- `redactions`

Audit events MUST be emitted for provider authentication changes, permission
changes, approval prompts, approval decisions, shell commands sent by agents,
configuration changes, subagent spawns, local protocol bridge changes, external
connector use, credential access attempts, and logout.

Audit records MUST redact secrets by default.

Audit records MUST NOT include raw credential values, provider tokens, private
keys, or unredacted approval secrets.

Audit logging SHOULD support tamper-evident hash chaining.

The user MUST be able to configure audit log path and retention.

Audit log failure MUST be reported to the user. If policy marks audit logging
as required, Mezzanine MUST deny auditable actions while logging is unavailable.

## 27. Extension and Versioning

Mezzanine configuration, message protocol, audit records, and persisted session
state MUST include explicit version identifiers.

Implementations MUST reject unsupported major versions unless a documented
migration path is available.

Implementations SHOULD preserve unknown extension fields when reading and
writing structured Mezzanine data.

Extensions MUST NOT weaken core security, shell-only, local-message, terminal
compatibility, or authentication requirements.

Extensions that expose remote access MUST be disabled by default and MUST
require explicit user configuration.

## 28. References

Normative:

- RFC 2119, "Key words for use in RFCs to Indicate Requirement Levels":
  <https://www.rfc-editor.org/rfc/rfc2119>
- POSIX Shell Command Language:
  <https://pubs.opengroup.org/onlinepubs/9799919799/utilities/V3_chap02.html>
- POSIX `exec` utility:
  <https://pubs.opengroup.org/onlinepubs/009695399/utilities/exec.html>
- Xterm Control Sequences:
  <https://www.xfree86.org/4.7.0/ctlseqs.html>
- OSC 133 shell integration convention:
  <https://contour-terminal.org/vt-extensions/osc-133-shell-integration/>
- JSON-RPC 2.0:
  <https://www.jsonrpc.org/specification>
- Language Server Protocol base protocol:
  <https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/>
- XDG Base Directory Specification:
  <https://specifications.freedesktop.org/basedir-spec/latest/>
- Model Context Protocol base protocol:
  <https://modelcontextprotocol.io/specification/2025-11-25/basic>
- Model Context Protocol tool result semantics:
  <https://modelcontextprotocol.io/specification/2025-06-18/server/tools>

Informative:

- Sudoers command argument and digest matching:
  <https://www.sudo.ws/docs/man/1.9.9/sudoers.man/>
- OpenAI Structured Outputs documentation:
  <https://platform.openai.com/docs/guides/structured-outputs>
- OpenAI Function Calling documentation:
  <https://platform.openai.com/docs/guides/function-calling>
