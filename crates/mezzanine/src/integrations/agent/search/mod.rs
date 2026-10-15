//! Static search orchestration over the existing runtime-owned HTTP port.
//!
//! The bridge owns MAAP settlement/permissions; this module owns one bounded
//! exchange and adapter decoding, never retries, caches, hidden fetches or paid
//! routing. Dropping its future drops the owned HTTP future. Progress is only
//! cleaned presentation data; actor attempt/revision authority remains outside.
//! Strict future normalization is separate from the legacy DDG compatibility
//! path. No alternate client, SDK, plugin registry or runtime is introduced.

use super::AsyncProviderHttpTransport;
use crate::integrations::http::HttpCompletion;
use mez_agent::search::{SearchLimits, SearchRequest, SearchResult};
use mez_agent::{ProviderHttpError, ProviderHttpRequest, ProviderHttpTimeouts};
use std::{collections::BTreeMap, future::Future, pin::Pin};

mod duckduckgo;
mod normalization;
pub use normalization::{NormalizedSearchResult, decode_complete_json, normalize_strict};

/// Retained legacy exchange facts used by the unchanged MAAP result envelope.
pub(super) struct SearchExchange {
    /// Typed ordered hits and provenance facts, not a duplicate text corpus.
    pub result: SearchResult,
    /// HTTP status returned by the existing transport.
    pub status_code: u16,
    /// Bytes retained by the transport before legacy local truncation.
    pub body_bytes: usize,
    /// Bytes after legacy local truncation (including its appended notice).
    pub returned_bytes: usize,
}

/// Transport/status failures retaining the bridge's historical error shape.
pub(super) enum SearchExchangeError {
    /// Existing HTTP transport failure, including its explicit timeout kind.
    Transport(ProviderHttpError),
    /// Non-2xx status and retained body length; no raw-body publication.
    Http { status_code: u16, body_bytes: usize },
}

/// Executes exactly one unauthenticated legacy DDG GET using ordinary HTTP
/// completion, publishing only cumulative complete clean anchors.
pub(super) async fn execute_legacy_ddg<T: AsyncProviderHttpTransport>(
    transport: &T,
    request: &SearchRequest,
    on_progress: &mut (impl FnMut(String) + Send),
) -> Result<SearchExchange, SearchExchangeError> {
    let limits = SearchLimits::legacy_ddg(request.max_results);
    let mut headers = BTreeMap::new();
    headers.insert("user-agent".to_string(), "mez".to_string());
    let wire = ProviderHttpRequest {
        method: "GET".to_string(),
        url: request.legacy_ddg_url(),
        headers,
        body: String::new(),
        timeouts: ProviderHttpTimeouts::from_total(limits.deadline_ms()),
        max_response_bytes: Some(limits.transport_bytes()),
    };
    let mut observed = Vec::new();
    let mut published = String::new();
    let mut on_chunk = |chunk: Vec<u8>| {
        observed.extend_from_slice(&chunk);
        let source = legacy_progress_source(&observed, request, limits.results());
        if !source.is_empty() && source != published {
            published.clone_from(&source);
            on_progress(source);
        }
        Box::pin(async {}) as Pin<Box<dyn Future<Output = ()> + Send>>
    };
    let exchange = transport
        .send_async_with_body_chunks(&wire, &mut on_chunk)
        .await
        .map_err(SearchExchangeError::Transport)?;
    let transport_truncated = exchange.completion == HttpCompletion::Truncated;
    let response = exchange.response;
    if !(200..=299).contains(&response.status_code) {
        return Err(SearchExchangeError::Http {
            status_code: response.status_code,
            body_bytes: response.body.len(),
        });
    }
    let (body, truncated) =
        super::network::truncate_text_to_bytes(&response.body, limits.transport_bytes());
    Ok(SearchExchange {
        result: SearchResult::legacy_ddg(
            request,
            duckduckgo::decode(&body, limits.results()),
            truncated || transport_truncated,
        ),
        status_code: response.status_code,
        body_bytes: response.body.len(),
        returned_bytes: body.len(),
    })
}

/// Withholds invalid UTF-8 and incomplete scalars/anchors exactly as before.
fn legacy_progress_source(bytes: &[u8], request: &SearchRequest, limit: usize) -> String {
    let valid = match std::str::from_utf8(bytes) {
        Ok(value) => value,
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).unwrap_or("")
        }
        Err(_) => return String::new(),
    };
    SearchResult::legacy_ddg(request, duckduckgo::decode(valid, limit), false).render_text()
}
