//! Control protocol test helpers.

/// Builds compact JSON-RPC requests for control tests.
#[derive(Debug, Clone)]
pub(crate) struct JsonRpcRequestBuilder {
    id: u64,
    method: String,
    params_json: Option<String>,
}

impl JsonRpcRequestBuilder {
    /// Creates a request builder for one method.
    pub(crate) fn method(method: &str) -> Self {
        Self {
            id: 1,
            method: method.to_string(),
            params_json: None,
        }
    }

    /// Sets the request id.
    pub(crate) fn id(mut self, id: u64) -> Self {
        self.id = id;
        self
    }

    /// Sets raw JSON params.
    pub(crate) fn params_json(mut self, params_json: &str) -> Self {
        self.params_json = Some(params_json.to_string());
        self
    }

    /// Returns the serialized JSON-RPC request.
    pub(crate) fn build(self) -> String {
        match self.params_json {
            Some(params) => format!(
                r#"{{"jsonrpc":"2.0","id":{},"method":"{}","params":{}}}"#,
                self.id, self.method, params
            ),
            None => format!(
                r#"{{"jsonrpc":"2.0","id":{},"method":"{}"}}"#,
                self.id, self.method
            ),
        }
    }
}
