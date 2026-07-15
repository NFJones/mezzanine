//! Permissions Classification implementation.
//!
//! This module owns the permissions classification boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{
    EffectiveCommandEffects, MezError, PathScopes, Result,
    paths::{path_in_read_scope, resolved_read_path},
};

// Shell candidate splitting, tokenization, and effect classification.

/// Carries Shell Analysis state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) struct ShellAnalysis {
    /// Stores the candidates value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) candidates: Vec<String>,
    /// Stores the unsafe syntax value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) unsafe_syntax: bool,
}

/// Runs the analyze shell operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn analyze_shell(command: &str) -> ShellAnalysis {
    let mut candidates = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote = QuoteState::Ground;
    let mut unsafe_syntax = false;

    while let Some(ch) = chars.next() {
        match quote {
            QuoteState::Ground => match ch {
                '\'' => {
                    quote = QuoteState::Single;
                    current.push(ch);
                }
                '"' => {
                    quote = QuoteState::Double;
                    current.push(ch);
                }
                '\\' => {
                    current.push(ch);
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                ';' | '\n' => push_candidate(&mut candidates, &mut current),
                '|' => {
                    if chars.peek() == Some(&'|') {
                        let _ = chars.next();
                    }
                    push_candidate(&mut candidates, &mut current);
                }
                '&' => {
                    if chars.peek() == Some(&'&') {
                        let _ = chars.next();
                    }
                    push_candidate(&mut candidates, &mut current);
                }
                '<' | '>' | '`' | '$' | '(' | ')' => {
                    unsafe_syntax = true;
                    current.push(ch);
                }
                _ => current.push(ch),
            },
            QuoteState::Single => {
                if ch == '\'' {
                    quote = QuoteState::Ground;
                }
                current.push(ch);
            }
            QuoteState::Double => {
                if ch == '"' {
                    quote = QuoteState::Ground;
                } else if ch == '$' || ch == '`' {
                    unsafe_syntax = true;
                }
                current.push(ch);
            }
        }
    }

    if quote != QuoteState::Ground {
        unsafe_syntax = true;
    }
    push_candidate(&mut candidates, &mut current);

    ShellAnalysis {
        candidates,
        unsafe_syntax,
    }
}

/// Carries Quote State state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QuoteState {
    /// Represents the Ground case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Ground,
    /// Represents the Single case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Single,
    /// Represents the Double case for this enumeration.
    ///
    /// Callers use this variant to describe one explicit state or command path
    /// without relying on stringly typed status values.
    Double,
}

/// Runs the push candidate operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn push_candidate(candidates: &mut Vec<String>, current: &mut String) {
    let candidate = current.trim();
    if !candidate.is_empty() {
        candidates.push(candidate.to_string());
    }
    current.clear();
}

/// Runs the tokenize single candidate operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn tokenize_single_candidate(command: &str) -> Option<Vec<String>> {
    let analysis = analyze_shell(command);
    if analysis.unsafe_syntax || analysis.candidates.len() != 1 {
        return None;
    }
    tokenize_shell_words(&analysis.candidates[0])
}

/// Runs the tokenize shell words operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn tokenize_shell_words(command: &str) -> Option<Vec<String>> {
    let tokens = shlex::split(command)?;
    (!tokens.is_empty()).then_some(tokens)
}

/// Runs the remaining args are read paths operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn remaining_args_are_read_paths(
    remaining: &[String],
    allowed_options: &[String],
    scopes: Option<&PathScopes>,
) -> bool {
    let mut paths_only = false;
    let mut index = 0;
    while index < remaining.len() {
        let token = &remaining[index];
        if token == "--" {
            paths_only = true;
            index += 1;
            continue;
        }
        if !paths_only && token.starts_with('-') {
            let option_name = token.split_once('=').map(|(name, _)| name).unwrap_or(token);
            if !read_path_option_is_allowed(option_name, allowed_options) {
                return false;
            }
            index += 1;
            continue;
        }
        if token_has_shell_syntax(token) || !path_in_read_scope(token, scopes) {
            return false;
        }
        index += 1;
    }
    true
}

/// Runs the read path option is allowed operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn read_path_option_is_allowed(option: &str, allowed_options: &[String]) -> bool {
    if allowed_options.iter().any(|allowed| allowed == option) {
        return true;
    }
    if option.starts_with("--") || !option.starts_with('-') || option.len() <= 2 {
        return false;
    }
    option[1..].chars().all(|flag| {
        let short = format!("-{flag}");
        allowed_options.iter().any(|allowed| allowed == &short)
    })
}

/// Runs the remaining args are script then read paths operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn remaining_args_are_script_then_read_paths(
    remaining: &[String],
    allowed_options: &[String],
    scopes: Option<&PathScopes>,
) -> bool {
    let mut index = 0;
    while index < remaining.len() {
        let token = &remaining[index];
        if token == "--" {
            index += 1;
            break;
        }
        if !token.starts_with('-') {
            break;
        }
        let option_name = token.split_once('=').map(|(name, _)| name).unwrap_or(token);
        if option_name == "-i" || option_name.starts_with("--in-place") {
            return false;
        }
        if !allowed_options.iter().any(|allowed| allowed == option_name) {
            return false;
        }
        index += 1;
    }
    if index >= remaining.len() {
        return false;
    }
    index += 1;
    remaining_args_are_read_paths(&remaining[index..], &[], scopes)
}

/// Runs the find args are read only operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn find_args_are_read_only(remaining: &[String], scopes: Option<&PathScopes>) -> bool {
    let dangerous = [
        "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fls", "-fprint", "-fprintf",
    ];
    if remaining
        .iter()
        .any(|token| dangerous.contains(&token.as_str()) || token_has_shell_syntax(token))
    {
        return false;
    }

    let mut index = 0;
    while index < remaining.len() && !remaining[index].starts_with('-') {
        if !path_in_read_scope(&remaining[index], scopes) {
            return false;
        }
        index += 1;
    }

    while index < remaining.len() {
        let token = remaining[index].as_str();
        match token {
            "-print" | "-print0" | "-ls" | "-not" | "!" => index += 1,
            "-maxdepth" | "-mindepth" | "-name" | "-iname" | "-path" | "-ipath" | "-type" => {
                let Some(value) = remaining.get(index + 1) else {
                    return false;
                };
                if token_has_shell_syntax(value) {
                    return false;
                }
                index += 2;
            }
            _ => return false,
        }
    }

    true
}

/// Runs the literal output args are safe operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn literal_output_args_are_safe(remaining: &[String]) -> bool {
    if remaining.is_empty() {
        return false;
    }
    let remaining = if remaining.first().is_some_and(|token| token == "--") {
        &remaining[1..]
    } else {
        remaining
    };
    !remaining.is_empty() && remaining.iter().all(|token| !token_has_shell_syntax(token))
}

/// Runs the git read only args are safe operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn git_read_only_args_are_safe(
    subcommand: &str,
    remaining: &[String],
    scopes: Option<&PathScopes>,
) -> bool {
    if validate_git_read_only_subcommand(subcommand).is_err() {
        return false;
    }
    let mut pathspecs_only = false;
    for token in remaining {
        if token_has_shell_syntax(token) {
            return false;
        }
        if pathspecs_only {
            if !path_in_read_scope(token, scopes) {
                return false;
            }
            continue;
        }
        if token == "--" {
            pathspecs_only = true;
            continue;
        }
        if git_read_only_arg_is_blocked(subcommand, token) {
            return false;
        }
    }
    true
}

/// Runs the validate git read only subcommand operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_git_read_only_subcommand(subcommand: &str) -> Result<()> {
    match subcommand {
        "diff" | "log" | "show" | "rev-parse" => Ok(()),
        _ => Err(MezError::invalid_args(
            "unknown git read-only command rule subcommand",
        )),
    }
}

/// Runs the git read only arg is blocked operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn git_read_only_arg_is_blocked(_subcommand: &str, token: &str) -> bool {
    matches!(
        token,
        "-o" | "--output" | "-p" | "--paginate" | "--ext-diff" | "--external" | "--textconv"
    ) || token.starts_with("--output=")
        || token.starts_with("--paginate=")
        || token.starts_with("--ext-diff=")
        || token.starts_with("--external=")
        || token.starts_with("--textconv=")
}

/// Runs the token has shell syntax operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn token_has_shell_syntax(token: &str) -> bool {
    token.contains(';')
        || token.contains('|')
        || token.contains('&')
        || token.contains('$')
        || token.contains('`')
        || token.contains('<')
        || token.contains('>')
}

/// Runs the remaining args are executable probes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn remaining_args_are_executable_probes(
    remaining: &[String],
    allowed_options: &[String],
) -> bool {
    let mut index = 0usize;
    let mut names_started = false;
    while index < remaining.len() {
        let token = &remaining[index];
        if token == "--" && !names_started {
            names_started = true;
            index += 1;
            continue;
        }
        if !names_started && token.starts_with('-') {
            let option_name = token.split_once('=').map(|(name, _)| name).unwrap_or(token);
            if !allowed_options.iter().any(|allowed| allowed == option_name) {
                return false;
            }
            index += 1;
            continue;
        }
        names_started = true;
        if !is_executable_probe_name(token) {
            return false;
        }
        index += 1;
    }
    names_started
}

/// Runs the is executable probe name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn is_executable_probe_name(token: &str) -> bool {
    !token.is_empty()
        && !token.starts_with('-')
        && !token_has_shell_syntax(token)
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+'))
}

/// Runs the uname args are safe operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn uname_args_are_safe(remaining: &[String]) -> bool {
    remaining.iter().all(|token| {
        if token_has_shell_syntax(token) {
            return false;
        }
        match token.as_str() {
            "-a"
            | "--all"
            | "-s"
            | "--kernel-name"
            | "-n"
            | "--nodename"
            | "-r"
            | "--kernel-release"
            | "-v"
            | "--kernel-version"
            | "-m"
            | "--machine"
            | "-p"
            | "--processor"
            | "-i"
            | "--hardware-platform"
            | "-o"
            | "--operating-system" => true,
            value if value.starts_with('-') && value.len() > 2 => value[1..].bytes().all(|byte| {
                matches!(
                    byte,
                    b'a' | b's' | b'n' | b'r' | b'v' | b'm' | b'p' | b'i' | b'o'
                )
            }),
            _ => false,
        }
    })
}

/// Runs the classify tokens operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn classify_tokens(
    tokens: &[String],
    scopes: Option<&PathScopes>,
) -> EffectiveCommandEffects {
    if tokens.is_empty() {
        return EffectiveCommandEffects::unknown();
    }

    let command = tokens[0].as_str();
    let mut effects = EffectiveCommandEffects::empty();
    match command {
        "pwd" | "test" | "command" | "type" | "which" | "uname" | "hostname" | "printf" => effects,
        "ls" => {
            effects.reads = collect_listing_read_path_args(&tokens[1..], scopes);
            effects.unknown = effects.reads.iter().any(|path| path == "<unknown>");
            effects
        }
        "cat" | "head" | "tail" | "wc" | "grep" | "rg" => {
            effects.reads = collect_read_path_args(&tokens[1..], scopes);
            effects.unknown = effects.reads.iter().any(|path| path == "<unknown>");
            effects
        }
        "sed" | "awk" => {
            effects.reads = collect_script_command_read_path_args(&tokens[1..], scopes);
            effects.unknown = effects.reads.iter().any(|path| path == "<unknown>");
            effects
        }
        "find" => {
            effects.reads = collect_find_root_args(&tokens[1..], scopes);
            effects.unknown = effects.reads.iter().any(|path| path == "<unknown>");
            effects
        }
        "git"
            if matches!(
                tokens.get(1).map(String::as_str),
                Some("status" | "diff" | "log" | "show" | "rev-parse")
            ) =>
        {
            effects.reads = vec![
                scopes
                    .map(|scope| scope.current_directory.clone())
                    .unwrap_or_else(|| ".".to_string()),
            ];
            effects
        }
        "rm" => {
            effects.destructive = true;
            effects.deletes = tokens[1..].to_vec();
            effects
        }
        "sudo" | "su" => {
            effects.privilege_change = true;
            effects.unknown = true;
            effects
        }
        "kill" => {
            effects.process_control = true;
            effects.destructive = true;
            effects
        }
        "env" | "xargs" => EffectiveCommandEffects::unknown(),
        "sh" | "bash" | "zsh" | "fish" | "python" | "python3" | "ruby" | "perl" | "node"
        | "make" | "npm" | "pnpm" | "yarn" | "cargo" => EffectiveCommandEffects::unknown(),
        _ => EffectiveCommandEffects::unknown(),
    }
}

impl EffectiveCommandEffects {
    /// Runs the empty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn empty() -> Self {
        Self {
            reads: Vec::new(),
            writes: Vec::new(),
            creates: Vec::new(),
            deletes: Vec::new(),
            touches: Vec::new(),
            network: false,
            credentials: false,
            process_control: false,
            destructive: false,
            privilege_change: false,
            unknown: false,
        }
    }

    /// Runs the unknown operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn unknown() -> Self {
        let mut effects = Self::empty();
        effects.unknown = true;
        effects
    }
}

/// Runs the collect read path args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn collect_read_path_args(args: &[String], scopes: Option<&PathScopes>) -> Vec<String> {
    args.iter()
        .filter(|arg| !arg.starts_with('-'))
        .map(|arg| resolved_read_path(arg, scopes).unwrap_or_else(|| "<unknown>".to_string()))
        .collect()
}

/// Runs the collect listing read path args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn collect_listing_read_path_args(
    args: &[String],
    scopes: Option<&PathScopes>,
) -> Vec<String> {
    let mut paths = Vec::new();
    let mut paths_only = false;
    for arg in args {
        if !paths_only && arg == "--" {
            paths_only = true;
            continue;
        }
        if !paths_only && arg.starts_with('-') {
            continue;
        }
        paths.push(arg.as_str());
    }
    if paths.is_empty() {
        paths.push(".");
    }
    paths
        .into_iter()
        .map(|path| {
            if is_current_directory_reference(path) {
                ".".to_string()
            } else {
                resolved_read_path(path, scopes).unwrap_or_else(|| "<unknown>".to_string())
            }
        })
        .collect()
}

/// Runs the is current directory reference operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn is_current_directory_reference(path: &str) -> bool {
    path == "." || path.trim_end_matches('/') == "."
}

/// Runs the collect script command read path args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn collect_script_command_read_path_args(
    args: &[String],
    scopes: Option<&PathScopes>,
) -> Vec<String> {
    let mut index = 0;
    while index < args.len() && args[index].starts_with('-') {
        index += 1;
    }
    if index < args.len() {
        index += 1;
    }
    collect_read_path_args(&args[index..], scopes)
}

/// Runs the collect find root args operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn collect_find_root_args(args: &[String], scopes: Option<&PathScopes>) -> Vec<String> {
    let roots = args
        .iter()
        .take_while(|arg| !arg.starts_with('-'))
        .cloned()
        .collect::<Vec<_>>();
    let roots = if roots.is_empty() {
        vec![".".to_string()]
    } else {
        roots
    };
    roots
        .iter()
        .map(|root| resolved_read_path(root, scopes).unwrap_or_else(|| "<unknown>".to_string()))
        .collect()
}
