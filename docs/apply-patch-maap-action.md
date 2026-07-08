# `apply_patch` MAAP action reference

This document explains the Mezzanine `apply_patch` MAAP action: what it is
for, which patch shapes it accepts, how the runtime applies a patch, and how
to recover when a patch does not apply cleanly.

## What `apply_patch` is

`apply_patch` is Mezzanine's structured file-content mutation action. It is a
semantic MAAP action, not a pane shell executable.

Use `apply_patch` when the goal is to change file contents, including:

- creating a new text file,
- making a localized edit to an existing file,
- deleting a file through an explicit patch operation,
- moving a file as part of an update operation, or
- replacing an entire file intentionally.

Do **not** invoke `apply_patch` inside `shell_command`. If a model writes shell
text such as `printf ... | apply_patch`, Mezzanine rejects it because
`apply_patch` is not available as a pane-shell program.

Use `shell_command` instead for:

- repository inspection,
- `rg`, `sed`, `cat`, `git`, builds, and tests,
- directory creation,
- path moves or deletions that are not file-content patch operations, and
- raw diff application through explicit tools such as `git apply`.

## Core contract

The model-facing payload is a patch string. Canonical patches start with
`*** Begin Patch` and end with `*** End Patch`.

```text
*** Begin Patch
*** Update File: src/lib.rs
@@ fn owner
-old_line();
+new_line();
*** End Patch
```

At MAAP validation time, Mezzanine rejects payloads that are empty, missing the
begin/end markers, or contain no file operations.

## Supported file operations

Within one patch block, Mezzanine accepts one or more file operations.

### Add a file

```text
*** Begin Patch
*** Add File: docs/example.txt
+first line
+second line
*** End Patch
```

- Every content line in an add-file block must begin with `+`.
- Empty content is allowed and creates a zero-byte regular file.

### Update a file

```text
*** Begin Patch
*** Update File: src/lib.rs
@@ fn owner
-old_line();
+new_line();
*** End Patch
```

- An update operation contains one or more hunks.
- Each canonical hunk starts with `@@`.
- Text after `@@` is treated as an optional anchor, not as executable shell
  syntax.
- Multiple anchor fragments may appear in one header, separated by additional
  `@@` markers.

Mezzanine also supports an update-plus-move form:

```text
*** Begin Patch
*** Update File: src/old_name.rs
*** Move to: src/new_name.rs
@@
-old
+new
*** End Patch
```

### Delete a file

```text
*** Begin Patch
*** Delete File: old.txt
*** End Patch
```

Delete operations carry no hunk body.

## Hunk body rules

Inside an update hunk, each line must begin with exactly one of:

- a space for context,
- `-` for removed content, or
- `+` for added content.

`*** End of File` marks a missing final newline.

For ordinary edits, the most reliable hunk shape is:

- a small owner range,
- a distinctive `@@` header anchor, and
- 1 to 6 exact old/context lines copied verbatim from the current file.

Mezzanine treats copied old/context lines as authoritative current-file
evidence. Inferred or reconstructed old context is intentionally fragile and is
more likely to produce a mismatch diagnostic.

## Whole-file replacement

Intentional full replacement uses the explicit `@@ replace whole file`
convention.

```text
*** Begin Patch
*** Update File: note.txt
@@ replace whole file
+new
+body
*** End Patch
```

Rules:

- the whole-file replacement hunk must be the only hunk for that file,
- it must be the only update hunk in the operation, and
- it may contain only added lines.

If old-context lines appear in a whole-file replacement hunk, Mezzanine rejects
the patch with a specific validation error.

## Path safety and normalization

Patch header paths are resolved relative to the pane current working directory.
They must not be absolute and must not contain `..` traversal.

Mezzanine normalizes several safe copied path forms before validation:

- leading `./`,
- leading `a/` and `b/` prefixes from unified diffs, and
- no-op `.` path segments.

The runtime also resolves symlinks before making filesystem safety decisions.
It rejects targets whose resolved path:

- escapes the pane current working directory, or
- resolves to a non-regular filesystem node.

Those checks happen before the write phase so special files cannot stall patch
execution.

## Compatibility forms Mezzanine accepts

The canonical recommendation is still: emit a clean Mezzanine patch block
directly in the `apply_patch.patch` field. However, Mezzanine accepts several
common compatibility wrappers and diff-shaped variants so recoverable model
output can still succeed.

### Wrapper normalization

Mezzanine can normalize patch text that arrives as:

- a Markdown fenced block,
- a heredoc-wrapped payload,
- an accidental `apply_patch <<'PATCH'` wrapper, or
- a uniformly indented patch block.

These are compatibility paths, not the preferred format.

### Lenient first update hunk

The first update hunk may omit the opening `@@` header. This exists for
compatibility with Codex-style patch output. Later hunks still require proper
headers.

### Unified-diff range metadata inside a Mezzanine hunk header

Mezzanine accepts headers such as:

```text
@@ -10,7 +10,8 @@ fn method
```

The old/new line ranges are not trusted as direct placement authority. They are
only conservative disambiguation hints after the hunk body and anchors have been
considered.

### Raw unified diffs

If the `apply_patch.patch` payload is a raw unified diff with `---`, `+++`, and
`@@` markers, Mezzanine attempts to convert it to a Mezzanine patch before
planning.

This supports common model output such as:

```diff
--- a/note.txt
+++ b/note.txt
@@ -1,2 +1,2 @@
-old
+new
 context
```

Important limits:

- conversion is for diff shapes Mezzanine can represent safely,
- deleted-file unified diffs are intentionally not auto-converted, and
- if entirely non-Mezzanine diff application is required, use `shell_command`
  with an explicit tool such as `git apply`.

## Runtime architecture

`apply_patch` is a semantic action. The runtime does not trust the model to
write shell patch logic directly.

For shell-backed execution, the action is multi-phase:

1. **Validation and parsing**
   - validate the payload shape,
   - normalize compatibility wrappers if present,
   - optionally convert raw unified diffs,
   - parse Mezzanine file operations and hunks.
2. **Read/snapshot phase**
   - read the target paths through generated shell code,
   - capture regular-file bytes plus resolved-path metadata,
   - classify missing, non-regular, and out-of-scope targets.
3. **Rust-side patch planning**
   - apply parsed hunks against the captured snapshots,
   - compute final file bytes and any planned failures,
   - build a verified write plan.
4. **Write phase**
   - verify the file still matches the previously captured snapshot,
   - create parent directories when needed,
   - write exact output bytes through bounded base64 transport,
   - emit a bounded unified diff preview for successful mutations.

When native execution is enabled for eligible local actions, Mezzanine still
reuses the same parser, matcher, path-safety checks, snapshot verification, and
planned-failure logic.

## Why `apply_patch` is multi-phase

The split read/write design protects against common mutation hazards:

- stale target contents,
- symlink or path changes between inspection and write,
- ambiguous or misplaced hunks,
- oversized shell payload lines, and
- partial transport failures.

Generated shell transactions base64-encode file content and keep physical shell
lines short enough to stay below common PTY canonical-line limits.

## Matching behavior

Mezzanine's matcher is intentionally more capable than a naive exact string
splice.

It supports:

- exact context matching,
- anchor-guided matching,
- structural-anchor scoping for repeated regions,
- conservative use of unified old-line hints for disambiguation,
- blank-line tolerant context handling in supported cases, and
- detection of already-present replacement blocks or distinctive added lines.

This helps patches stay local and recoverable, especially in repeated or
partially changed files.

## Failure model and diagnostics

`apply_patch` failures are usually recoverable and are designed to feed the next
correction step rather than end the task immediately.

### Validation failures

Validation rejects malformed payloads before execution, for example:

- missing `*** Begin Patch`,
- missing `*** End Patch`,
- no file operations,
- unsafe patch paths, or
- malformed hunk lines.

These failures mean the patch structure is wrong, not that current file context
needs to be reread.

### Hunk mismatch diagnostics

When a hunk does not match, the matcher reports structured, model-usable clues,
including combinations of:

- `failure_code`,
- affected path,
- matched-candidate line or span information,
- missing anchor information,
- replacement-presence hints,
- suggested read ranges,
- current file context near the likely owner region, and
- explicit next-step guidance.

Common guidance categories include:

- reread the reported region,
- reread candidate regions when repeated matches are ambiguous,
- refresh or correct the `@@` header anchor, or
- skip/reconcile the hunk because the intended replacement may already be
  present.

### Transport and execution failures

The runtime also surfaces distinct recovery paths for cases such as:

- pane input delivery failure,
- transport truncation during read or write setup,
- payload size limits,
- snapshot checksum or byte-count mismatch,
- path safety rejection, and
- execution-mode changes during a multi-phase patch.

These failures have different next steps from a content mismatch. For example,
transport truncation usually calls for a smaller patch split by file or owner
range rather than a reread of current file text.

### Partial success

If a multi-file patch applies some file operations before a later operation
fails, Mezzanine preserves the completed file mutations and reports which paths
were already applied. Recovery should then target only the remaining work.

## Recommended authoring pattern

For the highest success rate:

1. inspect the owner file with a bounded read,
2. copy exact old/context lines from that read,
3. emit one small patch for one coherent owner range,
4. prefer distinctive `@@` anchors when context may repeat,
5. use multiple small hunks instead of one brittle large hunk, and
6. after a mismatch, reread only the implicated current region before retrying.

Avoid retrying substantially the same patch after a mismatch. Mezzanine's
diagnostics are designed to tell the next model step which region or ambiguity
needs attention.

## Examples

### Localized update

```text
*** Begin Patch
*** Update File: src/lib.rs
@@ fn build_request
-    let timeout = 30;
+    let timeout = 60;
*** End Patch
```

### Add a new file

```text
*** Begin Patch
*** Add File: docs/example.txt
+created by apply_patch
*** End Patch
```

### Delete a file

```text
*** Begin Patch
*** Delete File: obsolete.txt
*** End Patch
```

### Move while updating content

```text
*** Begin Patch
*** Update File: src/old.rs
*** Move to: src/new.rs
@@
-pub fn old_name() {}
+pub fn new_name() {}
*** End Patch
```

### Whole-file replacement

```text
*** Begin Patch
*** Update File: note.txt
@@ replace whole file
+replacement
+content
*** End Patch
```

## Relationship to the spec and tests

For normative behavior, use:

- `SPEC.md` for the authoritative contract,
- `src/agent/semantic/patch/` for the parser, matcher, snapshot, and
  transaction implementation, and
- `src/agent/tests/part_02.rs` plus `src/runtime/tests/part_06.rs` for
  representative accepted forms and recovery behavior.

This document is a contributor and operator guide. If this page and `SPEC.md`
ever disagree, treat `SPEC.md` as the source of truth.
