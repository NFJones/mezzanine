//! Runtime control adapters for external harness presentation and buffers.
//!
//! Pane status and notices remain structured, bounded state rather than raw
//! terminal injection. Paste-buffer methods reuse the mux-owned bounded store.

use crate::runtime::RuntimePaneHarnessStatus;

use super::{
    EventKind, MezError, Result, RuntimeSessionService, json_escape, pane_target_checked_resolved,
    runtime_json_bool_field, runtime_json_string_field, runtime_pane_by_id,
};

const MAX_HARNESS_SOURCE_CHARS: usize = 64;
const MAX_PANE_STATUS_TEXT_CHARS: usize = 96;
const MAX_PANE_NOTICE_TEXT_CHARS: usize = 512;

impl RuntimeSessionService {
    /// Sets or clears one source-owned pane status without changing focus.
    pub(super) fn dispatch_runtime_pane_status(
        &mut self,
        caller_client_id: &mez_core::ids::ClientId,
        params: &str,
    ) -> Result<String> {
        let pane_id = self.external_control_pane_id(params)?;
        let source =
            required_bounded_string(params, "source", "pane/status", MAX_HARNESS_SOURCE_CHARS)?;
        let value = serde_json::from_str::<serde_json::Value>(params)
            .map_err(|_| MezError::invalid_args("pane/status params must be a JSON object"))?;
        let object = value
            .as_object()
            .ok_or_else(|| MezError::invalid_args("pane/status params must be a JSON object"))?;
        let status = match object.get("state") {
            Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::String(state)) => {
                if !matches!(
                    state.as_str(),
                    "running" | "waiting" | "blocked" | "failed" | "complete"
                ) {
                    return Err(MezError::invalid_args(
                        "pane/status state must be running, waiting, blocked, failed, complete, or null",
                    ));
                }
                let text = optional_bounded_string(
                    object.get("text"),
                    "pane/status text",
                    MAX_PANE_STATUS_TEXT_CHARS,
                )?;
                Some(RuntimePaneHarnessStatus {
                    state: state.clone(),
                    text,
                })
            }
            _ => {
                return Err(MezError::invalid_args(
                    "pane/status requires state to be a string or null",
                ));
            }
        };
        let owner = format!("{}:{source}", caller_client_id.as_str());
        self.presentation
            .set_pane_harness_status(&pane_id, &owner, status.clone());
        let (state, text) = status
            .map(|status| {
                (
                    json_optional_string(Some(&status.state)),
                    json_optional_string(status.text.as_deref()),
                )
            })
            .unwrap_or_else(|| ("null".to_string(), "null".to_string()));
        Ok(format!(
            r#"{{"pane_id":"{}","source":"{}","state":{state},"text":{text}}}"#,
            json_escape(&pane_id),
            json_escape(&source)
        ))
    }

    /// Emits one bounded pane-scoped notice into the retained event log.
    pub(super) fn dispatch_runtime_pane_notice(&mut self, params: &str) -> Result<String> {
        let pane_id = self.external_control_pane_id(params)?;
        let source =
            required_bounded_string(params, "source", "pane/notice", MAX_HARNESS_SOURCE_CHARS)?;
        let severity =
            runtime_json_string_field(params, "severity").unwrap_or_else(|| "info".to_string());
        if !matches!(severity.as_str(), "info" | "warning" | "error" | "success") {
            return Err(MezError::invalid_args(
                "pane/notice severity must be info, warning, error, or success",
            ));
        }
        let text =
            required_bounded_string(params, "text", "pane/notice", MAX_PANE_NOTICE_TEXT_CHARS)?;
        self.append_lifecycle_event(
            EventKind::Message,
            format!(
                r#"{{"pane_id":"{}","source":"{}","severity":"{}","text":"{}"}}"#,
                json_escape(&pane_id),
                json_escape(&source),
                severity,
                json_escape(&text)
            ),
        )?;
        Ok(format!(
            r#"{{"pane_id":"{}","source":"{}","severity":"{}","emitted":true}}"#,
            json_escape(&pane_id),
            json_escape(&source),
            severity
        ))
    }

    /// Creates or explicitly replaces one bounded internal paste buffer.
    pub(super) fn dispatch_runtime_buffer_create(&mut self, params: &str) -> Result<String> {
        let name = runtime_json_string_field(params, "name")
            .ok_or_else(|| MezError::invalid_args("buffer/create requires name"))?;
        let content = runtime_json_string_field(params, "content")
            .ok_or_else(|| MezError::invalid_args("buffer/create requires content"))?;
        let replace = runtime_json_bool_field(params, "replace").unwrap_or(false);
        let existed = self.paste_buffers().get(&name).is_some();
        if existed && !replace {
            return Err(MezError::conflict("paste buffer already exists"));
        }
        self.paste_buffers_mut().set_with_origin(
            name.clone(),
            content.clone(),
            Some("control:buffer/create".to_string()),
        )?;
        Ok(format!(
            r#"{{"name":"{}","bytes":{},"created":{},"replaced":{}}}"#,
            json_escape(&name),
            content.len(),
            !existed,
            existed
        ))
    }

    /// Deletes one named internal paste buffer.
    pub(super) fn dispatch_runtime_buffer_delete(&mut self, params: &str) -> Result<String> {
        let name = runtime_json_string_field(params, "name")
            .ok_or_else(|| MezError::invalid_args("buffer/delete requires name"))?;
        let deleted = self.paste_buffers_mut().delete(&name);
        if !deleted {
            return Err(MezError::new(
                crate::error::MezErrorKind::NotFound,
                "paste buffer not found",
            ));
        }
        if self.active_paste_buffer() == Some(name.as_str()) {
            self.set_active_paste_buffer(None);
        }
        Ok(format!(
            r#"{{"name":"{}","deleted":true}}"#,
            json_escape(&name)
        ))
    }

    fn external_control_pane_id(&self, params: &str) -> Result<String> {
        let pane_id = pane_target_checked_resolved(&self.session, params)?
            .map(Ok)
            .unwrap_or_else(|| self.active_pane_id())?;
        runtime_pane_by_id(&self.session, &pane_id)?;
        Ok(pane_id)
    }
}

fn required_bounded_string(
    params: &str,
    field: &str,
    method: &str,
    max_chars: usize,
) -> Result<String> {
    let value = runtime_json_string_field(params, field)
        .ok_or_else(|| MezError::invalid_args(format!("{method} requires {field}")))?;
    if value.trim().is_empty() {
        return Err(MezError::invalid_args(format!(
            "{method} {field} must not be empty"
        )));
    }
    if value.chars().count() > max_chars {
        return Err(MezError::invalid_args(format!(
            "{method} {field} exceeds {max_chars} characters"
        )));
    }
    Ok(value)
}

fn optional_bounded_string(
    value: Option<&serde_json::Value>,
    field: &str,
    max_chars: usize,
) -> Result<Option<String>> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) if value.chars().count() <= max_chars => {
            Ok(Some(value.clone()))
        }
        Some(serde_json::Value::String(_)) => Err(MezError::invalid_args(format!(
            "{field} exceeds {max_chars} characters"
        ))),
        Some(_) => Err(MezError::invalid_args(format!(
            "{field} must be a string or null"
        ))),
    }
}

fn json_optional_string(value: Option<&str>) -> String {
    value
        .map(|value| format!(r#""{}""#, json_escape(value)))
        .unwrap_or_else(|| "null".to_string())
}
