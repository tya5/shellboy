# Request Input Byte Accounting (#186)

## Contract

Each conversation model attempt records the measured UTF-8 byte length of the
exact JSON body `send` serializes, after `context_role` mapping, dialect
conversion (Responses, Chat Completions, Anthropic Messages), cache hints and
`extra_body`. The length is partitioned by the source of every byte. These are
byte counts of what was sent, never token estimates: Shellboy does not split
the provider's token totals by byte share and does not re-tokenize locally.
The provider-reported input and cached token totals stay separate values.

Categories (stable JSON keys): `system` (fixed Shellboy prompt),
`instructions` (AGENTS/developer instruction snapshot), `context` (the saved
`Shellboy context v1` line, #183), `user`, `assistant`, `reasoning`,
`tool_call`, `tool_result`, `spill_placeholder` (tool results inside a spill
node), `summary`, `notice` (delivered events and other system/developer
notices, attachment update notices), `attachment` (expanded attachment content,
including detached attachment notices) and `other`. `other` holds every byte no
item produced: JSON framing, model, tools, effort, stream/store flags, session
cache key, cache markers and `extra_body`. Categories sum exactly to the total.

## Tagging and conversion

`prepare_request` tags each canonical input item at the entry to conversion:
the two leading items by construction, replayed items by their saved node kind
(`context`, `summary`/`compact_summary`, `event`) and otherwise by shape (spill
nodes mark tool results as placeholders), and attachment binding marks expanded
content parts inside a user message. Encoders carry the tag onto the wire
fragments they build through a local-only marker that is stripped before the
body leaves the encoder; the untagged encoding is byte-identical, and the HTTP
send path never sees tags.

- Dropped items (other dialects' opaque blocks, Responses reasoning in Chat and
  Messages) are not counted.
- Merged items are split by original source: Chat assistant/tool-call merges
  keep each content part and call; Messages same-role merges keep each block;
  joined leading system text charges each item's escaped text to its own
  source and the `\n\n` separators to `other`. Every current merge is
  splittable, so no `merged` category is emitted. A future conversion that
  cannot split must add one rather than guess.
- Moved system content counts as its source where it lands (Messages leading
  system/developer items in `system`, later ones as user blocks).
- A message wrapper's framing goes with the item that created it. A member's
  key, colon and one separating comma go with the member's label; a cache
  marker inside a labelled block is `other`, so cache hints change only `other`.

Measurement walks the final body once, counting exactly the compact serde_json
encoding, and is checked against the serialized length; a mismatch, unresolved
label or tag/input count mismatch records nothing (unknown) rather than a
partial answer. Measurement happens once at preparation, before admission,
on a clone of the canonical request (one extra encoding per attempt, no I/O).

## Storage

No new table, column, migration or setting. The fixed-size object
`{"unit":"utf8_bytes","total":N,"categories":{...13 keys}}` (about 300 bytes)
is saved as `input_bytes` in `model_attempt.input_manifest_json` on the existing
admission insert, next to the other manifest facts. Existing manifest
accounting and GC own it. Unmeasured and pre-#186 attempts have null: unknown,
not zero. Readers validate the shape and the exact sum before showing it.
An admitted attempt's bytes are what was built for sending, not proof that the
provider received them.

Compaction summary requests and embedding requests are not measured.

## CLI

- `usage get --attempt ID` adds `input_bytes` (object or null) beside the
  existing `usage` (provider-reported `input_tokens`, `cached_tokens`).
- Ungrouped `usage get` (Thread, Run or Turn scope) adds `latest_request`: the
  newest admitted attempt of the scope's own Turns (children are not searched;
  the requested period does not apply), with `attempt_id`, `turn_id`, `state`,
  `started_at`, `usage` and `input_bytes`, or null. It searches only the newest
  33 Turns, one attempt index seek per Turn.

## F6 (approved option 3)

F6 details of a model answer read the adopted attempt (the existing exact
`output_item` relation) and show, in the 実行/Execution group, before the
existing cached-token and request-ID fields: `入力のバイト数（UTF-8）`
(Input bytes (UTF-8)) as the total then every nonzero source, `入力トークン
（提供元の報告）` (Input tokens (provider-reported)) and `キャッシュ済み入力
トークン（提供元の報告）` (Cached input tokens (provider-reported)). Saved field
values are language-neutral (counts, the saved JSON, fixed unknown markers)
and F6 localizes labels and values. Null bytes show `未記録`/Not recorded and
unreported tokens `未報告`/Not reported. No glyph is added; no ratio or
conversion between bytes and tokens is shown.

## Validation

Synthetic tests cover every category in all three dialects, with and without
cache hints and `context_role = "user"`: body byte-identity with and without
tags, exact sum, Chat merges, Messages system join and moved system content,
opaque/reasoning drops, attachments, `extra_body`, and cache markers changing
only `other`. A loopback test checks that the saved total equals the
Content-Length actually received and is read back by `usage get --attempt`.
No real provider call or user data is involved.
