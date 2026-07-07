# `apply_patch` MAAP action

`apply_patch` is Mezzanine's model-facing semantic action for filesystem
content mutation. It exists so agents can create, update, move-with-edit,
replace, and delete files without treating those changes as arbitrary shell
text. In the MAAP action surface, `apply_patch` is the canonical path for file
content edits; local inspection, validation, directory creation, path-only
moves, and other non-content operations belong in `shell_command` instead.

This document summarizes the normative behavior in [`SPEC.md`](../SPEC.md) and
the current implementation in `src/agent/maap.rs`,
`src/agent/semantic/patch/`, and `src/subagent/validation.rs`.

## What `apply_patch` is for

Use `apply_patch` when an agent needs to change file contents in the active pane
working directory. The action supports:

- creating a new file;
- applying localized edits to an existing file;
- moving a file while also changing its contents;
- deleting a file through an explicit patch directive; and
- replacing the full contents of a file intentionally.

The action is designed for small, explicit, reviewable edits. Mezzanine uses a
structured patch grammar, validates the patch before execution, and returns
diagnostics that help an agent recover from stale or ambiguous hunks.

## What `apply_patch` is not for

`apply_patch` is a MAAP semantic action, not a shell program. Agents must not:

- invoke `apply_patch` from a `shell_command` payload;
- wrap the patch in shell heredocs or here-strings;
- use it for directory creation, path-only renames, recursive deletion, or
  general process execution; or
- delete and recreate a file merely to change its contents when a normal update
  hunk would be sufficient.

For those cases, use `shell_command` and ordinary shell tools instead.

## Canonical patch format

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
- a small amount of exact current old/context text copied verbatim from a fresh
  file read or action result.

Mezzanine also supports the explicit whole-file replacement convention:

- `@@ replace whole file`

That special update form must be the only hunk in that file operation and may
contain only added lines plus an optional `*** End of File` marker.

## Compatibility behavior

The runtime accepts a wider compatibility surface than the canonical form so it
can repair common model output shapes. For example, Mezzanine may accept:

- many raw unified diffs and convert them to Mezzanine patch form before
  validation;
- omitted `@@` on the first update hunk only;
- safe `./`, `a/`, or `b/` path prefixes;
- Markdown-fenced or uniformly indented patch payloads; and
- accidental shell-style `apply_patch <<...` wrappers.

Agents should still emit the canonical unwrapped Mezzanine patch format because
it is the least ambiguous and easiest to recover when a patch fails.

Delete-style unified diffs are not part of that compatibility path. The
runtime's unified-diff conversion is intentionally narrower than the canonical
Mezzanine patch grammar, so the most portable representation is still an
explicit `*** Begin Patch` block.

## Action lifecycle

`apply_patch` stays a semantic MAAP action from the model's point of view even
when Mezzanine lowers it into transport-specific local work.

The main stages are:

1. compatibility normalization;
2. MAAP validation and patch parsing;
3. target snapshot collection for touched paths;
4. hunk matching and change planning against those snapshots; and
5. final write execution or a structured failure result.

More concretely:

- `src/agent/semantic/patch/mod.rs` first attempts compatibility conversion
  such as unified-diff normalization, then validates the resulting payload;
- `src/agent/maap.rs` and the patch parser require a non-empty Mezzanine patch
  block with `*** Begin Patch`, `*** End Patch`, and at least one file
  operation;
- patch headers are parsed early so touched paths and scope checks can happen
  before mutation; and
- `strip` is intentionally unsupported for Mezzanine patch blocks even though
  some non-Mezzanine patch tools expose it.

Shell-backed execution is deliberately two phase. Mezzanine first snapshots the
current target files for the touched paths, then builds a verified write plan
from those snapshots. Native execution reuses the same parser, matcher, path
safety checks, and preimage verification logic, but applies the resulting
changes directly through Rust rather than by sending pane shell input.

## Major implementation components

The current implementation splits `apply_patch` into a small set of focused
owners:

- `src/agent/maap.rs` validates the action payload at the MAAP boundary,
  applies compatibility normalization such as unified-diff conversion when
  applicable, and rejects non-Mezzanine patch payloads before execution
  planning.
- `src/agent/semantic/patch/parser.rs` normalizes wrapped or fenced patch text
  and parses file operations, hunk anchors, unified-diff old-line hints, and
  whole-file replacement hunks into typed patch structures.
- `src/agent/semantic/patch/snapshot.rs` parses the shell read-phase snapshot
  transport into typed path states such as regular file, missing path,
  non-regular target, outside-working-directory target, or resolution error.
  The current matcher pipeline is text-oriented, so regular files must decode as
  UTF-8 before hunk matching can proceed.
- `src/agent/semantic/patch/matcher.rs` applies update hunks to current text
  snapshots, including exact matching, bounded tolerant matching,
  ambiguity detection, and model-correctable mismatch diagnostics.
- `src/agent/semantic/patch/transaction.rs` generates the shell-backed read and
  write phases, including marker-framed base64 snapshot transport, verified
  write commands, and the runtime-generated diff preview shown after successful
  mutation.
- `src/subagent/validation.rs` checks touched paths ahead of execution so
  subagent write scopes and path boundaries are enforced consistently.

This split is important operationally: parsing, matching, snapshot handling,
and transport generation are separate on purpose so stale-context failures can
produce specific diagnostics without blurring syntax errors, safety failures,
and write-time races into one generic patch error.

## Shell-backed transaction flow

When `apply_patch` runs through the pane shell, the flow is deliberately a
planned transaction instead of a blind one-shot rewrite:

1. Mezzanine parses the patch and derives the full touched-path set from file
   operations, including move-with-edit destinations.
2. The generated read phase emits a marker-framed snapshot stream for each
   touched path, including base64-encoded path metadata, resolved paths, path
   status, and file bytes for regular files.
3. Rust parses those snapshots, normalizes regular files into current text
   state, and applies patch hunks against the read-phase preimage rather than
   against guessed shell output.
4. If planning succeeds, Mezzanine generates a write phase that re-resolves the
   target path, rechecks the expected resolved location, verifies the original
   bytes for existing files, and only then writes the verified final bytes.
5. After successful writes, Mezzanine renders the resulting diff/change preview
   itself. If a later file operation fails, earlier per-file writes remain in
   place and the diagnostic identifies both the already-applied path set and
   the failed operation.

That transaction shape explains why `apply_patch` failures are usually
recoverable with better context: the failure is typically coming from Mezzanine
rejecting a stale or unsafe plan, not from a shell script partially guessing
where to edit.

## Path, scope, and safety rules

`apply_patch` path headers are relative to the pane current working directory.
They must not use absolute paths or `..` traversal.

Before reading or writing the target, the patch helper resolves symlinks and
checks filesystem safety. A target is rejected when the resolved path:

- escapes the pane working directory;
- resolves to a non-regular filesystem node; or
- changes unexpectedly before final write verification.

When a symlink resolves to a regular file inside the pane working directory,
Mezzanine may patch the resolved file, but it re-resolves the target and checks
the preimage again immediately before writing final bytes so symlink races fail
safely.

Subagent validation also checks touched paths before execution so write scopes
and path boundaries can be enforced consistently.

## Matching, ambiguity, and recovery

`apply_patch` does not treat an `@@` header as sufficient placement authority on
its own. Anchors are ordered substring constraints, while hunk body context is
still required to prove the edit location.

The matcher in `src/agent/semantic/patch/matcher.rs` applies hunks against a
snapshot of the current file and can use several bounded recovery strategies,
including anchor-constrained search and conservative old-line range hints from
unified diff headers. It must still fail closed when the result is ambiguous.

After exact old-context matching, the matcher may fall back in deterministic
order to trailing-whitespace-insensitive matching,
surrounding-whitespace-insensitive matching, and a limited punctuation/space
normalization pass. Those compatibility modes do not give the patch permission
to rewrite unchanged context from the patch text: when a non-exact match is
accepted, current unchanged context is preserved from the real target file.
The matcher may also tolerate omitted blank-only separator lines in bounded
cases, but it must still reject nonblank gaps, tied candidates, near-ties, and
other unresolved ambiguity.

When a hunk does not match cleanly, the runtime returns a diagnostic that can
include:

- the failed path;
- whether context was missing or ambiguous;
- the relevant anchor state or old-line hint; and
- bounded replacement-span guidance when the intended change may already be
  present nearby.

The intended repair loop is:

1. inspect the implicated current file region;
2. determine whether the intended change already landed or was superseded by a
   nearby edit; and then
3. retry with a smaller fresh hunk using copied current context.

Recoverable `apply_patch` failures are not considered terminal task failures.

## Runtime behavior and invariants

Several runtime invariants are important when documenting or debugging patch
application:

- `apply_patch` is the baseline semantic local action for file-content
  mutation.
- Shell-backed `apply_patch` is a stateful read-then-write transaction, not a
  single blind shell rewrite.
- The shell-backed read phase uses marker-framed snapshot transport and base64
  payloads so Mezzanine can reconstruct exact current-file state before it
  plans any write.
- Mezzanine synthesizes the concrete local execution plan and the user-facing
  change preview; agents do not need to generate shell wrappers or diffs.
- Generated shell transactions avoid model-authored heredocs and move file
  content through bounded encoded shell lines rather than embedding raw patch or
  file bytes in ad hoc shell source.
- The action uses a short explicit timeout rather than inheriting the ordinary
  shell-command default so malformed or blocked patches fail quickly.
- Structured action results keep the semantic action identity as
  `apply_patch` even when the underlying transport is pane-shell or native.
- If a multi-file patch applies changes to one path and later fails on another,
  already-completed per-file mutations are preserved. The diagnostic identifies
  what succeeded and what still needs retry.
- Shell-backed execution still uses the pane environment so the action works in
  the pane's actual filesystem context, including remote panes.
- The current hunk-matching pipeline is text-based rather than byte-oriented,
  so non-UTF-8 regular files fail during snapshot normalization instead of being
  patched as opaque binary blobs.

This means that a successful or failed action result is reporting the outcome
of Mezzanine's patch transaction, not the outcome of a model-authored shell
wrapper. The generated transport details are runtime-owned implementation
machinery.

The implementation owners for these behaviors are primarily:

- `src/agent/semantic/patch/mod.rs`
- `src/agent/semantic/patch/matcher.rs`
- `src/agent/semantic/patch/snapshot.rs`
- `src/agent/semantic/patch/transaction.rs`
- `src/subagent/validation.rs`

## When to choose `apply_patch` vs `shell_command`

Choose `apply_patch` for:

- editing text files;
- creating a file from explicit content;
- deleting a file through a patch directive; and
- moving a file when the same operation also changes file contents.

Choose `shell_command` for:

- reading files;
- running tests, formatters, or builds;
- creating directories;
- path-only renames or bulk file operations; and
- non-content workflows such as `git apply`, `mv`, `rm`, or `mkdir`.

This split keeps file-content mutation in a structured semantic action while
leaving general filesystem and process control in the shell surface.

## Current boundaries and deliberate non-goals

Some constraints are intentional design boundaries, not accidental omissions:

- `apply_patch` is for content mutation, not general filesystem orchestration.
  Path-only renames, directory creation, recursive deletion, and other bulk
  filesystem workflows still belong in `shell_command`.
- The action accepts some unified-diff-shaped payloads for compatibility, but
  raw unified diff is still an interop convenience rather than the primary
  contract. The canonical and most repairable representation remains an
  explicit Mezzanine `*** Begin Patch` block.
- There is no standalone `*** Replace File` directive. Whole-file replacement is
  expressed only through the single-hunk `@@ replace whole file` convention.
- `strip`-style path rewriting is intentionally unsupported for Mezzanine patch
  blocks.
- The current implementation is designed around bounded text editing with
  explicit context, not binary patching or arbitrary patch-tool emulation.

## Minimal examples

### Add a file

```text
*** Begin Patch
*** Add File: docs/example.txt
+hello
*** End Patch
```

### Update a file with copied context

```text
*** Begin Patch
*** Update File: src/lib.rs
@@ fn build_runtime @@
-    let mode = "old";
+    let mode = "new";
*** End Patch
```

### Replace a whole file intentionally

```text
*** Begin Patch
*** Update File: docs/example.txt
@@ replace whole file
+new contents
*** End Patch
```

## Practical guidance for authors

When writing documentation, tests, or prompts that discuss `apply_patch`, keep
these practical rules visible:

- prefer the canonical Mezzanine patch format even when compatibility parsing
  accepts diff-shaped alternatives;
- copy old/context lines from current file content instead of reconstructing
  likely code from memory;
- prefer several small anchored hunks over one large brittle patch;
- re-read only when the current evidence is stale, truncated, or does not cover
  the intended hunk; and
- treat a mismatch as a cue to inspect current state, not as a cue to ask the
  user for manual editing immediately.

Those constraints are central to Mezzanine's patch recovery loop and are the
main reason `apply_patch` is safer and more repairable than raw shell-authored
file rewrites.
