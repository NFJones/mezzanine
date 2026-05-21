---
name: create-skill
description: Create or modify concise Mezzanine skills in user or project scope. Use when the user asks to add, update, refactor, or repair a skill, SKILL.md, or skill resources.
---

# Create Skill

Create the smallest skill that satisfies the user's intent.

## Scope

- User scope: `<config-root>/skills/<skill-name>/SKILL.md`. If the active Mezzanine config root is unavailable, use `~/.config/mezzanine`.
- Project scope: `<project-root>/.mezzanine/skills/<skill-name>/SKILL.md`.
- Default to user scope. Use project scope only when the user explicitly asks for a repo/project-scoped skill or says the skill must live with the current repository.

## Create

1. Derive a lowercase hyphenated name under 64 characters.
2. Create a directory whose basename exactly matches the skill name.
3. Write `SKILL.md` with only this YAML front matter:
   - `name`
   - `description`
4. Put triggering guidance in `description`; put only execution guidance in the body.
5. Keep the body terse, imperative, and focused on non-obvious workflow steps.
6. Add `scripts/`, `references/`, `assets/`, or `agents/` only when the user's requested workflow needs them. Do not create README, changelog, install guide, or other auxiliary docs.

## Modify

1. Read the existing `SKILL.md` and any directly relevant resource files.
2. Preserve the skill name unless the user asks for a rename.
3. Replace stale guidance instead of appending duplicate sections.
4. Remove placeholder or obsolete resources when they no longer support the workflow.
5. Keep user/project scope unchanged unless the user asks to move or copy the skill.

## Validate

- Directory basename matches front-matter `name`.
- Name uses only lowercase ASCII letters, digits, and hyphens.
- `description` clearly says when to use the skill.
- Body contains only information required to accomplish the requested workflow.
- Optional resources are referenced from `SKILL.md` and are actually needed.
