//! Dependency-neutral project instruction discovery contracts.
//!
//! This module owns the escaped discovery record consumed by the agent harness
//! and the parser for shell-produced records. Filesystem traversal, shell
//! command construction, and command execution remain product responsibilities.

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
) -> Result<Vec<DiscoveredInstructionFile>, String> {
    let mut files = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_instruction_line)
        .collect::<Result<Vec<DiscoveredInstructionFile>, String>>()?;
    files.sort_by_key(|file| instruction_scope_depth(&file.scope_root));
    Ok(files)
}

fn parse_instruction_line(line: &str) -> Result<DiscoveredInstructionFile, String> {
    let fields = split_fields(line)?;
    let mut path = None;
    let mut scope = None;
    let mut bytes = None;
    let mut truncated = None;
    let mut content = None;
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            return Err("instruction record field is malformed".to_string());
        };
        match key {
            "path" => path = Some(value.to_string()),
            "scope" => scope = Some(value.to_string()),
            "bytes" => {
                bytes = Some(
                    value
                        .parse::<usize>()
                        .map_err(|_| "invalid instruction byte count".to_string())?,
                );
            }
            "truncated" => {
                truncated = Some(
                    value
                        .parse::<bool>()
                        .map_err(|_| "invalid instruction truncated flag".to_string())?,
                );
            }
            "content" => content = Some(value.to_string()),
            _ => {}
        }
    }
    Ok(DiscoveredInstructionFile {
        path: path.ok_or_else(|| "instruction path missing".to_string())?,
        scope_root: scope.ok_or_else(|| "instruction scope missing".to_string())?,
        bytes: bytes.ok_or_else(|| "instruction bytes missing".to_string())?,
        truncated: truncated.ok_or_else(|| "instruction truncated flag missing".to_string())?,
        content: content.ok_or_else(|| "instruction content missing".to_string())?,
    })
}

fn split_fields(line: &str) -> Result<Vec<String>, String> {
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
                let escaped = chars
                    .next()
                    .ok_or_else(|| "trailing instruction escape".to_string())?;
                field.push(match escaped {
                    '\\' => '\\',
                    't' => '\t',
                    'n' => '\n',
                    'r' => '\r',
                    _ => return Err("unsupported instruction escape".to_string()),
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
mod tests {
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
