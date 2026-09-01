# Protocol conventions

## Purpose

Define terminology and data conventions shared by the Mezzanine protocol
references. Protocol-specific transport, authentication, authorization, and
error rules remain with their owning protocol.

## JSON and text

Protocol payloads are UTF-8 JSON values. JSON object fields are case-sensitive.
Text media types use standard HTTP-style media types; plain text is written as
`text/plain; charset=utf-8` and JSON payloads as `application/json` unless a
protocol defines a more specific type.

Timestamps are RFC 3339 strings with an offset unless a protocol explicitly
permits a documented monotonic form. Byte lengths are octet lengths, not
Unicode character counts.

## Identities and references

Session, client, pane, agent, action, and message IDs are opaque stable strings
within the scope defined by their protocol. Send an exact domain ID back
unchanged; do not parse prefixes, rely on ordering, or create an ID on behalf
of a runtime-owned identity.

JSON-RPC request IDs are non-null strings or integers, and responses return the
same value unchanged. Ordered event IDs and event cutoffs are non-negative
integers; replay semantics use their numeric ordering. Do not treat either
category as a domain-object identity.

Where a control method accepts a target object, exact IDs take precedence over
names or indexes. Ambiguous targets fail rather than selecting an arbitrary
object.

## Extensions and forward compatibility

Protocol-defined extension data belongs under an `extensions` object unless the
owning contract says otherwise. Receivers must ignore extension keys they do
not understand. New message types use a reverse-DNS or URI-like namespace.

Unknown required fields, malformed envelopes, unsupported versions, and
out-of-range limits are errors. A client should use the negotiated capability
set rather than probing unsupported mutations.

## Security boundary

All received protocol data is untrusted input. Authentication material, bearer
tokens, credentials, and secret-bearing configuration must never be echoed in
responses, notifications, diagnostics, transcripts, or audit output. A local
transport does not itself grant authority: role, policy, approval, and target
scope remain enforced by the receiving service.

## Related pages

- [`mezctl/2` and `mezctl/3` control JSON-RPC](control-json-rpc.md)
- [`maap/1` action protocol](maap.md)
- [`mmp/1` local messages](mmp.md)
