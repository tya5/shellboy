# Reactive history retention (#58)

This is the adopted correction beneath the conversation redesign (#41). Keep
the existing canonical DAG, DB worker, bounded history/source windows, layout
cache and notification-driven refresh. No protocol, schema, producer event,
worker or queue is added.

## Follow and asynchronous snapshots

Follow belongs to explicit navigation and saved view restoration. A background
HistoryWindow/rebase reply cannot turn it off. While following, an anchor window
must not replace a newer tail with an older page. Merge verified overlapping
canonical ordinals when safe, preserve the current tail and bounded provisional
streams, and schedule the ordinary latest-page refresh when the reply cannot
establish a current tail. Never claim discontinuous ordinals are contiguous.

Keep provisional/committed preview bindings until the corresponding canonical
node is actually installed with a body (or a verified truly empty body). Merely
receiving an ID, rebasing the window, or a read failure does not retire paid text.
An explicit selected-path change may relocate removed canonical content, but
does not silently reclassify audit/provisional content as adopted history.

History and queue already share a serialized DB read_revision and view/generation
fences. Reuse those facts; do not add a second request engine. A stale paired
reply must not overwrite newer queue facts or topology. If it owns a live
navigation slot, release/reissue that slot rather than leaving loading stuck.
Node-only hydration is independent of a later queue-only snapshot: stamp each
source window using the same existing read counter, and compare against that
node's accepted source-window stamp. Do not advance queue read_revision from a
node-only reply. Retain generation, navigation and membership validation.

## Finite working set and hydration

Protect the last actually painted viewport IDs, capped by the existing128-node
projection, plus the finite current source neighborhood and retained provisional
windows. Replace that set on the next paint and reset it on view/generation
change. Do not pin an entire historical Turn or run history_layout inside cache
trimming. Protected windows remain the existing bounded body-window sizes; the
body cache budget excludes this finite working set as it already excluded the
three-node neighborhood. Other views and offscreen bodies remain reclaimable.

Explicit ephemeral eviction and read-order scalars on the presentation snapshot
distinguish evicted content from legitimately empty tool/event nodes. They are
not serialized, persisted or included in semantic projection identity. Eviction
releases payload while preserving the scalar source identity/order and eviction
fact. Every evicted kind can request the existing bounded source reader; no
Prose/Reasoning-only test may prevent hydration. Loading/unavailable content has
an explicit placeholder, rather than an empty successful row. Existing finite
in-flight request limits still apply. A failed read does not erase a known body.

## Diagnostics and validation

Use the existing best-effort256-entry diagnostic writer, injected as an optional
handle into the UI. Fixed reasons and scalar counts describe window replacement,
follow changes and cache eviction. Include Thread/generation/view/read identities
where available; never log text, commands, titles, output, paths or credentials.
No I/O, formatting of unbounded bodies, DB work or writer join runs on the UI
thread. Do not emit one record per animation frame or per output byte.

Test follow versus paused views, multiple commits, missing/empty canonical bodies,
reversed same-generation replies, newer queue facts with older required body
hydration, tiny shared budgets across multiple Threads, visible tool/event/prose
retention and offscreen reload. Synthetic long history and loopback streams must
retain current-Turn content between requests; paused/explicit navigation remains
stable. Record native PTY observation separately from TestBackend checks.
