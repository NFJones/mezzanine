# Mezzanine helper tools

This directory contains small contributor-facing utilities that are useful while
maintaining Mezzanine but are not part of the installed `mez` runtime.

## Alacritty palette conversion

Use `alacritty-to-mez-theme.py` to turn a terminal color-scheme source in
Alacritty TOML format into a Mezzanine theme candidate. The helper can emit the
compact Rust `UiThemePalette` snippet used by built-in themes or a partial
`[themes.<name>]` configuration fragment for local experimentation.

```sh
python3 tools/alacritty-to-mez-theme.py --name apprentice apprentice-default.toml
python3 tools/alacritty-to-mez-theme.py --name apprentice --format config apprentice-default.toml
```

The mapping is intentionally opinionated and mirrors Mezzanine's compact theme
palette inputs:

| Mez palette field | Alacritty source |
| --- | --- |
| `surface` | `colors.primary.background` |
| `foreground` | `colors.primary.foreground` |
| `primary` | `colors.normal.green` |
| `secondary` | `colors.normal.blue` |
| `tertiary` | `colors.normal.yellow` |
| `danger` | `colors.normal.red` |
| `muted` | `colors.normal.white`, falling back to `colors.bright.black` |
| `thinking` | `colors.bright.green`, falling back to `muted` |
| `agent_prompt_background` | a small lightened or darkened variant of `surface` |

Generated snippets are starting points. Before adding a built-in theme, compare
the output against the upstream palette, add a fidelity target in terminal tests,
and run the required repository validation.
