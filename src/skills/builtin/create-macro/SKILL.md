---
name: create-macro
description: Create or modify Mezzanine agent macros in user or project scope. Use when the user asks to add, update, refactor, or repair a macro, MACRO.md, or macro workflow.
---

# Create Macro

Create the smallest agent macro that satisfies the user's intent.

## Scope

- User scope: `<config-root>/macros/<macro-name>/MACRO.md`. If the active Mezzanine config root is unavailable, use `~/.config/mezzanine`.
- Project scope: `<project-root>/.mezzanine/macros/<macro-name>/MACRO.md`.
- Default to user scope. Use project scope only when the user explicitly asks for a repo/project-scoped macro or says the macro must live with the current repository.

## Create

1. Derive a lowercase hyphenated name under 64 characters.
2. Create a directory whose basename exactly matches the macro name.
3. Write `MACRO.md` with only this YAML front matter:
   - `name`
   - `description`
4. Put triggering guidance in `description`; put macro execution prompts in the body.
5. Add a `## Steps` section containing an ordered list of prompt steps.
6. Write each step as a prompt suitable for the regular agent shell. Steps may include slash commands such as `/loop` when that is part of the requested workflow.
7. Keep the sequence focused; do not add auxiliary files unless the requested workflow needs them.

## Modify

1. Read the existing `MACRO.md` before editing.
2. Preserve the macro name unless the user asks for a rename.
3. Replace stale prompts instead of appending duplicate steps.
4. Keep user/project scope unchanged unless the user asks to move or copy the macro.
5. Preserve the macro's intended workflow and ordered step semantics.

## Validate

- Directory basename matches front-matter `name`.
- Name uses only lowercase ASCII letters, digits, and hyphens.
- `description` clearly says when to invoke the macro.
- `MACRO.md` contains a non-empty `## Steps` ordered list.
- Each step is a complete prompt that can be submitted to a regular agent shell.
- The sequence can run in one persistent subagent session without relying on manual intervention between steps.
