//! Control Idempotency implementation.
//!
//! This module owns the control idempotency boundary for Mezzanine. It keeps related
//! state transitions and helper routines localized so neighboring modules
//! interact through typed APIs instead of duplicating subsystem details.

use super::{BTreeMap, MezError, Result, VecDeque, json_escape};

// JSON-RPC request parsing and idempotency cache.

/// Defines the DEFAULT IDEMPOTENCY CACHE ENTRIES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_IDEMPOTENCY_CACHE_ENTRIES: usize = 4096;
/// Defines the DEFAULT IDEMPOTENCY CACHE BYTES const used by this subsystem.
///
/// Keeping this value documented makes the contract explicit at the module
/// boundary and avoids relying on call-site inference.
const DEFAULT_IDEMPOTENCY_CACHE_BYTES: usize = 16 * 1024 * 1024;

/// Carries Json Rpc Request state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonRpcRequest {
    /// Stores the id value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub id: String,
    /// Stores the method value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub method: String,
    /// Stores the params value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub params: Option<String>,
}

/// Carries Cached Control Response state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedControlResponse {
    /// Stores the method value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub method: String,
    /// Stores the params value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub params: Option<String>,
    /// Stores the response value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub response: String,
}

/// Carries Control Idempotency Cache state for this subsystem.
///
/// The type keeps related data explicit so callers can inspect and move
/// structured runtime state without parsing display text.
#[derive(Debug)]
pub struct ControlIdempotencyCache {
    /// Stores the completed value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) completed: BTreeMap<String, CachedControlResponse>,
    /// Stores the insertion order value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) insertion_order: VecDeque<String>,
    /// Stores the entry bytes value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) entry_bytes: BTreeMap<String, usize>,
    /// Stores the retained bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) retained_bytes: usize,
    /// Stores the max entries value for this data structure.
    ///
    /// The field is part of the structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_entries: usize,
    /// Stores the max bytes value for this data structure.
    ///
    /// The field is part of structured state exchanged across this module
    /// boundary and should remain aligned with the owning type invariant.
    pub(super) max_bytes: usize,
}

impl ControlIdempotencyCache {
    /// Runs the with limits operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            completed: BTreeMap::new(),
            insertion_order: VecDeque::new(),
            entry_bytes: BTreeMap::new(),
            retained_bytes: 0,
            max_entries: max_entries.max(1),
            max_bytes: max_bytes.max(1),
        }
    }

    /// Runs the len operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.completed.len()
    }

    /// Runs the is empty operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.completed.is_empty()
    }

    /// Runs the retained bytes operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    #[cfg(test)]
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Runs the cached response operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn cached_response(
        &self,
        cache_key: &str,
        method: &str,
        params: &Option<String>,
    ) -> Result<Option<String>> {
        let Some(cached) = self.completed.get(cache_key) else {
            return Ok(None);
        };
        if cached.method == method && &cached.params == params {
            Ok(Some(cached.response.clone()))
        } else {
            Err(MezError::conflict(
                "idempotency key was reused with different request data",
            ))
        }
    }

    /// Runs the remember response operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    pub fn remember_response(
        &mut self,
        cache_key: impl Into<String>,
        method: impl Into<String>,
        params: Option<String>,
        response: impl Into<String>,
    ) {
        self.ensure_default_limits();
        let cache_key = cache_key.into();
        let entry = CachedControlResponse {
            method: method.into(),
            params,
            response: response.into(),
        };
        let bytes = cached_response_bytes(&cache_key, &entry);
        self.remove_entry(&cache_key);
        if bytes > self.max_bytes {
            return;
        }
        self.retained_bytes = self.retained_bytes.saturating_add(bytes);
        self.entry_bytes.insert(cache_key.clone(), bytes);
        self.insertion_order.push_back(cache_key.clone());
        self.completed.insert(cache_key, entry);
        self.enforce_limits();
    }

    /// Runs the ensure default limits operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn ensure_default_limits(&mut self) {
        if self.max_entries == 0 {
            self.max_entries = DEFAULT_IDEMPOTENCY_CACHE_ENTRIES;
        }
        if self.max_bytes == 0 {
            self.max_bytes = DEFAULT_IDEMPOTENCY_CACHE_BYTES;
        }
    }

    /// Runs the enforce limits operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn enforce_limits(&mut self) {
        self.ensure_default_limits();
        while self.completed.len() > self.max_entries || self.retained_bytes > self.max_bytes {
            let Some(cache_key) = self.insertion_order.pop_front() else {
                break;
            };
            self.remove_entry(&cache_key);
        }
    }

    /// Runs the remove entry operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn remove_entry(&mut self, cache_key: &str) {
        self.insertion_order.retain(|stored| stored != cache_key);
        self.completed.remove(cache_key);
        if let Some(bytes) = self.entry_bytes.remove(cache_key) {
            self.retained_bytes = self.retained_bytes.saturating_sub(bytes);
        }
    }
}

impl Default for ControlIdempotencyCache {
    /// Runs the default operation for this subsystem.
    ///
    /// The function keeps parsing, state changes, and error propagation in
    /// the owning module so callers receive typed results instead of relying
    /// on duplicated control-flow logic.
    fn default() -> Self {
        Self::with_limits(
            DEFAULT_IDEMPOTENCY_CACHE_ENTRIES,
            DEFAULT_IDEMPOTENCY_CACHE_BYTES,
        )
    }
}

/// Runs the cached response bytes operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
fn cached_response_bytes(cache_key: &str, entry: &CachedControlResponse) -> usize {
    cache_key
        .len()
        .saturating_add(entry.method.len())
        .saturating_add(entry.params.as_ref().map_or(0, String::len))
        .saturating_add(entry.response.len())
}

/// Runs the parse json rpc request operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub fn parse_json_rpc_request(body: &str) -> Result<JsonRpcRequest> {
    let value = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|_| MezError::invalid_args("JSON-RPC request body must be valid JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| MezError::invalid_args("JSON-RPC request must be an object"))?;
    let version = object
        .get("jsonrpc")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| MezError::invalid_args("JSON-RPC request must include jsonrpc"))?;
    if version != "2.0" {
        return Err(MezError::invalid_args(
            "JSON-RPC request must use jsonrpc version 2.0",
        ));
    }
    let method = object
        .get("method")
        .and_then(serde_json::Value::as_str)
        .filter(|method| !method.is_empty())
        .ok_or_else(|| MezError::invalid_args("JSON-RPC request must include method"))?
        .to_string();
    let id =
        json_rpc_id_json(object.get("id").ok_or_else(|| {
            MezError::invalid_args("JSON-RPC request must include a non-null id")
        })?)?;
    let params = match object.get("params") {
        Some(params) if params.is_object() => Some(
            serde_json::to_string(params)
                .map_err(|_| MezError::invalid_args("JSON-RPC params must be serializable"))?,
        ),
        Some(_) => {
            return Err(MezError::invalid_args(
                "JSON-RPC request params must be an object when present",
            ));
        }
        None => None,
    };

    Ok(JsonRpcRequest { id, method, params })
}

/// Runs the json rpc id json operation for this subsystem.
///
/// The function keeps parsing, state changes, and error propagation in
/// the owning module so callers receive typed results instead of relying
/// on duplicated control-flow logic.
pub(super) fn json_rpc_id_json(value: &serde_json::Value) -> Result<String> {
    match value {
        serde_json::Value::String(id) if !id.is_empty() => Ok(format!(r#""{}""#, json_escape(id))),
        serde_json::Value::Number(number) if number.is_i64() || number.is_u64() => {
            Ok(number.to_string())
        }
        _ => Err(MezError::invalid_args(
            "JSON-RPC request id must be a non-null string or integer",
        )),
    }
}
