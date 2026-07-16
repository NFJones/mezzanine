---
name: add-doc
description: Use when the user asks to save durable documentation or reference content into memory.
---

Use this workflow when the user asks to save, remember, import, or preserve documentation, API reference text, guide material, or other reusable reference content for future tasks.

First inspect or fetch the referenced material when it is not already present in the current prompt. Use the smallest direct action that can read the source. If the source needs a capability that is not currently available, request that capability instead of substituting unrelated memory or shell actions.

Do not store secrets, credentials, private tokens, sensitive personal data, current-turn scratch notes, action results, CI logs, transient task state, or repository state that is cheap to rediscover. If the referenced content is not durable and reusable beyond the current task, say so and do not store it.

Before storing, convert the material into readable Markdown. Preserve source names, section headings, links, version/date context when available, and the reusable facts or procedures that future agents should consult. Keep the memory focused enough to retrieve and read later; do not dump noisy raw logs or unrelated surrounding text.

Use `memory_store` directly when it is available. If memory actions are absent, request the `memory` capability before proceeding. Do not use `memory_search` as a placeholder before storing user-requested documentation.

When storing documentation:

- Set `kind` to `documentation`.
- Set `content` to the curated readable Markdown body.
- Set `keywords` to a short list of durable retrieval anchors such as product, API, library, feature, version, or topic names.
- Set `scope` to `project` for project-specific documentation and `global` only for cross-project reference material.
- Set `expires_in_days` to a long effectively non-expiring retention horizon, such as `36500`, when the active action schema supports explicit retention. Use a shorter value only when the user asks for one. Use `null` only when explicit retention is unavailable or the user asks to rely on the configured default, and mention that choice in the final response.
- Use `priority` only when the documentation is clearly important for future retrieval; omit it or set it to `null` when unsure.

After a successful store, report the memory kind, scope, keywords, and retention choice. Do not claim the memory was stored until the action result confirms success.
