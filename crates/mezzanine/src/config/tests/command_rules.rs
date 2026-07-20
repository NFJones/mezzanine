//! Config command rules tests.

use super::*;

/// Verifies validates command rule schema in toml array tables.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn validates_command_rule_schema_in_toml_array_tables() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[[permissions.command_rules]]\npattern = [\"cargo\", \"test\"]\ndecision = \"allow\"\nscope = \"user\"\nmatch = \"prefix\"\njustification = \"test runner\"\n",
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies schema v21 accepts typed Bubblewrap authority and complete rule
/// effects without exposing raw backend arguments or inferring authority from
/// the command pattern.
#[test]
fn validates_bubblewrap_authority_and_complete_rule_effects() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        r#"version = 21
[permissions]
sandbox = "bubblewrap"
read_scopes = ["."]
write_scopes = ["target"]
network_policy = "deny"

[permissions.bubblewrap]
executable = "/usr/bin/bwrap"
unavailable = "fail"
network = "isolated"
environment = "minimal"

[[permissions.command_rules]]
id = "cargo-test"
pattern = ["cargo", "test"]
decision = "allow"
scope = "user"
match = "prefix"

[permissions.command_rules.effects]
completeness = "complete"
read_scopes = ["."]
write_scopes = ["target"]
network = false
credentials = false
process_control = false
"#,
        ConfigScope::Primary,
    );

    assert!(validation.valid, "{:?}", validation.diagnostics);
}

/// Verifies complete effects are an explicit allow-rule contract: every
/// boolean requirement must be present, and prompt/forbid rules cannot attach
/// effects that later sandbox compilation might mistake for granted authority.
#[test]
fn rejects_incomplete_or_non_allow_rule_effects() {
    let incomplete = validate_config_text(
        ConfigFormat::Toml,
        r#"version = 21
[[permissions.command_rules]]
id = "cargo-test"
pattern = ["cargo", "test"]
decision = "allow"

[permissions.command_rules.effects]
completeness = "complete"
read_scopes = ["."]
write_scopes = ["target"]
network = false
"#,
        ConfigScope::Primary,
    );
    assert!(!incomplete.valid);
    assert!(incomplete.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "permissions.command_rules.effects"
            && diagnostic.message.contains("credentials")
            && diagnostic.message.contains("process_control")
    }));

    let prompted = validate_config_text(
        ConfigFormat::Toml,
        r#"version = 21
[[permissions.command_rules]]
id = "network-command"
pattern = ["curl"]
decision = "prompt"

[permissions.command_rules.effects]
completeness = "unknown"
network = true
"#,
        ConfigScope::Primary,
    );
    assert!(!prompted.valid);
    assert!(prompted.diagnostics.iter().any(|diagnostic| {
        diagnostic.path == "permissions.command_rules.effects"
            && diagnostic.message.contains("allow rules")
    }));
}

/// Verifies command rule match examples must match rule.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_rule_match_examples_must_match_rule() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[[permissions.command_rules]]\npattern = [\"cargo\", \"test\"]\ndecision = \"allow\"\nscope = \"user\"\nmatch = \"prefix\"\nmatch_examples = [\"cargo build\"]\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.match_examples")
    );
}

/// Verifies command rule not match examples must not match rule.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn command_rule_not_match_examples_must_not_match_rule() {
    let validation = validate_config_text(
        ConfigFormat::Json,
        r#"{"permissions":{"command_rules":[{"pattern":["cargo","test"],"decision":"allow","scope":"user","match":"prefix","not_match_examples":["cargo test --all"]}]}}"#,
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.not_match_examples")
    );
}

/// Verifies exact sha256 command rule examples are validated.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn exact_sha256_command_rule_examples_are_validated() {
    let example = "printf 'ok\\n'";
    let example_toml = example.replace('\\', "\\\\");
    let digest = exact_command_sha256("unix-like", &normalize_exact_command_text(example, false));
    let valid = validate_config_text(
        ConfigFormat::Toml,
        &format!(
            "[[permissions.command_rules]]\npattern = [\"digest\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"exact_sha256\"\nexact_sha256 = \"{digest}\"\nshell_classification = \"unix-like\"\nmatch_examples = [\"{example_toml}\"]\nnot_match_examples = [\"printf other\"]\n"
        ),
        ConfigScope::Primary,
    );

    assert!(valid.valid, "{:?}", valid.diagnostics);

    let invalid = validate_config_text(
        ConfigFormat::Toml,
        &format!(
            "[[permissions.command_rules]]\npattern = [\"digest\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"exact_sha256\"\nexact_sha256 = \"{digest}\"\nshell_classification = \"unix-like\"\nmatch_examples = [\"printf other\"]\n"
        ),
        ConfigScope::Primary,
    );

    assert!(!invalid.valid);
}

/// Verifies rejects unknown command rule keys and values.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_unknown_command_rule_keys_and_values() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[[permissions.command_rules]]\npattern = [\"cargo\"]\ndecision = \"auto\"\nscope = \"built-in\"\nunknown = true\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.decision")
    );
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.scope")
    );
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.unknown")
    );
}

/// Verifies rejects invalid exact sha256 command rule metadata.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
#[test]
fn rejects_invalid_exact_sha256_command_rule_metadata() {
    let validation = validate_config_text(
        ConfigFormat::Toml,
        "[[permissions.command_rules]]\npattern = [\"digest\"]\ndecision = \"allow\"\nscope = \"session\"\nmatch = \"exact_sha256\"\nexact_sha256 = \"not-a-digest\"\nshell_classification = \"bad class\"\n",
        ConfigScope::Primary,
    );

    assert!(!validation.valid);
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.exact_sha256")
    );
    assert!(
        validation
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == "permissions.command_rules.shell_classification")
    );
}
