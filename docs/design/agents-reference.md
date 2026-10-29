# Per-Thread instruction reference (#51)

A Thread owns one lexical absolute `agents_path`, independently of cwd. Root
creation defaults to AGENTS.md in its effective cwd; children and forks inherit
the parent's current reference, not a historical Turn snapshot. Explicit
`agents` on thread.create/set/fork overrides this default. Relative references
resolve against the effective cwd after any cwd argument; otherwise the target
Thread/parent cwd (startup cwd for a root) is used. No ancestor scan, composition,
canonicalization or reference-clearing operation is added. Empty explicit paths
are invalid arguments.

Explicit selection validates a readable regular UTF-8 file of at most 8 MiB,
using the existing bounded control-file workers, including a limit-plus-one read
against growth races. Explicit missing paths fail with invalid_argument; unreadable/invalid UTF-8 paths fail with io_failed;
oversized content fails with input_limit. Default missing instructions still
mean no instructions at Turn preparation. Validation is not a pin: later edits,
removal and access changes follow existing Turn preparation behavior. I/O stays
off UI and database workers. Validate before atomic mutation; recheck the source
Thread revision to reject changes during validation rather than persisting a
reference resolved against stale cwd. Other settings never partially commit.

Humans and authenticated models have the existing thread.set authority, including
explicit other-Thread targets; instruction references have no special ownership
permission. The existing event table records context_changed/agents_reference
with real caller identity, old/new path and time only when the resolved path
changes. Explicit selection of an unchanged path is still validated. Creating or
forking with an override records the default/inherited path as the old reference.

Reference changes do not wake work or replace the current Turn snapshot. A
change during a Turn is delivered exactly once at the next *new* Turn, never to
later requests or explicit retries of the old Turn. Only agents_reference is
filtered this way; other context events retain their existing timing.
instructions_observed remains the independent record of actual file observation.
No new table, worker or queue is required.

CLI exposes --agents on create/set/fork and agents_path in thread get. TUI exposes
an optional reference in create/fork/settings forms and /agents [PATH] (without
an argument, display the current reference). The current Thread view shows the
reference. Paths remain literal data and must be rendered safely.
