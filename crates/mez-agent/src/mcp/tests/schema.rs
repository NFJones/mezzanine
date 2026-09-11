//! Bounded MCP schema admission, assertion, diagnostic, and cache tests.
//!
//! These tests protect the single lower-crate schema owner: the admitted
//! dialect and assertion set, the no-I/O reference screen, the bounded
//! resource limits, the secret-free diagnostics, and the compiled-schema cache.

use super::super::{
    DEFAULT_MCP_INSTANCE_MAX_BYTES, DEFAULT_MCP_SCHEMA_MAX_BYTES, DEFAULT_MCP_SCHEMA_MAX_DEPTH,
    DEFAULT_MCP_SCHEMA_MAX_NODES, MCP_SCHEMA_GENERATION_DIGEST_HEX_CHARS,
    MCP_SCHEMA_GENERATION_PREFIX, McpSchemaFailure, McpSchemaGeneration, McpSchemaLimits,
    McpSchemaValidator, is_mcp_schema_generation,
};

const SERVER: &str = "fs";
const TOOL: &str = "read_file";

/// Builds one bounded argument fixture with `count` repeated instance nodes.
fn repeated_nodes(count: usize) -> String {
    vec!["1"; count].join(",")
}

/// Verifies every admitted assertion family accepts conforming arguments and
/// rejects one violating argument as a bounded, model-repairable diagnostic.
#[test]
fn supported_keyword_matrix_accepts_and_rejects_arguments() {
    let cases = [
        (
            r#"{"type":"object","properties":{"kind":{"enum":["a","b"]}}}"#,
            r#"{"kind":"a"}"#,
            r#"{"kind":"c"}"#,
        ),
        (
            r#"{"type":"object","properties":{"kind":{"const":"a"}}}"#,
            r#"{"kind":"a"}"#,
            r#"{"kind":"b"}"#,
        ),
        (
            r#"{"type":"object","properties":{"a":{"type":"string"}},"additionalProperties":false}"#,
            r#"{"a":"x"}"#,
            r#"{"a":"x","b":1}"#,
        ),
        (
            r#"{"type":"object","properties":{"outer":{"type":"object","required":["inner"],"properties":{"inner":{"type":"string"}}}}}"#,
            r#"{"outer":{"inner":"x"}}"#,
            r#"{"outer":{}}"#,
        ),
        (
            r#"{"type":"object","properties":{"items":{"type":"array","minItems":1,"maxItems":2,"items":{"type":"string"}}}}"#,
            r#"{"items":["a"]}"#,
            r#"{"items":["a","b","c"]}"#,
        ),
        (
            r#"{"type":"object","properties":{"v":{"anyOf":[{"type":"string"},{"type":"integer"}]}}}"#,
            r#"{"v":1}"#,
            r#"{"v":true}"#,
        ),
        (
            r#"{"type":"object","properties":{"v":{"oneOf":[{"type":"string"},{"type":"integer"}]}}}"#,
            r#"{"v":"x"}"#,
            r#"{"v":true}"#,
        ),
        (
            r#"{"type":"object","allOf":[{"required":["a"]},{"properties":{"a":{"type":"string"}}}]}"#,
            r#"{"a":"x"}"#,
            r#"{}"#,
        ),
        (
            r#"{"type":"object","properties":{"v":{"not":{"type":"string"}}}}"#,
            r#"{"v":1}"#,
            r#"{"v":"x"}"#,
        ),
        (
            r#"{"type":"object","properties":{"v":{"type":["string","null"]}}}"#,
            r#"{"v":null}"#,
            r#"{"v":1}"#,
        ),
        (
            r#"{"type":"object","properties":{"name":{"type":"string","pattern":"^[a-z]+$"}}}"#,
            r#"{"name":"abc"}"#,
            r#"{"name":"ABC"}"#,
        ),
        (
            r#"{"type":"object","properties":{"n":{"type":"integer","minimum":1,"maximum":3,"multipleOf":1}}}"#,
            r#"{"n":2}"#,
            r#"{"n":4}"#,
        ),
        (
            r#"{"type":"object","properties":{"s":{"type":"string","minLength":2,"maxLength":4}}}"#,
            r#"{"s":"ab"}"#,
            r#"{"s":"a"}"#,
        ),
        (
            r#"{"type":"object","properties":{"o":{"type":"object","minProperties":1,"maxProperties":2}}}"#,
            r#"{"o":{"a":1}}"#,
            r#"{"o":{}}"#,
        ),
    ];

    let mut validator = McpSchemaValidator::default();
    for (schema, accepted, rejected) in cases {
        validator
            .validate_arguments(SERVER, TOOL, schema, accepted)
            .unwrap_or_else(|diagnostic| {
                panic!("{schema} rejected conforming arguments {accepted}: {diagnostic}")
            });
        let diagnostic = validator
            .validate_arguments(SERVER, TOOL, schema, rejected)
            .expect_err(&format!("{schema} accepted violating arguments {rejected}"));
        assert_eq!(
            diagnostic.category(),
            McpSchemaFailure::InstanceViolatesSchema,
            "{schema}"
        );
        assert!(diagnostic.keyword().is_some(), "{schema}");
        assert!(diagnostic.is_model_repairable(), "{schema}");
    }
}

/// Verifies `format` and content keywords stay annotations, so an approximate
/// email address or encoded body never fails an otherwise valid call.
#[test]
fn format_and_content_keywords_are_annotations() {
    let mut validator = McpSchemaValidator::default();
    let schema = r#"{"type":"object","properties":{"email":{"type":"string","format":"email"},"body":{"type":"string","contentMediaType":"application/json","contentEncoding":"base64"}}}"#;
    validator
        .validate_arguments(
            SERVER,
            TOOL,
            schema,
            r#"{"email":"not-an-email","body":"not base64"}"#,
        )
        .unwrap();
}

/// Verifies the applicator vocabulary this validator documents as supported is
/// enforced, including the `unevaluated` keywords that bound open objects and
/// arrays.
#[test]
fn unevaluated_applicators_are_enforced() {
    let mut validator = McpSchemaValidator::default();
    let object =
        r#"{"type":"object","properties":{"a":{"type":"string"}},"unevaluatedProperties":false}"#;
    validator
        .validate_arguments(SERVER, TOOL, object, r#"{"a":"x"}"#)
        .unwrap();
    let diagnostic = validator
        .validate_arguments(SERVER, TOOL, object, r#"{"a":"x","b":1}"#)
        .unwrap_err();
    assert_eq!(
        diagnostic.category(),
        McpSchemaFailure::InstanceViolatesSchema
    );
    assert_eq!(diagnostic.keyword(), Some("unevaluatedProperties"));

    let array = r#"{"type":"object","properties":{"items":{"type":"array","prefixItems":[{"type":"string"}],"unevaluatedItems":false}}}"#;
    validator
        .validate_arguments(SERVER, TOOL, array, r#"{"items":["a"]}"#)
        .unwrap();
    let diagnostic = validator
        .validate_arguments(SERVER, TOOL, array, r#"{"items":["a",1]}"#)
        .unwrap_err();
    assert_eq!(
        diagnostic.category(),
        McpSchemaFailure::InstanceViolatesSchema
    );
    assert_eq!(diagnostic.keyword(), Some("unevaluatedItems"));
}

/// Verifies annotation and extension keywords are harmless metadata: they never
/// change admission or assertion outcomes and never reach diagnostics.
#[test]
fn annotations_and_extensions_are_harmless_metadata() {
    let mut validator = McpSchemaValidator::default();
    let schema = r#"{
        "$schema":"https://json-schema.org/draft/2020-12/schema",
        "title":"Read file",
        "description":"Reads one file",
        "examples":[{"path":"README.md"}],
        "default":{"path":"README.md"},
        "$comment":"server-side note",
        "deprecated":false,
        "x-vendor-extension":{"secret":"server-side"},
        "type":"object",
        "properties":{"path":{"type":"string","title":"Path","examples":["a"],"default":"a","x-note":"n"}},
        "required":["path"]
    }"#;
    validator
        .validate_arguments(SERVER, TOOL, schema, r#"{"path":"README.md"}"#)
        .unwrap();

    let diagnostic = validator
        .validate_arguments(SERVER, TOOL, schema, r#"{}"#)
        .unwrap_err();
    assert_eq!(diagnostic.keyword(), Some("required"));
    let message = diagnostic.message();
    assert!(!message.contains("server-side"), "{message}");
    assert!(!message.contains("Read file"), "{message}");
}

/// Verifies screening stops at annotation and extension values: a `$schema`,
/// `$ref`, `$dynamicRef`, or `$recursiveRef` shaped string inside `examples`,
/// `default`, `const`, `enum`, `description`, `$comment`, or an `x-*` extension
/// is data, so the tool stays admitted and the reference budget is untouched.
#[test]
fn annotation_payloads_are_not_screened_as_schema_keywords() {
    let mut validator = McpSchemaValidator::default();
    for schema in [
        r#"{"type":"object","examples":[{"$ref":"https://example.com/schema.json"}]}"#,
        r#"{"type":"object","default":{"$schema":"http://json-schema.org/draft-07/schema#"}}"#,
        r#"{"type":"object","const":{"$recursiveRef":"https://example.com/schema.json"}}"#,
        r#"{"type":"object","properties":{"kind":{"enum":[{"$ref":"file:///etc/passwd"}]}}}"#,
        r#"{"type":"object","properties":{"kind":{"type":"string","x-vendor":{"$ref":"https://example.com/schema.json"}}}}"#,
        r#"{"type":"object","description":"$ref https://example.com/schema.json","$comment":"$dynamicRef #anchor"}"#,
    ] {
        validator
            .admit_tool_schema(SERVER, TOOL, schema)
            .unwrap_or_else(|diagnostic| {
                panic!("annotation payload withdrew {schema}: {diagnostic}")
            });
    }

    // Real schema positions stay screened, including inside nested applicators.
    for schema in [
        r#"{"type":"object","properties":{"a":{"$ref":"https://example.com/schema.json"}}}"#,
        r#"{"type":"object","$defs":{"a":{"$ref":"https://example.com/schema.json"}}}"#,
        r#"{"type":"object","allOf":[{"$dynamicRef":"https://example.com/schema.json"}]}"#,
        r#"{"type":"object","unevaluatedProperties":{"$ref":"https://example.com/schema.json"}}"#,
    ] {
        assert_eq!(
            validator
                .admit_tool_schema(SERVER, TOOL, schema)
                .unwrap_err()
                .category(),
            McpSchemaFailure::UnsupportedReference,
            "{schema}"
        );
    }
    assert_eq!(
        validator
            .admit_tool_schema(
                SERVER,
                TOOL,
                r#"{"type":"object","properties":{"a":{"$schema":"http://json-schema.org/draft-07/schema#"}}}"#
            )
            .unwrap_err()
            .category(),
        McpSchemaFailure::UnsupportedDialect
    );
}

/// Verifies a schema that declares a different dialect is rejected instead of
/// being silently reinterpreted under the admitted 2020-12 semantics.
#[test]
fn unsupported_dialect_is_rejected() {
    let mut validator = McpSchemaValidator::default();
    let diagnostic = validator
        .admit_tool_schema(
            SERVER,
            TOOL,
            r#"{"$schema":"http://json-schema.org/draft-07/schema#","type":"object"}"#,
        )
        .unwrap_err();
    assert_eq!(diagnostic.category(), McpSchemaFailure::UnsupportedDialect);
    assert_eq!(diagnostic.keyword(), Some("$schema"));
    assert!(!diagnostic.is_model_repairable());

    let generation = validator
        .admit_tool_schema(
            SERVER,
            TOOL,
            r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object"}"#,
        )
        .unwrap();
    assert!(is_mcp_schema_generation(generation.as_str()));
}

/// Verifies remote, file, relative, oversized, and recursive references are
/// rejected during screening without performing any I/O, while a bounded
/// same-document JSON Pointer reference is admitted and still enforced.
#[test]
fn references_are_screened_without_io() {
    let mut validator = McpSchemaValidator::default();
    for reference in [
        "https://example.test/schema.json",
        "http://127.0.0.1:9/schema.json",
        "file:///etc/passwd",
        "other.json#/definitions/kind",
    ] {
        let schema =
            format!(r#"{{"type":"object","properties":{{"a":{{"$ref":"{reference}"}}}}}}"#);
        let diagnostic = validator
            .admit_tool_schema(SERVER, TOOL, &schema)
            .expect_err(&format!("{reference} was admitted"));
        assert_eq!(
            diagnostic.category(),
            McpSchemaFailure::UnsupportedReference,
            "{reference}"
        );
        assert_eq!(diagnostic.keyword(), Some("$ref"));
    }

    let oversized = format!(r##"{{"$ref":"#{}"}}"##, "a".repeat(600));
    assert_eq!(
        validator
            .admit_tool_schema(SERVER, TOOL, &oversized)
            .unwrap_err()
            .category(),
        McpSchemaFailure::UnsupportedReference
    );
    let recursive = r##"{"type":"object","properties":{"a":{"$recursiveRef":"#"}}}"##;
    assert_eq!(
        validator
            .admit_tool_schema(SERVER, TOOL, recursive)
            .unwrap_err()
            .category(),
        McpSchemaFailure::UnsupportedReference
    );

    let same_document = r##"{"type":"object","properties":{"kind":{"$ref":"#/$defs/kind"}},"$defs":{"kind":{"enum":["a","b"]}}}"##;
    validator
        .validate_arguments(SERVER, TOOL, same_document, r#"{"kind":"a"}"#)
        .unwrap();
    assert_eq!(
        validator
            .validate_arguments(SERVER, TOOL, same_document, r#"{"kind":"c"}"#)
            .unwrap_err()
            .category(),
        McpSchemaFailure::InstanceViolatesSchema
    );
}

/// Verifies only the whole-document fragment and `/`-prefixed JSON Pointer
/// fragments are admitted: plain-name anchors are rejected as unsupported rather
/// than resolved, while the documented pointer forms still compile and assert.
#[test]
fn references_admit_only_json_pointer_fragments() {
    let mut validator = McpSchemaValidator::default();
    for reference in ["#anchor", "#kind", "#a/b", "#meta"] {
        let schema =
            format!(r#"{{"type":"object","properties":{{"a":{{"$ref":"{reference}"}}}}}}"#);
        let diagnostic = validator
            .admit_tool_schema(SERVER, TOOL, &schema)
            .expect_err(&format!("{reference} was admitted"));
        assert_eq!(
            diagnostic.category(),
            McpSchemaFailure::UnsupportedReference,
            "{reference}"
        );
        assert_eq!(diagnostic.keyword(), Some("$ref"), "{reference}");
    }
    assert_eq!(
        validator
            .admit_tool_schema(SERVER, TOOL, r##"{"type":"object","$dynamicRef":"#meta"}"##)
            .unwrap_err()
            .category(),
        McpSchemaFailure::UnsupportedReference
    );

    validator
        .admit_tool_schema(
            SERVER,
            TOOL,
            r##"{"type":"object","properties":{"self":{"$ref":"#"}}}"##,
        )
        .expect("whole-document fragment");
    let pointer = r##"{"type":"object","properties":{"kind":{"$ref":"#/$defs/kind"}},"$defs":{"kind":{"enum":["a"]}}}"##;
    validator
        .validate_arguments(SERVER, TOOL, pointer, r#"{"kind":"a"}"#)
        .unwrap();
    assert_eq!(
        validator
            .validate_arguments(SERVER, TOOL, pointer, r#"{"kind":"b"}"#)
            .unwrap_err()
            .category(),
        McpSchemaFailure::InstanceViolatesSchema
    );
}

/// Verifies an invalid regular expression is rejected at admission and that
/// every documented resource limit rejects hostile input safely.
#[test]
fn invalid_regex_and_resource_limits_are_rejected() {
    let mut validator = McpSchemaValidator::default();
    let diagnostic = validator
        .admit_tool_schema(
            SERVER,
            TOOL,
            r#"{"type":"object","properties":{"a":{"type":"string","pattern":"("}}}"#,
        )
        .unwrap_err();
    assert_eq!(diagnostic.category(), McpSchemaFailure::InvalidSchema);
    // The dialect rejects a malformed `pattern` through its own bounded
    // meta-schema keyword instead of echoing the offending pattern text.
    assert!(
        diagnostic
            .keyword()
            .is_some_and(|keyword| keyword.len() <= 32)
    );
    assert!(!diagnostic.is_model_repairable());

    let mut bounded = McpSchemaValidator::new(McpSchemaLimits {
        regex_backtrack_limit: 1,
        ..McpSchemaLimits::default()
    });
    let backtracking =
        r#"{"type":"object","properties":{"a":{"type":"string","pattern":"(a+)+$"}}}"#;
    let diagnostic = bounded
        .validate_arguments(
            SERVER,
            TOOL,
            backtracking,
            r#"{"a":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!"}"#,
        )
        .unwrap_err();
    assert_eq!(diagnostic.keyword(), Some("pattern"));
    assert!(diagnostic.is_model_repairable());

    let oversized_schema = format!(
        r#"{{"type":"object","description":"{}"}}"#,
        "x".repeat(DEFAULT_MCP_SCHEMA_MAX_BYTES)
    );
    assert_eq!(
        validator
            .admit_tool_schema(SERVER, TOOL, &oversized_schema)
            .unwrap_err()
            .category(),
        McpSchemaFailure::SchemaTooLarge
    );

    let nested = format!(
        r#"{{"type":"object","properties":{{"a":{}{}}}}}"#,
        "[".repeat(DEFAULT_MCP_SCHEMA_MAX_DEPTH),
        "]".repeat(DEFAULT_MCP_SCHEMA_MAX_DEPTH)
    );
    assert_eq!(
        validator
            .admit_tool_schema(SERVER, TOOL, &nested)
            .unwrap_err()
            .category(),
        McpSchemaFailure::SchemaTooDeep
    );

    let complex_schema = format!(
        r#"{{"type":"object","x":[{}]}}"#,
        repeated_nodes(DEFAULT_MCP_SCHEMA_MAX_NODES + 1)
    );
    assert_eq!(
        validator
            .admit_tool_schema(SERVER, TOOL, &complex_schema)
            .unwrap_err()
            .category(),
        McpSchemaFailure::SchemaTooComplex
    );

    let oversized_arguments = format!(
        r#"{{"a":"{}"}}"#,
        "x".repeat(DEFAULT_MCP_INSTANCE_MAX_BYTES)
    );
    assert_eq!(
        validator
            .validate_arguments(SERVER, TOOL, r#"{"type":"object"}"#, &oversized_arguments)
            .unwrap_err()
            .category(),
        McpSchemaFailure::InstanceTooLarge
    );
    let deeply_nested_arguments = format!(
        r#"{{"a":{}{}}}"#,
        "[".repeat(DEFAULT_MCP_SCHEMA_MAX_DEPTH),
        "]".repeat(DEFAULT_MCP_SCHEMA_MAX_DEPTH)
    );
    assert_eq!(
        validator
            .validate_arguments(
                SERVER,
                TOOL,
                r#"{"type":"object"}"#,
                &deeply_nested_arguments
            )
            .unwrap_err()
            .category(),
        McpSchemaFailure::InstanceTooDeep
    );
    let complex_arguments = format!(
        r#"{{"x":[{}]}}"#,
        repeated_nodes(DEFAULT_MCP_SCHEMA_MAX_NODES + 1)
    );
    assert_eq!(
        validator
            .validate_arguments(SERVER, TOOL, r#"{"type":"object"}"#, &complex_arguments)
            .unwrap_err()
            .category(),
        McpSchemaFailure::InstanceTooComplex
    );
}

/// Verifies root admission separates schema-side faults from repairable
/// instance-side failures.
#[test]
fn root_admission_rejects_non_object_schemas() {
    let mut validator = McpSchemaValidator::default();
    assert_eq!(
        validator
            .admit_tool_schema(SERVER, TOOL, "not-json")
            .unwrap_err()
            .category(),
        McpSchemaFailure::SchemaNotJson
    );
    assert_eq!(
        validator
            .admit_tool_schema(SERVER, TOOL, "[]")
            .unwrap_err()
            .category(),
        McpSchemaFailure::SchemaNotObject
    );
    let diagnostic = validator
        .admit_tool_schema(SERVER, TOOL, r#"{"type":"string"}"#)
        .unwrap_err();
    assert_eq!(diagnostic.category(), McpSchemaFailure::SchemaRootNotObject);
    assert!(diagnostic.category().is_schema_fault());
    assert!(!diagnostic.is_model_repairable());
    validator
        .admit_tool_schema(SERVER, TOOL, r#"{"type":["object","null"]}"#)
        .unwrap();

    let diagnostic = validator
        .validate_arguments(SERVER, TOOL, r#"{"type":"object"}"#, "3")
        .unwrap_err();
    assert_eq!(
        diagnostic.category(),
        McpSchemaFailure::InstanceRootNotObject
    );
    assert!(diagnostic.is_model_repairable());
    assert_eq!(
        validator
            .validate_arguments(SERVER, TOOL, r#"{"type":"object"}"#, "not-json")
            .unwrap_err()
            .category(),
        McpSchemaFailure::InstanceNotJson
    );
}

/// Verifies diagnostics carry only a bounded pointer, keyword, and category and
/// never echo instance values or unbounded untrusted key text.
#[test]
fn diagnostics_never_contain_instance_values() {
    let mut validator = McpSchemaValidator::default();
    let schema = r#"{"type":"object","properties":{"kind":{"enum":["allowed"]}}}"#;
    let secret = "sk-live-0123456789abcdef";
    let diagnostic = validator
        .validate_arguments(SERVER, TOOL, schema, &format!(r#"{{"kind":"{secret}"}}"#))
        .unwrap_err();
    assert_eq!(
        diagnostic.category(),
        McpSchemaFailure::InstanceViolatesSchema
    );
    assert_eq!(diagnostic.keyword(), Some("enum"));
    assert_eq!(diagnostic.pointer(), Some("/kind"));
    let message = diagnostic.message();
    assert!(!message.contains(secret), "{message}");
    assert!(!message.contains("allowed"), "{message}");

    let long_key = "k".repeat(400);
    let diagnostic = validator
        .validate_arguments(
            SERVER,
            TOOL,
            r#"{"type":"object","additionalProperties":false}"#,
            &format!(r#"{{"{long_key}":1}}"#),
        )
        .unwrap_err();
    // Annotating a property with the false schema surfaces as a bounded keyword
    // rather than as unbounded or attacker-controlled key text.
    assert!(
        diagnostic
            .keyword()
            .is_some_and(|keyword| keyword.len() <= 32)
    );
    let pointer = diagnostic.pointer().expect("bounded pointer");
    assert!(pointer.len() <= 128, "{pointer}");
    let message = diagnostic.message();
    assert!(!message.contains(&long_key), "{message}");
}

/// Verifies one tool schema generation is stable for identical bytes and
/// distinct across schema, tool, and server identity.
#[test]
fn schema_generations_are_stable_and_identity_specific() {
    let schema = r#"{"type":"object"}"#;
    let baseline = McpSchemaGeneration::derive(SERVER, TOOL, schema);
    assert_eq!(baseline, McpSchemaGeneration::derive(SERVER, TOOL, schema));
    assert_ne!(
        baseline,
        McpSchemaGeneration::derive(SERVER, "other", schema)
    );
    assert_ne!(baseline, McpSchemaGeneration::derive("other", TOOL, schema));
    assert_ne!(
        baseline,
        McpSchemaGeneration::derive(SERVER, TOOL, r#"{"type":"object","title":"x"}"#)
    );
    assert!(is_mcp_schema_generation(baseline.as_str()));
    assert!(!is_mcp_schema_generation(""));
    assert!(!is_mcp_schema_generation("mcp-schema-v1"));
    assert!(!is_mcp_schema_generation("mcp-schema-v1:ZZZZ"));
    assert_eq!(
        baseline.as_str().len(),
        MCP_SCHEMA_GENERATION_PREFIX.len() + 1 + MCP_SCHEMA_GENERATION_DIGEST_HEX_CHARS
    );
}

/// Verifies generation identity cannot alias across components: the digest input
/// length-prefixes the server, tool, and schema, so adjacent identities that
/// would share one separator-joined stream stay distinct, and the full SHA-256
/// digest is retained.
#[test]
fn schema_generation_identity_cannot_alias_across_components() {
    let schema = r#"{"type":"object"}"#;
    let left = McpSchemaGeneration::derive("a", "b\u{0}c", schema);
    let right = McpSchemaGeneration::derive("a\u{0}b", "c", schema);
    assert_ne!(left, right);
    assert_ne!(
        McpSchemaGeneration::derive("a:b", "c", schema),
        McpSchemaGeneration::derive("a", "b:c", schema)
    );
    assert_ne!(
        McpSchemaGeneration::derive("server", "tool", r#"{"type":"object"}"#),
        McpSchemaGeneration::derive("server", "tool", r#"{"type":"object"}x"#)
    );
    assert!(is_mcp_schema_generation(left.as_str()));
    assert_eq!(
        left.as_str().len(),
        MCP_SCHEMA_GENERATION_PREFIX.len() + 1 + MCP_SCHEMA_GENERATION_DIGEST_HEX_CHARS
    );
}

/// Verifies compiled schemas are reused by generation, evicted under the
/// configured capacity, and dropped when a server's metadata is invalidated.
#[test]
fn compiled_schemas_are_cached_evicted_and_invalidated() {
    let limits = McpSchemaLimits::default().with_cache_capacity(2);
    let mut validator = McpSchemaValidator::new(limits);
    let first = r#"{"type":"object","properties":{"a":{"type":"string"}}}"#;
    let generation = validator.admit_tool_schema(SERVER, TOOL, first).unwrap();
    assert_eq!(validator.compilation_count(), 1);
    assert_eq!(
        validator.admit_tool_schema(SERVER, TOOL, first).unwrap(),
        generation
    );
    assert_eq!(validator.compilation_count(), 1);
    assert_eq!(validator.cache_hit_count(), 1);
    assert_eq!(validator.cached_schema_count(), 1);

    let second = r#"{"type":"object","properties":{"b":{"type":"string"}}}"#;
    validator.admit_tool_schema(SERVER, TOOL, second).unwrap();
    assert_eq!(validator.compilation_count(), 2);

    let third = r#"{"type":"object","properties":{"c":{"type":"string"}}}"#;
    validator.admit_tool_schema(SERVER, TOOL, third).unwrap();
    assert_eq!(validator.cached_schema_count(), 2);
    assert!(validator.eviction_count() >= 1);

    assert!(validator.invalidate_server(SERVER) >= 1);
    assert_eq!(validator.cached_schema_count(), 0);
    assert!(is_mcp_schema_generation(generation.as_str()));
}

/// Verifies one server's invalidation leaves another server's compiled schemas
/// available, because schema generations are server scoped.
#[test]
fn invalidation_is_scoped_to_one_server() {
    let mut validator = McpSchemaValidator::default();
    let schema = r#"{"type":"object"}"#;
    validator.admit_tool_schema(SERVER, TOOL, schema).unwrap();
    validator.admit_tool_schema("other", TOOL, schema).unwrap();
    assert_eq!(validator.invalidate_server(SERVER), 1);
    assert_eq!(validator.cached_schema_count(), 1);
    assert_eq!(validator.compilation_count(), 2);
    validator.admit_tool_schema(SERVER, TOOL, schema).unwrap();
    assert_eq!(validator.compilation_count(), 3);
    assert_eq!(validator.cached_schema_count(), 2);
}
