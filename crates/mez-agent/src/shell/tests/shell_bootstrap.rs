//! Agent tests for shell bootstrap behavior.
//!
//! This bounded leaf owns the scenarios for this concern while shared
//! fixtures remain in the parent module.

use super::*;

#[test]
/// Verifies the POSIX bootstrap retains generic pane discovery while omitting
/// named runtime and SDK installation probes.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn bootstrap_script_is_valid_shell() {
    let script = bootstrap_script();
    assert!(!script.is_empty());
    assert!(script.contains("uname -s"));
    assert!(script.contains("uname -m"));
    assert!(script.contains("hostname"));
    assert!(script.contains("whoami"));
    assert!(script.contains("home_directory"));
    assert!(script.contains("SHELL"));
    assert!(script.contains("PATH"));
    assert!(script.contains("pwd"));
    assert!(script.contains(".git"));
    assert!(script.contains("VIRTUAL_ENV"));
    assert!(script.contains("CONDA_PREFIX"));
    assert!(script.contains("NIX_PROFILES"));
    assert!(script.contains("NODE_VIRTUAL_ENV"));
    for forbidden in [
        "RUSTUP_HOME",
        "CARGO_HOME",
        "command -v zig",
        "command -v go",
        "command -v deno",
        "command -v bun",
        "command -v node",
        "command -v python3",
        "command -v javac",
        "command -v dotnet",
        "command -v dart",
        "command -v kotlinc",
        "command -v ruby",
        "command -v php",
        "command -v composer",
        "command -v erl",
        "command -v elixir",
        "command -v ghc",
        "command -v cabal",
        "command -v stack",
    ] {
        assert!(!script.contains(forbidden), "unexpected probe: {forbidden}");
    }
    assert!(script.contains("bootstrap"));
    assert!(script.contains("complete"));
    assert!(script.contains("AGENTS.md"));
    assert!(script.contains("mez_inst_"));
    assert!(script.contains("mez_probe_tool"));
    assert!(script.contains("tool\\t%s"));
}

#[test]
/// Verifies Fish bootstrap retains the generic pane protocol while omitting
/// the same named runtime and SDK discovery removed from POSIX bootstrap.
fn fish_bootstrap_script_emits_bootstrap_and_instruction_markers() {
    let script = bootstrap_script_for_classification(ShellClassification::Fish);

    assert!(script.contains("function mez_bootstrap_field"));
    assert!(script.contains("status fish-path"));
    assert!(script.contains("home_directory \"$HOME\""));
    assert!(script.contains("mez_bootstrap_field shell_class fish"));
    assert!(script.contains("AGENTS.md"));
    assert!(script.contains("instruction\\tpath=%s"));
    assert!(script.contains("bootstrap\\tcomplete"));
    assert!(script.contains("function mez_probe_tool"));
    assert!(script.contains("tool\\t%s"));
    assert!(script.contains("VIRTUAL_ENV"));
    assert!(script.contains("CONDA_PREFIX"));
    assert!(script.contains("NIX_PROFILES"));
    assert!(script.contains("NODE_VIRTUAL_ENV"));
    for forbidden in [
        "RUSTUP_HOME",
        "CARGO_HOME",
        "command -s zig",
        "command -s go",
        "command -s deno",
        "command -s bun",
        "command -s node",
        "command -s python3",
        "command -s javac",
        "command -s dotnet",
        "command -s dart",
        "command -s kotlinc",
        "command -s ruby",
        "command -s php",
        "command -s composer",
        "command -s erl",
        "command -s elixir",
        "command -s ghc",
        "command -s cabal",
        "command -s stack",
    ] {
        assert!(!script.contains(forbidden), "unexpected probe: {forbidden}");
    }
}

#[test]
/// Verifies that the bootstrap output parser extracts instruction files from
/// the synthetic bootstrap output emitted by instruction discovery shell code.
fn parse_bootstrap_env_output_extracts_instruction_files() {
    use std::path::Path;

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
bootstrap\tcomplete\t0\n\
instruction\tpath=./AGENTS.md\tscope=.\tbytes=12\ttruncated=false\tcontent=root guide\\n\n\
instruction\tpath=./src/AGENTS.md\tscope=./src\tbytes=7\ttruncated=false\tcontent=child\\n\n";

    let (_signature, _inventory, instruction_files) =
        parse_bootstrap_env_output(output, Path::new("/bin/sh"));

    assert_eq!(instruction_files.len(), 2);
    assert_eq!(instruction_files[0].path, "./AGENTS.md");
    assert_eq!(instruction_files[0].scope_root, ".");
    assert_eq!(instruction_files[0].content, "root guide\n");
    assert_eq!(instruction_files[1].path, "./src/AGENTS.md");
    assert_eq!(instruction_files[1].scope_root, "./src");
    assert_eq!(instruction_files[1].content, "child\n");
}

#[test]
/// Verifies parse bootstrap env output handles empty fields.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn parse_bootstrap_env_output_handles_empty_fields() {
    use std::path::Path;

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
bootstrap\tcomplete\t1714500000\n";

    let (signature, _inventory, _instruction_files) =
        parse_bootstrap_env_output(output, Path::new("/bin/sh"));

    let sig = signature.expect("signature should be parsed");
    assert_eq!(sig.os, "Linux");
    assert_eq!(sig.shell_classification, ShellClassification::PosixSh);
    assert_eq!(sig.shell_version, None);
    assert_eq!(sig.path, None);
    assert_eq!(sig.kernel_version, None);
    assert_eq!(sig.project_root, None);
    assert!(!sig.git_repo);
}

#[test]
/// Verifies that tool discovery lines in bootstrap output do not interfere
/// with instruction file extraction and that mixed output is parsed correctly.
fn parse_bootstrap_env_output_isolates_instructions_from_tools() {
    use std::path::Path;

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
instruction\tpath=./AGENTS.md\tscope=.\tbytes=6\ttruncated=false\tcontent=hello\\n\n\
tool\tsed\t1\t/usr/bin/sed\tGNU sed 4.9\tcommand -v sed\t0\t/usr/bin/sed --version\t0\t0\n\
tool\tgrep\t1\t/usr/bin/grep\tGNU grep 3.11\tcommand -v grep\t0\t/usr/bin/grep --version\t0\t0\n\
bootstrap\tcomplete\t0\n";

    let (_signature, inventory, instruction_files) =
        parse_bootstrap_env_output(output, Path::new("/bin/sh"));

    assert_eq!(instruction_files.len(), 1);
    assert_eq!(instruction_files[0].content, "hello\n");
    let inv = inventory.expect("tool inventory should be parsed");
    assert!(inv.sed);
    assert!(inv.grep);
}

#[test]
/// Verifies parse bootstrap env output parses complete synthetic output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn parse_bootstrap_env_output_parses_complete_synthetic_output() {
    use std::path::Path;

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\tkernel_version\t6.8.0-generic\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
env\tuser_id\t1000\n\
env\tprimary_group_id\t1000\n\
env\tactive_group\t1000:me\n\
env\tactive_group\t27:sudo\n\
env\tshell_path\t/bin/bash\n\
env\tshell_class\tbash\n\
env\tshell_version\tGNU bash, version 5.2.21\n\
env\tpath\t/usr/local/bin:/usr/bin:/bin\n\
env\tcwd\t/home/me/project\n\
env\tproject_root\t/home/me/project\n\
env\tgit_repo\t1\n\
env\tcontainer\tdocker\n\
env\tenv_manager\tvirtualenv:/home/me/.venv\n\
env\tenv_manager\trustup:/home/me/.rustup\n\
env\tenv_manager\tcargo-bin:/home/me/.cargo/bin\n\
bootstrap\tcomplete\t1714500000\n\
tool\tsed\t1\t/usr/bin/sed\tGNU sed 4.9\tcommand -v sed\t0\t/usr/bin/sed --version\t0\t1714500000\n\
tool\tgrep\t1\t/usr/bin/grep\tGNU grep 3.11\tcommand -v grep\t0\t/usr/bin/grep --version\t0\t1714500000\n";

    let (signature, inventory, instruction_files) =
        parse_bootstrap_env_output(output, Path::new("/bin/bash"));

    let sig = signature.expect("signature should be parsed");
    assert_eq!(sig.os, "Linux");
    assert_eq!(sig.arch, "x86_64");
    assert_eq!(sig.kernel_version.as_deref(), Some("6.8.0-generic"));
    assert_eq!(sig.host, "myhost");
    assert_eq!(sig.user, "me");
    assert_eq!(sig.user_id, Some(1000));
    assert_eq!(sig.primary_group_id, Some(1000));
    assert_eq!(
        sig.active_groups,
        vec![
            super::EnvironmentGroup {
                id: 27,
                name: "sudo".to_string(),
            },
            super::EnvironmentGroup {
                id: 1000,
                name: "me".to_string(),
            },
        ]
    );
    assert_eq!(sig.home_directory, None);
    assert_eq!(sig.shell_path, "/bin/bash");
    assert_eq!(sig.shell_classification, ShellClassification::Bash);
    assert_eq!(
        sig.shell_version.as_deref(),
        Some("GNU bash, version 5.2.21")
    );
    assert_eq!(sig.path.as_deref(), Some("/usr/local/bin:/usr/bin:/bin"));
    assert_eq!(sig.working_directory, "/home/me/project");
    assert_eq!(sig.project_root.as_deref(), Some("/home/me/project"));
    assert!(sig.git_repo);
    assert_eq!(sig.container.as_deref(), Some("docker"));
    assert_eq!(
        sig.environment_managers,
        vec![
            "cargo-bin:/home/me/.cargo/bin".to_string(),
            "rustup:/home/me/.rustup".to_string(),
            "virtualenv:/home/me/.venv".to_string()
        ]
    );

    let inv = inventory.expect("inventory should be parsed");
    assert!(inv.sed);
    assert!(inv.grep);

    assert!(
        instruction_files.is_empty(),
        "no instruction lines in synthetic output"
    );
}

#[test]
/// Verifies bootstrap parsing does not retain mismatched `$SHELL` metadata over
/// the resolved pane shell when constructing execution identity.
///
/// Async pane workers can fall back to `/bin/sh` even when the outer test or
/// launcher environment exports `SHELL=/bin/bash`. The bootstrap metadata still
/// records that environment shell, but runtime wrapper flags must stay aligned
/// with the actual resolved pane shell to avoid passing bash-only options to
/// `/bin/sh`.
fn parse_bootstrap_env_output_prefers_resolved_shell_for_mismatched_metadata() {
    use std::path::Path;

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\tmyhost\n\
env\tuser\tme\n\
env\tshell_path\t/bin/bash\n\
env\tshell_class\tbash\n\
env\tshell_version\tGNU bash, version 5.2.21\n\
env\tcwd\t/repo\n\
bootstrap\tcomplete\t1714500000\n";

    let (signature, _inventory, _instruction_files) =
        parse_bootstrap_env_output(output, Path::new("/bin/sh"));

    let sig = signature.expect("signature should be parsed");
    assert_eq!(sig.shell_path, "/bin/sh");
    assert_eq!(sig.shell_classification, ShellClassification::PosixSh);
    assert_eq!(sig.shell_version, None);
}

#[test]
/// Verifies parse bootstrap env output returns none for empty output.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn parse_bootstrap_env_output_returns_none_for_empty_output() {
    use std::path::Path;

    let (signature, inventory, instruction_files) =
        parse_bootstrap_env_output("", Path::new("/bin/sh"));
    assert!(signature.is_none());
    assert!(inventory.is_none());
    assert!(instruction_files.is_empty());
}

#[test]
/// Verifies shell classification as str matches spec.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn shell_classification_as_str_matches_spec() {
    assert_eq!(ShellClassification::Bash.as_str(), "bash");
    assert_eq!(ShellClassification::Zsh.as_str(), "zsh");
    assert_eq!(ShellClassification::Fish.as_str(), "fish");
    assert_eq!(ShellClassification::PosixSh.as_str(), "posix-sh");
    assert_eq!(ShellClassification::UnknownUnix.as_str(), "unknown-unix");
}

#[test]
/// Verifies shell classification classifies by binary name.
///
/// This regression scenario documents the behavior being protected so a
/// failure points at a concrete contract change rather than an incidental
/// implementation detail.
fn shell_classification_classifies_by_binary_name() {
    use std::path::Path;

    assert_eq!(
        ShellClassification::classify(Path::new("/bin/bash")),
        ShellClassification::Bash
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/usr/bin/zsh")),
        ShellClassification::Zsh
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/usr/local/bin/fish")),
        ShellClassification::Fish
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/bin/sh")),
        ShellClassification::PosixSh
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/bin/dash")),
        ShellClassification::PosixSh
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/usr/bin/ksh")),
        ShellClassification::PosixSh
    );
    assert_eq!(
        ShellClassification::classify(Path::new("/opt/custom-shell")),
        ShellClassification::UnknownUnix
    );
    assert_eq!(
        ShellClassification::classify(Path::new("")),
        ShellClassification::UnknownUnix
    );
}

#[test]
/// Verifies that shell version probe output wins over filename-derived
/// classification. The bootstrap parser receives both fields, and the probed
/// runtime shell identity is more authoritative than `$SHELL` basename text.
fn shell_classification_probe_takes_precedence_over_reported_name() {
    assert_eq!(
        ShellClassification::classify_with_probe(Path::new("/bin/sh"), Some("fish, version 3.7.1")),
        ShellClassification::Fish
    );

    let output = "env\tos\tLinux\n\
env\tarch\tx86_64\n\
env\thost\thost\n\
env\tuser\tuser\n\
env\tshell_path\t/bin/sh\n\
env\tshell_class\tsh\n\
env\tshell_version\tfish, version 3.7.1\n\
env\tcwd\t/repo\n\
env\tgit_repo\t0\n\
bootstrap\tcomplete\t0\n";
    let (signature, _, _) = parse_bootstrap_env_output(output, Path::new("/bin/sh"));
    let signature = signature.unwrap();

    assert_eq!(signature.shell_classification, ShellClassification::Fish);
}

#[test]
/// Verifies the pre-bootstrap identity probe keeps Fish/POSIX parsing limited
/// to one common simple command and derives renamed Fish from version evidence.
fn shell_identity_probe_is_syntax_neutral_and_parses_renamed_fish() {
    let marker = marker();
    let command =
        shell_identity_probe_command(marker.as_str(), "turn-1", "agent-1", "pane-1").unwrap();
    assert!(command.starts_with("/bin/sh -c "), "{command}");
    assert!(command.ends_with(" \"$SHELL\""), "{command}");

    let output = format!(
        "noise\n\u{1e}mez_shell_identity_begin={}\r\n\u{1e}mez_shell_path=/opt/custom-shell\r\n\u{1e}mez_shell_version=fish, version 3.7.1\r\n\u{1e}mez_shell_identity_end={}\r\n",
        marker.as_str(),
        marker.as_str()
    );
    let result = parse_shell_identity_probe_output(&output, marker.as_str())
        .unwrap()
        .unwrap();

    assert_eq!(result.shell_path, "/opt/custom-shell");
    assert_eq!(result.shell_classification, ShellClassification::Fish);
    assert_eq!(result.shell_version.as_deref(), Some("fish, version 3.7.1"));
}

#[test]
/// Verifies that runtime wrapper selection and bootstrap helpers choose Fish
/// native commands for Fish panes and POSIX commands for POSIX-like panes.
fn shell_classification_selects_matching_wrappers_and_probe_commands() {
    let transaction =
        ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/fish"), "true").unwrap();

    assert!(
        transaction
            .render_for_classification(ShellClassification::Fish)
            .contains("command env -u BASH_ENV -u ENV -u ZDOTDIR")
    );
    let posix = transaction.render_for_classification(ShellClassification::PosixSh);
    assert!(decoded_posix_wrapper_source(&posix).contains("env -u MEZ_MARKER_TOKEN"));
    let bash = ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/bash"), "true")
        .unwrap()
        .render_for_classification(ShellClassification::Bash);
    assert!(
        decoded_posix_wrapper_source(&bash)
            .contains("'/bin/bash' --noprofile --norc \"$MEZ_COMMAND_FILE\"")
    );
    let zsh = ShellTransaction::new(marker(), "t1", "a1", "p1", Path::new("/bin/zsh"), "true")
        .unwrap()
        .render_for_classification(ShellClassification::Zsh);
    assert!(decoded_posix_wrapper_source(&zsh).contains("'/bin/zsh' -f \"$MEZ_COMMAND_FILE\""));
    assert_eq!(
        readiness_probe_command_for_classification(ShellClassification::Fish),
        "true"
    );
    assert_eq!(
        readiness_probe_command_for_classification(ShellClassification::PosixSh),
        ":"
    );
    assert!(
        bootstrap_script_for_classification(ShellClassification::Fish)
            .contains("mez_bootstrap_field shell_class fish")
    );
    assert!(
        bootstrap_script_for_classification(ShellClassification::PosixSh)
            .contains("mez_bootstrap_field shell_class")
    );
}
