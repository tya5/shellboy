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

### Persistence-only Rust fork helpers

`History::fork` and `history::fork_thread` only inherit the saved instruction
reference. They reject any explicit `agents: Some(...)`, including an empty
string, with invalid_argument before writes, directing callers to Control
thread.fork. These DB-only helpers do not acquire file workers or invent an
actor. Their returned agents_path is the actual inherited path. Product
Control/CLI/TUI fork supports explicit references through validation, CAS and
audit before invoking the primitive with an inherited/default argument.

## Outbound provenance (#48)

The instruction developer message includes a one-line, safely quoted source-path
header under the [Control contract](control.md#working-instruction-provenance-header-48).
Its path follows the captured Turn snapshot, including after thread.set changes
the configured reference. The prefix is not part of the saved body/hash. It does
not change explicit selection, missing/empty-file behavior or instruction roles.

## Configured common instructions (#270)

`extra_instructions = ["/absolute/path/to/common.md"]` is an ordered, opt-in owner-wide list. No directory is
searched, linked file expanded, or conventional global filename implicitly loaded. Paths must be absolute UTF-8,
at most 4096 bytes each; at most eight entries are accepted. Each file must be regular, UTF-8 and at most 8192 bytes
of captured content, including an empty file. This bounds extra content to 64 KiB. Symlinks to regular files follow
the ordinary file-read rules. These files are trusted user configuration, not a sandbox or a secret store.

At new-Turn preparation, the existing bounded control-file lane reads the current adopted list and all file bodies.
Missing, unreadable, nonregular, invalid-UTF-8 or oversized configured files fail preparation visibly; nothing is
silently excluded and no partially assembled request is sent. Existing recovery/retry reporting surfaces the error
and path. Unlike optional Thread AGENTS absence, explicitly registered common-file absence is an error.

Built-in instructions remain first, then each common file in configured order as a separate developer item, then
Thread AGENTS if present. Headers identify the source path; body text does not gain timestamps or changing metadata.
This preserves the common prompt prefix across Thread-role changes. Instructions are not merged into a user's
AGENTS file, and files should be short guides pointing to capabilities rather than copied tool manuals.

The existing immutable instruction snapshot stores the composed bodies and source hashes/sizes. Same-Turn tool
continuations, retries and recovered/resumed attempts reuse it, including after restart. New Turns read again;
file edits and configuration changes never enter an already prepared Turn. An unprepared failed Turn can retry
preparation after the cause is fixed. `config.set` adopts this list without restart; direct disk edits remain
restart-only. This per-Turn rule deliberately differs from per-Run connection-generation pinning.

`thread get.instruction_sources` reports extra paths, content hashes and captured byte sizes for the latest prepared
Turn, not a prediction of the next one. `thread request` reports saved source metadata and separate instruction
items (characters/hashes); `--body` can show the saved content. Existing instruction-change context events include
the composite hash. No new worker, database table, queue or filesystem watcher is added.
