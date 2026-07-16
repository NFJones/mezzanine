This directory contains built-in Mezzanine macro assets embedded with `include_dir`.

Each built-in macro should live in its own subdirectory as `<macro-name>/MACRO.md`.
The `MACRO.md` file uses the same front matter and `## Steps` ordered-list format
as user and project macros. The directory is intentionally empty until Mezzanine
ships a built-in macro, so adding embedded loading support does not change the
current effective macro catalog.
