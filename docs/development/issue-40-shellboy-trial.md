# Issue #40: developing Shellboy inside Shellboy

## Scope and execution boundary

The user handed off implementation after read-only preparation. The coder works
from `91f92c2` in a task-specific worktree, with separate leader, reviewer and
integrator Threads. Implementation commits are split into unchanged-input
retry, index stop-code rendering, and draft-save rejection wording. No schema,
protocol version, dependency, worker or queue is added. The installed release
continues to own the development conversations; locally built debug binaries
and synthetic temporary stores are used for tests. The owner is not replaced
or restarted.

The main measurement window starts at the user's handoff signal,
2026-10-29 16:41:31 +09:00, and ends at the final issue report. Read-only
preparation is counted separately. The leader collects final model/Thread/Turn/
message counts and measured cost for the integrator to append; no tentative
count or inferred price is presented as final. The coder's configured model is
`gpt-6.1-sol` through the `litellm` connection.

## Implementation observations

- The initial focused test command could not find `cargo` on PATH. Tests did not
  start; adding `$HOME/.cargo/bin` to the child shell environment exposed the
  pinned Cargo 1.99.1 without changing installed tools or dependencies.
- The integrator also encountered an initial PATH failure (`rustc` unavailable,
  exit 127 before testing). A command-local `$HOME/.cargo/bin` PATH correction
  restarted the same integrated SHA without source changes.
- No automatic context compaction or context loss has been observed in the
  coder's implementation interval so far. This is not a claim about the other
  Threads or unobserved provider behavior.
- Incoming sibling messages can interrupt a shell tool's foreground observation.
  The returned execution ID was retained and inspected with `shell read` or
  `shell get`; interruption was not treated as command failure or completion.
- File edits and formatting succeeded. The existing regression explicitly
  asserted the buggy one-attempt behavior, so its two protected-history fixtures
  were retained and expanded rather than removing that coverage.
- Routine evidence/review coordination uses direct sibling messages. Human
  intervention established scope, the handoff gate and push ownership; no human
  source-code repair has been needed during the coder's implementation so far.

## Decisions

No-candidate reduction retains normal retry policy without spending the
reduction right. Structured `automatic_trim_attempt` diagnosis must also be
saved on intermediate failed attempts; otherwise a later success would hide
why the reduction was not applied. Status codes use serde's existing wire
names, not a parallel spelling table.

The optional draft notice is included because the post-#38 frontend has no
competing change in that branch. A rejected snapshot is not in the backlog even
with a live worker, so neither capacity nor stopped-worker wording promises an
automatic retry for it. Receiver closure is checked even before pump observes it.
Queue size, closing admission, coalescing and warning ownership are unchanged.

## Validation and continuation

Focused macOS arm64 checks use only loopback model mocks and temporary SQLite
stores. Responses retries: 22 passed, 1 existing ignored test. Library: 627
passed, 10 existing ignored tests. See [acceptance](../validation/acceptance.md)
for commands, coverage and remaining limits.

The coder's standard check at `e01d397` uses
`CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 RUST_TEST_THREADS=2 sh scripts/check.sh`.
It ran as a background shell, retained its execution ID and was observed using
`shellboy wait`. Observation timeouts and incoming-message interruptions left
the target running; the final wait reported completion, and a subsequent get
confirmed exit 0. The complete shell took 324,536 ms (about 5 minutes 25 seconds),
including the brief preceding review notification: this run did not exceed ten
minutes. No artificial delay was added to claim a longer test. Format, strict
all-target Clippy and all-target tests pass: 86 groups, 1,443 passed, 0 failed,
28 existing ignored tests. The record does not establish ten-minute execution
on this machine.

Independent review approved all three implementation commits without findings
and repeated Responses retries and the final library suite in its own worktree.
No review-fix commit was necessary. Post-integration standard checks and final
aggregate measurements remain separate evidence to append. The integrator
alone performs any approved push and records three-OS CI; local success is not
CI success.
