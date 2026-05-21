//! Config Parsers implementation.
//!
//! This module owns the config parsers boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{BTreeMap, clean_value};

// JSON path and value parsers used by config validation.

/// Carries Json Path Parser state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) struct JsonPathParser<'a> {
    /// Stores the chars value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) chars: std::iter::Peekable<std::str::Chars<'a>>,
    /// Stores the paths value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) paths: Vec<String>,
    /// Stores the stack value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) stack: Vec<String>,
}

/// Carries Json Value Parser state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
pub(super) struct JsonValueParser<'a> {
    /// Stores the chars value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) chars: std::iter::Peekable<std::str::Chars<'a>>,
    /// Stores the values value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) values: BTreeMap<String, String>,
    /// Stores the stack value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) stack: Vec<String>,
}

impl<'a> JsonValueParser<'a> {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn new(text: &'a str) -> Self {
        Self {
            chars: text.chars().peekable(),
            values: BTreeMap::new(),
            stack: Vec::new(),
        }
    }

    /// Runs the parse values operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse_values(&mut self) -> BTreeMap<String, String> {
        self.skip_ws();
        if self.peek() == Some('{') {
            self.parse_object();
        }
        std::mem::take(&mut self.values)
    }

    /// Runs the parse object operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse_object(&mut self) {
        self.consume('{');
        loop {
            self.skip_ws();
            match self.peek() {
                Some('}') => {
                    self.consume('}');
                    break;
                }
                Some('"') => {
                    let key = self.parse_string();
                    self.skip_ws();
                    if self.peek() != Some(':') {
                        self.skip_value();
                        continue;
                    }
                    self.consume(':');
                    self.stack.push(key);
                    self.skip_ws();
                    if self.peek() == Some('{') {
                        self.parse_object();
                    } else {
                        let value = self.parse_scalar_value();
                        self.values.insert(self.stack.join("."), value);
                    }
                    self.stack.pop();
                    self.skip_ws();
                    if self.peek() == Some(',') {
                        self.consume(',');
                    }
                }
                Some(_) => self.skip_value(),
                None => break,
            }
        }
    }

    /// Runs the parse scalar value operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse_scalar_value(&mut self) -> String {
        self.skip_ws();
        match self.peek() {
            Some('"') => self.parse_string(),
            Some('[') => self.take_balanced('[', ']'),
            Some('{') => self.take_balanced('{', '}'),
            _ => {
                let mut value = String::new();
                while let Some(ch) = self.peek() {
                    if matches!(ch, ',' | '}' | ']') {
                        break;
                    }
                    value.push(ch);
                    let _ = self.chars.next();
                }
                clean_value(&value)
            }
        }
    }

    /// Runs the parse string operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse_string(&mut self) -> String {
        let mut value = String::new();
        self.consume('"');
        let mut escaped = false;
        for ch in self.chars.by_ref() {
            if escaped {
                value.push(ch);
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => break,
                _ => value.push(ch),
            }
        }
        value
    }

    /// Runs the skip value operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn skip_value(&mut self) {
        self.skip_ws();
        match self.peek() {
            Some('"') => {
                let _ = self.parse_string();
            }
            Some('{') => {
                let _ = self.take_balanced('{', '}');
            }
            Some('[') => {
                let _ = self.take_balanced('[', ']');
            }
            _ => {
                while let Some(ch) = self.peek() {
                    if matches!(ch, ',' | '}' | ']') {
                        break;
                    }
                    let _ = self.chars.next();
                }
            }
        }
    }

    /// Runs the take balanced operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn take_balanced(&mut self, open: char, close: char) -> String {
        let mut output = String::new();
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for ch in self.chars.by_ref() {
            output.push(ch);
            if escaped {
                escaped = false;
                continue;
            }
            if in_string {
                match ch {
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                ch if ch == open => depth += 1,
                ch if ch == close => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        output
    }

    /// Runs the skip ws operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            let _ = self.chars.next();
        }
    }

    /// Runs the consume operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn consume(&mut self, expected: char) {
        if self.peek() == Some(expected) {
            let _ = self.chars.next();
        }
    }

    /// Runs the peek operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }
}

impl<'a> JsonPathParser<'a> {
    /// Runs the new operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn new(text: &'a str) -> Self {
        Self {
            chars: text.chars().peekable(),
            paths: Vec::new(),
            stack: Vec::new(),
        }
    }

    /// Runs the parse paths operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse_paths(&mut self) -> Vec<String> {
        self.skip_ws();
        if self.peek() == Some('{') {
            self.parse_object();
        }
        std::mem::take(&mut self.paths)
    }

    /// Runs the parse object operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse_object(&mut self) {
        self.consume('{');
        loop {
            self.skip_ws();
            match self.peek() {
                Some('}') => {
                    self.consume('}');
                    break;
                }
                Some('"') => {
                    let key = self.parse_string();
                    self.skip_ws();
                    if self.peek() != Some(':') {
                        self.skip_value();
                        continue;
                    }
                    self.consume(':');
                    self.stack.push(key);
                    self.paths.push(self.stack.join("."));
                    self.skip_ws();
                    if self.peek() == Some('{') {
                        self.parse_object();
                    } else {
                        self.skip_value();
                    }
                    self.stack.pop();
                    self.skip_ws();
                    if self.peek() == Some(',') {
                        self.consume(',');
                    }
                }
                Some(_) => self.skip_value(),
                None => break,
            }
        }
    }

    /// Runs the parse string operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn parse_string(&mut self) -> String {
        let mut value = String::new();
        self.consume('"');
        let mut escaped = false;
        for ch in self.chars.by_ref() {
            if escaped {
                value.push(ch);
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => break,
                _ => value.push(ch),
            }
        }
        value
    }

    /// Runs the skip value operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn skip_value(&mut self) {
        self.skip_ws();
        match self.peek() {
            Some('"') => {
                let _ = self.parse_string();
            }
            Some('{') => self.skip_balanced('{', '}'),
            Some('[') => self.skip_balanced('[', ']'),
            _ => {
                while let Some(ch) = self.peek() {
                    if matches!(ch, ',' | '}' | ']') {
                        break;
                    }
                    let _ = self.chars.next();
                }
            }
        }
    }

    /// Runs the skip balanced operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn skip_balanced(&mut self, open: char, close: char) {
        let mut depth = 0usize;
        let mut in_string = false;
        let mut escaped = false;
        for ch in self.chars.by_ref() {
            if escaped {
                escaped = false;
                continue;
            }
            if in_string {
                match ch {
                    '\\' => escaped = true,
                    '"' => in_string = false,
                    _ => {}
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                ch if ch == open => depth += 1,
                ch if ch == close => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    /// Runs the skip ws operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn skip_ws(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            let _ = self.chars.next();
        }
    }

    /// Runs the consume operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn consume(&mut self, expected: char) {
        if self.peek() == Some(expected) {
            let _ = self.chars.next();
        }
    }

    /// Runs the peek operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub(super) fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }
}
