//! Permissions Rules implementation.
//!
//! This module owns the permissions rules boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use std::fmt::Write as _;

use super::{
    ArgumentPolicy, CommandRule, CommandRuleScope, EffectiveCommandEffects, MezError, PathScopes,
    Result, RuleDecision, RuleMatch, analyze_shell, classify_tokens, tokenize_shell_words,
    validate_git_read_only_subcommand,
};

// Built-in rules, rule-store codec, and exact command hashing.

/// Runs the builtin rules operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn builtin_rules() -> Vec<CommandRule> {
    let git_status_options = ["--short", "--branch", "--porcelain", "--ignored"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let text_read_options = ["-n", "-v", "-i", "-H", "--line-number", "--ignore-case"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let script_read_options = ["-n", "-E", "-r", "--regexp-extended"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let listing_read_options = [
        "-1",
        "-a",
        "-A",
        "-C",
        "-d",
        "-F",
        "-G",
        "-h",
        "-l",
        "-m",
        "-p",
        "-q",
        "-r",
        "-R",
        "-s",
        "-S",
        "-t",
        "-u",
        "-x",
        "--all",
        "--almost-all",
        "--classify",
        "--color",
        "--directory",
        "--format",
        "--group-directories-first",
        "--human-readable",
        "--indicator-style",
        "--recursive",
        "--reverse",
        "--size",
        "--sort",
        "--time",
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect::<Vec<_>>();
    let type_options = ["-a", "-p", "-P", "-t"]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    let mut rules = Vec::new();
    push_rule(&mut rules, ["pwd"], RuleDecision::Allow, RuleMatch::Exact);
    push_rule(&mut rules, ["test"], RuleDecision::Allow, RuleMatch::Prefix);
    push_rule_with_policy(
        &mut rules,
        ["command", "-v"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ExecutableProbe {
            allowed_options: Vec::new(),
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["type"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ExecutableProbe {
            allowed_options: type_options,
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["which"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ExecutableProbe {
            allowed_options: vec!["-a".to_string()],
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["uname"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::UnameProbe,
    );
    push_rule(
        &mut rules,
        ["hostname"],
        RuleDecision::Allow,
        RuleMatch::Exact,
    );
    push_rule(&mut rules, ["env"], RuleDecision::Prompt, RuleMatch::Prefix);
    push_rule_with_policy(
        &mut rules,
        ["printf"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::LiteralOutput,
    );
    push_rule_with_policy(
        &mut rules,
        ["ls"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ReadPaths {
            allowed_options: listing_read_options,
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["cat"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ReadPaths {
            allowed_options: Vec::new(),
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["head"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ReadPaths {
            allowed_options: vec!["-n".to_string()],
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["tail"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ReadPaths {
            allowed_options: vec!["-n".to_string()],
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["wc"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ReadPaths {
            allowed_options: vec!["-l".to_string(), "-c".to_string(), "-w".to_string()],
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["grep"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ReadPaths {
            allowed_options: text_read_options.clone(),
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["rg"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ReadPaths {
            allowed_options: text_read_options,
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["sed"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ScriptThenReadPaths {
            allowed_options: script_read_options.clone(),
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["awk"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ScriptThenReadPaths {
            allowed_options: Vec::new(),
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["find"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::FindReadOnly,
    );
    push_rule(
        &mut rules,
        ["xargs"],
        RuleDecision::Prompt,
        RuleMatch::Prefix,
    );
    push_rule_with_policy(
        &mut rules,
        ["git", "status"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::ReadPaths {
            allowed_options: git_status_options,
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["git", "diff"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::GitReadOnly {
            subcommand: "diff".to_string(),
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["git", "log"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::GitReadOnly {
            subcommand: "log".to_string(),
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["git", "show"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::GitReadOnly {
            subcommand: "show".to_string(),
        },
    );
    push_rule_with_policy(
        &mut rules,
        ["git", "rev-parse"],
        RuleDecision::Allow,
        RuleMatch::Prefix,
        ArgumentPolicy::GitReadOnly {
            subcommand: "rev-parse".to_string(),
        },
    );
    rules
}

/// Runs the classify shell command operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn classify_shell_command(
    command: &str,
    scopes: Option<&PathScopes>,
) -> Result<Vec<EffectiveCommandEffects>> {
    let analysis = analyze_shell(command);
    if analysis.unsafe_syntax {
        return Ok(vec![EffectiveCommandEffects::unknown()]);
    }
    let mut effects = Vec::new();
    for candidate in analysis.candidates {
        let tokens = tokenize_shell_words(&candidate)
            .ok_or_else(|| MezError::invalid_args("unable to tokenize shell command"))?;
        effects.push(classify_tokens(&tokens, scopes));
    }
    Ok(effects)
}

/// Runs the push rule operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn push_rule<const N: usize>(
    rules: &mut Vec<CommandRule>,
    pattern: [&str; N],
    decision: RuleDecision,
    rule_match: RuleMatch,
) {
    if let Ok(rule) = CommandRule::new(pattern, decision, rule_match) {
        let rule = rule.with_scope(CommandRuleScope::BuiltIn);
        rules.push(rule);
    }
}

/// Runs the push rule with policy operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn push_rule_with_policy<const N: usize>(
    rules: &mut Vec<CommandRule>,
    pattern: [&str; N],
    decision: RuleDecision,
    rule_match: RuleMatch,
    argument_policy: ArgumentPolicy,
) {
    if let Ok(rule) = CommandRule::new(pattern, decision, rule_match) {
        rules.push(
            rule.with_argument_policy(argument_policy)
                .with_scope(CommandRuleScope::BuiltIn),
        );
    }
}

/// Runs the encode rule record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn encode_rule_record(rule: &CommandRule) -> String {
    let argument_policy = match &rule.argument_policy {
        ArgumentPolicy::None => "none".to_string(),
        ArgumentPolicy::ExecutableProbe { allowed_options } => {
            format!("executable_probe:{}", allowed_options.join(","))
        }
        ArgumentPolicy::UnameProbe => "uname_probe".to_string(),
        ArgumentPolicy::LiteralOutput => "literal_output".to_string(),
        ArgumentPolicy::ReadPaths { allowed_options } => {
            format!("read_paths:{}", allowed_options.join(","))
        }
        ArgumentPolicy::ScriptThenReadPaths { allowed_options } => {
            format!("script_then_read_paths:{}", allowed_options.join(","))
        }
        ArgumentPolicy::FindReadOnly => "find_read_only".to_string(),
        ArgumentPolicy::GitReadOnly { subcommand } => {
            format!("git_read_only:{subcommand}")
        }
    };
    [
        scope_name(rule.scope).to_string(),
        decision_name(rule.decision).to_string(),
        match_record_value(&rule.rule_match),
        argument_policy,
        rule.pattern.join("\u{1f}"),
        rule.justification.clone().unwrap_or_default(),
        rule.id.clone().unwrap_or_default(),
        rule.declared_effects
            .as_ref()
            .and_then(|effects| serde_json::to_string(effects).ok())
            .unwrap_or_default(),
    ]
    .into_iter()
    .map(|field| escape_record_field(&field))
    .collect::<Vec<_>>()
    .join("\t")
}

/// Runs the decode rule record operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn decode_rule_record(line: &str) -> Result<CommandRule> {
    let fields = split_record_fields(line);
    if !matches!(fields.len(), 6 | 8) {
        return Err(MezError::invalid_args(
            "command rule record has invalid field count",
        ));
    }
    let scope = parse_scope(&fields[0])?;
    let decision = parse_decision(&fields[1])?;
    let rule_match = parse_match(&fields[2])?;
    let argument_policy = parse_argument_policy(&fields[3])?;
    let pattern = fields[4].split('\u{1f}').collect::<Vec<_>>();
    let mut rule = CommandRule::new(pattern, decision, rule_match)?
        .with_argument_policy(argument_policy)
        .with_scope(scope);
    if !fields[5].is_empty() {
        rule = rule.with_justification(fields[5].clone());
    }
    if fields.len() == 8 {
        if !fields[6].is_empty() {
            rule = rule.with_id(fields[6].clone())?;
        }
        if !fields[7].is_empty() {
            let effects = serde_json::from_str(&fields[7]).map_err(|_| {
                MezError::invalid_args("command rule record has invalid declared effects")
            })?;
            rule = rule.with_declared_effects(effects)?;
        }
    }
    Ok(rule)
}

/// Runs the parse argument policy operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_argument_policy(value: &str) -> Result<ArgumentPolicy> {
    if value == "none" {
        return Ok(ArgumentPolicy::None);
    }
    if value == "find_read_only" {
        return Ok(ArgumentPolicy::FindReadOnly);
    }
    if value == "uname_probe" {
        return Ok(ArgumentPolicy::UnameProbe);
    }
    if value == "literal_output" {
        return Ok(ArgumentPolicy::LiteralOutput);
    }
    if let Some(options) = value.strip_prefix("executable_probe:") {
        return Ok(ArgumentPolicy::ExecutableProbe {
            allowed_options: split_policy_options(options),
        });
    }
    if let Some(options) = value.strip_prefix("read_paths:") {
        return Ok(ArgumentPolicy::ReadPaths {
            allowed_options: split_policy_options(options),
        });
    }
    if let Some(options) = value.strip_prefix("script_then_read_paths:") {
        return Ok(ArgumentPolicy::ScriptThenReadPaths {
            allowed_options: split_policy_options(options),
        });
    }
    if let Some(subcommand) = value.strip_prefix("git_read_only:") {
        validate_git_read_only_subcommand(subcommand)?;
        return Ok(ArgumentPolicy::GitReadOnly {
            subcommand: subcommand.to_string(),
        });
    }
    Err(MezError::invalid_args(
        "unknown command rule argument policy",
    ))
}

/// Runs the split policy options operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_policy_options(options: &str) -> Vec<String> {
    options
        .split(',')
        .filter(|option| !option.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Runs the scope name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn scope_name(scope: CommandRuleScope) -> &'static str {
    match scope {
        CommandRuleScope::BuiltIn => "built-in",
        CommandRuleScope::Session => "session",
        CommandRuleScope::Project => "project",
        CommandRuleScope::User => "user",
        CommandRuleScope::Managed => "managed",
    }
}

/// Runs the parse scope operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_scope(value: &str) -> Result<CommandRuleScope> {
    match value {
        "built-in" => Ok(CommandRuleScope::BuiltIn),
        "session" => Ok(CommandRuleScope::Session),
        "project" => Ok(CommandRuleScope::Project),
        "user" => Ok(CommandRuleScope::User),
        "managed" => Ok(CommandRuleScope::Managed),
        _ => Err(MezError::invalid_args("unknown command rule scope")),
    }
}

/// Runs the decision name operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn decision_name(decision: RuleDecision) -> &'static str {
    match decision {
        RuleDecision::Forbid => "deny",
        RuleDecision::Prompt => "prompt",
        RuleDecision::Allow => "allow",
    }
}

/// Runs the parse decision operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_decision(value: &str) -> Result<RuleDecision> {
    match value {
        "deny" | "forbid" => Ok(RuleDecision::Forbid),
        "prompt" => Ok(RuleDecision::Prompt),
        "allow" => Ok(RuleDecision::Allow),
        _ => Err(MezError::invalid_args("unknown command rule decision")),
    }
}

/// Runs the match record value operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn match_record_value(rule_match: &RuleMatch) -> String {
    match rule_match {
        RuleMatch::Prefix => "prefix".to_string(),
        RuleMatch::Exact => "exact".to_string(),
        RuleMatch::ExactSha256 {
            digest_hex,
            shell_classification,
        } => format!("exact_sha256:{shell_classification}:{digest_hex}"),
    }
}

/// Runs the parse match operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn parse_match(value: &str) -> Result<RuleMatch> {
    match value {
        "prefix" => Ok(RuleMatch::Prefix),
        "exact" => Ok(RuleMatch::Exact),
        value if value.starts_with("exact_sha256:") => {
            let mut parts = value.splitn(3, ':');
            let _kind = parts.next();
            let shell_classification = parts.next().unwrap_or_default().to_string();
            let digest_hex = parts.next().unwrap_or_default().to_string();
            validate_shell_classification(&shell_classification)?;
            validate_sha256_hex(&digest_hex)?;
            Ok(RuleMatch::ExactSha256 {
                digest_hex,
                shell_classification,
            })
        }
        _ => Err(MezError::invalid_args("unknown command rule match kind")),
    }
}

/// Runs the normalize exact command text operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn normalize_exact_command_text(input: &str, strip_submit_newline: bool) -> String {
    let mut normalized = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                let _ = chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(ch);
        }
    }
    if strip_submit_newline && normalized.ends_with('\n') {
        let _ = normalized.pop();
    }
    normalized
}

/// Runs the exact command sha256 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn exact_command_sha256(shell_classification: &str, normalized_command_text: &str) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"mez-command-sha256-v1\0");
    bytes.extend_from_slice(shell_classification.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(normalized_command_text.as_bytes());
    sha256_hex(&bytes)
}

/// Runs the validate shell classification operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_shell_classification(shell_classification: &str) -> Result<()> {
    if shell_classification.is_empty()
        || !shell_classification
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(MezError::invalid_args(
            "exact_sha256 shell classification must be non-empty ASCII [A-Za-z0-9._-]",
        ));
    }
    Ok(())
}

/// Runs the validate sha256 hex operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn validate_sha256_hex(digest_hex: &str) -> Result<()> {
    if digest_hex.len() != 64 || !digest_hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MezError::invalid_args(
            "exact_sha256 digest must be 64 hexadecimal characters",
        ));
    }
    Ok(())
}

/// Runs the sha256 hex operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn sha256_hex(input: &[u8]) -> String {
    let digest = sha256(input);
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// Runs the sha256 operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn sha256(input: &[u8]) -> [u8; 32] {
    /// Defines the K const used by this subsystem.
    ///
    /// Keeping this value documented makes the contract explicit at the module
    /// boundary and avoids relying on call-site inference.
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let mut h = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut message = input.to_vec();
    message.push(0x80);
    while (message.len() % 64) != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (index, word) in w.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7)
                ^ w[index - 15].rotate_right(18)
                ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17)
                ^ w[index - 2].rotate_right(19)
                ^ (w[index - 2] >> 10);
            w[index] = w[index - 16]
                .wrapping_add(s0)
                .wrapping_add(w[index - 7])
                .wrapping_add(s1);
        }

        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];

        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[index])
                .wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut digest = [0u8; 32];
    for (index, value) in h.into_iter().enumerate() {
        digest[index * 4..index * 4 + 4].copy_from_slice(&value.to_be_bytes());
    }
    digest
}

/// Runs the escape record field operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn escape_record_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

/// Runs the split record fields operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn split_record_fields(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            current.push(match ch {
                't' => '\t',
                'n' => '\n',
                other => other,
            });
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '\t' => {
                fields.push(std::mem::take(&mut current));
            }
            _ => current.push(ch),
        }
    }
    fields.push(current);
    fields
}
