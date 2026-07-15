//! Provider-independent network action planning and result shaping.
//!
//! This module lowers validated `web_search` and `fetch_url` MAAP actions into
//! permission-facing pseudo commands and model-visible structured envelopes.
//! Concrete HTTP transport, response limits, search-result parsing, and product
//! error projection remain responsibilities of the composition crate.

use std::error::Error;
use std::fmt;

use crate::{AgentAction, AgentActionPayload, shell_quote};

/// Runtime-generated execution data for one network-backed semantic action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkActionPlan {
    /// Concise user-facing summary shown before the network request starts.
    pub summary: String,
    /// Classifier-friendly pseudo command representing the same network effect.
    pub policy_command: String,
}

/// Returns the provider-independent plan for a network-backed MAAP action.
pub fn network_action_plan(action: &AgentAction) -> Option<NetworkActionPlan> {
    match &action.payload {
        AgentActionPayload::WebSearch { query, domains, .. } => {
            let mut full_query = query.to_string();
            for domain in domains {
                full_query.push_str(" site:");
                full_query.push_str(domain);
            }
            let url = format!(
                "https://duckduckgo.com/html/?q={}",
                urlencoding::encode(&full_query)
            );
            Some(NetworkActionPlan {
                summary: format!("I’ll search the web for `{query}`."),
                policy_command: format!("curl {}", shell_quote(&url)),
            })
        }
        AgentActionPayload::FetchUrl { url, format, .. } => {
            let mut summary = format!("I’ll fetch `{url}`.");
            if let Some(format) = format {
                summary.push_str(&format!(" Format hint: {format}."));
            }
            Some(NetworkActionPlan {
                summary,
                policy_command: format!("curl {}", shell_quote(url)),
            })
        }
        _ => None,
    }
}

/// Returns the user-facing summary for a network-backed action.
pub fn network_action_summary(action: &AgentAction) -> Option<String> {
    network_action_plan(action).map(|plan| plan.summary)
}

/// Builds compact structured content for a network-backed action result.
pub fn network_action_structured_content_json(
    action: &AgentAction,
    approval: serde_json::Value,
    response: serde_json::Value,
) -> NetworkActionPlanResult<String> {
    let Some(plan) = network_action_plan(action) else {
        return Err(NetworkActionPlanError::new(
            "network structured content requires a network-backed action",
        ));
    };
    Ok(serde_json::json!({
        "kind": action.action_type(),
        "summary": plan.summary,
        "policy_command": plan.policy_command,
        "approval": approval,
        "response": response
    })
    .to_string())
}

/// Result returned by network-action planning and structured result shaping.
pub type NetworkActionPlanResult<T> = Result<T, NetworkActionPlanError>;

/// Typed failure returned when network result shaping receives another action kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkActionPlanError {
    message: String,
}

impl NetworkActionPlanError {
    /// Creates a network planning failure with a stable diagnostic message.
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the unformatted planning diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for NetworkActionPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for NetworkActionPlanError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SayStatus;

    #[test]
    /// Verifies web-search planning combines domain filters into an encoded
    /// backend query while preserving the concise model-authored search term in
    /// the user-facing summary.
    fn network_action_plan_encodes_web_search_query_and_domains() {
        let action = AgentAction {
            id: "search-1".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::WebSearch {
                query: "mez terminal".to_string(),
                domains: vec!["example.com".to_string()],
                recency_days: None,
                max_results: None,
            },
        };

        let plan = network_action_plan(&action).unwrap();

        assert_eq!(plan.summary, "I’ll search the web for `mez terminal`.");
        assert!(plan.policy_command.starts_with("curl "));
        assert!(plan.policy_command.contains("mez%20terminal"));
        assert!(plan.policy_command.contains("site%3Aexample.com"));
    }

    #[test]
    /// Verifies fetch planning retains the optional format hint in display text
    /// while quoting the exact validated URL in its permission-facing pseudo
    /// command.
    fn network_action_plan_preserves_fetch_url_and_format_hint() {
        let action = AgentAction {
            id: "fetch-1".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::FetchUrl {
                url: "https://example.test/data.json?x=one&y=two".to_string(),
                format: Some("json".to_string()),
                max_bytes: Some(4096),
            },
        };

        let plan = network_action_plan(&action).unwrap();

        assert_eq!(
            plan.summary,
            "I’ll fetch `https://example.test/data.json?x=one&y=two`. Format hint: json."
        );
        assert!(
            plan.policy_command
                .contains("https://example.test/data.json?x=one&y=two")
        );
    }

    #[test]
    /// Verifies non-network actions have no execution plan and produce a typed
    /// structured-envelope error rather than an empty or misleading network
    /// result payload.
    fn network_action_planning_rejects_non_network_actions() {
        let action = AgentAction {
            id: "say-1".to_string(),
            rationale: String::new(),
            payload: AgentActionPayload::Say {
                status: SayStatus::Progress,
                text: "working".to_string(),
                content_type: "text/plain".to_string(),
            },
        };

        assert!(network_action_plan(&action).is_none());
        let error = network_action_structured_content_json(
            &action,
            serde_json::Value::Null,
            serde_json::Value::Null,
        )
        .unwrap_err();
        assert_eq!(
            error.message(),
            "network structured content requires a network-backed action"
        );
    }
}
