//! Tests for instruction discovery command planning and output parsing.

use std::{fs, path::PathBuf, process::Command};

use super::{
    InstructionDiscoveryConfig, parse_instruction_discovery_output, plan_instruction_discovery,
};

/// Verifies that discovery planning emits a pane-shell command rooted at the
/// project and walking from the task directory upward.
#[test]
fn instruction_discovery_plan_uses_pane_shell_relative_walk() {
    let plan = plan_instruction_discovery(
        "/repo",
        "/repo/src/main.rs",
        &InstructionDiscoveryConfig::default(),
    )
    .unwrap();

    assert_eq!(plan.project_root, PathBuf::from("/repo"));
    assert_eq!(plan.max_bytes, 32_768);
    assert!(plan.shell_command.contains("cd '/repo'"));
    assert!(plan.shell_command.contains("dir='./src'"));
    assert!(plan.shell_command.contains("'AGENTS.md'"));
    assert!(plan.shell_command.contains("head -c 32768"));
}

/// Verifies that the generated pane-shell discovery command selects at most
/// one instruction file in each directory. The filename list is ordered by
/// precedence, so `AGENTS.md` suppresses a same-directory fallback while a
/// child directory can still use the fallback when the preferred file is
/// absent.
#[test]
fn instruction_discovery_shell_plan_selects_one_file_per_directory_by_precedence() {
    let root =
        std::env::temp_dir().join(format!("mez-instruction-precedence-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("AGENTS.md"), "root agents").unwrap();
    fs::write(root.join("README.md"), "root readme").unwrap();
    fs::write(root.join("src").join("README.md"), "src readme").unwrap();

    let plan = plan_instruction_discovery(
        &root,
        root.join("src").join("main.rs"),
        &InstructionDiscoveryConfig {
            project_filenames: vec!["AGENTS.md".to_string(), "README.md".to_string()],
            max_bytes: 32_768,
            include_hidden_directories: false,
        },
    )
    .unwrap();
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&plan.shell_command)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    let files = parse_instruction_discovery_output(&stdout).unwrap();

    assert_eq!(files.len(), 2, "{stdout}");
    assert_eq!(files[0].path, "./AGENTS.md");
    assert_eq!(files[0].content, "root agents\n");
    assert_eq!(files[1].path, "./src/README.md");
    assert_eq!(files[1].content, "src readme\n");

    let _ = fs::remove_dir_all(root);
}

/// Verifies that discovery planning rejects task paths outside the configured
/// project root before a shell command can be created.
#[test]
fn instruction_discovery_rejects_paths_outside_project_root() {
    let error = plan_instruction_discovery(
        "/repo",
        "/other/src/main.rs",
        &InstructionDiscoveryConfig::default(),
    )
    .unwrap_err();

    assert_eq!(error.kind(), crate::error::MezErrorKind::InvalidArgs);
}

/// Verifies that escaped content from the shell command is decoded into the
/// text passed to the agent harness.
#[test]
fn instruction_discovery_output_parser_decodes_escaped_content() {
    let output =
        "path=./AGENTS.md\tscope=.\tbytes=12\ttruncated=false\tcontent=line one\\nline\\ttwo\n";

    let files = parse_instruction_discovery_output(output).unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "./AGENTS.md");
    assert_eq!(files[0].scope_root, ".");
    assert_eq!(files[0].content, "line one\nline\ttwo");
}

/// Verifies that parsed instruction files are ordered from parent scope to
/// nested child scopes for deterministic prompt assembly.
#[test]
fn instruction_discovery_output_parser_orders_parent_scopes_first() {
    let output = "\
path=./src/AGENTS.md\tscope=./src\tbytes=8\ttruncated=false\tcontent=child\\n
path=./AGENTS.md\tscope=.\tbytes=6\ttruncated=false\tcontent=root\\n
path=./src/bin/AGENTS.md\tscope=./src/bin\tbytes=13\ttruncated=false\tcontent=grandchild\\n
";

    let files = parse_instruction_discovery_output(output).unwrap();

    assert_eq!(files[0].scope_root, ".");
    assert_eq!(files[1].scope_root, "./src");
    assert_eq!(files[2].scope_root, "./src/bin");
}
