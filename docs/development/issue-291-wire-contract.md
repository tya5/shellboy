# Core291 Shell Write Result: Narrow Contract

Owner: newly reassigned sole author, thread38fcb3e3. This is not an ownership
inference from the shared Git author name. Original4376 foreign WT is untouched.
Only narrow4376 DTO/root/test semantics may be reused; mixedf49d/c09 are excluded.
Runtime, authorization, receipts, stdin behavior and error production are unchanged.

Authority: control/runtime.rs shell.write directly serializes shell::StdinWriteResult.
The operation registry already names ShellWriteResult; its DTO must match producer.

| Field / case | Declared wire rule | Fixture oracle |
| --- | --- | --- |
| bytes_written | required u64, including0 and partial values | actual StdinWriteResult encoded -> typed DTO -> equal JSON |
| status | required closed enum complete/timeout/closed/stopped/failed | actual enum five variants; unknown/null/wrong type rejected |
| stdin_open | required bool, independent of bytes count | both bool values roundtrip; absent/null/wrong type rejected |
| reason | Option<String>, actual producer always emits null or string | actual null/value equal roundtrip; absent decodes None and serializes null (documented normalization, not equality) |
| empty complete write | Complete, bytes_written0, stdin_open true | synthetic actual producer struct; no real shell needed |
| additive unknown fields | preserve arbitrary JSON via flattened map | added null/object/legacy execution_id/state remain value-equivalent |
| obsolete declaration only | execution_id/bytes_written/state without real required fields | typed success DTO rejects; do not invent execution ID |
| successful Failed status | successful result data, not ApiError | Response::success -> generic envelope/typed result exact |
| operation error | existing ApiError envelope; no success-result coercion | error validates independently, cannot decode as success DTO |
| required fields | bytes_written/status/stdin_open absent/null/wrong type | finite negative matrix, no schema-driven producer change |
| legacy consumer | generic Value successful result still reads unchanged | encode actual producer -> Response<Value> roundtrip |
| exported schema | status/root/result match actual producer; reason nullable/optional | regular Rust generator + existing committed golden, not sole wire proof |

Implementation boundary: src/protocol.rs DTO/status/schema root,
tests/protocol.rs existing registered fixtures, generated protocol.schema.json,
and concise acceptance/status documentation. No runtime/control mutations, wrapper,
new API, schema migration, payload authority extension or installed change.

O297 owns protocol/schema additions and its new41 repair. Build-free preparation
may proceed, but O297 has heavy priority. After its landing, fresh rebase and own
author conflict resolution retain both contracts; regenerate schema, review fixed
source, focused checks and normal/all-OS CI on the same SHA. Old mixed CI success
is historical only and is not fresh-main evidence. S6 short design alignment is
requested before payload source reuse. No user DB/RPC/provider/browser is needed.
