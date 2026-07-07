# `apply_patch` MAAP action

`apply_patch` is Mezzanine's model-facing semantic action for filesystem
content mutation. It exists so agents can create, update, move-with-edit,
replace, and delete files without treating those changes as arbitrary shell
text. In the MAAP action surface, `apply_patch` is the canonical path for file
content edits; local inspection, validation, directory creation, path-only
moves, and other non-content operations belong in `shell_command` instead.

This document summarizes the normative behavior in [`SPEC.md`](../SPEC.md) and
the current implementation in `src/agent/semantic/patch/` and
`src/subagent/validation.rs`.

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

- raw unified diffs and convert them to Mezzanine patch form;
- omitted `@@` on the first update hunk only;
- safe `./`, `a/`, or `b/` path prefixes;
- Markdown-fenced or uniformly indented patch payloads; and
- accidental shell-style `apply_patch <<...` wrappers.

Agents should still emit the canonical unwrapped Mezzanine patch format because
it is the least ambiguous and easiest to recover when a patch fails.

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
- Mezzanine synthesizes the concrete local execution plan and the user-facing
  change preview; agents do not need to generate shell wrappers or diffs.
- The action uses a short explicit timeout rather than inheriting the ordinary
  shell-command default so malformed or blocked patches fail quickly.
- If a multi-file patch applies changes to one path and later fails on another,
  already-completed per-file mutations are preserved. The diagnostic identifies
  what succeeded and what still needs retry.
- Shell-backed execution still uses the pane environment so the action works in
  the pane's actual filesystem context, including remote panes.

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
