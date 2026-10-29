//! Bounded exact-member edits in JSON-with-comments, retaining authored bytes.
//!
//! A small token tree admits JSON plus line/block comments and trailing commas,
//! never JSON5 expressions or duplicate decoded keys. Existing strict-array logic
//! decides ownership/equality/order/capacity; this module edits only owned value
//! tokens, necessary commas, or one new field/member. Comments and unrelated
//! whitespace remain byte-exact. Every result is reparsed and compared with the
//! expected semantic document before it can reach descriptor-safe publication.

use super::reconciliation::{Artifact, pointer_parts, reconcile_array};
use crate::error::{MezError, Result};
use serde_json::Value;
use std::collections::BTreeSet;
use std::ops::Range;

mod boolean;

/// Shares the same bounded duplicate-refusing parser with default selection.
pub(super) fn document(bytes: &[u8]) -> Result<Value> {
    Ok(parse(bytes)?.value())
}

/// Reconciles only an exact compiled boolean leaf, preserving authored trivia.
pub(super) fn reconcile_boolean(
    current: Option<&[u8]>,
    previous: Option<&Artifact>,
    desired: Option<&Artifact>,
) -> Result<Option<Vec<u8>>> {
    boolean::reconcile(current, previous, desired)
}

/// One bounded token value and its exact original byte interval.
struct Node {
    span: Range<usize>,
    kind: Kind,
}
/// Values retain only bounded structural metadata needed for exact edits.
enum Kind {
    Scalar(Value),
    Object(Vec<(String, Node)>, Vec<usize>),
    Array(Vec<Node>, Vec<usize>),
}
impl Node {
    /// Builds semantic data only for ownership comparison, never output rewriting.
    fn value(&self) -> Value {
        match &self.kind {
            Kind::Scalar(value) => value.clone(),
            Kind::Object(fields, _) => Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| (key.clone(), value.value()))
                    .collect(),
            ),
            Kind::Array(values, _) => Value::Array(values.iter().map(Node::value).collect()),
        }
    }
    /// Resolves only object ancestors; unknown/wrong-type parents remain conflicts.
    fn parent(&self, parts: &[String]) -> Result<&Node> {
        let mut node = self;
        for key in parts {
            let Kind::Object(fields, _) = &node.kind else {
                return Err(MezError::conflict("bootstrap JSONC parent must be object"));
            };
            node = fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, node)| node)
                .ok_or_else(|| MezError::conflict("bootstrap JSONC parent unavailable"))?;
        }
        Ok(node)
    }
}
/// Finite byte/token/depth reader; errors never contain authored content.
struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
    nodes: usize,
}
impl Parser<'_> {
    /// Consumes whitespace/comments without changing or evaluating their contents.
    fn trivia(&mut self) -> Result<()> {
        loop {
            while self
                .bytes
                .get(self.pos)
                .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
            {
                self.pos += 1;
            }
            if self.bytes.get(self.pos..self.pos + 2) == Some(b"//") {
                self.pos += 2;
                while self
                    .bytes
                    .get(self.pos)
                    .is_some_and(|b| !matches!(b, b'\r' | b'\n'))
                {
                    self.pos += 1;
                }
            } else if self.bytes.get(self.pos..self.pos + 2) == Some(b"/*") {
                self.pos += 2;
                while self.bytes.get(self.pos..self.pos + 2) != Some(b"*/") {
                    if self.pos >= self.bytes.len() {
                        return Err(invalid());
                    }
                    self.pos += 1;
                }
                self.pos += 2;
            } else {
                return Ok(());
            }
        }
    }
    /// Consumes one fixed structural delimiter, rejecting incomplete grammar.
    fn take(&mut self, byte: u8) -> Result<()> {
        self.trivia()?;
        if self.bytes.get(self.pos) != Some(&byte) {
            return Err(invalid());
        }
        self.pos += 1;
        Ok(())
    }
    /// Decodes one JSON string with serde's escape/control/UTF-8 validation.
    fn string(&mut self) -> Result<String> {
        let start = self.pos;
        self.take(b'"')?;
        loop {
            match self.bytes.get(self.pos) {
                Some(b'\\') => {
                    self.pos += 2;
                }
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => self.pos += 1,
                None => return Err(invalid()),
            }
        }
        serde_json::from_slice(&self.bytes[start..self.pos]).map_err(|_| invalid())
    }
    /// Reads strict JSON values plus qualified comments/trailing commas only.
    fn node(&mut self, depth: usize) -> Result<Node> {
        if depth > 64 || self.nodes >= 65_536 {
            return Err(invalid());
        }
        self.nodes += 1;
        self.trivia()?;
        let start = self.pos;
        let kind = match self.bytes.get(self.pos).copied() {
            Some(b'{') => {
                self.pos += 1;
                let mut fields = Vec::new();
                let mut commas = Vec::new();
                let mut keys = BTreeSet::new();
                self.trivia()?;
                if self.bytes.get(self.pos) != Some(&b'}') {
                    loop {
                        self.trivia()?;
                        let key = self.string()?;
                        if !keys.insert(key.clone()) {
                            return Err(invalid());
                        }
                        self.take(b':')?;
                        fields.push((key, self.node(depth + 1)?));
                        self.trivia()?;
                        if self.bytes.get(self.pos) != Some(&b',') {
                            break;
                        }
                        commas.push(self.pos);
                        self.pos += 1;
                        self.trivia()?;
                        if self.bytes.get(self.pos) == Some(&b'}') {
                            break;
                        }
                    }
                }
                self.take(b'}')?;
                Kind::Object(fields, commas)
            }
            Some(b'[') => {
                self.pos += 1;
                let mut values = Vec::new();
                let mut commas = Vec::new();
                self.trivia()?;
                if self.bytes.get(self.pos) != Some(&b']') {
                    loop {
                        values.push(self.node(depth + 1)?);
                        self.trivia()?;
                        if self.bytes.get(self.pos) != Some(&b',') {
                            break;
                        }
                        commas.push(self.pos);
                        self.pos += 1;
                        self.trivia()?;
                        if self.bytes.get(self.pos) == Some(&b']') {
                            break;
                        }
                    }
                }
                self.take(b']')?;
                Kind::Array(values, commas)
            }
            Some(b'"') => Kind::Scalar(Value::String(self.string()?)),
            Some(_) => {
                while self.bytes.get(self.pos).is_some_and(|b| {
                    !matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b',' | b']' | b'}' | b'/')
                }) {
                    self.pos += 1;
                }
                if self.pos == start {
                    return Err(invalid());
                }
                Kind::Scalar(
                    serde_json::from_slice(&self.bytes[start..self.pos]).map_err(|_| invalid())?,
                )
            }
            None => return Err(invalid()),
        };
        Ok(Node {
            span: start..self.pos,
            kind,
        })
    }
}
/// Stable content-free failure, never a raw document/key/path diagnostic.
fn invalid() -> MezError {
    MezError::invalid_args("bootstrap JSONC grammar or bounds unavailable")
}
/// Parses at most one MiB, 65,536 nodes and 64 structural levels.
fn parse(bytes: &[u8]) -> Result<Node> {
    if bytes.len() > 1024 * 1024 || std::str::from_utf8(bytes).is_err() {
        return Err(invalid());
    }
    let mut parser = Parser {
        bytes,
        pos: if bytes.starts_with(b"\xef\xbb\xbf") {
            3
        } else {
            0
        },
        nodes: 0,
    };
    let node = parser.node(0)?;
    parser.trivia()?;
    if parser.pos != bytes.len() {
        return Err(invalid());
    }
    Ok(node)
}
/// Selects one of the loader's fixed effective plugin locations, never an input
/// pointer. Root plugin wins; otherwise an existing nested tui plugin survives
/// normalization by receiving the new member in its own array.
pub(super) fn plugin_pointer(bytes: &[u8]) -> Result<&'static str> {
    let root = parse(bytes)?;
    let Kind::Object(fields, _) = &root.kind else {
        return Err(invalid());
    };
    if fields.iter().any(|(key, _)| key == "plugin") {
        return Ok("/plugin");
    }
    if let Some((_, tui)) = fields.iter().find(|(key, _)| key == "tui")
        && let Kind::Object(fields, _) = &tui.kind
        && fields.iter().any(|(key, _)| key == "plugin")
    {
        return Ok("/tui/plugin");
    }
    Ok("/plugin")
}

/// Converts only equivalent exact array-entry ownership, never a whole file.
fn strict_entry(artifact: Option<&Artifact>, pointer: &str) -> Result<Option<Artifact>> {
    match artifact {
        Some(
            Artifact::JsoncArrayEntry {
                pointer: old,
                value,
            }
            | Artifact::JsonArrayEntry {
                pointer: old,
                value,
            },
        ) if old == pointer => Ok(Some(Artifact::JsonArrayEntry {
            pointer: old.clone(),
            value: value.clone(),
        })),
        None => Ok(None),
        _ => Err(MezError::conflict(
            "bootstrap JSONC ownership location/kind changed",
        )),
    }
}
/// Borrows only normalized exact-member data for span planning.
fn entry_value(artifact: &Artifact) -> Option<&Value> {
    match artifact {
        Artifact::JsonArrayEntry { value, .. } => Some(value),
        _ => None,
    }
}

/// One precise token edit; empty output removes only the selected byte interval.
struct Edit {
    span: Range<usize>,
    bytes: Vec<u8>,
}
/// Applies nonoverlapping edits in reverse byte order without reformatting input.
fn apply(bytes: &[u8], mut edits: Vec<Edit>) -> Result<Vec<u8>> {
    edits.sort_by_key(|edit| (edit.span.start, edit.span.end));
    for pair in edits.windows(2) {
        if pair[0].span.end > pair[1].span.start {
            return Err(invalid());
        }
    }
    let mut result = bytes.to_vec();
    for edit in edits.into_iter().rev() {
        result.splice(edit.span, edit.bytes);
    }
    if result.len() > 1024 * 1024 {
        return Err(invalid());
    }
    Ok(result)
}
/// Appends one item/field before a closing delimiter while preserving trivia and
/// trailing-comma style. A required separator is inserted before trailing comments.
fn append(edits: &mut Vec<Edit>, end: usize, last: Option<usize>, trailing: bool, value: Vec<u8>) {
    if let Some(last) = last
        && !trailing
    {
        edits.push(Edit {
            span: last..last,
            bytes: b",".to_vec(),
        });
    }
    let mut bytes = b" ".to_vec();
    bytes.extend(value);
    if trailing {
        bytes.push(b',');
    }
    edits.push(Edit {
        span: end - 1..end - 1,
        bytes,
    });
}
/// Plans exact compiled array membership and validates the final semantic result.
/// Uninstall retains the containing document; unknown or edited ownership conflicts.
pub(super) fn reconcile(
    current: Option<&[u8]>,
    previous: Option<&Artifact>,
    desired: Option<&Artifact>,
    deduplicate: bool,
) -> Result<Option<Vec<u8>>> {
    if let (
        Some(
            Artifact::JsoncArrayEntry {
                pointer: old_pointer,
                value,
            }
            | Artifact::JsonArrayEntry {
                pointer: old_pointer,
                value,
            },
        ),
        Some(Artifact::JsoncArrayEntry {
            pointer: new_pointer,
            ..
        }),
    ) = (previous, desired)
        && old_pointer != new_pointer
    {
        // Both exact locations were compiler-qualified by the caller. Remove
        // intact old membership in memory, then perform an unowned-target check
        // at the new location; neither partial buffer can reach publication.
        let old = Artifact::JsoncArrayEntry {
            pointer: old_pointer.clone(),
            value: value.clone(),
        };
        let removed = reconcile(current, Some(&old), None, deduplicate)?;
        return reconcile(removed.as_deref(), None, desired, deduplicate);
    }
    let pointer = match desired.or(previous) {
        Some(Artifact::JsoncArrayEntry { pointer, .. }) => pointer,
        _ => return Err(invalid()),
    };
    let old = strict_entry(previous, pointer)?;
    let new = strict_entry(desired, pointer)?;
    let bytes = current.unwrap_or(b"{}");
    let root = parse(bytes)?;
    let original = root.value();
    let normalized = serde_json::to_vec(&original).map_err(|_| invalid())?;
    let expected = reconcile_array(
        Some(&normalized),
        old.as_ref(),
        new.as_ref(),
        pointer,
        deduplicate,
    )?
    .ok_or_else(invalid)?;
    let expected = super::strict_json::decode(&expected)?;
    if original == expected {
        return Ok(Some(bytes.to_vec()));
    }
    let parts = pointer_parts(pointer)?;
    let parent = root.parent(&parts[..parts.len() - 1])?;
    let Kind::Object(fields, parent_commas) = &parent.kind else {
        return Err(invalid());
    };
    let key = &parts[parts.len() - 1];
    let target = fields
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, node)| node);
    let old_value = old.as_ref().and_then(entry_value);
    let new_value = new.as_ref().and_then(entry_value);
    let mut edits = Vec::new();
    if let Some(target) = target {
        let Kind::Array(items, commas) = &target.kind else {
            return Err(invalid());
        };
        let indices = old_value
            .map(|old| {
                items
                    .iter()
                    .enumerate()
                    .filter_map(|(index, node)| (node.value() == *old).then_some(index))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let removed = if new_value.is_some() {
            indices.iter().skip(1).copied().collect::<Vec<_>>()
        } else {
            indices.clone()
        };
        let mut remove_commas = BTreeSet::new();
        for index in removed {
            edits.push(Edit {
                span: items[index].span.clone(),
                bytes: Vec::new(),
            });
            if index < commas.len() {
                remove_commas.insert(commas[index]);
            } else if index > 0 {
                remove_commas.insert(commas[index - 1]);
            }
        }
        for pos in remove_commas {
            edits.push(Edit {
                span: pos..pos + 1,
                bytes: Vec::new(),
            });
        }
        if let Some(new) = new_value {
            let encoded = serde_json::to_vec(new).map_err(|_| invalid())?;
            if let Some(index) = indices.first() {
                if old_value != new_value {
                    edits.push(Edit {
                        span: items[*index].span.clone(),
                        bytes: encoded,
                    });
                }
            } else {
                append(
                    &mut edits,
                    target.span.end,
                    items.last().map(|item| item.span.end),
                    !items.is_empty() && commas.len() == items.len(),
                    encoded,
                );
            }
        }
    } else {
        let new = new_value.ok_or_else(invalid)?;
        let field = format!(
            "{}: [{}]",
            serde_json::to_string(key).map_err(|_| invalid())?,
            serde_json::to_string(new).map_err(|_| invalid())?
        )
        .into_bytes();
        append(
            &mut edits,
            parent.span.end,
            fields.last().map(|(_, node)| node.span.end),
            !fields.is_empty() && parent_commas.len() == fields.len(),
            field,
        );
    }
    let result = apply(bytes, edits)?;
    if parse(&result)?.value() != expected {
        return Err(invalid());
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Supplies only exact test-owned member data, never filesystem authority.
    fn artifact(value: &str) -> Artifact {
        Artifact::JsoncArrayEntry {
            pointer: "/plugin".into(),
            value: Value::String(value.into()),
        }
    }

    /// Comments, CRLF, Unicode and escaped strings remain opaque authored bytes.
    /// Add/upgrade/remove edits are local, missing fields are inserted without
    /// reformatting siblings, and a semantic noop keeps exact representation.
    #[test]
    fn bootstrap_jsonc_local_edits_preserve_authored_bytes() {
        let old = artifact("old");
        let new = artifact("new");
        let input = "// head\r\n{ \"plugin\": [\"user\", \"old\" /*member*/,], \"theme\": \"雪\", \"url\":\"https://x/*not-comment*/\" }\r\n";
        let repeated = reconcile(Some(input.as_bytes()), Some(&old), Some(&old), true)
            .unwrap()
            .unwrap();
        assert_eq!(repeated, input.as_bytes());
        let upgraded = reconcile(Some(input.as_bytes()), Some(&old), Some(&new), true)
            .unwrap()
            .unwrap();
        assert_eq!(
            String::from_utf8(upgraded.clone()).unwrap(),
            input.replace("\"old\"", "\"new\"")
        );
        let removed = reconcile(Some(&upgraded), Some(&new), None, true)
            .unwrap()
            .unwrap();
        assert!(
            String::from_utf8(removed)
                .unwrap()
                .ends_with("\"theme\": \"雪\", \"url\":\"https://x/*not-comment*/\" }\r\n")
        );
        for input in [
            "\u{feff}{/*empty*/}",
            "{/*empty*/}",
            "{\"theme\":\"雪\", // trailing\n}",
            "{\"theme\":\"雪\" // no trailing\n}",
            "{\"plugin\":[/*empty*/]}",
            "{\"plugin\":[\"user\" // trailing note\n]}",
        ] {
            let added = reconcile(Some(input.as_bytes()), None, Some(&new), true)
                .unwrap()
                .unwrap();
            assert_eq!(
                parse(&added).unwrap().value()["plugin"]
                    .as_array()
                    .unwrap()
                    .last(),
                Some(&Value::String("new".into()))
            );
            assert!(String::from_utf8(added.clone()).unwrap().contains(
                if input.contains("empty") {
                    "/*empty*/"
                } else {
                    "//"
                }
            ));
            assert_eq!(
                reconcile(Some(&added), Some(&new), Some(&new), true)
                    .unwrap()
                    .unwrap(),
                added
            );
        }
    }

    /// Every owned-member position pattern up to five items, with and without a
    /// trailing comma, must deduplicate/remove exact copies while preserving all
    /// authored comment tokens and sibling ordering. Delimiter choices are checked
    /// by independent final semantic parsing, not merely by an output substring.
    #[test]
    fn bootstrap_jsonc_all_duplicate_positions_preserve_comments_and_semantics() {
        let owned = artifact("owned");
        for count in 1..=5 {
            for mask in 1..(1usize << count) {
                for trailing in [false, true] {
                    let values = (0..count)
                        .map(|index| {
                            if mask & (1 << index) != 0 {
                                "owned".to_string()
                            } else {
                                format!("user-{index}")
                            }
                        })
                        .collect::<Vec<_>>();
                    let mut text = format!(
                        "{{\"plugin\":[{}",
                        values
                            .iter()
                            .enumerate()
                            .map(|(index, value)| format!(
                                "{} /*c{index}*/",
                                serde_json::to_string(value).unwrap()
                            ))
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    if trailing {
                        text.push(',');
                    }
                    text.push_str("],\"theme\":true}");
                    let mut seen = false;
                    let expected = values
                        .iter()
                        .filter(|value| {
                            if value.as_str() != "owned" {
                                true
                            } else if seen {
                                false
                            } else {
                                seen = true;
                                true
                            }
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    let repaired =
                        reconcile(Some(text.as_bytes()), Some(&owned), Some(&owned), true)
                            .unwrap()
                            .unwrap();
                    assert_eq!(
                        parse(&repaired).unwrap().value()["plugin"],
                        serde_json::json!(expected)
                    );
                    let removed = reconcile(Some(text.as_bytes()), Some(&owned), None, true)
                        .unwrap()
                        .unwrap();
                    assert_eq!(
                        parse(&removed).unwrap().value()["plugin"],
                        serde_json::json!(
                            values
                                .iter()
                                .filter(|value| value.as_str() != "owned")
                                .collect::<Vec<_>>()
                        )
                    );
                    for index in 0..count {
                        assert!(
                            String::from_utf8_lossy(&repaired).contains(&format!("/*c{index}*/"))
                        );
                        assert!(
                            String::from_utf8_lossy(&removed).contains(&format!("/*c{index}*/"))
                        );
                    }
                }
            }
        }
    }

    /// JSON5, duplicate decoded keys at any depth, non-UTF8, malformed strings or
    /// comments and finite byte/depth/node/capacity violations reject promptly.
    /// Valid unreceipted matching members and edited/missing ownership also refuse
    /// mutation, rather than treating supported comments as an ownership grant.
    #[test]
    fn bootstrap_jsonc_grammar_bounds_and_ownership_fail_closed() {
        let owned = artifact("owned");
        for bytes in [
            b"{'plugin':[]}".as_slice(),
            b"{\"plugin\":NaN}",
            b"{\"plugin\":[],\"plugin\":[]}",
            br#"{"plugin":[],"\u0070lugin":[]}"#,
            b"{\"plugin\":[],\"nested\":{\"x\":1,\"x\":2}}",
            b"{/*unterminated}",
            b"{\"plugin\":[,]}",
            b"{\"plugin\":[\"owned\",,]}",
            b"\xff",
            b"{\"plugin\":null}",
            b"{\"plugin\":[\"owned\"]}",
        ] {
            assert!(reconcile(Some(bytes), None, Some(&owned), true).is_err());
        }
        assert!(reconcile(Some(b"{\"plugin\":[]}"), Some(&owned), Some(&owned), true).is_err());
        assert!(reconcile(Some(b"{\"plugin\":[\"edited\"]}"), Some(&owned), None, true).is_err());
        let oversized = vec![b' '; 1024 * 1024 + 1];
        assert!(parse(&oversized).is_err());
        let deep = format!("{}0{}", "[".repeat(66), "]".repeat(66));
        assert!(parse(deep.as_bytes()).is_err());
        let nodes = serde_json::to_vec(&serde_json::json!(vec![false; 65_536])).unwrap();
        assert!(parse(&nodes).is_err());
        let capacity =
            serde_json::to_vec(&serde_json::json!({"plugin":vec!["user";4097]})).unwrap();
        assert!(reconcile(Some(&capacity), None, Some(&owned), true).is_err());
    }
}
