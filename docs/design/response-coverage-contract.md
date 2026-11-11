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
2. The first fragment of a new incarnation starts at sequence0/scalaroffset0;
   subsequent fragments must match the next expected sequence and scalar offset.
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

No-adoption, Unknown and Unrepresented states leave witness NULL. This includes
successful adoption without exhaustive supported reconciliation. No negative
witness is saved; no later upgrade/rewrite of a saved witness. Only covered-positive
(nonempty or explained empty) is saved NULL-to-once with the successful adoption
COMMIT and exact frozen seal. NULL never means covered empty and cannot be
retrospectively classified. Retry uses a new attempt/incarnation.

Ordinary V1 covers only public ordinary answer text for all three adapters. It
must never suppress a whole preview that also contains reasoning under #299's
separate domain. Reasoning retains its independent visibility/retention contract;
ordinary coverage is not authority over it. Unknown unsupported remains NULL.

Finite tests must cover projection-vs-mixed-offset divergence with reasoning,
scalar/byte differences for astral text, each checked overflow, wrong first offset,
pre-seal exact duplicate, post-seal same duplicate -> Unknown, duplicate/conflicting
seal, unknown irreversibility, and exact ordinal identities with same-node members.
These are design obligations, not executed results. Wire placement, dependencies,
new schema number/constructor and live-off behavior still need explicit adoption.

## Five Implementation Scope Proposals (No Code Authority Yet)

| Scope | Concrete file/function assignment and proposal | Bound / unresolved adoption |
| --- | --- | --- |
| Optional live placement | Core protocol.rs EventLiveText optional identity V1; EventTextDelta optional V1 sequence/projection-offset metadata, reuse text. change_clock.rs live_text projection at498 preserves producer segment boundaries for V1 instead of current adjacent-text coalescing at488. Add optional terminal-seal lane on EventsWaitResult so a terminal attempt does not require a nonempty live-text entry. Bridge forwards to Studio LiveFrame and live_end seal envelope | Existing cursor/filter and max-thread bounds apply; missing/evicted seal -> Unknown, never replay fabricated from text. Terminal lane bounded by existing changes batch64, per seal2048; no new polling/fetch. Exact field spellings require adoption |
| Saved lookup placement | status.rs WorkItem optional positive coverage V1 only for exact model-attempt lookup, next to response_nodes/truncated. Reuse response_nodes ordinal/item/node identifiers; no duplicate members list | NULL/overbound/unknown absent; do not attach another attempt's witness to an aggregate Thread item. Pre-materialization2048 and final-response bound required |
| Producer and save constructors | New private control/driver/coverage.rs: Producer::begin(attempt:&AttemptIdentity)->Producer; Producer::admit_text(&mut self, scalar_offset:u64,text:&str)->Result<FragmentMeta,Unknown>; Producer::freeze(self)->FrozenSeal; private reconcile_ordinary(seal:FrozenSeal,response:&ModelResponse)->Result<PositiveCoverage,Unknown>. adoption.rs accepts optional PositiveCoverage and private save_once_tx(tx:&Transaction, attempt:&str, witness:&PositiveCoverage)->Result<()> within existing Applied/output insertion transaction | Private fields prohibit arbitrary JSON constructor; only normalized ordinary-text request producer can issue seal. PositiveCoverage includes the exact attempt. Rollback leaves NULL. Completion publish consumes committed result, never precommit token. Request scope owns/destroys state on cancel/shutdown |
| Crypto | Rust reuses existing exact sha2=0.11.1 (Cargo.toml53; Sha256 incremental Digest API), no new Rust dependency. TS currently has no SHA dependency; candidate @noble/hashes=1.8.0, sha256.create().update()/digest() synchronous browser incremental API, not WebCrypto transcript buffering | npm metadata confirms exact1.8.0 exists; this turn has NOT audited package/license/provenance or browser-qualified memory/API. D owns final dependency validation/lockfile. Fixed hash state+previous fingerprint, payload processed incrementally; no custom crypto implementation. New dependency still requires leader approval |
| Migration and old live-off | Coverage branches from fresh main after302 schema42 lands, then separate next migration (candidate43 only if actual fresh max42); never modify302's unlanded42 or preclaim a number. D sole owner updates live.ts/handoff/conversation adapter: Verifying -> SealedAwaitingCoverage -> VerifiedAwaitingAllFinal -> Replaced; any gap/late/etc -> UnknownHeld; legacy -> LegacyHeld | Current terminal-live-off must NOT simply drop nonempty preview when positive proof is missing. Keep existing bounded owner retention and visible pending/unavailable terminal explanation; on expiry show explicit unavailable disposition, not a claim that saved answer covers it. Exact presentation/expiry behavior and legacy policy require D/leader adoption; Unknown alone does not solve current body-disappearance incidents |

The final seal lane requires producer-owned bounded terminal metadata retention
under the existing change cursor; if that cannot fit current lifecycle, report a
specific lifecycle gap rather than silently retain all sealed attempts. Metadata
may expire and become unavailable, never silently increase preview retention.
No listener/queue/task is created by this doc. Runtime implementation must name
its owner, cap and shutdown path before introducing any such state.

Public Q1 projection must return the exact attempt identity and all current-member
facts as already bounded; proof never bypasses Studio FINAL ownership/path/CAS.
Ordinary replacement does not retire reasoning-domain content. Legacy/Unknown
policy is not declared fixed by a Core witness; D current-visible representation
work remains independently required. No all-USER incident fix claim is made.

## Constructor And Retention Clarification

The earlier reconcile_ordinary(seal,response) signature is INSUFFICIENT and is
superseded. Producer::freeze(self) returns private FrozenAdmission containing
FrozenSeal AND bounded OrdinaryEqualityEvidence: separate incremental SHA-256
of exact admitted ordinary UTF-8 concatenation, checked scalar/byte totals, and
an irreversible supported/lossless flag. No second body or segment list is saved.
Only the producer can construct this evidence. reconcile_ordinary takes this
FrozenAdmission plus borrowed adopted ModelResponse, not a bare seal.

V1 positive algorithm is deliberately narrower than arbitrary semantic exclusion:
iterate the saved output's supported ordinary answer-text members in authoritative
ordinal/content order; reject tool-only, refusal, partial, opaque, unsupported
member or any ambiguous ordering/mapping. Incrementally hash the same exact UTF-8
concatenation with a separate equality-domain header, compare digest AND scalar/
byte totals to the producer evidence. No trim/NFC/CRLF rewrite. Supported mapping
must make these bytes exactly the displayed ordinary-text projection; unsupported
cross-member presentation separators make Unknown. This is lossless ordinary
projection equality, not inferring meaning from the transcript-identity digest.
A positive also requires the exact segment-identity seal and successful adoption.
The equality evidence never travels to consumers as private provider content.

Initial V1 allows covered_empty only with zero admitted ordinary scalars/bytes and
zero supported saved answer members, plus explicit_no_answer terminal display.
Nonempty-to-empty deliberate exclusion is NOT implementable by this equality
algorithm and remains Unknown/NULL; future exclusion proofs require separate
scope adoption. It must not mint coverage from a seal alone or invent an exclusion.
Finite controls include same-size different bytes/order, normalization differences,
unsupported member boundaries, zero versus nonzero ordinary projection and stale
transaction. Hash collision resistance is a stated crypto assumption, not a claim
of mathematical byte equality or arbitrary semantic reconciliation.

V1 segment preservation must fit the CURRENT live registry entry-count, delta-
count and UTF-8 retention budgets; it never increases those limits. Any inability
to preserve original boundaries under those caps marks coverage Unknown before
coalescing/dropping; existing legacy projection may continue independently.
Terminal seal belongs to the existing attempt/live owner only: at most one seal
per retained live attempt, charged to that owner's existing byte/count budget,
dropped on the existing attempt replacement/eviction/disconnect/shutdown path.
No separately retained terminal history map is authorized. If current lifecycle
removes the owner before wait delivery, the seal is unavailable/Unknown; batch64
is a delivery batch limit, NOT a storage-retention cap or permission to retain.
A future implementation must bind these rules to actual existing constants, not
invent longer retention to make coverage succeed.

UnknownHeld and LegacyHeld expire at their EXISTING owner lifetime/retention
limit, never pin or indefinitely retain a preview. At that point body storage is
released and the view transitions to a bounded explicit terminal disposition:
`preview unavailable; saved-answer coverage not established`, not an empty answer
or a false `saved` claim. The exact UI wording/presentation remains D-owned, but
the semantic distinction is mandatory. No new expiry timer or lifetime is adopted;
reuse existing owner eviction and current-view state. If that state itself is
no longer owned, no orphaned disposition/cache is retained. This does not claim
the current live-off disappearance defect is fixed without D's implementation.

### Supported Lossless Producer Evidence (Narrow Initial V1)

For initial positive issuance, the producer must track ONE explicit normalized
ordinary source message/content-part identity and bind it to ONE exact saved
assistant message / ordinary output_text member. Each admitted fragment must be
attributed to that same identity in contiguous order. A missing source-item
identity, second ordinary part, rewritten/replaced terminal part, appended saved
ordinary text, refusal, truncation, tool/opaque substitution or unsupported mapping
sets an irreversible Unknown flag. Existing adapter normalization must demonstrate
this identity attribution; if it drops it, that adapter gets no positive until the
producer boundary preserves it. Merely receiving Responses-shaped deltas is not
that demonstration. This supports a narrow common contract, not all output shapes.

FrozenAdmission includes closed source-part identity, admitted count, checked
counters and producer observed no-transform/no-replacement flags in addition to
the separate equality digest. Saved response must have exactly the supported
single ordinary member attributed to that same part and no disallowed replacement.
The digest check is only the byte-integrity check AFTER this provenance and
lossless-shape proof, never its substitute. model/reconcile.rs mismatch is currently
DIAGNOSTIC ONLY, so absence of a diagnostic is not adopted as this witness. New
private constructor must explicitly collect/check the required producer facts.
Unknown if these facts are not available from a particular adapter.

Actual existing Core caps: change_clock.rs MAX_LIVE_TEXT_ATTEMPTS=16 and
MAX_ATTEMPT_TEXT_BYTES=64*1024, including per-delta record size. V1 identities,
fragment metadata, private equality state and terminal seal are charged within
these same per-owner budgets, never added on top. Existing registry admission
refuses more retained attempts; teardown/clear removes its seal too. Preserving
more fragments may therefore reach the SAME cap sooner and turn coverage Unknown.
No new independent map, larger cap or terminal TTL is selected. Delivery batch64
is independent. Studio's existing preview bound is not increased either.

### D Final Five-Scope Source Receipt

D confirms no existing TS incremental SHA dependency. @noble/hashes1.8.0 is only a
candidate whose npm existence was checked; D has NOT audited that release or
qualified exports, browser sync API, vectors or bounded memory. No install.
WebCrypto whole-input async digest does not directly meet synchronous incremental
fixed-state requirements. Required validation: selected package incremental API,
canonical Rust/TS vectors, synchronous browser operation and bounded UTF-8 chunks
without full-input concatenation. Crypto remains unqualified, not called audited.

D sole TS seams: web/data/src/live.ts LiveFrame/LiveText.apply/finish/reset/Preview;
protocol.ts PushFrame/parsePush live/live_end; store.ts onLive/finishLive handoff
binding; ui-core/FINAL/model/Conversation. Coordinate data/handoff files with S
before writes; D retains sole Conversation/model ownership.

Concrete state names align D's existing owner:
receiving -> boundedHeldUnverified (legacy/Unknown/missing/late/mismatch reason)
-> positiveVerifiedAwaitingFinal -> canonicalOwned only at atomic complete fresh
all-cohort FINAL mount with original/scope/generation/ref CAS. Revoke/revision/
unmount invalidate closure and rederive only under current bounded original
qualification. No permanent suppressed set or retention. On existing retention
exhaustion release preview bytes and expose explicit unavailable terminal state
only while the view is still owned; no unconditional zero-disappearance guarantee.
Receipt/read bounds do not grow. Empty explanation and reasoning-domain exclusions
remain as the normative rules above. New live-off behavior is proposed, not code.

### TS Crypto Candidate Rejection

Leader rejects the historical @noble/hashes1.8.0 candidate: reported official
SECURITY support policy excludes versions below2.4. The earlier candidate/API
references are historical and NOT adopted dependencies or qualified API contracts.
D is authorized to investigate exact2.4.0 disposably only. Production installation,
API binding and lockfile changes await those actual results and explicit adoption.
Rust continues to reuse existing sha2; no custom browser crypto or unsupported
package is a fallback. This correction does not block independent302 work.

### Exact2.4.0 Disposable Qualification Receipt

D reports @noble/hashes=2.4.0, import sha2.js, synchronous create/update/digest/
destroy type check PASS; 24 known UTF-8 chunk vectors PASS; 4MiB processed in
1KiB chunks with fixed block identity; existing Vite client build PASS; copied
lockfile existing entries unchanged. Canonical transcript cross-Rust/TS contract
vectors are still a separate gate, not these generic byte vectors.

Evidence: private artifact d-crypto-qualification/report.md. Support policy>=2.4
is not an independent audit: README's audit is1.0.0 January2022, not a2.4 assurance.
Advisory empty result is not a safety guarantee. Actual browser runtime/heap/GC
behavior unmeasured. This is disposable qualification, NOT production dependency
adoption. No package installation or API binding in production by this doc.
Byte hashing remains separate from producer coverage authority and consumer FINAL
ownership. Leader still decides exact dependency and implementation scope.
