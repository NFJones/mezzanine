---
name: add-research
description: Use when the user asks to save durable research findings into memory.
---

Use this workflow when the user asks to save, remember, or preserve research findings, investigation results, comparisons, decisions, external-source findings, or synthesized conclusions that should inform future planning.

First inspect, fetch, or verify the referenced research source when it is not already present in the current prompt. Use the smallest direct action that can read the source. If the source needs a capability that is not currently available, request that capability instead of substituting unrelated memory or shell actions.

Do not store secrets, credentials, private tokens, sensitive personal data, current-turn scratch notes, action results, CI logs, transient task state, or repository state that is cheap to rediscover. If the finding is speculative, one-off, or useful only for the current turn, say so and do not store it.

Before storing, turn the findings into readable Markdown. Separate observed facts from interpretation, include source names or links when available, preserve dates or version context that affects freshness, and keep unresolved uncertainty explicit. Prefer concise synthesized findings over raw pasted source text.

Use `memory_store` directly when it is available. If memory actions are absent, request the `memory` capability before proceeding. Do not use `memory_search` as a placeholder before storing user-requested research.

When storing research:

- Set `kind` to `research`.
- Set `content` to the curated readable Markdown research summary.
- Set `keywords` to a short list of durable retrieval anchors such as project, domain, tool, vendor, decision, or topic names.
- Set `scope` to `project` for research tied to the current project and `global` only for cross-project findings.
- Set `expires_in_days` to a long effectively non-expiring retention horizon, such as `36500`, when the active action schema supports explicit retention. Use a shorter value only when the user asks for one. Use `null` only when explicit retention is unavailable or the user asks to rely on the configured default, and mention that choice in the final response.
- Use `priority` only when the research is clearly important for future retrieval; omit it or set it to `null` when unsure.

After a successful store, report the memory kind, scope, keywords, and retention choice. Do not claim the memory was stored until the action result confirms success.
