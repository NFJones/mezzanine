//! Exact boolean-leaf ownership using the shared bounded JSONC token tree.
//!
//! Missing object ancestors may be created but never become owned. Removal
//! retains containers and every authored comment, including trivia inside the
//! owned key/colon/value syntax. A missing owned leaf may be repaired or already
//! removed; an edited populated leaf is always a conflict. Each result is parsed
//! again and compared with the expected one-leaf semantic edit before publication.

use super::*;

/// Extracts only the identical boolean ownership location from compiled input.
fn value(artifact: Option<&Artifact>, pointer: &str) -> Result<Option<bool>> {
    match artifact {
        None => Ok(None),
        Some(Artifact::JsoncBoolEntry {
            pointer: old,
            value,
        }) if old == pointer => Ok(Some(*value)),
        _ => Err(MezError::conflict(
            "bootstrap JSONC boolean ownership changed",
        )),
    }
}

/// Mutates one semantic leaf while retaining all object ancestors and siblings.
fn edit_document(
    document: &mut Value,
    parts: &[String],
    old: Option<bool>,
    new: Option<bool>,
) -> Result<()> {
    let mut parent = document;
    for key in &parts[..parts.len() - 1] {
        let object = parent.as_object_mut().ok_or_else(invalid)?;
        if !object.contains_key(key) {
            if new.is_none() {
                return Ok(());
            }
            object.insert(key.clone(), serde_json::json!({}));
        }
        parent = object.get_mut(key).ok_or_else(invalid)?;
    }
    let object = parent.as_object_mut().ok_or_else(invalid)?;
    let key = &parts[parts.len() - 1];
    let previous = old.map(Value::Bool);
    if let Some(current) = object.get(key)
        && Some(current) != previous.as_ref()
    {
        return Err(MezError::conflict(
            "bootstrap owned JSONC boolean changed; no overwrite",
        ));
    }
    if let Some(value) = new {
        object.insert(key.clone(), Value::Bool(value));
    } else {
        object.remove(key);
    }
    Ok(())
}

/// Removes owned syntax as separate tokens, preserving comments between them.
fn remove_field(bytes: &[u8], parent: &Node, index: usize, edits: &mut Vec<Edit>) -> Result<()> {
    let Kind::Object(fields, commas) = &parent.kind else {
        return Err(invalid());
    };
    let mut parser = Parser {
        bytes,
        pos: if index == 0 {
            parent.span.start + 1
        } else {
            commas[index - 1] + 1
        },
        nodes: 0,
    };
    parser.trivia()?;
    let start = parser.pos;
    let key = parser.string()?;
    let end = parser.pos;
    if key != fields[index].0 {
        return Err(invalid());
    }
    parser.take(b':')?;
    let colon = parser.pos - 1;
    for span in [start..end, colon..colon + 1, fields[index].1.span.clone()] {
        edits.push(Edit {
            span,
            bytes: Vec::new(),
        });
    }
    if let Some(comma) = commas
        .get(index)
        .or_else(|| index.checked_sub(1).and_then(|index| commas.get(index)))
    {
        edits.push(Edit {
            span: *comma..*comma + 1,
            bytes: Vec::new(),
        });
    }
    Ok(())
}

/// Plans token edits at the existing leaf or nearest missing object ancestor.
fn changes(bytes: &[u8], root: &Node, parts: &[String], new: Option<bool>) -> Result<Vec<Edit>> {
    let mut node = root;
    for (depth, key) in parts.iter().enumerate() {
        let Kind::Object(fields, commas) = &node.kind else {
            return Err(invalid());
        };
        let found = fields.iter().position(|(name, _)| name == key);
        if let Some(index) = found {
            if depth + 1 < parts.len() {
                node = &fields[index].1;
                continue;
            }
            let mut edits = Vec::new();
            if let Some(value) = new {
                edits.push(Edit {
                    span: fields[index].1.span.clone(),
                    bytes: value.to_string().into_bytes(),
                });
            } else {
                remove_field(bytes, node, index, &mut edits)?;
            }
            return Ok(edits);
        }
        let Some(value) = new else {
            return Ok(Vec::new());
        };
        let mut nested = Value::Bool(value);
        for name in parts[depth + 1..].iter().rev() {
            nested = Value::Object([(name.clone(), nested)].into_iter().collect());
        }
        let mut field = serde_json::to_vec(key).map_err(|_| invalid())?;
        field.push(b':');
        field.extend(serde_json::to_vec(&nested).map_err(|_| invalid())?);
        let mut edits = Vec::new();
        append(
            &mut edits,
            node.span.end,
            fields.last().map(|(_, value)| value.span.end),
            commas.len() == fields.len() && !fields.is_empty(),
            field,
        );
        return Ok(edits);
    }
    Err(invalid())
}

/// Returns exact bytes for one qualified leaf; absent uninstall remains absent.
pub(super) fn reconcile(
    current: Option<&[u8]>,
    previous: Option<&Artifact>,
    desired: Option<&Artifact>,
) -> Result<Option<Vec<u8>>> {
    let pointer = match desired.or(previous) {
        Some(Artifact::JsoncBoolEntry { pointer, .. }) => pointer,
        _ => return Err(invalid()),
    };
    let old = value(previous, pointer)?;
    let new = value(desired, pointer)?;
    if current.is_none() && new.is_none() {
        return Ok(None);
    }
    let bytes = current.unwrap_or(b"{}\n");
    let root = parse(bytes)?;
    let parts = pointer_parts(pointer)?;
    let mut expected = root.value();
    edit_document(&mut expected, &parts, old, new)?;
    if root.value() == expected {
        return Ok(Some(bytes.to_vec()));
    }
    let result = apply(bytes, changes(bytes, &root, &parts, new)?)?;
    if parse(&result)?.value() != expected {
        return Err(invalid());
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture owns only the exact boolean leaf, never its object ancestors.
    fn artifact() -> Artifact {
        Artifact::JsoncBoolEntry {
            pointer: "/enabledFeatureFlags/EXTENSIONS".into(),
            value: true,
        }
    }

    /// Creation may add only missing ancestors; repeated installation is byte
    /// exact. Uninstall retains all original comments and authored sibling flags,
    /// including comments between the owned key, colon and boolean value.
    #[test]
    fn bootstrap_jsonc_boolean_preserves_comments_and_unowned_containers() {
        let entry = artifact();
        for bytes in [
            b"{}".as_slice(),
            br#"{/*root*/"theme":"dark",}"#,
            br#"{"enabledFeatureFlags":{/*flags*/"OTHER":false,},}"#,
        ] {
            let installed = reconcile(Some(bytes), None, Some(&entry)).unwrap().unwrap();
            assert_eq!(
                document(&installed)
                    .unwrap()
                    .pointer("/enabledFeatureFlags/EXTENSIONS"),
                Some(&Value::Bool(true))
            );
            assert_eq!(
                reconcile(Some(&installed), Some(&entry), Some(&entry))
                    .unwrap()
                    .unwrap(),
                installed
            );
            let removed = reconcile(Some(&installed), Some(&entry), None)
                .unwrap()
                .unwrap();
            let mut expected = document(bytes).unwrap();
            if expected.get("enabledFeatureFlags").is_none() {
                expected["enabledFeatureFlags"] = serde_json::json!({});
            }
            assert_eq!(document(&removed).unwrap(), expected);
            for comment in ["/*root*/", "/*flags*/"] {
                if bytes
                    .windows(comment.len())
                    .any(|bytes| bytes == comment.as_bytes())
                {
                    assert!(String::from_utf8_lossy(&removed).contains(comment));
                }
            }
        }
        for source in [
            r#"{"enabledFeatureFlags":{"EXTENSIONS"/*key*/:/*colon*/true/*value*/,"OTHER":false}}"#,
            r#"{"enabledFeatureFlags":{"OTHER":false,"EXTENSIONS"/*key*/:/*colon*/true/*value*/}}"#,
            r#"{"enabledFeatureFlags":{"EXTENSIONS"/*key*/:/*colon*/true/*value*/,}}"#,
        ] {
            let removed = reconcile(Some(source.as_bytes()), Some(&entry), None)
                .unwrap()
                .unwrap();
            for comment in ["/*key*/", "/*colon*/", "/*value*/"] {
                assert!(String::from_utf8_lossy(&removed).contains(comment));
            }
            assert!(
                document(&removed)
                    .unwrap()
                    .pointer("/enabledFeatureFlags/EXTENSIONS")
                    .is_none()
            );
        }
    }

    /// Matching unowned booleans and edited owned booleans are conflicts rather
    /// than inferred ownership. Wrong-type parents, duplicates/escaped aliases,
    /// malformed and oversized inputs must not produce replacement bytes.
    #[test]
    fn bootstrap_jsonc_boolean_refuses_ambiguous_or_edited_ownership() {
        let entry = artifact();
        for bytes in [
            br#"{"enabledFeatureFlags":{"EXTENSIONS":true}}"#.as_slice(),
            br#"{"enabledFeatureFlags":{"EXTENSIONS":false}}"#,
        ] {
            assert!(reconcile(Some(bytes), None, Some(&entry)).is_err());
        }
        for bytes in [
            br#"{"enabledFeatureFlags":{"EXTENSIONS":false}}"#.as_slice(),
            br#"{"enabledFeatureFlags":[]}"#,
            br#"{"enabledFeatureFlags":null}"#,
            br#"{"enabledFeatureFlags":{},"enabledFeatureFlags":{}}"#,
            br#"{"enabledFeatureFlags":{"EXTENSIONS":true,"\u0045XTENSIONS":true}}"#,
            b"{/*unfinished",
        ] {
            assert!(reconcile(Some(bytes), Some(&entry), Some(&entry)).is_err());
        }
        let oversized = vec![b' '; 1024 * 1024 + 1];
        assert!(reconcile(Some(&oversized), None, Some(&entry)).is_err());
        assert!(reconcile(None, Some(&entry), None).unwrap().is_none());
        assert!(
            reconcile(Some(b"{}"), Some(&entry), None)
                .unwrap()
                .is_some()
        );
        assert!(reconcile(None, None, Some(&entry)).unwrap().is_some());
    }
}
