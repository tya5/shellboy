# Responses Prompt Cache Diagnostics (#142, Part 1)

## Scope

Adopt only issue #142 part 1. The issue comment
https://github.com/tya5/shellboy/issues/142#issuecomment-6558350298 defers sticky
headers: no sticky header configuration, capture, forwarding or proxy-specific
behavior is added. Verification uses loopback fixtures, never a real provider.

`connections.<name>.prompt_cache_diagnostics` is a boolean, default false,
valid when enabled only for `api = "responses"`. It requires `prompt_caching`
(the existing master cache opt-out). Responses owns `prompt_cache_options` as
a dynamic request field; `extra_body` cannot set it. Other static provider
extensions remain unchanged. No TTL/mode or provider-specific compatibility
guesses are introduced. The first request, or a missing/invalid comparison identity, omits
the options object rather than inventing an identity.

## Identity And Adoption

When enabled, send the immediately preceding adopted response's opaque ID in
`prompt_cache_options.comparison_response_id`, on the same Thread.
Use exactly the context display's current connection/model scope and finite
recent-Turn selection: at most 33 Turns, with one completed-attempt index seek
per Turn. The shared selection CTE uses existing `turns_by_thread` and
`attempts_completed_scope`, then chooses the newest native attempt rowid among
those at most 33 witnesses. No history-length scan or persisted derived pointer
is added. No matching adopted attempt in that window means no comparison and
no `prompt_cache_options`; do not search an older window or manufacture evidence.

Failed, incomplete, cancelled and stale attempts cannot replace an adopted
identity. An adopted response with a missing/invalid ID never falls back to an
older response. IDs must be nonempty and at most 1024 UTF-8 bytes. Turns and
owner restarts use saved attempt rows; forks have no own completed attempt.
Conversation schema remains 19, unchanged from main; no migration, column,
index, extra adoption write, compatibility path or persistent cache is added.

## Bounded Observations

The provider's optional `prompt_cache_diagnostics` object is whitelisted to
`type`, `reason`, `cache_missed_tokens`. Keep only a nonempty string type of at
most 64 UTF-8 bytes, a string reason of at most 256 bytes, and an integer count
in `0..=9223372036854775807`. Unknown type/reason strings within those bounds
remain provider facts, not an enum-based inference. Invalid/oversized fields
are omitted, not truncated; absent/invalid type means no diagnostic object.
Unknown fields are discarded without recursive copying. The encoded retained
object is at most 2048 bytes. Missing diagnostics are JSON null, never zero,
cache hit, a guessed reason, or reconstructed from token totals.

Normalize this member independently of the existing usage-counter whitelist.
Save it as bounded metadata in `model_attempt.input_manifest_json`, like the
existing post-response HTTP observation, on the same guarded terminal attempt
transition. The manifest also records whether diagnosis was enabled and the
actual comparison ID at admission, even when absent. Failed/incomplete terminal
responses may supply reported facts, but are excluded from completed-attempt selection.
Late/cancelled results do not bypass transition guards. Existing manifest byte
accounting and GC already own these bytes; no second usage ledger is added.

## Inspection

`thread request --attempt` exposes saved diagnostics and uses the saved
comparison identity, not the Thread's current last response. Its rebuilt wire
body must remain byte-identical to the loopback request under unchanged config.
The next-request inspection uses the current bounded selection. A changed diagnosis
setting or missing historical request witness must be reported as unknown when
it prevents exact reconstruction, not filled from the current Thread.

`usage get --attempt ID` is an additive, single-attempt inspection, mutually
exclusive with aggregate scope/time/group/pagination options. It returns the
attempt's identity/state, bounded standard usage scalar projection and
`prompt_cache_diagnostics` (object or null). It is read-only, one primary-key
lookup with guarded bounded JSON projection; it does not load raw response,
error, history or arbitrarily large usage documents. Existing aggregate usage
queries/output and UI refresh work remain unchanged. The CLI, wire validation
and operation-field metadata describe the same optional selector.

## Acceptance

Loopback tests cover default-off, enabled first/next/tool-loop/cross-Turn IDs,
Thread isolation, restart/fork, missing IDs, failed/stale adoption, JSON/SSE
response facts, missing/malformed/oversized diagnostics, preserved zero and
unknown reasons, usage/attempt request visibility and exact wire reconstruction.
Config tests cover defaults, strict boolean, dialect/master-opt-out conflicts
and reserved extension fields. Bounded-query tests cover the 33/34-Turn boundary,
missing ID, scope, read-only behavior, index plan and work independent of older
history. No provider/cache effectiveness, LiteLLM behavior, sticky routing, real
network model or physical terminal claim.

## Supporting Surface Maintenance

The rare F8 confirmation result boxes its `Command` payload so the additive
usage selector does not enlarge every management result past the enum size
bound; confirmation behavior is unchanged. Storage bootstrap, migrations,
historical fixtures and release-upgrade/error-catalogue helpers remain identical
to main's schema19. Prior schema20 validation records are superseded, not current
upgrade requirements.
