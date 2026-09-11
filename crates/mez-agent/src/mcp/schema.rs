//! Bounded, I/O-free JSON Schema admission and instance validation for MCP tools.
//!
//! This module is the single lower-crate owner for MCP tool-input schema policy.
//! Snapshot restore, live tool discovery, and live tool-call planning all reach
//! schema admission through [`McpSchemaValidator`], so the admitted dialect,
//! instance assertions, diagnostics, and compilation cache cannot drift between
//! those paths.
//!
//! # Supported dialect
//!
//! The admitted dialect is JSON Schema 2020-12
//! (`https://json-schema.org/draft/2020-12/schema`). A tool schema that declares
//! any other `$schema` value is rejected as `unsupported_dialect`, because
//! silently switching assertion semantics would make approval decisions and
//! model repair guidance unreviewable. A schema that omits `$schema` is compiled
//! with 2020-12 semantics.
//!
//! # Supported assertions
//!
//! Admitted schemas may use the 2020-12 applicator and assertion vocabulary:
//! `type` (including type unions that admit objects), `enum`, `const`,
//! `properties`, `patternProperties`, `additionalProperties`,
//! `unevaluatedProperties`, `unevaluatedItems`, `required`,
//! `dependentRequired`, `dependentSchemas`, `propertyNames`, `items`,
//! `prefixItems`, `contains`, `minContains`, `maxContains`, `minItems`,
//! `maxItems`, `uniqueItems`, `minProperties`, `maxProperties`, `minLength`,
//! `maxLength`, `pattern`, `minimum`, `maximum`, `exclusiveMinimum`,
//! `exclusiveMaximum`, `multipleOf`, `if`/`then`/`else`, `allOf`, `anyOf`,
//! `oneOf`, `not`, and same-document `$ref`/`$dynamicRef` JSON Pointer
//! fragments. Together these enforce the assertions MCP tool arguments depend
//! on: enum/const, `additionalProperties`, nested `required`, array bounds,
//! combinations, type unions, patterns, and numeric/string/object bounds.
//!
//! `format` is an annotation rather than an assertion here: an approximately
//! well-formed email, hostname, or date-time string must not fail an otherwise
//! valid call. Content keywords (`contentMediaType`, `contentEncoding`) are
//! disabled for the same reason.
//!
//! # Annotations and extensions
//!
//! `title`, `description`, `examples`, `default`, `$comment`, `deprecated`,
//! `readOnly`, `writeOnly`, and extension keywords such as `x-*` are harmless
//! metadata. They are never asserted, never copied into diagnostics, and never
//! grant, extend, or withdraw approval. Annotation keywords that this validator
//! does not interpret are still tolerated, so a server can document its tools
//! freely.
//!
//! Keyword screening stops at data: annotation keywords and `x-*` extensions
//! carry values, not subschemas, so a `$schema`, `$ref`, `$dynamicRef`, or
//! `$recursiveRef` shaped string inside `title`, `description`, `examples`,
//! `default`, `const`, `enum`, or an extension neither withdraws the tool nor
//! counts against the reference budget. Screening descends only through schema
//! positions: `properties`, `patternProperties`, `additionalProperties`,
//! `items`, `prefixItems`, `contains`, `allOf`/`anyOf`/`oneOf`, `not`,
//! `if`/`then`/`else`, `dependentSchemas`, `propertyNames`,
//! `unevaluatedProperties`, `unevaluatedItems`, and `$defs`.
//!
//! # References
//!
//! Remote and file references are never fetched. This crate depends on
//! `jsonschema` with `default-features = false`, which removes the HTTP and file
//! retrievers from the build, and every candidate reference string is screened
//! before compilation. Only `$ref` and `$dynamicRef` values within
//! [`DEFAULT_MCP_SCHEMA_MAX_REFERENCE_BYTES`] that are same-document JSON
//! Pointer fragments - the whole-document fragment `#` or a `/`-prefixed pointer
//! fragment such as `#/$defs/kind` - are admitted. Plain-name anchors (`#name`),
//! every other reference, and `$recursiveRef` are rejected as
//! `unsupported_reference` without performing any I/O.
//!
//! # Limits
//!
//! | Bound | Constant | Default |
//! | --- | --- | --- |
//! | schema bytes | [`DEFAULT_MCP_SCHEMA_MAX_BYTES`] | 256 KiB |
//! | schema nesting depth | [`DEFAULT_MCP_SCHEMA_MAX_DEPTH`] | 32 |
//! | schema nodes (compile work) | [`DEFAULT_MCP_SCHEMA_MAX_NODES`] | 8192 |
//! | schema references | [`DEFAULT_MCP_SCHEMA_MAX_REFERENCES`] | 128 |
//! | single reference bytes | [`DEFAULT_MCP_SCHEMA_MAX_REFERENCE_BYTES`] | 512 |
//! | instance (arguments) bytes | [`DEFAULT_MCP_INSTANCE_MAX_BYTES`] | 1 MiB |
//! | instance nesting depth | [`DEFAULT_MCP_INSTANCE_MAX_DEPTH`] | 32 |
//! | instance nodes (validation work) | [`DEFAULT_MCP_INSTANCE_MAX_NODES`] | 8192 |
//! | cached compiled schemas | [`DEFAULT_MCP_SCHEMA_CACHE_CAPACITY`] | 64 |
//! | regex backtracking steps | [`DEFAULT_MCP_SCHEMA_REGEX_BACKTRACK_LIMIT`] | 100000 |
//! | regex compiled size bytes | [`DEFAULT_MCP_SCHEMA_REGEX_SIZE_LIMIT`] | 1 MiB |
//!
//! # Diagnostics
//!
//! A diagnostic carries exactly three fields: a category, an optional canonical
//! keyword, and an optional bounded instance pointer. Diagnostics never contain
//! instance values, schema secrets, or unbounded untrusted key text: pointers
//! are truncated to [`MCP_SCHEMA_MAX_DIAGNOSTIC_POINTER_SEGMENTS`] segments and
//! [`MCP_SCHEMA_MAX_DIAGNOSTIC_POINTER_BYTES`] bytes, keywords to
//! [`MCP_SCHEMA_MAX_DIAGNOSTIC_KEYWORD_BYTES`] bytes, and both are reduced to
//! printable ASCII before rendering.
//!
//! # Caching
//!
//! Compiled schemas are cached under [`McpSchemaGeneration`], a fingerprint over
//! the tool identity and the exact schema bytes. Re-admitting an unchanged schema
//! reuses the compiled validator; the cache is bounded to
//! [`McpSchemaLimits::max_cached_schemas`] entries with least-recently-used
//! eviction, and MCP metadata refreshes call
//! [`McpSchemaValidator::invalidate_server`] so a refreshed schema can never be
//! served from a pre-refresh compilation.
//!
//! The generation digest input is the version prefix followed by the
//! length-prefixed server identity, tool name, and schema bytes, so no two
//! distinct identities can share one digest input stream, and the full SHA-256
//! digest is retained as [`MCP_SCHEMA_GENERATION_DIGEST_HEX_CHARS`] lowercase
//! hexadecimal characters rather than a truncated prefix.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;

use jsonschema::Draft;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Dialect URI admitted for MCP tool-input schemas.
pub const MCP_SCHEMA_SUPPORTED_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Stable prefix of every MCP tool-schema generation fingerprint.
pub const MCP_SCHEMA_GENERATION_PREFIX: &str = "mcp-schema-v1";

/// Number of hexadecimal characters retained in one schema generation.
///
/// The full SHA-256 digest is retained (256 bits) so approval and dispatch
/// identity keeps the digest's collision resistance instead of a truncated
/// 128-bit prefix.
pub const MCP_SCHEMA_GENERATION_DIGEST_HEX_CHARS: usize = 64;

/// Maximum admitted tool-schema bytes.
pub const DEFAULT_MCP_SCHEMA_MAX_BYTES: usize = 256 * 1024;

/// Maximum admitted tool-schema nesting depth.
pub const DEFAULT_MCP_SCHEMA_MAX_DEPTH: usize = 32;

/// Maximum admitted tool-schema nodes, which bounds compilation work.
pub const DEFAULT_MCP_SCHEMA_MAX_NODES: usize = 8192;

/// Maximum admitted reference keywords in schema positions of one tool schema.
pub const DEFAULT_MCP_SCHEMA_MAX_REFERENCES: usize = 128;

/// Maximum admitted bytes for one same-document reference string.
pub const DEFAULT_MCP_SCHEMA_MAX_REFERENCE_BYTES: usize = 512;

/// Maximum admitted tool-argument bytes.
pub const DEFAULT_MCP_INSTANCE_MAX_BYTES: usize = 1024 * 1024;

/// Maximum admitted tool-argument nesting depth.
pub const DEFAULT_MCP_INSTANCE_MAX_DEPTH: usize = 32;

/// Maximum admitted tool-argument nodes, which bounds validation work.
pub const DEFAULT_MCP_INSTANCE_MAX_NODES: usize = 8192;

/// Maximum compiled tool schemas retained per registry.
pub const DEFAULT_MCP_SCHEMA_CACHE_CAPACITY: usize = 64;

/// Maximum regular-expression backtracking steps per `pattern` assertion.
pub const DEFAULT_MCP_SCHEMA_REGEX_BACKTRACK_LIMIT: usize = 100_000;

/// Maximum compiled regular-expression size in bytes per `pattern` assertion.
pub const DEFAULT_MCP_SCHEMA_REGEX_SIZE_LIMIT: usize = 1024 * 1024;

/// Maximum bytes retained for one diagnostic keyword.
pub const MCP_SCHEMA_MAX_DIAGNOSTIC_KEYWORD_BYTES: usize = 32;

/// Maximum bytes retained for one diagnostic pointer.
pub const MCP_SCHEMA_MAX_DIAGNOSTIC_POINTER_BYTES: usize = 128;

/// Maximum pointer segments retained for one diagnostic pointer.
pub const MCP_SCHEMA_MAX_DIAGNOSTIC_POINTER_SEGMENTS: usize = 8;

/// Maximum bytes retained for one diagnostic pointer segment.
pub const MCP_SCHEMA_MAX_DIAGNOSTIC_POINTER_SEGMENT_BYTES: usize = 24;

/// One bounded schema or instance failure category.
///
/// The category is the stable part of a diagnostic. Model-repairable categories
/// describe the model's own arguments; schema-side categories describe server
/// metadata that the model cannot repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSchemaFailure {
    /// The schema text was not valid JSON.
    SchemaNotJson,
    /// The schema exceeded the admitted byte limit.
    SchemaTooLarge,
    /// The schema root was not a JSON object.
    SchemaNotObject,
    /// The schema root `type` does not admit JSON objects.
    SchemaRootNotObject,
    /// The schema exceeded the admitted nesting depth.
    SchemaTooDeep,
    /// The schema exceeded the admitted node or reference budget.
    SchemaTooComplex,
    /// The schema declared an unsupported dialect.
    UnsupportedDialect,
    /// The schema used a reference that is not a bounded same-document reference.
    UnsupportedReference,
    /// The schema is not a valid schema in the supported dialect.
    InvalidSchema,
    /// The tool arguments were not valid JSON.
    InstanceNotJson,
    /// The tool arguments exceeded the admitted byte limit.
    InstanceTooLarge,
    /// The tool-argument root was not a JSON object.
    InstanceRootNotObject,
    /// The tool arguments exceeded the admitted nesting depth.
    InstanceTooDeep,
    /// The tool arguments exceeded the admitted node budget.
    InstanceTooComplex,
    /// The tool arguments failed one schema assertion.
    InstanceViolatesSchema,
}

impl McpSchemaFailure {
    /// Returns the stable snake-case category code used by diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SchemaNotJson => "schema_not_json",
            Self::SchemaTooLarge => "schema_too_large",
            Self::SchemaNotObject => "schema_not_object",
            Self::SchemaRootNotObject => "schema_root_not_object",
            Self::SchemaTooDeep => "schema_too_deep",
            Self::SchemaTooComplex => "schema_too_complex",
            Self::UnsupportedDialect => "unsupported_dialect",
            Self::UnsupportedReference => "unsupported_reference",
            Self::InvalidSchema => "invalid_schema",
            Self::InstanceNotJson => "instance_not_json",
            Self::InstanceTooLarge => "instance_too_large",
            Self::InstanceRootNotObject => "instance_root_not_object",
            Self::InstanceTooDeep => "instance_too_deep",
            Self::InstanceTooComplex => "instance_too_complex",
            Self::InstanceViolatesSchema => "instance_violates_schema",
        }
    }

    /// Returns the static human phrase rendered in model-visible diagnostics.
    pub fn phrase(self) -> &'static str {
        match self {
            Self::SchemaNotJson => "schema is not valid JSON",
            Self::SchemaTooLarge => "schema exceeds the admitted byte limit",
            Self::SchemaNotObject => "schema root is not a JSON object",
            Self::SchemaRootNotObject => "schema root type is not object",
            Self::SchemaTooDeep => "schema exceeds the admitted nesting depth",
            Self::SchemaTooComplex => "schema exceeds the admitted complexity budget",
            Self::UnsupportedDialect => "schema dialect is not supported",
            Self::UnsupportedReference => {
                "schema reference is not a bounded same-document reference"
            }
            Self::InvalidSchema => "schema is not valid in the supported dialect",
            Self::InstanceNotJson => "tool arguments are not valid JSON",
            Self::InstanceTooLarge => "tool arguments exceed the admitted byte limit",
            Self::InstanceRootNotObject => "tool arguments root is not a JSON object",
            Self::InstanceTooDeep => "tool arguments exceed the admitted nesting depth",
            Self::InstanceTooComplex => "tool arguments exceed the admitted complexity budget",
            Self::InstanceViolatesSchema => "tool arguments do not satisfy the tool input schema",
        }
    }

    /// Reports whether a bounded model repair can address this failure.
    ///
    /// Only instance-side failures are model-repairable. A schema-side failure
    /// is a metadata or operator failure: it takes the affected tool out of
    /// service instead of inviting the model to re-author the same call.
    pub fn is_model_repairable(self) -> bool {
        matches!(
            self,
            Self::InstanceNotJson
                | Self::InstanceTooLarge
                | Self::InstanceRootNotObject
                | Self::InstanceTooDeep
                | Self::InstanceTooComplex
                | Self::InstanceViolatesSchema
        )
    }

    /// Reports whether this failure describes server-owned schema metadata.
    pub fn is_schema_fault(self) -> bool {
        !self.is_model_repairable()
    }
}

/// One bounded, secret-free schema or instance diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSchemaDiagnostic {
    category: McpSchemaFailure,
    keyword: Option<String>,
    pointer: Option<String>,
}

impl McpSchemaDiagnostic {
    /// Builds one diagnostic from already bounded tokens.
    fn new(category: McpSchemaFailure, keyword: Option<&str>, pointer: Option<&str>) -> Self {
        Self {
            category,
            keyword: keyword.map(bounded_diagnostic_keyword),
            pointer: pointer.map(bounded_diagnostic_pointer),
        }
    }

    /// Returns the stable failure category.
    pub fn category(&self) -> McpSchemaFailure {
        self.category
    }

    /// Returns the bounded canonical keyword when the validator supplied one.
    pub fn keyword(&self) -> Option<&str> {
        self.keyword.as_deref()
    }

    /// Returns the bounded instance pointer when the validator supplied one.
    pub fn pointer(&self) -> Option<&str> {
        self.pointer.as_deref()
    }

    /// Renders one bounded, single-line diagnostic message.
    ///
    /// The message is safe for model-visible text and audit records: it contains
    /// only the static phrase, the stable category, and bounded tokens.
    pub fn message(&self) -> String {
        let mut message = format!(
            "{} [category={}",
            self.category.phrase(),
            self.category.as_str()
        );
        if let Some(keyword) = &self.keyword {
            message.push_str(", keyword=");
            message.push_str(keyword);
        }
        if let Some(pointer) = &self.pointer {
            message.push_str(", pointer=");
            message.push_str(pointer);
        }
        message.push(']');
        message
    }

    /// Reports whether a bounded model repair can address this diagnostic.
    pub fn is_model_repairable(&self) -> bool {
        self.category.is_model_repairable()
    }
}

impl fmt::Display for McpSchemaDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

/// Stable identity of one admitted tool-schema generation.
///
/// The fingerprint covers the owning server, the tool name, and the exact schema
/// bytes, so a refreshed schema, a renamed tool, or a schema served by a
/// different server all produce a different generation. Product plan and
/// approval identity use this value to detect that the metadata an approval was
/// granted against has changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct McpSchemaGeneration(String);

impl McpSchemaGeneration {
    /// Derives the stable generation fingerprint for one tool schema.
    pub fn derive(server_id: &str, tool_name: &str, schema_json: &str) -> Self {
        let mut digest = Sha256::new();
        digest.update(MCP_SCHEMA_GENERATION_PREFIX.as_bytes());
        for component in [server_id, tool_name, schema_json] {
            digest.update((component.len() as u64).to_be_bytes());
            digest.update(component.as_bytes());
        }
        let digest = digest.finalize();
        let mut hex = String::with_capacity(MCP_SCHEMA_GENERATION_DIGEST_HEX_CHARS);
        for byte in digest {
            hex.push(hex_digit(byte >> 4));
            hex.push(hex_digit(byte & 0x0f));
        }
        Self(format!("{MCP_SCHEMA_GENERATION_PREFIX}:{hex}"))
    }

    /// Returns the bounded generation string carried by plans and requests.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the owned bounded generation string.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for McpSchemaGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Reports whether one string is a bounded MCP schema generation.
///
/// Product execution identity uses this predicate so an unstamped or malformed
/// plan can never be treated as a schema-checked plan.
pub fn is_mcp_schema_generation(value: &str) -> bool {
    let Some(digest) = value
        .strip_prefix(MCP_SCHEMA_GENERATION_PREFIX)
        .and_then(|rest| rest.strip_prefix(':'))
    else {
        return false;
    };
    digest.len() == MCP_SCHEMA_GENERATION_DIGEST_HEX_CHARS
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Returns one lowercase hexadecimal digit for a nibble value.
fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        _ => char::from(b'a' + (nibble - 10)),
    }
}

/// Explicit work limits for schema admission and instance validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSchemaLimits {
    /// Maximum admitted tool-schema bytes.
    pub max_schema_bytes: usize,
    /// Maximum admitted tool-schema nesting depth.
    pub max_schema_depth: usize,
    /// Maximum admitted tool-schema nodes, which bounds compilation work.
    pub max_schema_nodes: usize,
    /// Maximum admitted reference keywords in one tool schema.
    pub max_references: usize,
    /// Maximum admitted bytes for one same-document reference string.
    pub max_reference_bytes: usize,
    /// Maximum admitted tool-argument bytes.
    pub max_instance_bytes: usize,
    /// Maximum admitted tool-argument nesting depth.
    pub max_instance_depth: usize,
    /// Maximum admitted tool-argument nodes, which bounds validation work.
    pub max_instance_nodes: usize,
    /// Maximum compiled tool schemas retained per validator.
    pub max_cached_schemas: usize,
    /// Maximum regular-expression backtracking steps per `pattern` assertion.
    pub regex_backtrack_limit: usize,
    /// Maximum compiled regular-expression size in bytes per `pattern` assertion.
    pub regex_size_limit: usize,
}

impl Default for McpSchemaLimits {
    fn default() -> Self {
        Self {
            max_schema_bytes: DEFAULT_MCP_SCHEMA_MAX_BYTES,
            max_schema_depth: DEFAULT_MCP_SCHEMA_MAX_DEPTH,
            max_schema_nodes: DEFAULT_MCP_SCHEMA_MAX_NODES,
            max_references: DEFAULT_MCP_SCHEMA_MAX_REFERENCES,
            max_reference_bytes: DEFAULT_MCP_SCHEMA_MAX_REFERENCE_BYTES,
            max_instance_bytes: DEFAULT_MCP_INSTANCE_MAX_BYTES,
            max_instance_depth: DEFAULT_MCP_INSTANCE_MAX_DEPTH,
            max_instance_nodes: DEFAULT_MCP_INSTANCE_MAX_NODES,
            max_cached_schemas: DEFAULT_MCP_SCHEMA_CACHE_CAPACITY,
            regex_backtrack_limit: DEFAULT_MCP_SCHEMA_REGEX_BACKTRACK_LIMIT,
            regex_size_limit: DEFAULT_MCP_SCHEMA_REGEX_SIZE_LIMIT,
        }
    }
}

impl McpSchemaLimits {
    /// Returns these limits with a different compiled-schema cache capacity.
    ///
    /// A capacity of zero would make every admission immediately unusable, so it
    /// is raised to one entry.
    pub fn with_cache_capacity(mut self, capacity: usize) -> Self {
        self.max_cached_schemas = capacity.max(1);
        self
    }
}

/// One compiled tool schema retained in the bounded cache.
#[derive(Debug)]
struct CachedToolSchema {
    server_id: String,
    validator: jsonschema::Validator,
}

/// Bounded, I/O-free admission and instance-validation owner for MCP tool schemas.
#[derive(Debug)]
pub struct McpSchemaValidator {
    limits: McpSchemaLimits,
    cache: BTreeMap<McpSchemaGeneration, CachedToolSchema>,
    recency: VecDeque<McpSchemaGeneration>,
    compilations: u64,
    cache_hits: u64,
    evictions: u64,
}

impl Default for McpSchemaValidator {
    fn default() -> Self {
        Self::new(McpSchemaLimits::default())
    }
}

impl McpSchemaValidator {
    /// Creates a validator with explicit limits.
    pub fn new(limits: McpSchemaLimits) -> Self {
        Self {
            limits: McpSchemaLimits {
                max_cached_schemas: limits.max_cached_schemas.max(1),
                ..limits
            },
            cache: BTreeMap::new(),
            recency: VecDeque::new(),
            compilations: 0,
            cache_hits: 0,
            evictions: 0,
        }
    }

    /// Returns the active limits.
    pub fn limits(&self) -> &McpSchemaLimits {
        &self.limits
    }

    /// Returns the number of compiled schemas currently cached.
    pub fn cached_schema_count(&self) -> usize {
        self.cache.len()
    }

    /// Returns how many schemas this validator compiled.
    pub fn compilation_count(&self) -> u64 {
        self.compilations
    }

    /// Returns how many admissions reused a cached compilation.
    pub fn cache_hit_count(&self) -> u64 {
        self.cache_hits
    }

    /// Returns how many cached compilations bounded eviction removed.
    pub fn eviction_count(&self) -> u64 {
        self.evictions
    }

    /// Admits one tool schema and returns its stable generation fingerprint.
    ///
    /// Admission parses, bounds, screens, and compiles the schema. A valid
    /// supported schema is admitted and cached; an invalid or unsupported schema
    /// returns a bounded diagnostic and leaves the tool unavailable.
    pub fn admit_tool_schema(
        &mut self,
        server_id: &str,
        tool_name: &str,
        schema_json: &str,
    ) -> Result<McpSchemaGeneration, McpSchemaDiagnostic> {
        let generation = McpSchemaGeneration::derive(server_id, tool_name, schema_json);
        if self.cache.contains_key(&generation) {
            self.cache_hits = self.cache_hits.saturating_add(1);
            self.touch(&generation);
            return Ok(generation);
        }
        let schema = admit_schema_document(schema_json, &self.limits)?;
        let validator = compile_schema(&schema, &self.limits)?;
        self.compilations = self.compilations.saturating_add(1);
        self.insert(
            generation.clone(),
            CachedToolSchema {
                server_id: server_id.to_string(),
                validator,
            },
        );
        Ok(generation)
    }

    /// Validates one tool call's arguments against the currently selected schema.
    ///
    /// The schema is admitted (or reused from the cache) first, then the
    /// arguments are bounded and asserted. The returned generation is the exact
    /// schema generation the arguments were checked against, so callers can bind
    /// approval and execution identity to it atomically.
    pub fn validate_arguments(
        &mut self,
        server_id: &str,
        tool_name: &str,
        schema_json: &str,
        arguments_json: &str,
    ) -> Result<McpSchemaGeneration, McpSchemaDiagnostic> {
        let generation = self.admit_tool_schema(server_id, tool_name, schema_json)?;
        let diagnostic = match self.cache.get(&generation) {
            Some(entry) => {
                let instance = admit_instance(arguments_json, &self.limits)?;
                entry
                    .validator
                    .iter_errors(&instance)
                    .next()
                    .map(|error| validation_diagnostic(&error))
            }
            // Unreachable in practice: admission just inserted this generation.
            // Treating it as unusable keeps unchecked arguments from being
            // accepted if that invariant is ever broken.
            None => Some(McpSchemaDiagnostic::new(
                McpSchemaFailure::InvalidSchema,
                None,
                None,
            )),
        };
        self.touch(&generation);
        match diagnostic {
            None => Ok(generation),
            Some(diagnostic) => Err(diagnostic),
        }
    }

    /// Drops every cached compilation owned by one server.
    ///
    /// MCP metadata refreshes call this so a pre-refresh compilation can never
    /// answer a post-refresh plan or approval check.
    pub fn invalidate_server(&mut self, server_id: &str) -> usize {
        let stale = self
            .cache
            .iter()
            .filter(|(_, entry)| entry.server_id == server_id)
            .map(|(generation, _)| generation.clone())
            .collect::<Vec<_>>();
        let removed = stale.len();
        for generation in stale {
            self.cache.remove(&generation);
        }
        self.recency
            .retain(|generation| self.cache.contains_key(generation));
        removed
    }

    /// Drops every cached compilation.
    pub fn invalidate_all(&mut self) {
        self.cache.clear();
        self.recency.clear();
    }

    /// Inserts one compiled schema and applies bounded least-recently-used eviction.
    fn insert(&mut self, generation: McpSchemaGeneration, entry: CachedToolSchema) {
        self.cache.insert(generation.clone(), entry);
        self.recency.push_back(generation);
        while self.cache.len() > self.limits.max_cached_schemas {
            let Some(evicted) = self.recency.pop_front() else {
                break;
            };
            if self.cache.remove(&evicted).is_some() {
                self.evictions = self.evictions.saturating_add(1);
            }
        }
    }

    /// Marks one cached generation as most recently used.
    fn touch(&mut self, generation: &McpSchemaGeneration) {
        if let Some(position) = self.recency.iter().position(|entry| entry == generation) {
            self.recency.remove(position);
        }
        self.recency.push_back(generation.clone());
    }
}

/// Parses, bounds, and screens one tool-schema document.
fn admit_schema_document(
    schema_json: &str,
    limits: &McpSchemaLimits,
) -> Result<Value, McpSchemaDiagnostic> {
    if schema_json.len() > limits.max_schema_bytes {
        return Err(McpSchemaDiagnostic::new(
            McpSchemaFailure::SchemaTooLarge,
            None,
            None,
        ));
    }
    let schema = serde_json::from_str::<Value>(schema_json)
        .map_err(|_| McpSchemaDiagnostic::new(McpSchemaFailure::SchemaNotJson, None, None))?;
    let object = schema
        .as_object()
        .ok_or_else(|| McpSchemaDiagnostic::new(McpSchemaFailure::SchemaNotObject, None, None))?;
    if object
        .get("type")
        .is_some_and(|schema_type| !type_union_admits_object(schema_type))
    {
        return Err(McpSchemaDiagnostic::new(
            McpSchemaFailure::SchemaRootNotObject,
            Some("type"),
            None,
        ));
    }
    measure(
        &schema,
        limits.max_schema_depth,
        limits.max_schema_nodes,
        McpSchemaFailure::SchemaTooDeep,
        McpSchemaFailure::SchemaTooComplex,
    )?;
    screen_schema_keywords(&schema, limits)?;
    Ok(schema)
}

/// Reports whether one `type` value admits JSON objects.
///
/// A malformed `type` value is left to dialect compilation so the resulting
/// diagnostic stays owned by the schema validator.
fn type_union_admits_object(schema_type: &Value) -> bool {
    match schema_type {
        Value::String(name) => name == "object",
        Value::Array(names) => names.iter().any(|name| name.as_str() == Some("object")),
        _ => true,
    }
}

/// Bounds one JSON value by nesting depth and node count.
fn measure(
    value: &Value,
    max_depth: usize,
    max_nodes: usize,
    too_deep: McpSchemaFailure,
    too_complex: McpSchemaFailure,
) -> Result<(), McpSchemaDiagnostic> {
    let mut stack = vec![(value, 1usize)];
    let mut nodes = 0usize;
    while let Some((node, depth)) = stack.pop() {
        if depth > max_depth {
            return Err(McpSchemaDiagnostic::new(too_deep, None, None));
        }
        nodes = nodes.saturating_add(1);
        if nodes > max_nodes {
            return Err(McpSchemaDiagnostic::new(too_complex, None, None));
        }
        match node {
            Value::Object(map) => {
                for child in map.values() {
                    stack.push((child, depth + 1));
                }
            }
            Value::Array(items) => {
                for child in items {
                    stack.push((child, depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Screens every schema keyword that could cause a fetch or a dialect switch.
fn screen_schema_keywords(
    schema: &Value,
    limits: &McpSchemaLimits,
) -> Result<(), McpSchemaDiagnostic> {
    let mut stack = vec![schema];
    let mut references = 0usize;
    while let Some(node) = stack.pop() {
        let Value::Object(map) = node else {
            continue;
        };
        for (keyword, child) in map {
            match keyword.as_str() {
                "$schema" => {
                    if child.as_str() != Some(MCP_SCHEMA_SUPPORTED_DIALECT) {
                        return Err(McpSchemaDiagnostic::new(
                            McpSchemaFailure::UnsupportedDialect,
                            Some("$schema"),
                            None,
                        ));
                    }
                }
                "$ref" | "$dynamicRef" => {
                    references = references.saturating_add(1);
                    let supported = child
                        .as_str()
                        .is_some_and(|reference| is_supported_reference(reference, limits));
                    if !supported {
                        return Err(McpSchemaDiagnostic::new(
                            McpSchemaFailure::UnsupportedReference,
                            Some(keyword),
                            None,
                        ));
                    }
                }
                "$recursiveRef" => {
                    return Err(McpSchemaDiagnostic::new(
                        McpSchemaFailure::UnsupportedReference,
                        Some(keyword),
                        None,
                    ));
                }
                _ => {}
            }
            push_schema_positions(keyword, child, &mut stack);
        }
    }
    if references > limits.max_references {
        return Err(McpSchemaDiagnostic::new(
            McpSchemaFailure::SchemaTooComplex,
            Some("$ref"),
            None,
        ));
    }
    Ok(())
}

/// Reports whether one reference string is a bounded same-document reference.
fn is_supported_reference(reference: &str, limits: &McpSchemaLimits) -> bool {
    // The whole-document fragment and `/`-prefixed JSON Pointer fragments resolve
    // inside the document itself. Plain-name anchors are rejected instead of
    // resolved, so the admitted reference form stays exactly the JSON Pointer
    // form this module documents.
    let is_pointer_fragment = reference == "#" || reference.starts_with("#/");
    is_pointer_fragment && reference.len() <= limits.max_reference_bytes
}

/// Schema-position keywords whose value is that keyword's own subschema.
const MCP_SCHEMA_SINGLE_SCHEMA_KEYWORDS: &[&str] = &[
    "additionalProperties",
    "items",
    "contains",
    "not",
    "if",
    "then",
    "else",
    "propertyNames",
    "unevaluatedProperties",
    "unevaluatedItems",
];

/// Schema-position keywords whose value is an array of subschemas.
const MCP_SCHEMA_SCHEMA_ARRAY_KEYWORDS: &[&str] = &["allOf", "anyOf", "oneOf", "prefixItems"];

/// Schema-position keywords whose value maps names to subschemas.
const MCP_SCHEMA_SCHEMA_MAP_KEYWORDS: &[&str] = &[
    "properties",
    "patternProperties",
    "dependentSchemas",
    "$defs",
];

/// Queues one keyword's subschema positions for screening.
///
/// Only applied subschemas are queued. Annotation and data keywords (`title`,
/// `description`, `examples`, `default`, `const`, `enum`, `$comment`,
/// `deprecated`, `readOnly`, `writeOnly`) and `x-*` extensions hold values rather
/// than subschemas, so a `$ref`-shaped string inside them is data and can neither
/// withdraw the tool nor consume the reference budget.
fn push_schema_positions<'a>(keyword: &str, child: &'a Value, stack: &mut Vec<&'a Value>) {
    if MCP_SCHEMA_SCHEMA_MAP_KEYWORDS.contains(&keyword) {
        if let Value::Object(subschemas) = child {
            stack.extend(subschemas.values());
        }
        return;
    }
    if MCP_SCHEMA_SCHEMA_ARRAY_KEYWORDS.contains(&keyword) {
        if let Value::Array(subschemas) = child {
            stack.extend(subschemas.iter());
        }
        return;
    }
    if MCP_SCHEMA_SINGLE_SCHEMA_KEYWORDS.contains(&keyword) {
        match child {
            // `items` admitted an array of subschemas in earlier drafts, so each
            // element keeps its schema position.
            Value::Array(subschemas) => stack.extend(subschemas.iter()),
            _ => stack.push(child),
        }
    }
}

/// Compiles one screened schema with the admitted dialect and local-only options.
fn compile_schema(
    schema: &Value,
    limits: &McpSchemaLimits,
) -> Result<jsonschema::Validator, McpSchemaDiagnostic> {
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .offline()
        .should_validate_formats(false)
        .without_content_media_type_support("application/json")
        .without_content_encoding_support("base64")
        .with_pattern_options(
            jsonschema::PatternOptions::fancy_regex()
                .backtrack_limit(limits.regex_backtrack_limit)
                .size_limit(limits.regex_size_limit),
        )
        .build(schema)
        .map_err(|error| compilation_diagnostic(&error))
}

/// Converts one compilation failure into a bounded diagnostic.
fn compilation_diagnostic(error: &jsonschema::ValidationError<'_>) -> McpSchemaDiagnostic {
    let keyword = error.kind().keyword();
    let category = if keyword.to_ascii_lowercase().contains("ref") {
        McpSchemaFailure::UnsupportedReference
    } else {
        McpSchemaFailure::InvalidSchema
    };
    McpSchemaDiagnostic {
        category,
        keyword: Some(bounded_diagnostic_keyword(keyword)),
        pointer: Some(bounded_diagnostic_pointer(error.schema_path().as_str())),
    }
}

/// Converts one instance assertion failure into a bounded diagnostic.
fn validation_diagnostic(error: &jsonschema::ValidationError<'_>) -> McpSchemaDiagnostic {
    McpSchemaDiagnostic {
        category: McpSchemaFailure::InstanceViolatesSchema,
        keyword: Some(bounded_diagnostic_keyword(error.kind().keyword())),
        pointer: Some(bounded_diagnostic_pointer(error.instance_path().as_str())),
    }
}

/// Parses and bounds one tool-argument instance.
fn admit_instance(
    arguments_json: &str,
    limits: &McpSchemaLimits,
) -> Result<Value, McpSchemaDiagnostic> {
    if arguments_json.len() > limits.max_instance_bytes {
        return Err(McpSchemaDiagnostic::new(
            McpSchemaFailure::InstanceTooLarge,
            None,
            None,
        ));
    }
    let instance = serde_json::from_str::<Value>(arguments_json)
        .map_err(|_| McpSchemaDiagnostic::new(McpSchemaFailure::InstanceNotJson, None, None))?;
    if !instance.is_object() {
        return Err(McpSchemaDiagnostic::new(
            McpSchemaFailure::InstanceRootNotObject,
            None,
            None,
        ));
    }
    measure(
        &instance,
        limits.max_instance_depth,
        limits.max_instance_nodes,
        McpSchemaFailure::InstanceTooDeep,
        McpSchemaFailure::InstanceTooComplex,
    )?;
    Ok(instance)
}

/// Reduces one untrusted token to bounded printable ASCII.
fn sanitized_token(raw: &str, max_bytes: usize) -> String {
    let mut sanitized = String::new();
    for character in raw.chars() {
        if sanitized.len() >= max_bytes {
            sanitized.push('~');
            break;
        }
        let printable = character.is_ascii_graphic() && character != '"' && character != '\\';
        sanitized.push(if printable { character } else { '_' });
    }
    sanitized
}

/// Reduces one untrusted pointer to a bounded set of bounded segments.
fn bounded_diagnostic_pointer(raw: &str) -> String {
    let mut pointer = String::new();
    for (kept, segment) in raw
        .split('/')
        .filter(|segment| !segment.is_empty())
        .enumerate()
    {
        if kept == MCP_SCHEMA_MAX_DIAGNOSTIC_POINTER_SEGMENTS {
            pointer.push_str("/~more");
            break;
        }
        pointer.push('/');
        pointer.push_str(&sanitized_token(
            segment,
            MCP_SCHEMA_MAX_DIAGNOSTIC_POINTER_SEGMENT_BYTES,
        ));
    }
    if pointer.is_empty() {
        pointer.push('/');
    }
    if pointer.len() > MCP_SCHEMA_MAX_DIAGNOSTIC_POINTER_BYTES {
        pointer = sanitized_token(&pointer, MCP_SCHEMA_MAX_DIAGNOSTIC_POINTER_BYTES - 1);
        pointer.push('~');
    }
    pointer
}

/// Reduces one canonical keyword to bounded printable ASCII.
fn bounded_diagnostic_keyword(raw: &str) -> String {
    sanitized_token(raw, MCP_SCHEMA_MAX_DIAGNOSTIC_KEYWORD_BYTES)
}
