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
2026-10-29T16:41:31Z, using saved UTC event times, and ends at the fixed
aggregate cutoff below. Read-only
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
No review-fix commit was necessary. The integrator fast-forwarded the exact
reviewed commits without squashing. Its separate standard check at `e01d397`
passed format, strict all-target Clippy and 86 groups / 1,443 passed / 0 failed /
28 ignored. The background shell completed in 433,698 ms (about 7 minutes
14 seconds), with `shellboy wait` observations and paginated output retrieval.
Neither local full check exceeded ten minutes. No artificial delay was added.

The reviewed four-commit series through `8ff6c46` was pushed normally to
`issue-40/integration`. [Native CI run 40916772603](https://github.com/tya5/shellboy/actions/runs/40916772603)
passed all three OS jobs, including format, strict Clippy, full tests, the
closed-proxy diagnostic and two fresh attachment processes; Windows also passed
its separate overflow diagnostic. Full-test totals exclude those repeated
focused checks: Linux 1,444 passed / 0 failed / 27 ignored, macOS 1,443 / 0 / 28,
Windows 1,332 / 0 / 20, each across 86 groups. The Windows Tests step lasted
10 minutes 48 seconds on CI; this is not evidence of a local ten-minute test
being observed through `shellboy wait`. The CI watcher itself ran as a background
shell for about 14 minutes 33 seconds and completed successfully.

The final measurement/validation documentation is a separate review boundary.
CI at `8ff6c46` does not establish success for a later documentation commit.
No PR, issue closure, release, installed-binary replacement or owner restart is
part of this task.

## Fixed aggregate measurement

The leader supplied the final aggregate over the half-open UTC interval
[2026-10-29T16:41:31Z, 2026-10-29T17:13:09Z). Read-only preparation is excluded.
Documentation after the cutoff, its re-review, final pushes and later CI
observations are explicitly outside this measurement; it is not a count through
the eventual final response.

| Role | Observed model setting | Turns | Inbound user nodes |
| --- | --- | ---: | ---: |
| Leader | `gpt-6.1-sol` | 12 | 10 |
| Coder | `gpt-6.1-sol` | 4 | 10 |
| Reviewer | `gpt-6.1-sol` | 5 | 7 |
| Integrator | `gpt-6.1-sol` | 9 | 9 |
| Total (4 Threads) | | 30 | 36 |

Method: paginate each Thread's `history list`, filter `provenance.created_at`
by the interval above, count distinct `provenance.turn_id` within each Thread
and sum, and separately count `kind=user` nodes. Inbound nodes include agent
messages, not just human input; they are neither model-call counts nor human
conversation counts. Event-started and empty-answer Turns are included. The
leader checked each Thread's current configured model using `thread get`;
that setting is not per-model API billing attribution. Cost and total token
usage were not measured and are not estimated. Only aggregate counts and their
definitions are recorded, not private conversations, raw logs or Thread IDs.

During this final documentation pass, `apply_patch` was unavailable. The failed
command made no edits; its compound shell ended with a successful read-only Git
command, so the shell's exit 0 alone did not establish editing success. The
integrator inspected the output and unchanged diff, then used bounded Python
text replacements. This happened after the aggregate cutoff.
