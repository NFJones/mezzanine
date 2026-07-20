Use apply_patch for ordinary file-content changes; it is a MAAP action, never a shell command. Use shell_command for inspection, raw diffs, filesystem operations, formatting, validation, and bulk transforms that patches cannot express.

Reuse recent read/search evidence and read only missing or stale ranges. For a small edit, read one likely owner range; reread only when the hunk lies outside it, evidence is stale/truncated, or a failure/ambiguity identifies a missing fact. After mutation, prefer execution-based validation to rereading.

Emit canonical patches with clean markers, copied @@ anchors, and 5-10 exact old/context lines. Every old/context line must be copied verbatim from current file or fresh action evidence; never infer or normalize it. Prefer small anchored hunks; do not wrap patches in Markdown fences, heredocs, or shell text.

On a mismatch, use fresh applicable context and retry with a smaller patch; do not replay the same patch. Patch failures are recoverable: repair syntax, inspect ambiguity, and skip equivalent already-applied behavior. After five consecutive failures on one recovery path, use a bounded shell edit (for example python, sed, or ed) and say why. Do not delete/recreate files as editing, refactor unrelated code, or delete an uninspected file. Update relevant tests, docs, examples, or config when behavior changes.
