//! Cli Help implementation.
//!
//! This module owns the cli help boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::Write;

// CLI help text.

/// Runs the print help operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn print_help<W: Write>(stdout: &mut W) {
    let _ = writeln!(
        stdout,
        "usage: mez [--json] <command> [options]\n\
\n\
options:\n\
  --json               emit machine-readable JSON instead of plaintext output\n\
\n\
commands:\n\
  -S <path>            select an explicit control socket\n\
  -L <name>            select a named socket under the runtime directory\n\
  new [--dry-run]      start a background session daemon and attach when interactive\n\
                       alias: new-session\n\
  serve [--attach-primary]\n\
                       start a foreground control daemon for a new session\n\
                       alias: daemon\n\
  list                 list resumable sessions known to this client\n\
                       alias: list-sessions\n\
  attach [SESSION_ID|INDEX] [--observer|--observe]\n\
                       reattach primary terminal or request observer access\n\
                       alias: attach-session\n\
  detach [--client-id ID]\n\
                       detach the current or specified control client\n\
                       alias: detach-client\n\
  kill-session --force terminate a live session through the control socket\n\
  snapshot list        list locally persisted snapshots\n\
  snapshot create      create a live snapshot through the control socket\n\
  snapshot inspect ID  inspect a snapshot manifest\n\
  snapshot delete ID   delete a snapshot manifest and local payload\n\
  snapshot resume ID [--serve] [--restart-command CMD]\n\
                       restore a persisted snapshot session model or service\n\
  snapshot resume-latest [--serve] [--session-id ID]\n\
                       restore or serve the newest persisted snapshot\n\
  snapshot resume-plan ID\n\
                       show restart limitations for a persisted snapshot\n\
  snapshot latest-plan [--session-id ID]\n\
                       show restart limitations for the newest snapshot\n\
  snapshot rollback-plan ID\n\
                       show whether a snapshot can act as a rollback point\n\
  config init          create ~/.config/mezzanine/config.toml if missing\n\
  config path          print the selected primary config path\n\
  config default       print the built-in default config\n\
  config validate      validate the selected primary config or a given file\n\
  config get [PATH]    show effective config and source layers\n\
  config layers        show config layer order, source, trust, and diagnostics\n\
  config set PATH VALUE [--scope user|project] [--file PATH]\n\
                       persist a scalar config mutation\n\
  config unset PATH [--scope user|project] [--file PATH]\n\
                       persist a scalar config removal\n\
  config trust list    list project trust records\n\
  config trust trust PATH|reject PATH|revoke PATH\n\
                       update project trust state\n\
  auth status          show provider authentication metadata\n\
  auth login [--browser | --device-code | --api-key [--api-key-file PATH]]\n\
                       sign in with ChatGPT by default; API-key setup remains available explicitly\n\
  auth logout          remove local authentication metadata\n\
  mcp list             list configured MCP servers and known tools\n\
  mcp add|remove|enable|disable|inspect\n\
                       edit or inspect configured MCP servers\n\
  memory list          list persistent agent memory records\n\
  memory add ID --scope S --content TEXT\n\
                       add or replace a persistent memory record\n\
  memory inspect|edit|delete|export\n\
                       inspect, edit, delete, or export persistent memory\n\
  help                 show this help\n\
  version, --version   show version information"
    );
}
