# Structured received-message provenance (#297)

Status: SOURCE/DESIGN proposal for independent S6 review. Not adopted for payload,
protocol, schema or runtime implementation. One implementation author may be
assigned only after leader adoption. Does not reopen Studio #24/v0.2.4 or block
Core #301/P1 closure. No compatibility aliases before 1.0.

## Source baseline and existing implementation

Core fetched origin/main and design parent:
`05e358703c7c407c26ef9c7dafeb8714d71e95ef` (#301 baseline).
Branch `docs/297-sender-design`, isolated worktree `core-297-sender-design`.
No existing *297* branch existed before creation. Studio source inspected at
local origin/main `57bcc77d6255036368b52b01fbbb8bfc24dce43a`; no assumption that
an older Studio checkout contains these files.

| Existing contract | Source | Consequence |
| --- | --- | --- |
| Credential-free sender kind/IDs/label | src/protocol/sender.rs: MessageSender::recorded/from_saved; docs/design/protocol.md sender inspection #59 | Reuse exactly. Model IDs come from validated Origin, label is self-asserted. Human flag is an unverified client claim, not authentication; model Origin wins. Old insufficient evidence stays unknown. Existing legacy complete OriginIdentity reader is saved-fact interpretation, NOT a new alias. |
| Body and receipt stored together | src/control/db.rs: send/send_content, especially1981-2021 | Actual accepted message_id is node ID; receipt.message_node_id binds sender_json to that body. MessageAccepted.message_id/thread_id is the public client name. No body rewrite/new sender table needed. |
| Existing history sender projection | src/history/relations.rs: provenance; protocol.rs NodeProvenance | User-node provenance.sender already reads exact body receipt; non-user sender omitted. list/history.get/window (with and without items) use the same provenance helper. Do not implement another sender resolver. |
| Typed body items | src/history/items.rs; history/window.rs; history/list.rs | Item identity/body ordering is separate. Node-level provenance on HistorySnippet covers each of its #296 items, including unavailable/deferred items; do not duplicate metadata per part or infer identity from text. |
| Request/reply durable links | src/control/reply.rs: reply; src/control/db.rs: message_details; protocol.rs MessageDetails | Original request receipt stores expect_reply, reply.state/node_id/by; delivered reply receipt stores reply_to=original message ID. message.get already exposes these; history provenance does not expose request/reply metadata yet. |
| Reply without delivery receipt | src/control/reply.rs: target=None branch | A reply to a human/external/unknown sender is a separate user node attached to original receipt.reply.node_id/by, not necessarily a new send receipt or selected-path entry. Existing provenance has a reverse fallback for its sender. Do not invent selection membership. |
| Admission notices and replay | src/control/reply.rs: admission_notice_tx/notice_tx; src/control/message_delivery.rs; control/receipt.rs | Model-facing notices retain existing message IDs/command/reply context. Stored developer text is not a UI join contract. Operation replay returns saved outcome or operation_expired, never reruns mutation; path_admission prevents duplicate notices. |
| Studio attribution already exists | web/data/src/sender.ts, protocol.ts HistoryItem.provenance; web/ui-core/src/sender-titles.ts | Reuse senderFact; bounded title resolver is currently16 candidates/attempts and one flight. #25 first8 is display fallback length, NOT an8-request resolver budget. |

Existing sender persistence/history projection is substantially implemented by #59.
The remaining Core #297 work is ONE exact structured request/reply projection and
its tests/documentation; sender DTO fields and notice production must not be
reimplemented. Studio title/header/styling is #25, not a Core prerequisite.

## Two concrete designs

| Dimension | A: direct body-node provenance from existing receipt (SELECTED) | B: notification with explicit target body node ID |
| --- | --- | --- |
| Persistence | Keep sender_json/result_json on existing receipt; derive body metadata through exact IDs | Every sender/label/reply notice producer needs a new typed target association, including identity-only notices and reply notices; old notices often lack it |
| Receipt/retry | Reuse accepted message_id and stored request/reply links in same transaction; no new mutation | Must keep notification identity/target atomically aligned with receipt and avoid duplicate notices on replay |
| History/window | One node metadata projection on returned snippet; works when notice is on another page/unselected | Need extra notice read/join/target lookup when notice and body interleave, are nonadjacent, on different pages or summary-replaced |
| Reply-only node | Exact original receipt.reply.node_id/by reverse relation already exists for sender; add request ID projection | Need new synthetic notice for a node that may have no selected notice/path entry |
| UI | Received body header consumes its own provenance; no notification line needed to identify sender | Two rendered sources require dedup/association/open-state complexity and extra missing-target states |
| Old records | Unknown sender / absent relation; no reconstruction | Missing target cannot be backfilled from prose/adjacency safely |
| Model replay | Existing admission notices remain model inputs, not UI authority | Risks changing provider replay/notice semantics just to serve UI |

Choose A. Do not add B as an alternate alias/fallback. Do not add sender fields to
provider body JSON: the body node is identified by exact public ID and its existing
NodeProvenance metadata. Keep model-facing notices unchanged for their original
purpose. Any UI suppression of a redundant notice is a separately reviewed Studio
presentation decision, never deletion of history or model input.

## Proposed public contract (not yet implemented)

Keep `NodeProvenance.sender: Option<MessageSender>` exactly as now. Add ONE optional
`NodeProvenance.message: Option<MessageProvenance>`; same declaration is shared by
history.get, list and window/items, no per-endpoint aliases. Proposed typed shape:

```text
MessageProvenance {
  expect_reply: bool,
  reply_to?: MessageId,
  reply_state?: pending | replied | ignored | cancelled,
  reply_node_id?: NodeId
}
```

Names are proposed public fields, NOT fields already supplied by current Core.
Do not expose result_json/receipt IDs/operation Origin/token, arbitrary reply Value
or whole notification payload. Reuse existing persisted states; do not add a
second lifecycle machine. Unknown future state fails closed to absent relation,
not successful reply. Emit reply_node_id only for actual replied state + saved ID.
No claimed guarantee that reply_node_id is selected, loaded, readable or consumed.
For ordinary receipt-backed messages, false may be obtained from the EXISTING
MessageAccepted omission-default only after validating the saved acceptance shape
and exact message/thread identity. Missing/malformed arbitrary result_json is NOT
sufficient ordinary-message evidence: omit message metadata instead. Do not invent
an old-record backfill. Human-directed assistant questions (to_human=true) remain
on the existing MessageDetails/human-duty path; this received-user projection does
not relabel them as incoming model messages or redesign their UI.

| CASE branch | Sender source | Message relationship / display qualification | Required named control |
| --- | --- | --- | --- |
| Ordinary receipt-backed human/external/model | Exact receipt.message_node_id=n.id; sanitize with existing MessageSender | message present, expect_reply=false, no reply state/target | ordinary_sender_exact_body |
| Request receipt with explicit expect_reply | Same exact receipt | expect_reply=true plus actual existing reply state; replied target if saved | request_saved_state_and_reply_target |
| Delivered reply with its own receipt | Own receipt sender_json, not request sender | expect_reply=false; reply_to from own result_json | delivered_reply_uses_reply_author_not_request_author |
| Reply-only node without own receipt | Original receipt where reply.node_id=n.id; reply.by for sender | reply_to=original message_node_id, expect_reply=false; no manufactured target delivery | reply_only_reverse_exact_request |
| Both own and reverse relation available | Own sender has precedence; validate reverse request agrees with explicit own reply_to | Equal facts collapse; conflicting relation becomes unavailable, never arbitrary first-row attribution | contradictory_reverse_link_not_guessed |
| No sufficient saved receipt facts | Existing sender unknown; non-user absent | message absent; absent does not prove ordinary/non-request | old_record_unknown_no_prose_join |
| Non-user notice/assistant/tool/summary | Sender stays omitted per existing contract | message omitted; do not inherit from a nearby user or notice text | notice_is_not_body_authority |

A received user remains Conversation even when sender unknown. Metadata absence is
not permission to hide a readable body. A model sender without a current Thread
title retains its exact recorded identity; deleted sender Thread does not erase
provenance. Label presence never supplies identity or authorization. Do not use
provenance.origin_thread_id (body creation context) as sender_thread_id.

### Durable projection and bounds

Use receipt data as the single saved authority; no new sender/notification table
or body mutation. Both delivered reply receipt.reply_to and original reply.node_id
are written within existing reply transaction. Reply-only reverse lookup must be
exact and conflict-aware. Current provenance SQL uses LIMIT1 ordered by rowid;
that incidental choice is NOT authority for newly projected contradictory links.

Existing receipt_message_node index supports direct lookup. No suitable reverse
reply-node index was found at this source pin. Implementation must demonstrate a
bounded indexed reverse lookup (up to two matches to detect ambiguity), e.g. a
reviewed expression index for stored reply.node_id, rather than repeat a whole
receipt scan for every window node. If an index migration is needed, choose its
number only against then-current main after separate schema adoption; this design
adds NO migration or claim that an index already exists. Existing old sender SQL
scan behavior is recorded, not approved as the new projection's performance plan.
Keep page <=200, existing typed item/source/response byte bounds. Message metadata
uses the explicit ID/serialized-byte admission below, no unbounded relationship
fetch, notification scan, all-history reconstruction or title lookup in Core.

Protocol/schema/version negotiation changes, generated output and downstream
strict decoding require explicit coordinated adoption, not an additive-alias
assumption. Extend the existing sender/provenance types; old request compatibility
fields are not added. Old saved records remain inspectable as unknown, not a
protocol compatibility layer. MessageDetails continues using existing reply_to;
when history adds the same fact it is the same meaning, not a competing key.

## Acceptance table: source -> saved -> replay -> selected UI facts

These are required controls, NOT passing tests or owner acceptance.

| Scenario | Durable/replay oracle | Window/items/consumer oracle |
| --- | --- | --- |
| A-model body, unrelated event, B-model body, then A notice | Each exact receipt stores its own sender; no text/position join | Sender stays on exact body across interleaving/nonadjacency |
| Body and notice on separate bounded pages | No history scan to find notice | Body metadata complete even with notice page unread |
| Held send, restart/admission repair | sender_json stable, no duplicate body/notice on original operation replay | Same message ID/sender after admission; held does not imply read/answered |
| Same operation replay/expired outcome | Same stored result or operation_expired; no repeated mutation | No extra sender line/body; current saved provenance remains from receipt |
| Request and delivered reply with label spoofing another Thread | reply_to and reply.by preserve actual request/reply author | Exact sender source + claimed label separated; no title/label matching |
| Human/external reply with no delivered send receipt | Original request's exact reply node retained/reachable | Reverse sender/request relation on that node; no invented selected row |
| Empty/old/malformed sender, unknown relation | unknown/absent, no inference from role/body | Readable user content remains; unknown attribution explicit |
| Selected path changes/summary replacement | Original IDs/provenance retained according to existing reachability | Only actual selected nodes rendered; no reselect original on identity match |
| Conflicting duplicate reverse relations | Detect ambiguity using bounded lookup | No arbitrary rowid winner for new request relation |
| items deferred/null/source or response budget | Same node provenance available, body unavailable state preserved | Sender metadata does not invent full text, item count, execution or reply completeness |
| Deleted sender Thread / rename | Saved sender ID stable; no current-title snapshot invented | Current title optional; link identity independent; no send-time title claim |

Reuse tests/reply.rs, existing sender unit tests and history/relations.rs provenance
control; register any new integration module in tests/integration/main.rs. A future
author runs only focused Rust checks through rust-slot; full CI on exact adopted
SHA is separate. This design task runs NO tests/build or installed-owner reads.

## Studio #25 partition and independent gates

Studio owns first8-character ID fallback when no current title, distinct restrained
received-message styling, and request/reply labels from the new exact metadata.
Existing senderFact/owner-scoped title cache/formatRoute are reused, not duplicated.
Current-title header design remains current observed sender title, never recipient
or send-time title. Append claimed label separately; unknown identity gets no
source link. Do not enlarge the current16/oneflight resolver or add per-row RPC.
New message metadata decoding belongs in existing strict Operations/protocol seam.
CASE selection supplies role/labels/relationships; JSX does not parse notice text.

Core author: exact metadata projection/types and bounded relation lookup, durable
replay/window tests after adoption. Studio author: decoder + existing classifier/
header/labels after Core contract reviewed. S6: independent SOURCE/DESIGN first,
then exact source/tests review. Leader: explicit adoption and one implementation
author; actual existing owner Thread with interleaved received messages is a
SEPARATE permissioned UI acceptance gate. Unit/schema/title-cache success, release
of #24 or this document cannot satisfy it. No migration, payload implementation,
heavy job, private DB/RPC/browser/installed owner change is authorized here.

## Adopted design detail: S6 three-condition closure

Leader selected A after S6 conditional review d9a2bcee; this completes the design,
not implementation authority. Previous f42f bytes are preserved privately.

### 1. Exact state and conflict matrix

| Saved input after exact receipt/identity validation | Projected message metadata |
| --- | --- |
| Valid saved MessageAccepted, omitted/false expect_reply, no reply state/ID | expect_reply=false; existing omission default is used ONLY for this validated shape |
| expect_reply=true, reply.state=pending | expect_reply=true, reply_state=pending, no reply_node_id |
| expect_reply=true, reply.state=replied, nonempty valid reply.node_id | expect_reply=true, reply_state=replied, reply_node_id=actual node ID |
| expect_reply=true, reply.state=ignored | expect_reply=true, reply_state=ignored, no reply_node_id |
| expect_reply=true, reply.state=cancelled | expect_reply=true, reply_state=cancelled, no reply_node_id |
| Valid delivered reply own receipt with explicit nonempty reply_to | expect_reply=false, reply_to=actual request ID; no request state inherited |
| Reverse-only: one exact original receipt with state=replied, reply.node_id=current body, reply.by valid, matching creation/target Thread | expect_reply=false, reply_to=original receipt.message_node_id; sender from explicit reply.by |
| NULL/malformed acceptance, null/nonboolean expect_reply, unknown/null/missing state on expected request, replied without ID, pending/ignored/cancelled with reply node ID, expect_reply=false with any reply state | message=None; do not invent false/terminal semantics |
| Own reply_to contradicts reverse link or two DISTINCT reverse receipts match (even identical values) | message=None; retain independently proved own sender and readable body; no arbitrary rowid winner |

IDs cannot equal their own source body for reply_to/reply_node_id; invalid cycles
at this direct-link level reject the whole message relation. No transitive walk.
A bad/new relationship never clears independently proved sender or typed content.
Own sender priority is limited to its exact validated receipt. Reverse ambiguity
cannot supply sender when no independent own receipt exists: keep sender unknown.

### 2. ID and response bytes: correct the earlier fixed-size claim

Source protocol.rs450-458 nonempty validates whitespace only: there is NO existing
per-ID byte maximum. Generated message/request/reply IDs are UUID strings, but that
is not a public validation cap. Thus earlier "fixed-size scalars" was imprecise.
For THIS NEW relation projection propose a read-only cap of256 UTF-8 bytes per ID,
nonempty, never truncate or normalize. This is a new design bound requiring
coordinated adoption, not a claim about existing arbitrary ID input limits. Longer
old IDs leave message=None, preserving saved sender/body and exact existing IDs.
No sender ID is rewritten to meet this cap. Two relation IDs + fixed enum/bool
have a maximum4096 serialized JSON bytes per message relation (including escaping).

Existing4MiB RESPONSE_TEXT_BYTES counts typed text ONLY. Do NOT subtract relation
bytes from that counter or alter text_complete/node body meaning. Existing8MiB
MAX_FRAME_BYTES remains total encoded response limit. Implement a response-byte
ledger for the SAME result, seeded with encoded base result/envelope bytes, then
charge each WHOLE optional message relation plus property/comma bytes against
8MiB, with at most4096 per relation. Insufficient bytes -> message=None atomically;
no half ID, truncated state, changed body item or newly invented wire reason.
Absence is unknown/unavailable relationship under the optional contract, not
ordinary-message proof. Apply the same rule to get/list/window before encoding;
no per-endpoint alias or silently overflowing envelope. Existing oversized-base
response failure behavior is not repaired by this feature. Ledger implementation
and byte accounting controls are REQUIRED before payload adoption, not present now.

### 3. Indexed exact reverse lookup

Proposed expression index SQL (migration candidate41 ONLY if then-current main
still has40; number/adoption requires separate leader authorization; edit no old
migration):

```sql
CREATE INDEX receipt_reply_node_lookup ON receipt(
  target_thread_id,
  (CASE WHEN json_valid(result_json) THEN
     CASE WHEN json_type(result_json,'$.reply.node_id')='text'
       THEN json_extract(result_json,'$.reply.node_id') END
   END)
);
```

Use the identical CASE expression in the exact query; do not use a different raw
json_extract predicate that prevents index selection or throws on malformed JSON:

```sql
SELECT id,message_node_id,result_json
FROM receipt INDEXED BY receipt_reply_node_lookup
WHERE target_thread_id=?1
  AND (CASE WHEN json_valid(result_json) THEN
         CASE WHEN json_type(result_json,'$.reply.node_id')='text'
           THEN json_extract(result_json,'$.reply.node_id') END
       END)=?2
LIMIT 2;
```

Boundedness comes from equality on BOTH index keys, not LIMIT alone. At most two
receipts materialize; two distinct receipts are ambiguity, regardless of values.
Validate actual node is user, exact current node ID, original request body exists,
original receipt target Thread equals the node's known creation Thread context,
reply.state=replied, and saved reply.node_id equals query node. Selected-view
Thread may differ after fork: never reinterpret recipient view as creator or
sender. Missing creation context -> reverse-only unavailable. For a delivered
reply with independent own receipt, use that receipt's exact thread/body identity;
any reverse consistency check must target its explicit original request, not guess
that delivered reply's destination equals request recipient. No global reverse
scan to discover a Thread when context is missing.

Authorized disposable :memory: SQL design check, Python SQLite3.53.0, synthetic
NULL/malformed/numeric/duplicate/wrong-thread rows: index construction/query safe;
EXPLAIN reports SEARCH receipt USING INDEX receipt_reply_node_lookup
(target_thread_id=? AND <expr>=?). Two duplicate synthetic receipts remain two
matches for ambiguity. Artifact: .local/leader/artifacts/core-297-design/
reverse-index-explain.txt. This is not production DB access, Rust/runtime tests,
full-scale performance or bundled SQLite validation; author must repeat exact
plan/byte/legacy validity controls using the actual build after adoption.

## Coordinated implementation adoption table

| Surface | Exact change after separate adoption | Sole responsibility / gate |
| --- | --- | --- |
| Public Core protocol/schema | Existing NodeProvenance + ONE MessageProvenance type, enum states and documented byte/absence contract; negotiated version/schema/generated artifacts together | Core implementation author (candidate O203522), leader protocol adoption first; no aliases |
| Durable storage/query | Existing receipt facts only, safe expression index in new approved migration + current schema; migration40 unchanged | Same Core author, source/index/EXPLAIN/schema review before runtime |
| Response construction | Shared exact projection and same-result serialized byte ledger; keep existing sender/typed text semantics | Same Core author; state/conflict/limit controls above |
| Studio strict decoding | Extend existing Operations/protocol types; validate optional whole relation, reuse senderFact | Separate explicit Studio owner after Core contract fixed; no arbitrary extras as authority |
| Studio title/style/reply labels (#25) | Existing16/oneflight title cache; first8 fallback display, claimed labels separate, exact relation labels | Studio UI author; actual owner Thread gate separate |
| Independent review/adoption | S6 final design then exact implementation source/evidence; leader selects one runtime author | Architect remains docs owner, no foreign edits/payload/schema authority now |

No payload/code/migration/test/build/installed-owner operation is performed by this
revision. The only executed SQL was explicitly authorized disposable-memory EXPLAIN.

### Implementation Sender Evidence Policy

An existing own receipt row is authoritative even when its saved sender is NULL,
malformed or exceeds the bounded sender input. Such a sender is `unknown`; it is
not inferred from another receipt. Only absence of an own receipt permits the
unique indexed reverse receipt's saved `reply.by` evidence. This intentionally
removes the previous COALESCE/row-order fallback, not a claim of byte-for-byte
legacy fallback compatibility. Invalid/ambiguous message relations never remove
independently valid own sender evidence or saved body text.

### Reverse Index Key Order Refinement

The adopted index orders the safe reply-node expression first and target Thread
second. Both equality keys and the CASE expression remain unchanged. An actual
bundled-SQLite full-schema synthetic comparison (512 rows, 256 target rows, two
pending) measured thread-first unforced pending lookup at 1549 VM steps instead of
17 before the index. Expression-first removes that thread-only prefix candidate.
The preserved unforced comparison, reverse two-key SEARCH, and fresh/migrated index
SQL convergence qualify this change; forced old-index usability alone does not.
These synthetic counts are not wall-clock or production-distribution guarantees.
