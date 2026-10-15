//! Transport-free search contracts shared by policy and concrete adapters.
//!
//! Plans are effect descriptions, never approval receipts. They contain no key,
//! client, configuration reader or runtime authority. Only the legacy DuckDuckGo
//! plan is executable today. Strict future normalization is deliberately distinct
//! from legacy HTML projection, whose permissive links and clipping quirks remain
//! compatibility behavior. Concrete URL parsing belongs to the product adapter.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

mod filters;
pub use filters::{HostnameUnion, UtcDate};

#[cfg(test)]
mod tests;

/// Backend identity only; naming a paid backend does not activate it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchBackend {
    /// Default, unauthenticated HTML backend.
    DuckDuckGo,
    /// Reserved future Brave identity.
    Brave,
    /// Reserved future Tavily identity.
    Tavily,
    /// Reserved future Exa identity.
    Exa,
    /// Reserved future SerpApi identity.
    SerpApi,
}

/// Content mode vocabulary, not model-authored backend-selection authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    /// Ordered web hits, optionally with provider snippets.
    Web,
    /// Source-extracted passages, not a generated answer.
    Extractive,
    /// Provider-generated material, never mislabeled as a snippet.
    Generated,
}

/// Fixed, code-owned endpoint identities. Paid endpoint catalogs are not added
/// until their concrete adapters are implemented and reviewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum FixedSearchEndpoint {
    /// Exact legacy HTTPS origin and HTML path.
    DuckDuckGoHtml,
}

impl FixedSearchEndpoint {
    /// Returns the complete code-owned endpoint, without query or credentials.
    pub const fn url(self) -> &'static str {
        match self { Self::DuckDuckGoHtml => "https://duckduckgo.com/html/" }
    }
}

/// Bounded logical identity such as a profile, configuration revision or
/// credential-slot name. This is not a secret container: only ASCII identifiers
/// are accepted and callers must never supply credential material.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SearchIdentity(String);

impl SearchIdentity {
    /// Accepts 1–128 ASCII identifier bytes; rejects arbitrary text and URLs.
    pub fn new(value: &str) -> Result<Self, SearchError> {
        if value.is_empty() || value.len() > 128 || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) {
            return Err(SearchError::InvalidRequest);
        }
        Ok(Self(value.to_string()))
    }
}

impl fmt::Debug for SearchIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("SearchIdentity([redacted])") }
}

/// Optional future configuration identity; absent on the implicit DDG path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct SearchPlanBinding {
    /// Explicit future profile name, not a user URL template.
    pub profile: Option<SearchIdentity>,
    /// Immutable configuration revision, never a fabricated epoch.
    pub config_revision: Option<SearchIdentity>,
    /// Logical credential binding only; no resolved secret or auth headers.
    pub credential_binding: Option<SearchIdentity>,
}

/// Validated upper bounds. Private fields prevent unchecked budget mutation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SearchLimits {
    results: usize,
    transport_bytes: usize,
    normalized_bytes: usize,
    deadline_ms: u64,
}

impl SearchLimits {
    /// Legacy DDG preset: default ten, hard fifty (including requested zero),
    /// 1MiB HTML, 256KiB final context and thirty seconds. The existing context
    /// renderer appends its truncation notice outside that content cap.
    pub fn legacy_ddg(requested: Option<u64>) -> Self {
        Self { results: requested.and_then(|n| usize::try_from(n).ok()).unwrap_or(10).min(50), transport_bytes: 1024 * 1024, normalized_bytes: 256 * 1024, deadline_ms: 30_000 }
    }

    /// Strict future preset: ten by default, disclosed backend-lower count cap,
    /// 1MiB body, 32KiB default / 256KiB hard complete output and 30s total.
    /// Zero counts/caps/budgets are invalid; callers cannot enlarge hard bounds.
    pub fn strict(requested: Option<u64>, backend_cap: usize, bytes: Option<usize>) -> Result<Self, SearchError> {
        let requested = requested.unwrap_or(10);
        let normalized_bytes = bytes.unwrap_or(32 * 1024);
        if requested == 0 || backend_cap == 0 || normalized_bytes == 0 || normalized_bytes > 256 * 1024 {
            return Err(SearchError::InvalidBounds);
        }
        Ok(Self { results: usize::try_from(requested).unwrap_or(usize::MAX).min(backend_cap).min(50), transport_bytes: 1024 * 1024, normalized_bytes, deadline_ms: 30_000 })
    }
    /// Effective maximum hit count, including a provider's lower cap.
    pub const fn results(self) -> usize { self.results }
    /// Maximum retained transport body bytes.
    pub const fn transport_bytes(self) -> usize { self.transport_bytes }
    /// Complete strict serialization/presentation budget (legacy content cap).
    pub const fn normalized_bytes(self) -> usize { self.normalized_bytes }
    /// Total exchange deadline, independent of response progress.
    pub const fn deadline_ms(self) -> u64 { self.deadline_ms }
}

/// Request facts retained without interpretation by the compatibility parser.
/// Debug deliberately omits search text and filters.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SearchRequest {
    /// Model-authored query; not a generated answer or provider selector.
    pub query: String,
    /// Legacy ordered site terms, NOT the strict hostname-union contract.
    pub domains: Vec<String>,
    /// Legacy best-effort label; DDG does not transmit this value.
    pub recency_days: Option<u64>,
    /// Requested count, distinct from effective and returned counts.
    pub max_results: Option<u64>,
}

impl fmt::Debug for SearchRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("SearchRequest([redacted])") }
}

impl SearchRequest {
    /// Preserves the exact ordered legacy ` site:` conjunction, with no strict
    /// hostname validation or recency transformation during extraction.
    pub fn legacy_ddg_full_query(&self) -> String {
        let mut query = self.query.clone();
        for domain in &self.domains { query.push_str(" site:"); query.push_str(domain); }
        query
    }
    /// Constructs the exact legacy GET URL used by both policy and execution.
    pub fn legacy_ddg_url(&self) -> String {
        format!("{}?q={}", FixedSearchEndpoint::DuckDuckGoHtml.url(), urlencoding::encode(&self.legacy_ddg_full_query()))
    }
    /// Hashes canonical typed fields, preserving order and Option presence.
    /// The digest is an identity, not a credential-safe logging authorization.
    pub fn digest(&self) -> Result<SearchDigest, SearchError> { digest(self) }
}

/// SHA256 identity of canonical request/filter or complete plan values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SearchDigest(String);

/// Immutable, secret-free effect description. No approval/config/auth I/O.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SearchExecutionPlan {
    backend: SearchBackend,
    mode: SearchMode,
    endpoint: FixedSearchEndpoint,
    request_digest: SearchDigest,
    limits: SearchLimits,
    binding: SearchPlanBinding,
}

impl fmt::Debug for SearchExecutionPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SearchExecutionPlan").field("backend", &self.backend).field("mode", &self.mode).field("limits", &self.limits).finish_non_exhaustive()
    }
}

impl SearchExecutionPlan {
    /// Describes the implicit no-key default, with no invented configuration or
    /// credential binding. It cannot route or authorize a paid backend.
    pub fn legacy_ddg(request: &SearchRequest) -> Result<Self, SearchError> {
        Ok(Self { backend: SearchBackend::DuckDuckGo, mode: SearchMode::Web, endpoint: FixedSearchEndpoint::DuckDuckGoHtml, request_digest: request.digest()?, limits: SearchLimits::legacy_ddg(request.max_results), binding: SearchPlanBinding::default() })
    }
    /// Full effect identity includes backend/mode/fixed endpoint/request/bounds
    /// and all optional revision/profile/credential-slot identities.
    pub fn digest(&self) -> Result<SearchDigest, SearchError> { digest(self) }
    /// Effective immutable limits for this effect.
    pub const fn limits(&self) -> SearchLimits { self.limits }
}

/// Encodes canonical structs rather than ambiguous concatenated query strings.
fn digest(value: &impl Serialize) -> Result<SearchDigest, SearchError> {
    let bytes = serde_json::to_vec(value).map_err(|_| SearchError::MalformedJson)?;
    Ok(SearchDigest(format!("{:x}", Sha256::digest(bytes))))
}

/// How a backend actually handled one requested filter; not a verified claim
/// that a query hint or best-effort label enforced a source-date restriction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterDisposition {
    /// Native API enforcement.
    Native,
    /// Query syntax hint only.
    Query,
    /// Checked local filtering.
    Local,
    /// Best effort without verified enforcement.
    BestEffort,
    /// Explicitly unsupported.
    Unsupported,
    /// No filter requested.
    NotRequested,
}

/// Narrow declared capabilities for a concrete adapter, not routing logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SearchCapabilities {
    /// Hostname filter support/strength.
    pub domains: FilterDisposition,
    /// Checked UTC source-date filter support/strength.
    pub dates: FilterDisposition,
    /// Whether source excerpts are actually available.
    pub excerpts: bool,
}

impl SearchCapabilities {
    /// Honest legacy declaration: ordered query hints and nontransmitted date
    /// labels, with no fabricated DDG snippets.
    pub const LEGACY_DDG: Self = Self { domains: FilterDisposition::Query, dates: FilterDisposition::BestEffort, excerpts: false };
}

/// Provenance of actual returned excerpt material.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExcerptProvenance {
    /// Provider snippet, not a checked extract or generated answer.
    Snippet,
    /// Passage extracted from source material.
    Extractive,
    /// Provider-generated content, labeled explicitly.
    Generated,
}

/// Excerpt text and inseparable provenance; absent when the backend has none.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchExcerpt {
    /// Actual returned text, bounded by strict normalization.
    pub text: String,
    /// Origin/interpretation of this material.
    pub provenance: ExcerptProvenance,
}

/// Basis for an observed source date, not a guessed recency claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceDateBasis {
    /// Source publication date reported by backend.
    Published,
    /// Source modification date reported by backend.
    Modified,
    /// Backend crawl/index observation, not source publication.
    Crawled,
}

/// Optional observed UTC source date with explicit interpretation.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchSourceDate {
    /// Checked calendar day in UTC.
    pub date: UtcDate,
    /// Meaning of the date supplied by the backend.
    pub basis: SourceDateBasis,
}

/// Ordered hit shared by text and JSON projections. Legacy DDG allows old
/// permissive links; future paths must run the strict product normalizer first.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchHit {
    /// Source title, not generated summary text.
    pub title: String,
    /// Complete citation URL; strict normalization never clips this field.
    pub url: String,
    /// Actual optional excerpt; DDG does not fabricate one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<SearchExcerpt>,
    /// Actual optional source-date fact.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_date: Option<SearchSourceDate>,
}

/// Provider-reported usage only. No invoice, score, estimate or charge inferred
/// from the number of hits, requests or configured plan.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedSearchUsage {
    /// Provider-reported count.
    pub units: u64,
    /// Provider-reported unit label (bounded by normalization).
    pub unit: String,
}

/// Single normalized value for both projections; model integration selects one
/// corpus representation, never full text plus a duplicate JSON hit corpus.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchResult {
    /// Original request count, absent for implicit default.
    pub requested_count: Option<u64>,
    /// Effective requested count after limits, not returned hits.
    pub effective_count: usize,
    /// Stable backend order of retained hits.
    pub hits: Vec<SearchHit>,
    /// Domain support actually exercised.
    pub domains: FilterDisposition,
    /// Date support actually exercised.
    pub dates: FilterDisposition,
    /// Bounded diagnostic facts, never raw response bodies.
    pub warnings: Vec<String>,
    /// Transport or normalization discarded data.
    pub truncated: bool,
    /// Optional bounded/sanitized remote request ID, not an authority token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Actual provider-reported usage; absent when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ReportedSearchUsage>,
}

impl SearchResult {
    /// Builds honest legacy facts without snippets/dates/usage/request IDs.
    pub fn legacy_ddg(request: &SearchRequest, hits: Vec<SearchHit>, truncated: bool) -> Self {
        Self { requested_count: request.max_results, effective_count: SearchLimits::legacy_ddg(request.max_results).results(), hits, domains: if request.domains.is_empty() { FilterDisposition::NotRequested } else { FilterDisposition::Query }, dates: if request.recency_days.is_some() { FilterDisposition::BestEffort } else { FilterDisposition::NotRequested }, warnings: Vec::new(), truncated, request_id: None, usage: None }
    }
    /// Renders numbered title/link hits and optional explicitly labeled excerpts
    /// from this same value. Citation URLs are always complete.
    pub fn render_text(&self) -> String {
        let mut output = String::new();
        for (index, hit) in self.hits.iter().enumerate() {
            output.push_str(&format!("{}. {}\n   {}\n", index + 1, hit.title, hit.url));
            if let Some(excerpt) = &hit.excerpt { output.push_str(&format!("   [{:?}] {}\n", excerpt.provenance, excerpt.text)); }
        }
        output.trim_end().to_string()
    }
}

/// Content-free failure taxonomy for strict future adapters. DDG action bridge
/// retains its existing network_request_failed/network_http_error projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchError {
    /// Invalid request or logical identity.
    InvalidRequest,
    /// Invalid bounds or impossible complete-output budget.
    InvalidBounds,
    /// Invalid, non-web or credential-bearing citation URL.
    InvalidUrl,
    /// Unsupported filter, mode or capability.
    Unsupported,
    /// Transport reported an incomplete response.
    Incomplete,
    /// Retained body exceeds the configured cap.
    Oversize,
    /// Malformed response encoding.
    MalformedEncoding,
    /// Malformed complete JSON or strict response shape.
    MalformedJson,
    /// HTTP 401.
    Authentication,
    /// HTTP 403.
    Forbidden,
    /// HTTP 402; not an inferred bill.
    PaymentRequired,
    /// HTTP 429.
    RateLimited,
    /// HTTP 202; not an empty successful strict response.
    Pending,
    /// Other non-success status, retained exactly.
    Http(u16),
    /// Credentials could not safely be supplied or output suppressed.
    CredentialSuppressed,
    /// Transport interruption, distinct from timeout.
    Transport,
    /// Existing explicit timeout phase.
    Timeout(crate::http::ProviderHttpTimeoutPhase),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "search contract failure: {self:?}") }
}
impl std::error::Error for SearchError {}
