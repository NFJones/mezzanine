# `apply_patch` MAAP action

`apply_patch` is Mezzanine's model-facing semantic action for filesystem
content mutation. It exists so agents can create, update, move-with-edit,
replace, and delete files without treating those changes as arbitrary shell
text. In the MAAP action surface, `apply_patch` is the canonical path for file
content edits; local inspection, validation, directory creation, path-only
moves, and other non-content operations belong in `shell_command` instead.

This document summarizes the current model-facing contract from `SPEC.md` and
the implementation in `src/agent/maap.rs`, `src/agent/semantic/patch/`,
`src/runtime/agent/shell_dispatch.rs`, and `src/subagent/validation.rs`, with
representative behavior locked in by `src/agent/tests/part_02.rs` and related
runtime tests.

## What `apply_patch` is for

Use `apply_patch` when an agent needs to change file contents in the active
pane working directory. The action supports:

- creating a new file;
- applying localized edits to an existing file;
- moving a file while also changing its contents;
- deleting a file through an explicit patch directive; and
- intentionally replacing the full contents of a file.

The action is designed for small, explicit, reviewable edits. Mezzanine parses
the patch into a structured representation, validates it before mutation, and
returns recovery-oriented diagnostics when the target file has drifted or the
requested hunk is ambiguous.

## What `apply_patch` is not for

`apply_patch` is a MAAP semantic action, not a shell program. Agents must not:

- invoke `apply_patch` from a `shell_command` payload;
- wrap the mutation in a shell heredoc or here-string as if `apply_patch` were
  a pane command;
- use it for directory creation, path-only renames, recursive deletion, or
  general process execution; or
- delete and recreate a file merely to change its contents when a normal
  update hunk would be sufficient.

For those cases, use `shell_command` and ordinary shell tools instead.

## Model-facing action shape

In a MAAP action batch, `apply_patch` is represented as an action object with a
single required payload field:

```json
{
  "type": "apply_patch",
  "patch": "*** Begin Patch\n*** Update File: src/lib.rs\n@@ fn demo()\n-old();\n+new();\n*** End Patch"
}
```

The `patch` field is the semantic patch text itself. It is not shell source,
not a raw `git apply` invocation, and not a Markdown diff example unless the
user explicitly asked to display one instead of executing it.

## Canonical patch grammar

The canonical Mezzanine patch format starts with `*** Begin Patch` and ends
with `*** End Patch`. Between those markers, the patch contains one or more
file operations:

- `*** Add File: <relative-path>`
- `*** Update File: <relative-path>`
- `*** Delete File: <relative-path>`

An update may optionally include `*** Move to: <relative-path>` immediately
after `*** Update File:` when the patch both renames and edits the file.

Update operations contain one or more hunks. Each hunk normally begins with an
`@@` header and then uses line prefixes:

- ` ` for unchanged context;
- `-` for removed lines; and
- `+` for added lines.

For reliable matching, the best patch shape is usually a small hunk with:

- a distinctive `@@` anchor copied from the current file; and
- a small amount of exact current old/context text copied verbatim from a
  fresh file read or action result.

Mezzanine also supports the explicit whole-file replacement convention:

- `@@ replace whole file`

That special update form must be the only hunk in that file operation and may
contain only added lines plus an optional `*** End of File` marker.

## Canonical examples

### Update an existing file

```text
*** Begin Patch
*** Update File: src/lib.rs
@@ fn demo() {
-    old();
+    new();
 }
*** End Patch
```

### Add a file

```text
*** Begin Patch
*** Add File: docs/example.txt
+hello
*** End Patch
```

### Move and edit in one operation

```text
*** Begin Patch
*** Update File: src/old_name.rs
*** Move to: src/new_name.rs
@@
-pub fn old_name() {}
+pub fn new_name() {}
*** End Patch
```

### Replace a whole file intentionally

```text
*** Begin Patch
*** Update File: generated.txt
@@ replace whole file
+fresh
+content
*** End Patch
```

## Compatibility surface vs. canonical output

The model-facing contract should always be the canonical unwrapped Mezzanine
patch grammar above. That is the most portable format and the easiest one to
repair after a failure.

The implementation still accepts a bounded compatibility surface so recovery is
 possible when provider output is slightly malformed. Current parser and test
coverage show support for several common wrappers and leniencies, including:

- omitted `@@` on the first update hunk only;
- blank hunk-body lines used as empty context lines;
- Markdown-fenced patch payloads;
- uniformly indented patch payloads;
- heredoc-wrapped patch text such as `<<'PATCH'`;
- accidental shell-style wrappers such as `apply_patch <<'PATCH'` around the
  patch text inside the action payload;
- safe copied path prefixes such as `./`, `a/`, and `b/`; and
- unified-range metadata embedded in a Mezzanine hunk header, where the range
  numbers are treated as hints rather than authority.

The lower-level patch planning code can also normalize some unified diff input,
but that should be treated as an implementation compatibility path rather than
the primary MAAP contract. For documentation, prompts, and examples, prefer an
explicit `*** Begin Patch` block.

## Validation at the MAAP boundary

Before execution, `src/agent/maap.rs` validates that the payload is shaped like
an `apply_patch` action rather than arbitrary shell text. In particular, the
current boundary requires:

- a non-empty `patch` string;
- Mezzanine patch text beginning with `*** Begin Patch`;
- a closing `*** End Patch` marker; and
- at least one file operation that yields touched paths.

That validation stage exists so obviously malformed payloads fail fast with a
targeted diagnostic instead of reaching shell dispatch or native write logic.

## Execution lifecycle

`apply_patch` remains a semantic MAAP action from the model's point of view
even when Mezzanine lowers it into transport-specific local work.

The major stages are:

1. compatibility normalization of the incoming patch text;
2. MAAP validation and parsing into typed patch operations and hunks;
3. touched-path derivation and subagent scope validation;
4. snapshot collection for each affected path;
5. hunk matching and change planning against those snapshots; and
6. verified writes or a structured failure result.

The important architectural property is that parsing, snapshotting, matching,
and writing are separate steps. That separation lets Mezzanine explain whether
a failure came from syntax, path safety, stale context, ambiguity, UTF-8
decoding, or write-time drift instead of collapsing everything into one generic
shell error.

## Shell-backed transaction flow

When `apply_patch` runs through the pane shell, the flow is deliberately a
transaction instead of a blind one-shot rewrite:

1. Mezzanine parses the patch and derives the touched-path set, including move
   destinations.
2. The generated read phase emits a marker-framed snapshot stream for each
   touched path, including base64-encoded metadata, resolved paths, path state,
   and file bytes for regular files.
3. Rust parses those snapshots and applies patch hunks against the captured
   preimage rather than against guessed shell output.
4. If planning succeeds, Mezzanine generates a write phase that re-resolves the
   target, rechecks the expected resolved location, verifies the original bytes
   for existing files, and only then writes the final bytes.
5. After successful writes, Mezzanine renders the resulting diff preview
   itself. If a later file operation fails, earlier per-file writes remain in
   place and the diagnostic identifies both the already-applied path set and
   the failed operation.

This read-then-write shape is why many `apply_patch` failures are recoverable:
the runtime is usually rejecting a stale, ambiguous, or unsafe mutation plan,
not failing halfway through an unstructured shell script.

## Native execution path

When local native execution is allowed, `apply_patch` can bypass pane-shell
dispatch and apply the change directly through Rust. The native path still uses
the same major invariants as the shell-backed path:

- the same parser and hunk matcher;
- the same path-safety checks;
- the same snapshot and preimage verification logic; and
- the same user-facing action/result type.

From the model's perspective, the action is still `apply_patch`. Only the
runtime transport changes.

## Path, scope, and safety rules

`apply_patch` path headers are relative to the pane current working directory.
They must not use absolute paths or `..` traversal.

Before reading or writing the target, Mezzanine resolves symlinks and checks
filesystem safety. A target is rejected when the resolved path:

- escapes the pane working directory;
- resolves to a non-regular filesystem node; or
- changes unexpectedly before final write verification.

When a symlink resolves to a regular file inside the working directory,
Mezzanine may patch the resolved file, but it re-resolves the target and
rechecks the preimage immediately before writing final bytes so symlink races
fail safely.

Subagent validation also checks touched paths before execution so write scopes
and path boundaries are enforced consistently.

## Matching behavior

`apply_patch` does not treat an `@@` header as sufficient placement authority
on its own. Anchors are ordered substring constraints, while hunk body context
is still required to prove the edit location.

The matcher in `src/agent/semantic/patch/matcher.rs` applies hunks against a
snapshot of the current file and can use several bounded recovery strategies,
including anchor-constrained search and conservative old-line range hints from
unified diff metadata. It still fails closed when the result is ambiguous.

After exact old-context matching, the matcher may fall back in deterministic
order to:

- trailing-whitespace-insensitive matching;
- surrounding-whitespace-insensitive matching; and
- a limited punctuation-and-spacing normalization pass.

Those compatibility modes do not let the patch rewrite unchanged context from
the patch text. When a non-exact match is accepted, current unchanged context
is preserved from the real target file.

The matcher may also tolerate omitted blank-only separator lines in bounded
cases, but it still rejects nonblank gaps, tied candidates, near-ties, and
other unresolved ambiguity.

## Mismatch diagnostics and recovery

When a hunk does not match cleanly, the runtime returns a structured,
model-correctable diagnostic instead of a bare `patch failed` message. Current
diagnostics can include:

- the failed path;
- a failure code such as `HUNK_CONTEXT_MISMATCH` or
  `HUNK_CONTEXT_AMBIGUOUS`;
- the number of failed old-context lines;
- matching attempts and the matching scope;
- header anchors or old-line hints that participated in the search;
- candidate line ranges or suggested reread ranges;
- a snapshot of nearby current file context; and
- replacement hints when the intended change may already be present.

Representative recovery guidance emitted by the matcher includes:

- reread the implicated region;
- reread candidate regions when multiple plausible matches exist;
- refresh or correct the missing `@@` header anchor; or
- skip or reconcile an already-applied change instead of forcing a retry.

The intended repair loop is:

1. inspect the implicated current file region;
2. decide whether the change already landed or was superseded nearby; and then
3. retry with a smaller fresh hunk using copied current context.

Recoverable `apply_patch` failures are not terminal task failures.

## Runtime invariants worth documenting

Several implementation details are important for operators and contributors:

- `apply_patch` is the baseline semantic local action for file-content
  mutation.
- Shell-backed `apply_patch` is a stateful read-then-write transaction, not a
  one-shot shell rewrite.
- The shell-backed read phase uses marker-framed, base64-encoded snapshot
  transport so Mezzanine can reconstruct exact current file state before it
  plans any write.
- Mezzanine synthesizes the concrete local execution plan and the user-facing
  diff preview; agents do not need to generate shell wrappers or raw diffs.
- The action uses explicit timeout handling instead of inheriting an unlimited
  shell mutation window.
- Multi-file patches preserve earlier successful file mutations even if a later
  file operation fails, and the resulting diagnostic reports both the applied
  path set and the failing operation.
- Regular-file patching is text-oriented today, so snapshot decoding must be
  valid UTF-8 before hunk matching proceeds.

## Choosing `apply_patch` vs. `shell_command`

Choose `apply_patch` for:

- ordinary file-content edits;
- multi-hunk textual updates;
- explicit add, update, delete, or move-with-edit operations; and
- changes where stale-context diagnostics and safe retry behavior matter.

Choose `shell_command` for:

- repository inspection and search;
- build, test, lint, and formatting commands;
- directory creation or deletion;
- path-only moves or filesystem orchestration; and
- raw shell tools that are not semantic file-content mutation.

In short: `apply_patch` is for content mutation, not general filesystem or
process orchestration.

## Practical guidance for authors

When writing prompts, docs, or examples about `apply_patch`, keep these rules
front and center:

- emit the patch text directly in the action payload;
- prefer small anchored hunks over large brittle rewrites;
- copy old/context lines verbatim from current file evidence;
- keep path headers relative to the working directory;
- do not treat `@@` headers as a substitute for real old-context lines; and
- on failure, inspect the reported current context before retrying.

The most important mental model is that `apply_patch` is a semantic,
prevalidated, snapshot-based edit transaction. That is the main reason it is
safer and more repairable than raw shell-authored patch application.

## Source map

The main owners for current behavior are:

- `src/agent/maap.rs`: MAAP-boundary validation for the model-facing action
  payload.
- `src/agent/semantic/patch/parser.rs`: wrapper normalization, patch parsing,
  directives, anchors, and hunk structures.
- `src/agent/semantic/patch/snapshot.rs`: typed decoding of read-phase
  snapshots.
- `src/agent/semantic/patch/matcher.rs`: hunk matching, ambiguity detection,
  and recovery diagnostics.
- `src/agent/semantic/patch/transaction.rs`: shell-backed read/write command
  generation.
- `src/subagent/validation.rs`: touched-path scope enforcement.
- `src/agent/tests/part_02.rs`: compatibility, whole-file replacement,
  ambiguity, and mismatch-diagnostic regression coverage.

For normative behavior, prefer `SPEC.md`. For the current concrete behavior of
the shipping implementation, use the files above.
