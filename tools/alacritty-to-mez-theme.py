#!/usr/bin/env python3
"""Convert an Alacritty color scheme into a Mezzanine theme candidate.

The helper is intentionally small and dependency-free beyond Python's standard
library. It accepts the Alacritty TOML files exposed by palette collections such
as TerminalColors and emits either the compact Rust `UiThemePalette` initializer
used by built-in Mezzanine themes or a partial custom-theme configuration
fragment. The generated values preserve the source palette anchors while leaving
Mezzanine's runtime theme derivation responsible for contrast-managed slots.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - Python < 3.11 runtime guard.
    tomllib = None  # type: ignore[assignment]


HEX_COLOR_RE = re.compile(r"^#[0-9a-fA-F]{6}$")
THEME_NAME_RE = re.compile(r"^[A-Za-z0-9_-]+$")
PALETTE_FIELDS = (
    "primary",
    "secondary",
    "tertiary",
    "surface",
    "foreground",
    "muted",
    "thinking",
    "danger",
    "agent_prompt_background",
)


def parse_args() -> argparse.Namespace:
    """Parse command-line arguments for one palette conversion."""
    parser = argparse.ArgumentParser(
        description="Convert Alacritty TOML colors into a Mez theme candidate."
    )
    parser.add_argument(
        "input",
        nargs="?",
        type=Path,
        help="Alacritty TOML file to read; stdin is used when omitted.",
    )
    parser.add_argument(
        "--name",
        required=True,
        help="Theme name to embed in the generated snippet.",
    )
    parser.add_argument(
        "--format",
        choices=("rust-palette", "config"),
        default="rust-palette",
        help="Output format to generate.",
    )
    args = parser.parse_args()
    if not THEME_NAME_RE.fullmatch(args.name):
        parser.error("--name must contain only ASCII letters, digits, '_' or '-'")
    return args


def load_toml(path: Path | None) -> dict:
    """Load TOML from a path or stdin and return the parsed object."""
    if tomllib is None:
        raise SystemExit("Python 3.11 or newer is required for standard-library TOML parsing")
    if path is None:
        return tomllib.loads(sys.stdin.read())
    with path.open("rb") as handle:
        return tomllib.load(handle)


def require_table(parent: dict, key: str) -> dict:
    """Return a nested TOML table or exit with a diagnostic."""
    value = parent.get(key)
    if not isinstance(value, dict):
        raise SystemExit(f"required table `{key}` is missing")
    return value


def normalize_hex(value: object, path: str) -> str:
    """Validate one TOML value as a full six-digit hex color."""
    if not isinstance(value, str) or not HEX_COLOR_RE.fullmatch(value):
        raise SystemExit(f"{path} must be a #rrggbb color")
    return value.lower()


def color(colors: dict, section: str, key: str) -> str:
    """Read one required Alacritty color value."""
    table = require_table(colors, section)
    return normalize_hex(table.get(key), f"colors.{section}.{key}")


def first_color(colors: dict, candidates: tuple[tuple[str, str], ...]) -> str:
    """Return the first available color from a list of table/key candidates."""
    for section, key in candidates:
        table = colors.get(section)
        if isinstance(table, dict) and key in table:
            return normalize_hex(table[key], f"colors.{section}.{key}")
    joined = ", ".join(f"colors.{section}.{key}" for section, key in candidates)
    raise SystemExit(f"one of {joined} is required")


def rgb_from_hex(value: str) -> tuple[int, int, int]:
    """Convert a #rrggbb color string into RGB channel integers."""
    return (int(value[1:3], 16), int(value[3:5], 16), int(value[5:7], 16))


def hex_from_rgb(red: int, green: int, blue: int) -> str:
    """Format RGB channels as a #rrggbb color string."""
    return f"#{red:02x}{green:02x}{blue:02x}"


def lifted_surface(surface: str) -> str:
    """Return a subtle prompt surface derived from the source background."""
    red, green, blue = rgb_from_hex(surface)
    average = (red + green + blue) / 3
    delta = 10 if average < 128 else -10
    return hex_from_rgb(
        max(0, min(255, red + delta)),
        max(0, min(255, green + delta)),
        max(0, min(255, blue + delta)),
    )


def derive_palette(document: dict) -> dict[str, str]:
    """Map Alacritty color tables onto Mezzanine compact palette fields."""
    colors = require_table(document, "colors")
    surface = color(colors, "primary", "background")
    muted = first_color(colors, (("normal", "white"), ("bright", "black")))
    return {
        "primary": color(colors, "normal", "green"),
        "secondary": color(colors, "normal", "blue"),
        "tertiary": color(colors, "normal", "yellow"),
        "surface": surface,
        "foreground": color(colors, "primary", "foreground"),
        "muted": muted,
        "thinking": first_color(colors, (("bright", "green"), ("normal", "white"))),
        "danger": color(colors, "normal", "red"),
        "agent_prompt_background": lifted_surface(surface),
    }


def render_rust_palette(name: str, palette: dict[str, str]) -> str:
    """Render a Rust match arm that initializes a `UiThemePalette`."""
    lines = [f'        "{name}" => Some(definition_from_palette(UiThemePalette {{']
    for field in PALETTE_FIELDS:
        lines.append(f'            {field}: "{palette[field]}",')
    lines.append("        })),")
    return "\n".join(lines)


def render_config_fragment(name: str, palette: dict[str, str]) -> str:
    """Render a TOML custom-theme fragment for local experimentation."""
    lines = [f"[themes.{name}.aliases]"]
    for field in PALETTE_FIELDS:
        if field == "agent_prompt_background":
            continue
        lines.append(f'{field} = "{palette[field]}"')
    lines.extend(["", f"[themes.{name}.colors]"])
    lines.append(f'agent_prompt_bg = "{palette["agent_prompt_background"]}"')
    return "\n".join(lines)


def main() -> int:
    """Run one conversion and write the selected snippet to stdout."""
    args = parse_args()
    palette = derive_palette(load_toml(args.input))
    if args.format == "config":
        print(render_config_fragment(args.name, palette))
    else:
        print(render_rust_palette(args.name, palette))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
