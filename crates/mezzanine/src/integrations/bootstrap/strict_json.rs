//! Duplicate-safe shared JSON decoding before exact installer ownership edits.
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

/// Rejects invalid/duplicate JSON before an installer computes replacement bytes.
pub(super) fn decode(bytes: &[u8]) -> crate::Result<Value> {
    serde_json::from_slice::<Unique>(bytes)
        .map(|value| value.0)
        .map_err(|_| {
            crate::MezError::invalid_args(
                "bootstrap requires unique strict JSON; authored document unchanged",
            )
        })
}
