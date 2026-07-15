//! Dependency-neutral project instruction discovery contracts.
//!
//! This module owns discovery configuration, command planning, the escaped
//! record consumed by the agent harness, and parsing shell-produced records.
//! Product crates retain pane-shell execution and filesystem side effects.

mod error;
mod planning;
mod types;

pub use error::{
    InstructionDiscoveryError, InstructionDiscoveryErrorKind, InstructionDiscoveryResult,
};
pub use planning::plan_instruction_discovery;
pub use types::{InstructionDiscoveryConfig, InstructionDiscoveryPlan};

/// One instruction file decoded from discovery command output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredInstructionFile {
    /// Path emitted by the discovery command.
    pub path: String,
    /// Directory scope where the instruction file was found.
    pub scope_root: String,
    /// Full file size reported by the shell command.
    pub bytes: usize,
    /// Whether content was truncated to the configured byte limit.
    pub truncated: bool,
    /// Escaped and decoded file content.
    pub content: String,
}

/// Parses escaped discovery command output into instruction files.
///
/// Empty lines are ignored. Records are sorted from parent scopes to deeper
/// child scopes so callers can apply broad guidance before narrow guidance.
pub fn parse_instruction_discovery_output(
    output: &str,
) -> InstructionDiscoveryResult<Vec<DiscoveredInstructionFile>> {
    let mut files = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_instruction_line)
        .collect::<InstructionDiscoveryResult<Vec<DiscoveredInstructionFile>>>()?;
    files.sort_by_key(|file| instruction_scope_depth(&file.scope_root));
    Ok(files)
}

fn parse_instruction_line(line: &str) -> InstructionDiscoveryResult<DiscoveredInstructionFile> {
    let fields = split_fields(line)?;
    let mut path = None;
    let mut scope = None;
    let mut bytes = None;
    let mut truncated = None;
    let mut content = None;
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            return Err(InstructionDiscoveryError::invalid_args(
                "instruction record field is malformed",
            ));
        };
        match key {
            "path" => path = Some(value.to_string()),
            "scope" => scope = Some(value.to_string()),
            "bytes" => {
                bytes = Some(value.parse::<usize>().map_err(|_| {
                    InstructionDiscoveryError::invalid_args("invalid instruction byte count")
                })?);
            }
            "truncated" => {
                truncated = Some(value.parse::<bool>().map_err(|_| {
                    InstructionDiscoveryError::invalid_args("invalid instruction truncated flag")
                })?);
            }
            "content" => content = Some(value.to_string()),
            _ => {}
        }
    }
    Ok(DiscoveredInstructionFile {
        path: path
            .ok_or_else(|| InstructionDiscoveryError::invalid_args("instruction path missing"))?,
        scope_root: scope
            .ok_or_else(|| InstructionDiscoveryError::invalid_args("instruction scope missing"))?,
        bytes: bytes
            .ok_or_else(|| InstructionDiscoveryError::invalid_args("instruction bytes missing"))?,
        truncated: truncated.ok_or_else(|| {
            InstructionDiscoveryError::invalid_args("instruction truncated flag missing")
        })?,
        content: content.ok_or_else(|| {
            InstructionDiscoveryError::invalid_args("instruction content missing")
        })?,
    })
}

fn split_fields(line: &str) -> InstructionDiscoveryResult<Vec<String>> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\t' => {
                fields.push(field);
                field = String::new();
            }
            '\\' => {
                let escaped = chars.next().ok_or_else(|| {
                    InstructionDiscoveryError::invalid_args("trailing instruction escape")
                })?;
                field.push(match escaped {
                    '\\' => '\\',
                    't' => '\t',
                    'n' => '\n',
                    'r' => '\r',
                    _ => {
                        return Err(InstructionDiscoveryError::invalid_args(
                            "unsupported instruction escape",
                        ));
                    }
                });
            }
            _ => field.push(ch),
        }
    }
    fields.push(field);
    Ok(fields)
}

fn instruction_scope_depth(scope: &str) -> usize {
    if scope == "." {
        return 0;
    }
    scope
        .trim_start_matches("./")
        .split('/')
        .filter(|part| !part.is_empty())
        .count()
}

#[cfg(test)]
mod parser_tests {
    use super::parse_instruction_discovery_output;

    #[test]
    /// Verifies escaped instruction records are decoded and ordered from broad
    /// repository scope to the narrower task scope consumed by the harness.
    fn instruction_discovery_parser_decodes_and_orders_records() {
        let output = "path=./nested/AGENTS.md\tscope=./nested\tbytes=5\ttruncated=false\tcontent=child\\n\n\
                      path=./AGENTS.md\tscope=.\tbytes=4\ttruncated=true\tcontent=root\\ttext";
        let files = parse_instruction_discovery_output(output).unwrap();

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].scope_root, ".");
        assert_eq!(files[0].content, "root\ttext");
        assert!(files[0].truncated);
        assert_eq!(files[1].scope_root, "./nested");
        assert_eq!(files[1].content, "child\n");
    }

    #[test]
    /// Verifies malformed instruction records fail at the agent contract
    /// boundary before product adapters attempt to use incomplete guidance.
    fn instruction_discovery_parser_rejects_malformed_records() {
        for output in [
            "scope=.\tbytes=1\ttruncated=false\tcontent=x",
            "path=x\tscope=.\tbytes=bad\ttruncated=false\tcontent=x",
            "path=x\tscope=.\tbytes=1\ttruncated=false\tcontent=bad\\q",
        ] {
            assert!(parse_instruction_discovery_output(output).is_err());
        }
    }
}

#[cfg(test)]
mod tests;
