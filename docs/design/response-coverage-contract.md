# Coverage Storage Candidate: One Witness On Existing Attempt

Design candidate only. Separate from302 notification code, schema42 and history
notification contracts. Leader adopts storage meaning; public fields, schema
number, fetch and live-off implementation remain unadopted.

Choose one nullable bounded witness on the existing model_attempt row, rather
than a new table. Working internal type names below are not public wire names.
Reuse attempt.id, turn_id, generation, response_id and existing output_item
ordinal/node linkage. No copied answer bodies or permanent cache.

| Private candidate | Meaning / authority | Consumer comparison / bound |
| --- | --- | --- |
| SealIdentity | Existing attempt ID plus producer-issued stream incarnation token, immutable final admitted-prefix sequence, admitted UTF-8 byte count and ordered admitted-segment transcript digest | Consumer retains the SAME producer token/offsets received with admitted deltas, independently computes prefix transcript identity; no ID from text matching. Wire carriage not yet adopted |
| DispositionSummary | Closed exhaustive reconciliation verdict for every admitted segment: represented in adopted answer, deliberately excluded with supported reason, or unresolved | Counts/digests alone are not semantic proof. Producer reconciliation must classify all segments; any unsupported/missing/late segment => no covered verdict. No unbounded segment vector stored |
| CoverageWitness | SealIdentity plus AdoptedCoveredNonempty / AdoptedCoveredEmpty / Unknown / Unrepresented, saved answer member count and exhaustive disposition summary | Positive requires complete producer reconciliation. Exact consumer seal equality plus current complete Q1 member enumeration; count equality alone is insufficient |
| Write guard | Witness initially NULL, set once inside successful adoption transaction after live-check/Applied and complete output_item inserts | One conditional NULL->value write; conflicting retry never overwrites. Rollback publishes no positive; postcommit publication uses only committed witness |
| Stream close | Freeze producer admitted-prefix seal before adoption coverage is computed | Commit-after arrivals cannot extend the frozen seal; reject or assign a new explicit incarnation, never silently append to old cohort |
| Covered empty | Empty adopted answer plus exhaustive explanation of all SAME-seal admitted segments | A nonempty preview may retire only if those segments have supported deliberate terminal disposition. Otherwise Unknown/Unrepresented even when output count0 |
| Bounds | Candidate witness cap2048 encoded UTF-8 bytes; IDs reuse existing maxima; fixed enum/digest/counter fields only, integer overflow fail closed | This is a proposed validation limit, not measured payload size or a new general budget. Oversize/unsupported => absent/Unknown, no truncation-to-covered |
| Retention | Witness follows model_attempt historical lifetime; do not pin bodies/streams indefinitely | Missing witness/members or revoked root/path => no usable current coverage; never reconstruct positive from output rows or strings. No GC implementation change selected |

The producer token must be allocated BEFORE the admitted stream begins and be
observable by the same live consumer; post-hoc token publication cannot prove it
identifies a preview that consumer held. Existing saved request generation alone
is insufficient. The digest matches identities, not representation semantics;
only exhaustive producer reconciliation supplies that separate verdict.

Existing optional model_stream_observation is not chosen as authority: it is not
always saved, its body retention differs, and no inspected contract binds it to
exact consumer-admitted seal. The candidate adds only the minimal attempt witness;
raw reconciliation logs or private provider events are never exposed.

D must confirm their live receiver can retain producer token, sequence and exact
UTF-8 prefix/digest alongside its existing seal without extra fetch. If current
public stream lacks them, this comparison needs a future minimal live-event
contract; do not claim present Q1 or a consumer-local token already supplies them.
Any dropped prefix/unsupported stream disposition remains Unknown under current
retention. No permanent preview cache and no implicit terminal-live-off change.

Sufficient future replacement would require BOTH committed positive exact-seal
coverage AND Studio's fresh complete all-FINAL-member visibility/CAS/revocation
checks. Historical witness is not current visibility. This proposal deliberately
makes no USER incident attribution and has no production implementation.

## D Consumer Source Handoff: Existing Comparison Is Insufficient

D11f469e1 reports current LiveFrame attempt_id/gap/deltas with Unicode-scalar
positions including reasoning. Preview exposes ordinary text only; reasoning
advances offsets without retained text. Gap/new attempt resets; bounded prefix
drop marks incomplete. live_end carries Thread only; finish toggles ending and a
later same-attempt frame can unset it. Local handoff epoch is not a producer seal.
Thus existing consumer CANNOT perform the candidate's exact producer-seal match.

Refine the single candidate: producer seal identity/coverage is one immutable
fact; consumer-admitted range/missing disposition is a separate local evidence
record tied to that producer incarnation. Do not equate Unicode scalar offsets,
UTF-16 retained limits, UTF-8 witness bytes or reasoning-excluded visible text.
Consumer retained-prefix digest must not claim to hash discarded reasoning or a
missing prefix. Any unsupported missing range remains Unknown, not covered by
matching the ordinary text. A future minimal public seal boundary must identify
attempt/incarnation and final producer prefix; consumer needs a way to match its
actual admitted range and gaps. No invented schema fields or fetch is adopted.
D source exact-blob handoff is pending; no additional same-question investigation.

## S6 Candidate Review Limits

S6 eeac0f04 accepts meaning consistency only. A future exact transcript encoding
must include segment boundaries/order, stream kind and duplicate delivery identity;
no UI trimming, CRLF normalization or JS code-unit identity. Missing, overflow and
unsupported disposition stay Unknown. Old witnesses NULL, no backfill or retention
extension. Explained exclusion is not permission for an unexplained blank UI:
nonempty-to-covered-empty needs a supported visible terminal disposition adopted
by leader, not disappearance authorized by digest alone. No measured witness size
or implementation signature qualification is claimed.

### D Exact Source Receipt

D one-time answer: fae390c live.ts blob
0f14c55d885c1ca0e1de7c5aafca6bf25be92d58. Public matching is NO.
Reusable thread/attempt IDs, gap and scalar offsets are not incarnation, segment
sequence, UTF-8 prefix counter or frozen final seal. Future adopted live events
could support fixed-size bounded incremental verification before reasoning/text
is discarded, without body copy or fetch. This is design possibility only, not
implemented/proven. Gap, dropped prefix, late fragment and unsupported encoding
remain Unknown. The concrete next scope decision must jointly address producer
live identity/order/seal and once-saved adoption witness, not add storage alone.

## Proposed End-To-End Contract V1 (Implementation Signature Pending)

This section supersedes the storage-only candidate. These are concrete proposed
names/types, NOT additions to an adopted public schema. No coverage code changes.
Existing notification302 migrations and source remain separate.

Types below use existing bounded opaque Thread/attempt IDs. New incarnation is a
producer-generated UUID allocated before any segment and never reused. JSON
counters are decimal strings in range0..u64::MAX, parsed exactly with BigInt in TS;
negative/overflow/noncanonical decimal is unsupported, never rounded Number.

| Proposed typed record | Exact fields / issue point | Consumer / persistence / tests |
| --- | --- | --- |
| StreamIdentityV1 | version:1, thread_id, attempt_id, incarnation | Attached to first and every v1 fragment; allocated before first producer admission. Consumer binds to original ONEowner/lifetime. Old frames remain legacy Unknown |
| StreamFragmentV1 | identity; sequence:u64-decimal; existing scalar offset; stream_kind:text-or-reasoning; existing text | Sequence0-based per incarnation, incremented per immutable fragment. Exact retransmission uses same sequence/kind/offset/text. Feed identity verifier BEFORE visible reasoning discard/trim; do not retain extra bodies |
| StreamSealV1 | identity; next_sequence; scalar_end; utf8_bytes; transcript_sha256; disposition:complete-or-unknown | Producer freezes after final admitted fragment and before adoption. No subsequent fragment may extend this incarnation. Unknown when producer overflow/drop/unrepresentable. Seal publication alone is not saved adoption |
| AttemptCoverageV1 | identity and full frozen seal; adoption:covered_nonempty-or-covered_empty-or-unknown-or-unrepresented; answer_member_count; presentation:canonical_answer-or-explicit_no_answer | Once saved in existing attempt nullable witness at successful adoption COMMIT; no backfill/rewrite. Only exhaustive reconciliation may issue covered. Nonempty preview -> covered_empty requires explicit_no_answer, never blank disappearance |
| ConsumerVerifierV1 | identity, next_sequence, scalar_end, utf8_bytes, incremental digest; state:verifying-or-sealed-or-unknown; one previous-fragment fingerprint | Fixed state per existing retained owner. No extra fetch/body cache. Exact immediate previous-sequence duplicate with identical fingerprint ignored; older or conflicting duplicate => Unknown. Seal must match all fields exactly |

Canonical transcript hash algorithm: SHA-256, bytes concatenated incrementally.
Header is ASCII `shellboy-preview-v1` followed by byte00, then UUID incarnation's
16 binary bytes. Each FIRST-admitted segment contributes byte01, sequence as u64
big-endian, stream kind byte00(text)/01(reasoning), existing Unicode-scalar offset
as u64 big-endian, UTF-8 byte length as u64 big-endian, then exact UTF-8 payload.
Payload must be Unicode scalar text; reject unpaired UTF-16 surrogates in TS rather
than TextEncoder replacement. No NFC, CRLF, trim or normalization. Terminal
contributes byte02 and next_sequence/scalar_end/utf8_bytes as u64 big-endian.
Digest lowercase hex64. Immediate identical retransmission contributes NO bytes;
all other duplicate/offset ambiguity makes verification Unknown. Digest identifies
transcript only; producer reconciliation separately proves semantic coverage.

Proposed bounds: ID256 UTF-8; encoded seal<=2048 bytes and encoded saved witness
<=2048 bytes, checked before materialization and whole-response accounting. Reuse
existing delta and preview lifetime/count/text caps unchanged. Add no unbounded
segment list. Reasoning is hashed before discard. Existing preview prefix drop,
gap or verifier inability to process any fragment makes that owner's verification
Unknown even if a later hash matches. No new retention extension or pinning.
Producer counters or encoding overflow freezes Unknown, never wraparound covered.
A crypto implementation/dependency choice is not approved by this design text;
Rust/TS must use one vetted implementation or audited incremental implementation,
with dependency/version/bounds adoption before code. Do not fake a synchronous
incremental hash using a full-transcript WebCrypto buffer.

| Branch | Producer / transaction rule | Consumer rule / required finite control |
| --- | --- | --- |
| Normal | Freeze seal; reconcile all admitted fragments against full saved output; insert witness with output_item in adoption transaction | Exact matching seal + committed coverage + complete fresh Q1 bodies/ALL FINAL members -> atomic ONEowner replacement |
| Immediate duplicate | Same identity/sequence/offset/kind/bytes, not a new admitted fragment | Ignore only exact previous fragment; conflicting or older replay => Unknown; no double hashing |
| Gap / dropped prefix | Do not manufacture complete seal/coverage if producer missed fragments | Any consumer gap, prefix drop or skipped reasoning => Unknown; existing retention unchanged |
| Late | After seal producer rejects old-incarnation admission; cannot mutate witness | Any observed old-incarnation post-seal fragment invalidates current verifier to Unknown, not append to sealed preview |
| Retry / reconnect | New attempt or incarnation when not exact retransmission; NULL witness on new attempt | Never transfer old coverage/verifier authority to new owner; original ABA/CAS fences apply |
| Rollback / stale / cancel | Failed adoption emits no positive; transaction rollback removes witness and output together | End/seal cannot substitute for adoption. Unknown/unrepresented keeps current bounded policy; no live-off relaxation |
| Empty | Covered_empty only when all same-seal fragments reconciled to supported explicit_no_answer terminal disposition | UI retains visible explicit terminal explanation; nonempty preview cannot silently vanish on count0/digest match |
| Missing legacy / eviction | Witness absent or retained body/member missing => no usable positive | Old producer/client combinations remain Unknown, no inferred seal from legacy live_end/thread or Q1 empty rows |

Issuance order: allocate identity -> admit/hash immutable fragments -> freeze seal
-> reconcile -> successful adoption transaction writes outputs + NULL-to-once
witness -> COMMIT -> publish committed coverage. Seal may arrive before commit,
but no positive publication before commit. No extra consumer acknowledgement RPC
is introduced; incomplete local receipt of producer sequence is simply Unknown.
Historical witness retention follows attempt lifetime; current path/root/FINAL
loss revokes current replacement authority independently. Existing legacy live_end
behavior/terminal-live-off is NOT changed by this proposed contract.

Implementation gates: one Rust/TS canonical byte-vector fixture including astral
Unicode/CRLF/reasoning, duplicate/gap/drop/late/retry/rollback/empty branches,
2048/ID256 +/-1 and counter overflow, atomic store failure, old NULL compatibility,
complete-member bounds and receipt/final response limits. Full runtime controls
and exact-SHA CI are separate from this design. Migration number, public method/
field placement, crypto dependency, constructor signature and any live-off change
require leader's final implementation scope after S6 review.

## One Core Contract Above Three Provider Adapters

The contract belongs ABOVE dialect normalization, at Core normalized stream
admission, not inside a provider's transport completion event. All adapters feed
the same typed ordered segments -> frozen seal -> adoption witness -> Studio ONE
owner. Current normalized events DO NOT yet supply the proposed incarnation,
sequence or frozen seal; shared Responses-shaped events alone do not prove it.

| Adapter / inspected source | Distinct terminal / output rules | Coverage handling and adapter-specific controls |
| --- | --- | --- |
| Responses, model/http/response.rs553 and dialect.rs | response.completed versus incomplete/failed; response.output and stream reconciliation are separate from adoption | Completed transport is not commit. Test completed-but-stale, incomplete output limit, refusal, tool-only and opaque output; unsupported representation Unknown |
| ChatCompletions, dialect.rs175-207 and dialect/chat.rs239-406 | [DONE] requires finish_reason; stop/tool_calls may normalize completed, length/content_filter do not; incomplete tool args rejected | Test DONE without reason, output after finish, duplicate finish, tool-only/stop-with-calls, refusal and partial. Never interpret stop as proof of saved ordinary answer |
| AnthropicMessages, dialect/messages.rs542-573 | message_stop plus recognized stop_reason; end_turn/stop_sequence/tool_use differ from max_tokens/context-window incomplete; content blocks independently typed | Test absent message_stop, unknown reason, block order/tool-use, thinking/opaque and partial. Neither message_stop nor tool_use establishes display-text coverage |

Initial positive coverage applies ONLY to explicitly supported normalized ordinary
display-text segments, with lossless mapping to the adopted supported answer items.
Refusal needs its own supported typed display mapping; until adopted, Unknown.
Tool-only, partial, cancellation, opaque content, unsupported normalization and
unexplained exclusions do not gain positive coverage from a terminal event.
Reasoning-publication permissions remain exactly #299 (Responses-only permitted
surface, opaque excluded); this design grants NO new Chat/Messages reasoning
publication. Private normalization may track non-public disposition without
exposing provider reasoning bytes. Where the proposed transcript kind requires
reasoning bytes the consumer is not allowed to receive, verification is Unknown;
do not leak those bytes to make a digest match. A text-only projection seal must
be explicitly selected and describe excluded kinds, not silently change scope.

Common finite controls for ALL adapters: exact identity/order/UTF8 vectors,
normal/duplicate/gap/drop/late/retry, overflow and bounds, rollback/stale adoption,
empty versus explicit terminal explanation, unsupported semantics -> Unknown,
old consumers/old NULL witness, and all-FINAL visibility separate from coverage.
Adapter controls above reuse existing output_limit_tests and reconciliation cases
where applicable; no provider-paid calls or invented uniform finish semantics.
This matrix is SOURCE design evidence, not adapter/runtime qualification.

## V1 Review Resolution: Normative Overrides

This section supersedes conflicting candidate wording above; no sixth record.

1. V1 is explicitly `ordinary_text_v1`: its transcript contains ONLY ordinary
   normalized display-text segments, never reasoning/tool/opaque/refusal bytes.
   Header domain is ASCII `shellboy-preview-ordinary-v1`, byte00 and incarnation
   bytes. All V1 stream_kind fields must equal text/byte00; other kinds are
   Unknown, not silently omitted from a transcript claiming to contain them.
   Segment scalar offsets are contiguous offsets WITHIN THIS PROJECTION, not
   the existing mixed reasoning+text offsets. Both may coexist in future stream
   envelopes, but mixed offsets cannot substitute for projection offsets. Core
   constructs the projection before public admission, consistently for all three
   adapters. No #299 reasoning authority is expanded. Producer coverage must
   independently attest every admitted ordinary segment; non-projected content
   cannot produce an unqualified all-content coverage claim.
2. Every new fragment starts at sequence0/scalaroffset0 for a new incarnation.
   Consumer validates canonical unsigned decimal u64 sequence AND projection
   scalar offset, checks expected sequence and scalar-end equality, counts actual
   Unicode scalars and UTF-8 bytes from exact payload, and performs checked u64
   additions for sequence/scalars/bytes. Kind is the closed text discriminator,
   not a free numeric counter. Overflow, invalid scalar encoding or inconsistent
   offset makes Unknown permanently for that owner. Existing mixed offset is
   retained only as legacy metadata, not authority for these counters.
3. BEFORE seal, only an exact immediate previous fragment retransmission is
   ignored without rehash. AFTER accepting a seal, ANY fragment (including an
   exact immediate previous retransmission) makes Unknown; there is no exception.
   An identical duplicate seal is idempotent. A different second seal, or seal
   with invalid counters/identity/version, makes Unknown. An identical seal never
   restores Unknown. Producer must stop old-incarnation fragments after seal.
4. ConsumerVerifierV1 and AttemptCoverageV1 consumer requirements additionally
   mandate exhaustive existing Q1 enumeration for the EXACT attempt and fresh
   current path: every ordinal/node/item identity must match, including multiple
   item ordinals sharing a node. truncated, missing ordinal/member, stale path,
   unsupported item or non-FINAL body makes replacement unavailable. Count is
   a consistency check only, not identity/coverage. No new member list is saved;
   reuse existing bounded attempt/output_item linkage and all-FINAL/CAS guards.

No-adoption states (rollback, stale turn, cancelled/failed attempt or declined
Applied transition) leave witness NULL; no negative witness is separately saved
outside adoption. A successful adoption may save Unknown/Unrepresented when
reconciliation cannot attest coverage; the same NULL-to-once rule applies. NULL
never means covered empty. Covered-positive, including empty, can only be saved
with the successful output/cohort adoption transaction and exact frozen seal.
Negative/Unknown witnesses are not later upgraded or rewritten; a retry uses a
new attempt/incarnation. This distinction does not claim every historical NULL
can be classified retrospectively.

Finite tests must cover projection-vs-mixed-offset divergence with reasoning,
scalar/byte differences for astral text, each checked overflow, wrong first offset,
pre-seal exact duplicate, post-seal same duplicate -> Unknown, duplicate/conflicting
seal, unknown irreversibility, and exact ordinal identities with same-node members.
These are design obligations, not executed results. Wire placement, dependencies,
new schema number/constructor and live-off behavior still need explicit adoption.
