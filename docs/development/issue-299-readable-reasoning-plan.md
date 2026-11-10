# Issue 299: Readable Reasoning Qualification Plan

Status: proposed; payload authority changes require independent S6 plan review.
Base: 45e91c6c5b677e73c1ff079203fa0259c52a0fb1. No schema migration.

## Verified Sources

- `src/history/items.rs`: marker-only `reasoning`; immutable node/JSON-pointer IDs;
  16 KiB per text leaf, 1 MiB source and 4 MiB response accounting in window.rs.
- `src/model/reconcile.rs:115`: stored Responses reasoning summary/content uses
  explicit `summary_text` / `reasoning_text` parts, not arbitrary object text.
- `src/model/dialect/messages.rs:588` and `src/model/opaque.rs`: Anthropic thinking
  is currently stored inside provider_opaque; replay envelope validation does not
  confer public history text authority. Signature/redacted blocks stay private.
- `src/compaction.rs:1621`: separate internal summary-material extraction already
  reads an exact Anthropic thinking leaf, but this is NOT a public DTO contract.
- `docs/design/effective-spec.md`: current 296 contract disallows reasoning text;
  update adopted design explicitly before implementing this authorized extension.

## Proposed Qualification Cases

| Case | Exact stored qualification | Public result | Named control |
| --- | --- | --- | --- |
| Responses summary | assistant_response/output reasoning, summary array part type summary_text with nonempty text string | ReasoningText at exact leaf pointer | responses_readable_summary |
| Responses content | same parent, content array part type reasoning_text with nonempty text string | ReasoningText at exact leaf pointer | responses_readable_content |
| Mixed ciphertext/readable | qualified leaves plus encrypted_content sibling | qualified leaves only; ciphertext never copied | mixed_readable_ciphertext_never_serialized |
| Encrypted-only | reasoning with no qualified readable leaf | ReasoningUnavailable at original reasoning pointer | encrypted_only_marker |
| Empty or malformed readable | empty strings, wrong types, missing part type or wrong container | ReasoningUnavailable, no guessed text | empty_malformed_reasoning_marker |
| Unknown nested fields | arbitrary text/summary object or unlisted part tag | unavailable reasoning marker, never recursively inspect extras | unknown_reasoning_fields_not_authority |
| Opaque provider envelope | provider_opaque, including thinking-looking block | ReasoningUnavailable; no public extraction by default | opaque_provider_never_interpreted |
| Redacted thinking | redacted_thinking or opaque data | ReasoningUnavailable, no decode | redacted_reasoning_marker |
| Unknown ordinary item | existing unrecognized item | existing Unavailable opaque unchanged | ordinary_unknown_unchanged |

Anthropic public thinking is a pending qualification decision, not silently accepted:
its current stored representation is an explicitly opaque envelope. Review whether
299's "public thinking block" authorizes a narrowly qualified exact leaf in this
representation, or whether this first increment must exclude all provider_opaque.
No provider rewrite or alternate inferred representation will be added by this task.

## Bounded Extension And Validation

Proposed public discriminant: `reasoning_text { item_identity, text, text_complete }`.
Keep each safe leaf's canonical node/JSON-pointer coordinates and saved leaf order.
Use the existing UTF-8 16 KiB prefix and include readable reasoning in 4 MiB response
text accounting; preserve atomic node arrays and response_budget deferred suffix.
No concatenated aggregates, decryption, argument/output traversal, new operation,
new paid requests or private database reads. No display/Conversation authority is
implied; consumers must treat this variant as Work/default closed, no tool count.

Implementation: one explicit qualification decision per row, reused by projection;
public enum/schema variant and text budget match updated exhaustively. Tests cover
all rows plus UTF-8 completeness, stable identities, mixed saved order, source and
response limits, schema encode/decode, negative secret serialization and existing
producer/legacy regression controls. Local Rust: rust-slot cargo check and focused
tests only; full standard/all-OS checks in authorized branch CI on exact reviewed SHA.

Providers belong to 293/T; this task owns history/items, history window accounting,
public protocol/schema and focused synthetic controls. Shared protocol conflicts
are resolved on this task branch with renewed validation, never in foreign WTs.

## Knowledge Search Limit

Documented hybrid RAG returned no hits with semantic_not_ready/index_incompatible.
No index rebuild, registration or embedding request was started. Current source and
issue 299 are authoritative for this plan; no prior-history claim is inferred.
