# Index pause, deterministic batches and explicit dimensions (#42/#43/#55)

This extends the existing index workers, job/space lifecycle and app_meta records.
No new table, worker, queue, provider catalogue, dimension-setting UI or generic
job engine is introduced. Existing source authorization, capacity checks,
late-result witnesses, cancellation and candidate activation fences remain.

## Pause and recovery observations

A saved pause records paused_at once when that pause/reason begins. Reobserving
or rendering it never advances the timestamp. Missing historical time remains
unknown; do not invent a past stop time. Status and context observations expose
paused_at plus a stable pause identity. Ordinary per-Thread observation markers
still deduplicate unchanged state at existing Turn boundaries. Human presentation
keeps actionable index pauses out of routine event grouping, leads with the
recovery path, and distinguishes the historical reason from a request currently
waiting. Index updates stop; existing indexed search and conversation may continue.
Immutable history remains after resumption, labeled as a historical record;
current status/attention clears through existing status/observation replies.
There is no new poller or claim of immediate worker-to-UI notification.

Index management renders preview loading/result/error in its body. P without a
selected source scope gives explicit guidance. T may retry globally, like the CLI,
using a read-only default preview of currently failed/retry-eligible authorized
work and the existing confirmation flow. Backfill/rebuild still need an explicit
source selection. New guide text describes actual working keys and prohibits
models from initiating paid retry/backfill/rebuild without explicit user consent.

## Batches

Embedding settings add batch_max_inputs (default1, range1..128) and
batch_max_chars (default64,000, range1..1,048,576). A multi-input configuration
requires its char cap to cover indexing.split_chars. Default1 retains the current
request shape/behavior and makes no claim that a proxy supports provider batching.
No provider limits are inferred: users choose caps for their configured proxy.
This is synchronous embeddings, not a deferred discount Batch API; token volume
and price are not assumed to fall.

At one serialized DB claim snapshot, capture due eligibility once and select
by existing next_attempt_at/id order, one space/group/request mode, fixed count
and Unicode-character caps. Bootstrap/first-dimension validation stays singleton;
only one unknown-space request per group may be inflight. No time window,
random grouping, latency adaptation or inferred provider tokenizer is used.
Whole network traces may vary as workers reach different saved snapshots; the
claim decision for an identical snapshot/settings/cutoff is deterministic.
An individually authorized input not eligible for aggregation stays singleton;
never silently truncate, resplit or discard it merely to fill a batch.

A malformed/mismatched multi response adopts nothing. Record failure on every
member, then requeue each with a durable singleton-fallback marker in existing
error metadata. Preserve singleton mode through existing length splits. The
aggregate failure does not multiply the endpoint's common-failure streak;
individual retries use the existing failure rules. Authentication/credit/missing
configuration still pause immediately, and cancellation does not authorize new
requests. Batch attempt count remains factual; guarantee an individual fallback
even if the configured ordinary retry count is zero. Errors may clear on eventual
success as they do today; no permanent per-attempt history table is added.
Validate indices/counts/dimensions/finite vectors before any adoption, and adopt
all valid members in one transaction with individual source/space witness checks.
The configured embedding request_timeout_ms applies once to the whole request;
query embeddings remain single-input.

## Explicit dimension operation

IndexArgs gains optional dimensions, valid only for rebuild; IndexProbeArgs has
optional dimensions. Bounds are1..8192 (the installed vec0 storage limit), not a
provider capability list. CLI adds index rebuild --dimensions N and index probe
[--dimensions N]. No new persistent config setting selects dimensions.

HTTP embeds separate expected_dimension (validation) and requested_dimensions
(wire option). Native/default mode omits dimensions exactly as before. Explicit
mode sends N and validates the actual vector length; never truncate, retry at a
native default, or accept a partial/misindexed response.

Freeze an optional request mode per rebuild group in app_meta, capture it in the
in-memory claim inside the same DB transaction, and bind it to the
new space. Existing/native spaces with no explicit request record remain native
mode, not an inferred request for their observed length. Rebuild without N keeps
the current matching active space's request mode. A changed endpoint/model starts
native mode unless N is explicit. Query embedding, including history-backed RAG,
and future active ingestion use the selected space's saved mode. Profile endpoint
fingerprints, split policy and observed dimensions remain independently checked.

The first candidate request is a real authorized source, not an extra probe.
Serialize its discovery against other bootstrap claims in the group. On success,
adopt its vector and bind the space/request mode; only then admit remaining jobs.
On dimension refusal/mismatch stop that candidate group, mark its remaining work
failed without further HTTP, preserve the active space, and record the reason.
Transient transport/rate/server errors and explicit input-length failures retain
existing retry/split rules for the same N. Common authentication/credit/configuration
errors retain existing endpoint-family pause rules.
Existing selection/retry can inspect or explicitly retry the failed candidate;
no alternative dimension is tried automatically. The old active vectors remain
searchable throughout. Switch only after complete catch-up, then old space data
becomes inactive and eligible for existing explicit GC.

Probe sends one fixed short input exactly once, with the same HTTP timeout and
optional request mode. Persist an observation keyed by configured connection/
endpoint/model/requested dimensions, successful or failed, in app_meta; expose
only the newest32 observations. It is not a capability catalogue or a promise
about future requests. Use ordinary mutation receipts/idempotence and owner-stop
cancellation. No hidden human-consent gate is added to Core.

Status exposes up to200 retained space facts (selected first), total/truncation,
connection name, model, observed/requested dimension, resident indexed segment
count, state and vector_payload_bytes=count*dimension*4. This arithmetic is
float32 vector payload, not SQLite file/WAL/allocation/reclaimable bytes. Candidate
creation temporarily adds storage; old data is not magically freed on switch.
Agent guidance obtains status/preview first, states count/characters, conditional
payload arithmetic, possible quality loss and potential API charges, and obtains
explicit consent. Probe itself can cost one small request; never guess support
from an aliased model name or automatically try a different N after failure.

Validation uses synthetic temporary stores, loopback HTTP and native PTY where
available. Test stable batches/caps/singleton fallback, reordered/invalid vectors,
first-request rejection under concurrent workers, active search during candidate
build, first-vector reuse, query/ingest request mode after switch/restart, bounded
probe records/receipts, saved pause times/dedup and real management key sequences.
No real API billing or user datastore changes are routine verification.

## Explicit inactive-space collection

Only explicit `storage collect --category index` / `all` collection removes retained
inactive spaces. Each DB transaction collects at most32 spaces in ID order;
existing DB serialization owns exclusion and shutdown. A space must be inactive,
not the selected active pointer, not referenced by a current rebuild/candidate
group, and have no running job. Revalidate these facts in the deletion transaction.
Validate generated vector-table identifiers before issuing DDL. Drop its vector
table and delete chunks/previews, job inputs/jobs, space and per-space request/
query metadata plus obsolete group mappings atomically. Any failure rolls back
the whole batch. No source node, unrelated bootstrap input or active/candidate
space is deleted. Collection reports `inactive_index_spaces_collected`; freed
SQLite pages may be reused, but physical database/WAL file shrink is not promised.

The default retry preview mirrors retry's saved target set: selected active,
current rebuild and current-profile active-group spaces, not every historical
failed space. Explicit space restricts known jobs; as in existing retry behavior,
current-profile failed admissions and current bootstrap groups remain eligible.
Known saved input segments contribute exact Unicode-character counts. Failed
admissions whose source has not yet been read count as unavailable sources, with
no invented segment/character estimate and no file/API reads or new authorization.
