//! Bootstrap and tool-discovery shell source plus bootstrap-output parsing.
//!
//! The scripts are deterministic protocol payloads. Parsing returns lower
//! agent contracts and discovered instruction metadata without product I/O.

use super::transaction::classify_version_probe;
use super::{EnvironmentSignature, ShellClassification, ToolInventory};
use crate::instructions::{DiscoveredInstructionFile, parse_instruction_discovery_output};
use std::path::Path;

/// Runs the tool discovery script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn tool_discovery_script() -> &'static str {
    "mez_discovered_at=$(date +%s 2>/dev/null || printf '0')\n\
mez_probe_tool() {\n\
  mez_tool=\"$1\"\n\
  mez_lookup_command=\"command -v $mez_tool\"\n\
  mez_path=$(command -v \"$mez_tool\" 2>/dev/null)\n\
  mez_lookup_status=$?\n\
  mez_version=\"\"\n\
  mez_version_command=\"\"\n\
  mez_version_status=\"\"\n\
  if [ \"$mez_lookup_status\" -eq 0 ]; then\n\
    mez_version_command=\"$mez_path --version\"\n\
    mez_version_output=$(\"$mez_path\" --version 2>/dev/null)\n\
    mez_version_status=$?\n\
    mez_version=$(printf '%s\\n' \"$mez_version_output\" | { IFS= read -r mez_first_line; printf '%s' \"$mez_first_line\"; })\n\
  fi\n\
  printf 'tool\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\n' \"$mez_tool\" \"$([ \"$mez_lookup_status\" -eq 0 ] && printf '1' || printf '0')\" \"$mez_path\" \"$mez_version\" \"$mez_lookup_command\" \"$mez_lookup_status\" \"$mez_version_command\" \"$mez_version_status\" \"$mez_discovered_at\"\n\
}\n\
for mez_tool in sed grep rg fd bat jq git; do\n\
  mez_probe_tool \"$mez_tool\"\n\
done\n\
mez_python_path=$(command -v python3 2>/dev/null)\n\
mez_python_lookup_status=$?\n\
if [ \"$mez_python_lookup_status\" -ne 0 ]; then\n\
  mez_python_path=$(command -v python 2>/dev/null)\n\
  mez_python_lookup_status=$?\n\
fi\n\
mez_python_version=\"\"\n\
mez_python_version_command=\"\"\n\
mez_python_version_status=\"\"\n\
if [ \"$mez_python_lookup_status\" -eq 0 ]; then\n\
  mez_python_version_command=\"$mez_python_path --version\"\n\
  mez_python_version_output=$(\"$mez_python_path\" --version 2>/dev/null)\n\
  mez_python_version_status=$?\n\
  mez_python_version=$(printf '%s\\n' \"$mez_python_version_output\" | { IFS= read -r mez_first_line; printf '%s' \"$mez_first_line\"; })\n\
fi\n\
printf 'tool\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\n' \"python\" \"$([ \"$mez_python_lookup_status\" -eq 0 ] && printf '1' || printf '0')\" \"$mez_python_path\" \"$mez_python_version\" \"command -v python3 || command -v python\" \"$mez_python_lookup_status\" \"$mez_python_version_command\" \"$mez_python_version_status\" \"$mez_discovered_at\"\n"
}

/// Runs the bootstrap script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn bootstrap_script() -> String {
    let mut script = "mez_discovered_at=$(date +%s 2>/dev/null || printf '0')\n\
mez_bootstrap_field() {\n\
  mez_key=\"$1\"\n\
  mez_value=\"$2\"\n\
  printf 'env\\t%s\\t%s\\n' \"$mez_key\" \"$mez_value\"\n\
}\n\
\n\
mez_bootstrap_field os \"$(uname -s 2>/dev/null || printf 'unknown')\"\n\
mez_bootstrap_field arch \"$(uname -m 2>/dev/null || printf 'unknown')\"\n\
mez_kernel=$(uname -r 2>/dev/null)\n\
if [ -n \"$mez_kernel\" ]; then\n\
  mez_bootstrap_field kernel_version \"$mez_kernel\"\n\
fi\n\
\n\
mez_bootstrap_field host \"$(hostname 2>/dev/null || printf 'unknown')\"\n\
mez_bootstrap_field user \"$(whoami 2>/dev/null || printf 'unknown')\"\n\
mez_bootstrap_field shell_path \"$SHELL\"\n\
\n\
mez_shell_name=$(printf '%s' \"$SHELL\" | { IFS=/ read -r _ _ _ _ _ _ _ _ _ _ _ mez_stem; printf '%s' \"$mez_stem\"; });\n\
mez_shell_name=${mez_shell_name:-sh}\n\
mez_bootstrap_field shell_class \"$mez_shell_name\"\n\
\n\
if command -v \"$SHELL\" >/dev/null 2>&1; then\n\
  mez_shell_ver=$(\"$SHELL\" --version 2>/dev/null | { IFS= read -r mez_first_line; printf '%s' \"$mez_first_line\"; })\n\
  if [ -n \"$mez_shell_ver\" ]; then\n\
    mez_bootstrap_field shell_version \"$mez_shell_ver\"\n\
  fi\n\
fi\n\
\n\
mez_bootstrap_field path \"$PATH\"\n\
mez_bootstrap_field cwd \"$(pwd 2>/dev/null || printf '/')\"\n\
\n\
mez_project_root=\"\"\n\
mez_search_dir=\"$(pwd 2>/dev/null)\"\n\
while [ -n \"$mez_search_dir\" ] && [ \"$mez_search_dir\" != \"/\" ]; do\n\
  if [ -d \"$mez_search_dir/.git\" ]; then\n\
    mez_project_root=\"$mez_search_dir\"\n\
    break\n\
  fi\n\
  mez_search_dir=$(dirname \"$mez_search_dir\" 2>/dev/null)\n\
done\n\
mez_bootstrap_field project_root \"$mez_project_root\"\n\
mez_bootstrap_field git_repo \"$([ -n \"$mez_project_root\" ] && printf '1' || printf '0')\"\n\
\n\
if [ -f /proc/1/cgroup ] 2>/dev/null; then\n\
  mez_container=$(grep -Eo 'docker|lxc|kubepods|libpod' /proc/1/cgroup 2>/dev/null | head -n1)\n\
  if [ -n \"$mez_container\" ]; then\n\
    mez_bootstrap_field container \"$mez_container\"\n\
  fi\n\
elif [ -f /.dockerenv ] 2>/dev/null; then\n\
  mez_bootstrap_field container docker\n\
fi\n\
\n\
if [ -n \"$VIRTUAL_ENV\" ]; then\n\
  mez_bootstrap_field env_manager \"virtualenv:$VIRTUAL_ENV\"\n\
fi\n\
if [ -n \"$CONDA_PREFIX\" ]; then\n\
  mez_bootstrap_field env_manager \"conda:$CONDA_PREFIX\"\n\
fi\n\
if [ -n \"$NIX_PROFILES\" ]; then\n\
  mez_bootstrap_field env_manager \"nix:$NIX_PROFILES\"\n\
fi\n\
if [ -n \"$NODE_VIRTUAL_ENV\" ]; then\n\
  mez_bootstrap_field env_manager \"node:$NODE_VIRTUAL_ENV\"\n\
fi\n\
mez_rustup_home=${RUSTUP_HOME:-$HOME/.rustup}\n\
if [ -d \"$mez_rustup_home\" ]; then\n\
  mez_rustup_home=$(CDPATH= cd -P -- \"$mez_rustup_home\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_rustup_home\" ] && mez_bootstrap_field env_manager \"rustup:$mez_rustup_home\"\n\
fi\n\
mez_cargo_home=${CARGO_HOME:-$HOME/.cargo}\n\
if [ -d \"$mez_cargo_home/bin\" ]; then\n\
  mez_cargo_bin=$(CDPATH= cd -P -- \"$mez_cargo_home/bin\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_cargo_bin\" ] && mez_bootstrap_field env_manager \"cargo-bin:$mez_cargo_bin\"\n\
fi\n\
mez_zig_bin=$(command -v zig 2>/dev/null || true)\n\
if [ -n \"$mez_zig_bin\" ] && [ -f \"$mez_zig_bin\" ] && [ ! -L \"$mez_zig_bin\" ]; then\n\
  mez_zig_root=$(CDPATH= cd -P -- \"$(dirname -- \"$mez_zig_bin\")\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_zig_root\" ] && [ \"$mez_zig_bin\" = \"$mez_zig_root/zig\" ] && mez_bootstrap_field env_manager \"zig:$mez_zig_root\"\n\
fi\n\
mez_go_bin=$(command -v go 2>/dev/null || true)\n\
if [ -n \"$mez_go_bin\" ] && [ -f \"$mez_go_bin\" ] && [ ! -L \"$mez_go_bin\" ]; then\n\
  mez_go_bin_dir=$(CDPATH= cd -P -- \"$(dirname -- \"$mez_go_bin\")\" 2>/dev/null && pwd -P)\n\
  mez_go_root=$(CDPATH= cd -P -- \"$mez_go_bin_dir/..\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_go_root\" ] && [ \"$mez_go_bin\" = \"$mez_go_root/bin/go\" ] && mez_bootstrap_field env_manager \"go:$mez_go_root\"\n\
fi\n\
mez_deno_bin=$(command -v deno 2>/dev/null || true)\n\
if [ -n \"$mez_deno_bin\" ] && [ -f \"$mez_deno_bin\" ] && [ ! -L \"$mez_deno_bin\" ]; then\n\
  mez_deno_root=$(CDPATH= cd -P -- \"$(dirname -- \"$mez_deno_bin\")\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_deno_root\" ] && [ \"$mez_deno_bin\" = \"$mez_deno_root/deno\" ] && mez_bootstrap_field env_manager \"deno:$mez_deno_root\"\n\
fi\n\
mez_bun_bin=$(command -v bun 2>/dev/null || true)\n\
if [ -n \"$mez_bun_bin\" ] && [ -f \"$mez_bun_bin\" ] && [ ! -L \"$mez_bun_bin\" ]; then\n\
  mez_bun_bin_dir=$(CDPATH= cd -P -- \"$(dirname -- \"$mez_bun_bin\")\" 2>/dev/null && pwd -P)\n\
  mez_bun_root=$(CDPATH= cd -P -- \"$mez_bun_bin_dir/..\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_bun_root\" ] && [ \"$mez_bun_bin\" = \"$mez_bun_root/bin/bun\" ] && mez_bootstrap_field env_manager \"bun:$mez_bun_root\"\n\
fi\n\
mez_node_bin=$(command -v node 2>/dev/null || true)\n\
if [ -n \"$mez_node_bin\" ] && [ -f \"$mez_node_bin\" ] && [ ! -L \"$mez_node_bin\" ]; then\n\
  mez_node_bin_dir=$(CDPATH= cd -P -- \"$(dirname -- \"$mez_node_bin\")\" 2>/dev/null && pwd -P)\n\
  mez_node_root=$(CDPATH= cd -P -- \"$mez_node_bin_dir/..\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_node_root\" ] && [ \"$mez_node_bin\" = \"$mez_node_root/bin/node\" ] && mez_bootstrap_field env_manager \"node-runtime:$mez_node_root\"\n\
fi\n\
mez_python_bin=$(command -v python3 2>/dev/null || true)\n\
if [ -n \"$mez_python_bin\" ] && [ -f \"$mez_python_bin\" ] && [ ! -L \"$mez_python_bin\" ]; then\n\
  mez_python_bin_dir=$(CDPATH= cd -P -- \"$(dirname -- \"$mez_python_bin\")\" 2>/dev/null && pwd -P)\n\
  mez_python_root=$(CDPATH= cd -P -- \"$mez_python_bin_dir/..\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_python_root\" ] && [ \"$mez_python_bin\" = \"$mez_python_root/bin/python3\" ] && mez_bootstrap_field env_manager \"python-runtime:$mez_python_root\"\n\
fi\n\
mez_jdk_bin=$(command -v javac 2>/dev/null || true)\n\
if [ -n \"$mez_jdk_bin\" ] && [ -f \"$mez_jdk_bin\" ] && [ ! -L \"$mez_jdk_bin\" ]; then\n\
  mez_jdk_bin_dir=$(CDPATH= cd -P -- \"$(dirname -- \"$mez_jdk_bin\")\" 2>/dev/null && pwd -P)\n\
  mez_jdk_root=$(CDPATH= cd -P -- \"$mez_jdk_bin_dir/..\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_jdk_root\" ] && [ \"$mez_jdk_bin\" = \"$mez_jdk_root/bin/javac\" ] && mez_bootstrap_field env_manager \"jdk-runtime:$mez_jdk_root\"\n\
fi\n\
	mez_dotnet_bin=$(command -v dotnet 2>/dev/null || true)\n\
	if [ -n \"$mez_dotnet_bin\" ] && [ -f \"$mez_dotnet_bin\" ] && [ ! -L \"$mez_dotnet_bin\" ]; then\n\
	  mez_dotnet_root=$(CDPATH= cd -P -- \"$(dirname -- \"$mez_dotnet_bin\")\" 2>/dev/null && pwd -P)\n\
	  [ -n \"$mez_dotnet_root\" ] && [ \"$mez_dotnet_bin\" = \"$mez_dotnet_root/dotnet\" ] && mez_bootstrap_field env_manager \"dotnet-sdk:$mez_dotnet_root\"\n\
	fi\n\
mez_dart_bin=$(command -v dart 2>/dev/null || true)\n\
if [ -n \"$mez_dart_bin\" ] && [ -f \"$mez_dart_bin\" ] && [ ! -L \"$mez_dart_bin\" ]; then\n\
  mez_dart_bin_dir=$(CDPATH= cd -P -- \"$(dirname -- \"$mez_dart_bin\")\" 2>/dev/null && pwd -P)\n\
  mez_dart_root=$(CDPATH= cd -P -- \"$mez_dart_bin_dir/..\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_dart_root\" ] && [ \"$mez_dart_bin\" = \"$mez_dart_root/bin/dart\" ] && mez_bootstrap_field env_manager \"dart-sdk:$mez_dart_root\"\n\
fi\n\
mez_kotlin_bin=$(command -v kotlinc 2>/dev/null || true)\n\
if [ -n \"$mez_kotlin_bin\" ] && [ -f \"$mez_kotlin_bin\" ] && [ ! -L \"$mez_kotlin_bin\" ]; then\n\
  mez_kotlin_bin_dir=$(CDPATH= cd -P -- \"$(dirname -- \"$mez_kotlin_bin\")\" 2>/dev/null && pwd -P)\n\
  mez_kotlin_root=$(CDPATH= cd -P -- \"$mez_kotlin_bin_dir/..\" 2>/dev/null && pwd -P)\n\
  [ -n \"$mez_kotlin_root\" ] && [ \"$mez_kotlin_bin\" = \"$mez_kotlin_root/bin/kotlinc\" ] && mez_bootstrap_field env_manager \"kotlin-jvm:$mez_kotlin_root\"\n\
fi\n\
\n\
mez_inst_max=32768\n\
mez_inst_cwd=\"$(pwd 2>/dev/null || printf '/')\"\n\
mez_inst_current=\"$mez_inst_cwd\"\n\
mez_inst_done=false\n\
while [ \"$mez_inst_done\" = \"false\" ]; do\n\
  if [ -f \"$mez_inst_current/AGENTS.md\" ]; then\n\
    mez_inst_file=\"$mez_inst_current/AGENTS.md\"\n\
    mez_inst_bytes=$(wc -c < \"$mez_inst_file\" 2>/dev/null | tr -d ' ')\n\
    [ -z \"$mez_inst_bytes\" ] && mez_inst_bytes=0\n\
    mez_inst_trunc=false; [ \"$mez_inst_bytes\" -gt \"$mez_inst_max\" ] && mez_inst_trunc=true\n\
    mez_inst_content=$(head -c \"$mez_inst_max\" \"$mez_inst_file\" 2>/dev/null | sed 's/\\\\/\\\\\\\\/g; s/\\t/\\\\t/g; s/\\r/\\\\r/g; s/$/\\\\n/' | tr -d '\\n')\n\
    printf 'instruction\\tpath=%s\\tscope=%s\\tbytes=%s\\ttruncated=%s\\tcontent=%s\\n' \"$mez_inst_file\" \"$mez_inst_current\" \"$mez_inst_bytes\" \"$mez_inst_trunc\" \"$mez_inst_content\"\n\
  fi\n\
  if [ \"$mez_inst_current\" = \"$mez_project_root\" ] || [ \"$mez_inst_current\" = \"/\" ] || [ -z \"$mez_project_root\" ]; then\n\
    mez_inst_done=true\n\
  else\n\
    mez_inst_current=$(dirname \"$mez_inst_current\" 2>/dev/null || printf '/')\n\
  fi\n\
done\n\
\n\
printf 'bootstrap\\tcomplete\\t%s\\n' \"$mez_discovered_at\"\n"
        .to_string();
    script.push_str(tool_discovery_script());
    script
}

/// Runs the fish bootstrap script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn fish_bootstrap_script() -> String {
    let mut script = "set -l mez_discovered_at (date +%s 2>/dev/null; or printf '0')\n\
function mez_bootstrap_field\n\
  set -l mez_key $argv[1]\n\
  set -l mez_value $argv[2]\n\
  printf 'env\\t%s\\t%s\\n' \"$mez_key\" \"$mez_value\"\n\
end\n\
\n\
mez_bootstrap_field os (uname -s 2>/dev/null; or printf 'unknown')\n\
mez_bootstrap_field arch (uname -m 2>/dev/null; or printf 'unknown')\n\
set -l mez_kernel (uname -r 2>/dev/null)\n\
if test -n \"$mez_kernel\"\n\
  mez_bootstrap_field kernel_version \"$mez_kernel\"\n\
end\n\
\n\
mez_bootstrap_field host (hostname 2>/dev/null; or printf 'unknown')\n\
mez_bootstrap_field user (whoami 2>/dev/null; or printf 'unknown')\n\
set -l mez_shell_path (status fish-path 2>/dev/null; or command -v fish 2>/dev/null; or printf '%s' \"$SHELL\")\n\
if test -z \"$mez_shell_path\"\n\
  set mez_shell_path \"$SHELL\"\n\
end\n\
mez_bootstrap_field shell_path \"$mez_shell_path\"\n\
mez_bootstrap_field shell_class fish\n\
set -l mez_shell_ver ($mez_shell_path --version 2>/dev/null | head -n 1)\n\
if test -n \"$mez_shell_ver\"\n\
  mez_bootstrap_field shell_version \"$mez_shell_ver\"\n\
end\n\
\n\
mez_bootstrap_field path \"$PATH\"\n\
set -l mez_cwd (pwd 2>/dev/null; or printf '/')\n\
mez_bootstrap_field cwd \"$mez_cwd\"\n\
\n\
set -l mez_project_root ''\n\
set -l mez_search_dir \"$mez_cwd\"\n\
while test -n \"$mez_search_dir\"; and test \"$mez_search_dir\" != '/'\n\
  if test -d \"$mez_search_dir/.git\"; or test -f \"$mez_search_dir/.git\"\n\
    set mez_project_root \"$mez_search_dir\"\n\
    break\n\
  end\n\
  set mez_search_dir (dirname \"$mez_search_dir\" 2>/dev/null; or printf '/')\n\
end\n\
mez_bootstrap_field project_root \"$mez_project_root\"\n\
if test -n \"$mez_project_root\"\n\
  mez_bootstrap_field git_repo 1\n\
else\n\
  mez_bootstrap_field git_repo 0\n\
end\n\
\n\
if test -f /proc/1/cgroup\n\
  set -l mez_container (grep -Eo 'docker|lxc|kubepods|libpod' /proc/1/cgroup 2>/dev/null | head -n 1)\n\
  if test -n \"$mez_container\"\n\
    mez_bootstrap_field container \"$mez_container\"\n\
  end\n\
else if test -f /.dockerenv\n\
  mez_bootstrap_field container docker\n\
end\n\
\n\
if test -n \"$VIRTUAL_ENV\"\n\
  mez_bootstrap_field env_manager \"virtualenv:$VIRTUAL_ENV\"\n\
end\n\
if test -n \"$CONDA_PREFIX\"\n\
  mez_bootstrap_field env_manager \"conda:$CONDA_PREFIX\"\n\
end\n\
if test -n \"$NIX_PROFILES\"\n\
  mez_bootstrap_field env_manager \"nix:$NIX_PROFILES\"\n\
end\n\
if test -n \"$NODE_VIRTUAL_ENV\"\n\
  mez_bootstrap_field env_manager \"node:$NODE_VIRTUAL_ENV\"\n\
end\n\
set -l mez_rustup_home \"$RUSTUP_HOME\"\n\
if test -z \"$mez_rustup_home\"\n\
  set mez_rustup_home \"$HOME/.rustup\"\n\
end\n\
if test -d \"$mez_rustup_home\"\n\
  set mez_rustup_home (cd \"$mez_rustup_home\" 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_rustup_home\"; and mez_bootstrap_field env_manager \"rustup:$mez_rustup_home\"\n\
end\n\
set -l mez_cargo_home \"$CARGO_HOME\"\n\
if test -z \"$mez_cargo_home\"\n\
  set mez_cargo_home \"$HOME/.cargo\"\n\
end\n\
if test -d \"$mez_cargo_home/bin\"\n\
  set -l mez_cargo_bin (cd \"$mez_cargo_home/bin\" 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_cargo_bin\"; and mez_bootstrap_field env_manager \"cargo-bin:$mez_cargo_bin\"\n\
end\n\
set -l mez_zig_bin (command -s zig 2>/dev/null)\n\
if test -n \"$mez_zig_bin\"; and test -f \"$mez_zig_bin\"; and not test -L \"$mez_zig_bin\"\n\
  set -l mez_zig_root (cd (dirname \"$mez_zig_bin\") 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_zig_root\"; and test \"$mez_zig_bin\" = \"$mez_zig_root/zig\"; and mez_bootstrap_field env_manager \"zig:$mez_zig_root\"\n\
end\n\
set -l mez_go_bin (command -s go 2>/dev/null)\n\
if test -n \"$mez_go_bin\"; and test -f \"$mez_go_bin\"; and not test -L \"$mez_go_bin\"\n\
  set -l mez_go_bin_dir (cd (dirname \"$mez_go_bin\") 2>/dev/null; and pwd -P)\n\
  set -l mez_go_root (cd \"$mez_go_bin_dir/..\" 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_go_root\"; and test \"$mez_go_bin\" = \"$mez_go_root/bin/go\"; and mez_bootstrap_field env_manager \"go:$mez_go_root\"\n\
end\n\
set -l mez_deno_bin (command -s deno 2>/dev/null)\n\
if test -n \"$mez_deno_bin\"; and test -f \"$mez_deno_bin\"; and not test -L \"$mez_deno_bin\"\n\
  set -l mez_deno_root (cd (dirname \"$mez_deno_bin\") 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_deno_root\"; and test \"$mez_deno_bin\" = \"$mez_deno_root/deno\"; and mez_bootstrap_field env_manager \"deno:$mez_deno_root\"\n\
end\n\
set -l mez_bun_bin (command -s bun 2>/dev/null)\n\
if test -n \"$mez_bun_bin\"; and test -f \"$mez_bun_bin\"; and not test -L \"$mez_bun_bin\"\n\
  set -l mez_bun_bin_dir (cd (dirname \"$mez_bun_bin\") 2>/dev/null; and pwd -P)\n\
  set -l mez_bun_root (cd \"$mez_bun_bin_dir/..\" 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_bun_root\"; and test \"$mez_bun_bin\" = \"$mez_bun_root/bin/bun\"; and mez_bootstrap_field env_manager \"bun:$mez_bun_root\"\n\
end\n\
set -l mez_node_bin (command -s node 2>/dev/null)\n\
if test -n \"$mez_node_bin\"; and test -f \"$mez_node_bin\"; and not test -L \"$mez_node_bin\"\n\
  set -l mez_node_bin_dir (cd (dirname \"$mez_node_bin\") 2>/dev/null; and pwd -P)\n\
  set -l mez_node_root (cd \"$mez_node_bin_dir/..\" 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_node_root\"; and test \"$mez_node_bin\" = \"$mez_node_root/bin/node\"; and mez_bootstrap_field env_manager \"node-runtime:$mez_node_root\"\n\
end\n\
set -l mez_python_bin (command -s python3 2>/dev/null)\n\
if test -n \"$mez_python_bin\"; and test -f \"$mez_python_bin\"; and not test -L \"$mez_python_bin\"\n\
  set -l mez_python_bin_dir (cd (dirname \"$mez_python_bin\") 2>/dev/null; and pwd -P)\n\
  set -l mez_python_root (cd \"$mez_python_bin_dir/..\" 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_python_root\"; and test \"$mez_python_bin\" = \"$mez_python_root/bin/python3\"; and mez_bootstrap_field env_manager \"python-runtime:$mez_python_root\"\n\
end\n\
set -l mez_jdk_bin (command -s javac 2>/dev/null)\n\
if test -n \"$mez_jdk_bin\"; and test -f \"$mez_jdk_bin\"; and not test -L \"$mez_jdk_bin\"\n\
  set -l mez_jdk_bin_dir (cd (dirname \"$mez_jdk_bin\") 2>/dev/null; and pwd -P)\n\
  set -l mez_jdk_root (cd \"$mez_jdk_bin_dir/..\" 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_jdk_root\"; and test \"$mez_jdk_bin\" = \"$mez_jdk_root/bin/javac\"; and mez_bootstrap_field env_manager \"jdk-runtime:$mez_jdk_root\"\n\
end\n\
	set -l mez_dotnet_bin (command -s dotnet 2>/dev/null)\n\
	if test -n \"$mez_dotnet_bin\"; and test -f \"$mez_dotnet_bin\"; and not test -L \"$mez_dotnet_bin\"\n\
	  set -l mez_dotnet_root (cd (dirname \"$mez_dotnet_bin\") 2>/dev/null; and pwd -P)\n\
	  test -n \"$mez_dotnet_root\"; and test \"$mez_dotnet_bin\" = \"$mez_dotnet_root/dotnet\"; and mez_bootstrap_field env_manager \"dotnet-sdk:$mez_dotnet_root\"\n\
	end\n\
set -l mez_dart_bin (command -s dart 2>/dev/null)\n\
if test -n \"$mez_dart_bin\"; and test -f \"$mez_dart_bin\"; and not test -L \"$mez_dart_bin\"\n\
  set -l mez_dart_bin_dir (cd (dirname \"$mez_dart_bin\") 2>/dev/null; and pwd -P)\n\
  set -l mez_dart_root (cd \"$mez_dart_bin_dir/..\" 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_dart_root\"; and test \"$mez_dart_bin\" = \"$mez_dart_root/bin/dart\"; and mez_bootstrap_field env_manager \"dart-sdk:$mez_dart_root\"\n\
end\n\
set -l mez_kotlin_bin (command -s kotlinc 2>/dev/null)\n\
if test -n \"$mez_kotlin_bin\"; and test -f \"$mez_kotlin_bin\"; and not test -L \"$mez_kotlin_bin\"\n\
  set -l mez_kotlin_bin_dir (cd (dirname \"$mez_kotlin_bin\") 2>/dev/null; and pwd -P)\n\
  set -l mez_kotlin_root (cd \"$mez_kotlin_bin_dir/..\" 2>/dev/null; and pwd -P)\n\
  test -n \"$mez_kotlin_root\"; and test \"$mez_kotlin_bin\" = \"$mez_kotlin_root/bin/kotlinc\"; and mez_bootstrap_field env_manager \"kotlin-jvm:$mez_kotlin_root\"\n\
end\n\
\n\
set -l mez_inst_max 32768\n\
set -l mez_inst_current \"$mez_cwd\"\n\
while true\n\
  if test -f \"$mez_inst_current/AGENTS.md\"\n\
    set -l mez_inst_file \"$mez_inst_current/AGENTS.md\"\n\
    set -l mez_inst_bytes (wc -c < \"$mez_inst_file\" 2>/dev/null | tr -d ' ')\n\
    if test -z \"$mez_inst_bytes\"\n\
      set mez_inst_bytes 0\n\
    end\n\
    set -l mez_inst_trunc false\n\
    if test \"$mez_inst_bytes\" -gt \"$mez_inst_max\"\n\
      set mez_inst_trunc true\n\
    end\n\
    set -l mez_inst_content (head -c \"$mez_inst_max\" \"$mez_inst_file\" 2>/dev/null | sed 's/\\\\/\\\\\\\\/g; s/\\t/\\\\t/g; s/\\r/\\\\r/g; s/$/\\\\n/' | tr -d '\\n')\n\
    printf 'instruction\\tpath=%s\\tscope=%s\\tbytes=%s\\ttruncated=%s\\tcontent=%s\\n' \"$mez_inst_file\" \"$mez_inst_current\" \"$mez_inst_bytes\" \"$mez_inst_trunc\" \"$mez_inst_content\"\n\
  end\n\
  if test \"$mez_inst_current\" = \"$mez_project_root\"; or test \"$mez_inst_current\" = '/'; or test -z \"$mez_project_root\"\n\
    break\n\
  end\n\
  set mez_inst_current (dirname \"$mez_inst_current\" 2>/dev/null; or printf '/')\n\
end\n\
\n\
printf 'bootstrap\\tcomplete\\t%s\\n' \"$mez_discovered_at\"\n"
        .to_string();
    script.push_str(fish_tool_discovery_script());
    script
}

/// Runs the fish tool discovery script operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn fish_tool_discovery_script() -> &'static str {
    "set -l mez_discovered_at (date +%s 2>/dev/null; or printf '0')\n\
function mez_probe_tool\n\
  set -l mez_tool $argv[1]\n\
  set -l mez_lookup_command \"command -v $mez_tool\"\n\
  set -l mez_path (command -v \"$mez_tool\" 2>/dev/null)\n\
  set -l mez_lookup_status $status\n\
  set -l mez_version ''\n\
  set -l mez_version_command ''\n\
  set -l mez_version_status ''\n\
  if test \"$mez_lookup_status\" -eq 0\n\
    set mez_version_command \"$mez_path --version\"\n\
    set -l mez_version_output ($mez_path --version 2>/dev/null | head -n 1)\n\
    set mez_version_status $status\n\
    set mez_version \"$mez_version_output\"\n\
  end\n\
  set -l mez_available 0\n\
  if test \"$mez_lookup_status\" -eq 0\n\
    set mez_available 1\n\
  end\n\
  printf 'tool\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\n' \"$mez_tool\" \"$mez_available\" \"$mez_path\" \"$mez_version\" \"$mez_lookup_command\" \"$mez_lookup_status\" \"$mez_version_command\" \"$mez_version_status\" \"$mez_discovered_at\"\n\
end\n\
for mez_tool in sed grep rg fd bat jq git\n\
  mez_probe_tool \"$mez_tool\"\n\
end\n\
set -l mez_python_path (command -v python3 2>/dev/null)\n\
set -l mez_python_lookup_status $status\n\
if test \"$mez_python_lookup_status\" -ne 0\n\
  set mez_python_path (command -v python 2>/dev/null)\n\
  set mez_python_lookup_status $status\n\
end\n\
set -l mez_python_version ''\n\
set -l mez_python_version_command ''\n\
set -l mez_python_version_status ''\n\
if test \"$mez_python_lookup_status\" -eq 0\n\
  set mez_python_version_command \"$mez_python_path --version\"\n\
  set -l mez_python_version_output ($mez_python_path --version 2>/dev/null | head -n 1)\n\
  set mez_python_version_status $status\n\
  set mez_python_version \"$mez_python_version_output\"\n\
end\n\
set -l mez_python_available 0\n\
if test \"$mez_python_lookup_status\" -eq 0\n\
  set mez_python_available 1\n\
end\n\
printf 'tool\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\t%s\\n' python \"$mez_python_available\" \"$mez_python_path\" \"$mez_python_version\" 'command -v python3 || command -v python' \"$mez_python_lookup_status\" \"$mez_python_version_command\" \"$mez_python_version_status\" \"$mez_discovered_at\"\n"
}

/// Runs the bootstrap script for classification operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn bootstrap_script_for_classification(classification: ShellClassification) -> String {
    if classification == ShellClassification::Fish {
        fish_bootstrap_script()
    } else {
        bootstrap_script()
    }
}

/// Runs the readiness probe command for classification operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn readiness_probe_command_for_classification(
    classification: ShellClassification,
) -> &'static str {
    if classification == ShellClassification::Fish {
        "true"
    } else {
        ":"
    }
}

/// Runs the parse bootstrap env output operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_bootstrap_env_output(
    output: &str,
    resolved_shell_path: &Path,
) -> (
    Option<EnvironmentSignature>,
    Option<ToolInventory>,
    Vec<DiscoveredInstructionFile>,
) {
    let mut os = String::new();
    let mut arch = String::new();
    let mut kernel_version: Option<String> = None;
    let mut host = String::new();
    let mut user = String::new();
    let mut shell_path = String::new();
    let mut shell_class: Option<String> = None;
    let mut shell_version: Option<String> = None;
    let mut path: Option<String> = None;
    let mut working_directory = String::new();
    let mut project_root: Option<String> = None;
    let mut git_repo = false;
    let mut container: Option<String> = None;
    let mut environment_managers: Vec<String> = Vec::new();
    let mut tool_output = String::new();
    let mut instruction_lines: Vec<String> = Vec::new();
    let mut in_tool_section = false;

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("tool\t") {
            in_tool_section = true;
        }
        if in_tool_section || line.starts_with("tool\t") {
            if !tool_output.is_empty() {
                tool_output.push('\n');
            }
            tool_output.push_str(line);
            continue;
        }
        if let Some(rest) = line.strip_prefix("instruction\t") {
            instruction_lines.push(rest.to_string());
            continue;
        }
        let Some((prefix, rest)) = line.split_once('\t') else {
            continue;
        };
        if prefix != "env" && prefix != "bootstrap" {
            continue;
        }
        let Some((key, value)) = rest.split_once('\t') else {
            continue;
        };
        match key {
            "os" => os = value.to_string(),
            "arch" => arch = value.to_string(),
            "kernel_version" => kernel_version = Some(value.to_string()),
            "host" => host = value.to_string(),
            "user" => user = value.to_string(),
            "shell_path" => shell_path = value.to_string(),
            "shell_class" => shell_class = Some(value.to_string()),
            "shell_version" => shell_version = Some(value.to_string()),
            "path" => path = Some(value.to_string()),
            "cwd" => working_directory = value.to_string(),
            "project_root" if !value.is_empty() => {
                project_root = Some(value.to_string());
            }
            "git_repo" => git_repo = value == "1",
            "container" => container = Some(value.to_string()),
            "env_manager" if !value.is_empty() => {
                environment_managers.push(value.to_string());
            }
            _ => {}
        }
    }

    environment_managers.sort();
    environment_managers.dedup();

    let shell_metadata_matches_resolved =
        shell_path.is_empty() || Path::new(&shell_path) == resolved_shell_path;
    if shell_path.is_empty() {
        shell_path = resolved_shell_path.to_string_lossy().into_owned();
    }
    let trusted_shell_version = shell_metadata_matches_resolved
        .then_some(shell_version.as_deref())
        .flatten();
    let trusted_shell_class = shell_metadata_matches_resolved
        .then_some(shell_class.as_deref())
        .flatten();
    let probe_classification = trusted_shell_version.and_then(classify_version_probe);
    let resolved_shell_classification =
        ShellClassification::classify_with_probe(resolved_shell_path, trusted_shell_version);
    let shell_classification = probe_classification
        .or_else(|| trusted_shell_class.map(ShellClassification::classify))
        .unwrap_or(resolved_shell_classification);

    let signature = if os.is_empty() && arch.is_empty() && host.is_empty() {
        None
    } else {
        if os.is_empty() {
            os = "unknown".to_string();
        }
        if arch.is_empty() {
            arch = "unknown".to_string();
        }
        if host.is_empty() {
            host = "unknown".to_string();
        }
        if user.is_empty() {
            user = "unknown".to_string();
        }
        if working_directory.is_empty() {
            working_directory = "/".to_string();
        }
        EnvironmentSignature::new(
            os,
            arch,
            kernel_version,
            host,
            user,
            shell_path,
            shell_classification,
            shell_version,
            path,
            working_directory,
            project_root,
            git_repo,
            container,
            environment_managers,
        )
        .ok()
    };

    let inventory = if tool_output.is_empty() {
        None
    } else {
        Some(ToolInventory::parse_bootstrap_output(&tool_output))
    };

    let instruction_files = if instruction_lines.is_empty() {
        Vec::new()
    } else {
        parse_instruction_discovery_output(&instruction_lines.join("\n")).unwrap_or_default()
    };

    (signature, inventory, instruction_files)
}
