//! Literal navigation arguments shared by native `/cd` ingress and completion.
//!
//! Quotes and backslashes group data; they never invoke a shell, expand variables,
//! substitute commands, glob, consult CDPATH or grant filesystem/process authority.
//! Options terminate at `--`. Runtime owners resolve home, validate directories,
//! gate native mode and busy state, and separately authorize shell replacement.

/// Stable syntax for navigation help and deterministic argument failures.
pub const CD_USAGE: &str = "usage: /cd [--sync-pane] [--] [directory]";

/// Requested navigation; parsing performs no filesystem or process operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CdOptions {
    /// Explicit one-shot request to replace the pane shell, subject to runtime guards.
    pub sync_pane: bool,
    /// Literal operand, or `None` for the runtime's resolved usable home.
    pub directory: Option<String>,
}

/// Parses zero/one literal operand and an optional unique `--sync-pane` flag.
/// `--` ends option parsing; quote delimiters are removed and backslash quotes
/// the next character outside single quotes. Leading `#`, `$`, backticks, globs
/// and shell operators are ordinary data. Unclosed quotes, trailing escapes,
/// empty/control-bearing operands, unknown/duplicate flags and extra operands
/// fail with the stable usage. `~user` shortcuts are unsupported; `./~user`
/// names a literal directory. An option-like directory requires `--` or `./`.
pub fn parse_cd_options(args: &str) -> Result<CdOptions, &'static str> {
    let words = literal_words(args)?;
    let mut options = CdOptions::default();
    let mut operands_only = false;
    for word in words {
        match word.as_str() {
            "--" if !operands_only => operands_only = true,
            "--sync-pane" if !operands_only && !options.sync_pane => options.sync_pane = true,
            _ if !operands_only && word.starts_with('-') => return Err(CD_USAGE),
            _ => {
                if word.is_empty()
                    || options.directory.is_some()
                    || (word.starts_with('~') && word != "~" && !word.starts_with("~/"))
                {
                    return Err(CD_USAGE);
                }
                options.directory = Some(word);
            }
        }
    }
    Ok(options)
}

/// Splits only the navigation grammar, retaining empty quoted tokens for rejection.
/// ASCII space/tab delimit outside quotes; control characters are never operands.
fn literal_words(args: &str) -> Result<Vec<String>, &'static str> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    for character in args.chars() {
        if character.is_control() && (character != '\t' || quote.is_some() || escaped) {
            return Err(CD_USAGE);
        }
        if escaped {
            word.push(character);
            escaped = false;
        } else if character == '\\' && quote != Some('\'') {
            escaped = true;
            started = true;
        } else if quote == Some(character) {
            quote = None;
        } else if quote.is_some() {
            word.push(character);
        } else if character == '\'' || character == '"' {
            quote = Some(character);
            started = true;
        } else if character == ' ' || character == '\t' {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(character);
            started = true;
        }
    }
    if quote.is_some() || escaped {
        return Err(CD_USAGE);
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Native navigation and its optional replacement request share one grammar.
    /// Flag order is immaterial before `--`; quoting and escaping preserve paths
    /// with spaces, Unicode and metacharacters as data without shell expansion.
    #[test]
    fn cd_options_preserve_literal_navigation_operands() {
        for (input, sync_pane, directory) in [
            ("", false, None),
            (" \t ", false, None),
            ("--sync-pane", true, None),
            ("~", false, Some("~")),
            ("~/work", false, Some("~/work")),
            ("../work", false, Some("../work")),
            ("--sync-pane /tmp/work", true, Some("/tmp/work")),
            ("/tmp/work --sync-pane", true, Some("/tmp/work")),
            ("'two spaces/雪'", false, Some("two spaces/雪")),
            ("two\\ spaces", false, Some("two spaces")),
            ("a' b'\" c\"", false, Some("a b c")),
            ("#directory", false, Some("#directory")),
            (
                "'$HOME/$(touch nope);*`id`'",
                false,
                Some("$HOME/$(touch nope);*`id`"),
            ),
            ("a;b&c|d*?", false, Some("a;b&c|d*?")),
            ("-- --sync-pane", false, Some("--sync-pane")),
            ("--sync-pane -- --sync-pane", true, Some("--sync-pane")),
            ("-- -", false, Some("-")),
            ("-- --", false, Some("--")),
            ("./-directory", false, Some("./-directory")),
            ("./~user", false, Some("./~user")),
        ] {
            assert_eq!(
                parse_cd_options(input),
                Ok(CdOptions {
                    sync_pane,
                    directory: directory.map(str::to_string)
                }),
                "{input:?}"
            );
        }
    }

    /// Malformed input never degrades into home navigation or a different path.
    /// Duplicate flags, unsupported shortcuts, extra operands and every embedded
    /// control are rejected before any runtime filesystem or shell operation.
    #[test]
    fn cd_options_reject_ambiguous_or_malformed_input() {
        for input in [
            "--sync-pane --sync-pane",
            "--unknown",
            "--sync-pane=yes",
            "-",
            "~user",
            "-- ~user/work",
            "one two",
            "-- one two",
            "''",
            "\"\"",
            "'unterminated",
            "\"unterminated",
            "trailing\\",
            "one\n",
            "one\r",
            "one\0",
            "one\u{7f}",
            "'one\tother'",
            "one\\\tother",
            "'one\u{1b}'",
            "-- --sync-pane --sync-pane",
        ] {
            assert_eq!(parse_cd_options(input), Err(CD_USAGE), "{input:?}");
        }
    }
}
