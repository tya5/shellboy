# Core302 Existing Producer Admission Predicates

SOURCE checkpoint only; no storage/wire/migration adoption or runtime changes.
Core source: 69bae5b9591aee2023c13e782ec9df343a041323. Architect reference:
Studio ec422bb8fd0810a786cc9d8c62766e77a0e7077c,
notification-semantic-descriptor.md SHA256
ff29162091c128c9c2c895791d534778a20b9ffe5614e28ea1442392803b0b31.

## Existing Evidence And Missing Authority

Let n be the history node, u its creation turn, e an event and d its delivery.
Necessary predicates: n.kind=event, n.turn_id=u.id,
u.thread_id=e.target_thread_id, d.event_id=e.id, d.recipient_id=u.run_id.
Shell additionally requires e.target_run_id=u.run_id. Context permits NULL before
adoption, or matching u.run_id after adoption. A processed attempt, when present,
must join model_attempt.turn_id to a turn with that run; pending is admission,
not consumption. Current viewed/fork Thread is NOT a creation-context predicate.

These are INSUFFICIENT for all five variants: generic history::append_node accepts
kind/payload/turn from its caller (src/history.rs126); it has no notification-only
marker. driver.rs645-649 uses event-{id}, root event_id and an existence check.
A generic same-context event-shaped node can match all these necessary predicates.
Neither names, neighboring text nor an existing delivery establishes authorship.
Publication with existing columns must therefore remain ABSENT, not root fallback.

## Five Producers

| Variant | Existing writer and saved facts | Admission status / missing proof |
| --- | --- | --- |
| shell_ended | driver/shell_output.rs209-223 commits terminal execution update, typed completion payload, target Thread/run, pending delivery together | Later driver admission matches run; no canonical node marker. Existing terminal retry returns without a second event. Typed snapshot constructor still NEW. |
| thread_settings_changed | db.rs1850 calls context::record with before/after settings | context.rs12 saves context_changed, Thread and subtype; target run NULL. Delivery created only once runnable turn exists. No saved canonical admission link. |
| instructions_observed | driver.rs553-565 saves observation plus instruction snapshot in same transaction when previous/current differ | Event snapshot transaction precedes notification-node admission transaction; subtype and private observation payload exist, but no bounded public semantic snapshot or canonical link. |
| turn_started | state.rs18-31 inserts preparing turn and context event in same caller transaction | driver.rs612 may create turn BEFORE context-delivery insertion at633, so own turn event can be admitted in that same later driver transaction. No delivery before runnable turn. No canonical marker; retained subtype must validate its producer turn/run separately. |
| recovery_resumed | db.rs2135-2153 and2901-2936 record only after resume_recovery_turn_tx returns true | Two legitimate writer forms: one saves run+turn in change, retry form saves run only. Do not require nonexistent saved turn in retry form. Exact node admission marker absent in both. |

## Lifecycle Counterexamples / Bounds

- Fork: thread_path may share an existing node. Use node.turn_id -> turn.thread/run;
  requiring viewed Thread=e.target_thread would incorrectly remove fork metadata.
- Multiple runs: driver.rs633 creates context pending deliveries while target_run
  is NULL and no processed delivery exists. Failed/cancelled attempt can leave an
  existing admitted node; a later run may have another pending delivery. Global
  event-{id} existence suppresses another node, so current delivery recipient is
  not automatically the existing node's creator. Never reassign a future marker.
- Adoption: driver/adoption.rs62 marks deliveries processed and pins context
  target_run only on successful adoption. Node admission can precede cancellation
  and remain without processed_attempt. Requiring processed would hide legitimate
  initial display; calling pending consumed would be false.
- Attempt retention: delivery.processed_attempt_id is nullable FK with default
  NO ACTION, no ON DELETE SET NULL (current.sql212-214). Finite source search found
  no production DELETE FROM model_attempt; observed deletes are tests. Payload
  retention is not row removal. Future missing referenced attempt must fail closed,
  but do not claim current GC deletes it. Verify included attempt similarly if used.
- Event GC: gc/event_retention.rs11 protects any node whose root event_id matches;
  it targets terminal shell events. At104 pending deliveries of terminal runs can
  still be deleted independently. Thus a mandatory delivery EXISTS may erase an
  admitted historical descriptor even while node/event survive. Context events
  are not candidates of this shell-only eligibility query.
- Proposed event FK with NO ACTION would obstruct event deletion while marked node
  remains. CASCADE must not delete history, SET NULL would erase semantic authority.
  A retention rule and marker lifetime must be explicitly adopted together; current
  root-based protection is not the new marker's proof or an approved GC change.
- Old nodes/retries: do not backfill marker when existence check finds a node. Old
  event/no-link remains absent; marker and snapshot must never be reassigned.

## Minimal Unadopted Storage/API Candidate

Architect's event.semantic_json plus node.notification_event_id is a candidate,
not existing evidence. Only driver canonical NEW node insert may set the marker
in its transaction; generic append must not take a marker argument. Typed event
constructors must save bounded closed facts with event creation. DB FK alone does
not prevent another writer from assigning a valid foreign event: enforce sole
writer in Rust API and audit every marker write. No generic payload-derived repair.

Before adoption decide whether durable marker itself records creation admission
without retaining delivery forever, or persist minimal immutable recipient/turn
witness; live delivery EXISTS alone is insufficient under terminal pending cleanup.
Preserve exact creator context on retries/multiple runs. Retention/FK policy is a
required design choice, not authority for GC edits. No new number/index/table or
five-wire additions are selected here. SQL plan work, if needed, must be disposable;
no user database or service is accessed. This checkpoint executes no qualification.

## Leader Adoption: Implementation Authorized

Leader explicitly adopts Core302 storage/wire scope after S6 fc6c3cd0. Final
architect contract: Studio commit d3e4fd71327e13403712e2943f2131d6742c5c46,
file SHA256 b68642d764e1f2a4675cba4dbd4d1d16aaab06b364a7f62d0613eba2ae4b63c2.
The candidate language above is historical SOURCE investigation, superseded for
implementation by this adoption and the final architect contract.

Migration0042 adds nullable node.notification_event_id referencing event with
NO ACTION, event.semantic_json and an indexed node-marker reference. Migrations40
and41 stay byte-immutable; conflicting42 requires reporting, never renumbering.
Existing node.turn_id is creator witness. Only private canonical admission writes
a marker on NEW node insertion in its transaction; old existing nodes get no link.
Typed producer snapshot is saved with event creation in a separate transaction.
Historical reads validate marker/snapshot/creator, NOT live delivery, processed
attempt or mutable event target_run. Old read-matrix conditions are SUPERSEDED.
GC source may add only indexed marked-node event protection alongside existing
root protection; no broader deletion, CASCADE, SET NULL or actual cleanup authority.

Adopted public scope: five closed facts, optional existing-history descriptor,
2048-byte pre-materialization bound, ID256, whole descriptor accounting before
receipt encoding and final8MiB response; typed body4MiB remains separate. No raw
body, arguments or changed setting values published. Required finite controls:
old NULL/unknown, delivery deletion, fork/retry marker immutability, GC protection,
fresh/upgrade SQL convergence, secret sentinels and encoded boundary +/-1.
O is sole implementation author. One heavy queue; no installed migration42,
owner/service/private DB/RPC/browser/paid-provider operation permission. Source,
focused, exact-SHA CI and fresh nonforce main qualification remain separate gates.
