//! Duplicate-safe JSON decoding before protocol or exact ownership interpretation.
//!
//! Generic serde_json::Value erases duplicate keys. This visitor retains ordinary
//! JSON values while rejecting duplicates at every depth, including escaped key
//! aliases. Input byte limits and serde's recursion guard stay caller-owned; no
//! user data is logged, no keys grant edit authority, and no I/O occurs here.

use serde::de::{Deserialize, Deserializer, Error, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

/// One recursively duplicate-safe JSON value, never an ownership receipt.
struct Unique(Value);

impl<'de> Deserialize<'de> for Unique {
    /// Decodes ordinary JSON with recursive unique-object membership checks.
    fn deserialize<D: Deserializer<'de>>(decoder: D) -> Result<Self, D::Error> {
        /// Builds only values that have not overwritten an earlier object key.
        struct Reader;
        impl<'de> Visitor<'de> for Reader {
            type Value = Unique;
            /// Describes only a generic value, never echoing authored input.
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("unique JSON value")
            }
            /// Null remains null rather than absence or inferred ownership.
            fn visit_unit<E: Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
            /// Boolean semantics are preserved unchanged.
            fn visit_bool<E: Error>(self, value: bool) -> Result<Unique, E> {
                Ok(Unique(value.into()))
            }
            /// Signed integers remain exact.
            fn visit_i64<E: Error>(self, value: i64) -> Result<Unique, E> {
                Ok(Unique(value.into()))
            }
            /// Unsigned integers remain exact.
            fn visit_u64<E: Error>(self, value: u64) -> Result<Unique, E> {
                Ok(Unique(value.into()))
            }
            /// JSON finite floating-point values retain ordinary parser semantics.
            fn visit_f64<E: Error>(self, value: f64) -> Result<Unique, E> {
                serde_json::Number::from_f64(value)
                    .map(|number| Unique(Value::Number(number)))
                    .ok_or_else(|| E::custom("JSON number unavailable"))
            }
            /// String contents remain opaque data, never an executable template.
            fn visit_str<E: Error>(self, value: &str) -> Result<Unique, E> {
                Ok(Unique(value.into()))
            }
            /// Arrays retain original semantic order and recursively checked values.
            fn visit_seq<S: SeqAccess<'de>>(self, mut sequence: S) -> Result<Unique, S::Error> {
                let mut values = Vec::new();
                while let Some(value) = sequence.next_element::<Unique>()? {
                    values.push(value.0);
                }
                Ok(Unique(Value::Array(values)))
            }
            /// Object membership is unique after JSON escape decoding.
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Unique, M::Error> {
                let mut object = serde_json::Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    if object.contains_key(&key) {
                        return Err(M::Error::custom("ambiguous JSON object"));
                    }
                    object.insert(key, map.next_value::<Unique>()?.0);
                }
                Ok(Unique(Value::Object(object)))
            }
        }
        decoder.deserialize_any(Reader)
    }
}

/// Rejects invalid/duplicate JSON before keys can select protocol or edit effects.
pub(crate) fn decode(bytes: &[u8]) -> crate::Result<Value> {
    serde_json::from_slice::<Unique>(bytes)
        .map(|value| value.0)
        .map_err(|_| crate::MezError::invalid_args("JSON requires unique object fields"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique JSON must preserve ordinary serde values exactly, including signed
    /// and unsigned bounds, floating numbers, nested arrays, null and escapes.
    #[test]
    fn strict_json_preserves_ordinary_value_semantics() {
        for bytes in [
            b"null".as_slice(),
            b"true",
            b"-9223372036854775808",
            b"18446744073709551615",
            b"1.25",
            b"-0.0",
            br#"{"nested":[null,true,{"value":"escaped\ntext","unicode":"\u0061"}],"number":4}"#,
        ] {
            assert_eq!(
                decode(bytes).unwrap(),
                serde_json::from_slice::<Value>(bytes).unwrap()
            );
        }
    }

    /// Duplicate aliases at any nested object level, trailing data and excessive
    /// depth reject before interpretation. Generic diagnostics never echo raw
    /// private identifiers, keys, values or serde parser excerpts.
    #[test]
    fn strict_json_rejects_nested_aliases_invalid_input_and_excessive_depth() {
        for input in [
            br#"{"PRIVATE":1,"PRIVATE":2}"#.as_slice(),
            br#"{"nested":[{"key":1,"\u006bey":2}]}"#,
            b"{} {}",
            b"{invalid}",
        ] {
            let error = decode(input).unwrap_err();
            assert!(!error.message().contains("PRIVATE"));
        }
        let input = format!("{}0{}", "[".repeat(129), "]".repeat(129));
        assert!(decode(input.as_bytes()).is_err());
    }
}
