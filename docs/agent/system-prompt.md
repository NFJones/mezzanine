# Agent instructions and customization

## Purpose

Choose the right place for agent guidance and adjust response style without
confusing prompt text with permission or execution authority.

## Prerequisites

Understand the [agent overview](overview.md) and
[safety and trust boundaries](../safety-and-trust/README.md).

## Choose the narrowest guidance

| Need | Use |
| --- | --- |
| Instructions for one task | The ordinary agent prompt. State the goal, owned files, constraints, and expected validation. |
| A directive for future turns in this pane session | `/directive <text>`; bare `/directive` or `/directive show` inspects it, and `/directive clear` removes it. |
| Repository workflow and conventions | Project `AGENTS.md` files, subject to the [current discovery limits](../safety-and-trust/project-trust-and-instructions.md#understand-repository-instructions). |
| Reusable text for this project or all projects | Enabled [context documents](context-and-continuity.md#reuse-guidance-across-conversations). |
| A workflow invoked only when wanted | An explicit [skill or macro](commands-skills-and-macros.md#invoke-a-skill-or-macro-explicitly). |
| Response tone and presentation | A configured personality selected with `/personality`. |
| General user-owned prompt additions | `agents.custom_system_prompt` in configuration. |

Keep instructions concise and non-secret. Prefer concrete boundaries such as
"edit only docs/" or "report review findings without implementing them" over
vague demands for autonomy. Do not duplicate large source references in every
prompt when the agent can inspect the relevant artifact directly.

Discovered project guidance is assembled before provider requests, but an
accepted provider request chain retains its existing guidance until the next
turn. The current pane bootstrap also uses fixed instruction filenames and
limits rather than the declared [instruction settings](../configuration/reference.md#instructions).
Changes to a directive or reusable guidance are not a way to rewrite an already
completed action or retroactively change a running turn's authority. Inspect
the effective context when a new instruction appears not to be applied.

## Select a personality

```text
/list-personalities
/personality
/personality <profile-id>
```

Replace `<profile-id>` with an ID from the configured catalog. `/personality
clear` or `/personality default` removes the pane's selection and falls back to
`agents.default_personality`, if configured; it does not force personalities off.
Profiles can include style, prompt additions, and model, planning, or routing
preferences; inspect their configuration rather than assuming every profile
changes only tone.
Clearing the personality selection does not undo model, planning, or routing
overrides already applied by that profile; inspect those controls separately.
`agents.default_personality` sets the configured default. See
[agents, providers, and authentication configuration](../configuration/agents-providers-and-auth.md)
for configuration guidance.

## Understand what guidance cannot do

Mez supplies built-in execution, evidence, editing, and trust instructions.
Your additions do not replace the runtime's action catalog or authorize access
to files, processes, credentials, or integrations. A personality cannot grant
approval, and a peer message cannot override user instructions. Repository,
skill, macro, MCP, and terminal content must not be treated as a source of new
security authority.

When behavior is unexpected, use `/status`, `/permissions`, and `/approval` to
inspect runtime state. Use `/show-context` to inspect conversation entries and
`/copy-context` for the assembled request or idle preview. Review exports before
sharing them: prompts and action results can contain private task data even
when credentials and hidden runtime policy are excluded.

## Check a customization

Try a small task with clear expected behavior before adopting a broad prompt
change. Check both a normal case and a boundary case: for example, ask for a
bounded edit, then ask for a review that must not edit files. Compare observed
actions, changed files, and validation results, not just the model's prose.

Model behavior is nondeterministic and provider-dependent. Successful local
tests or one successful conversation do not establish identical behavior across
models. Do not use secrets in evaluation prompts or fixtures.

## Related pages

- [Commands, skills, and macros](commands-skills-and-macros.md)
- [Context and continuity](context-and-continuity.md)
- [Providers and models](providers-and-models.md)
- [Normative prompt profile](../../SPEC.md#16-agent-system-prompt-profile)

## Next step

Use [Commands, skills, and macros](commands-skills-and-macros.md) to apply a
task-specific workflow, or return to [the agent section](README.md).
