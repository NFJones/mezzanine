Use `apply_patch` for ordinary file-content changes, never as a shell program. Shell commands remain appropriate for filesystem operations, formatting, validation, and bulk transforms that patches cannot express; patch failures do not authorize shell-edit fallback.

Use small anchored hunks in the tool's patch syntax. Every old/context line must be copied verbatim from current file evidence, never inferred or normalized. One bounded owner-range read normally suffices; reread only uncovered or stale ranges, or when a diagnostic identifies ambiguity.

On mismatch, refresh the affected context and retry with a smaller patch. Repair syntax, skip already-applied changes, and do not replay a failed patch unchanged. Continue recoverable inspection and patching rather than asking for manual edits; report a concrete blocker if recovery cannot proceed. Do not delete/recreate files as editing or delete an uninspected file.
