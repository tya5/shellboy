# Issues #44/#45: single-conversation Shellboy trial

## Scope and boundary

One existing conversation, model `gpt-6.1-sol`, with no child conversations,
delegation or sibling messaging. The agent implemented, tested, read back its
own diffs, committed and pushed the work. There is no claim of independent
review: passing self-review and CI cannot measure independent-review misses.
A human observer may review externally; no such review is claimed here.

Implementation began at 2026-10-29T20:32:42.412215+00:00. The fixed accounting cutoff is
2026-10-29T21:00:18.337931+00:00 (exclusive), after the first three-OS CI succeeded.
The metrics cover only this task's Run and exclude the preceding role-policy
clarification, subsequent record writing, final main push/CI and final answer.
The conversation already contained the previous team experiment; this was not
a fresh-context controlled comparison. No private IDs or raw logs are included.

## Work and evidence

- `ba0cb10`: shared activity classifier and typed thread-list activity, matching
  the footer while retaining native state. SQL-plan, lifecycle and UI fixtures.
- `8ecc48c`: self-review follow-up avoiding running-only lookups on completed
  answers and terminal Runs. No new schema/index/worker/poller.
- `68f4f5d`: shared read-time shell timing reader, transactional shell get and
  bounded status enrichment; saved terminal duration and output retention facts.
- macOS arm64 / Rust 1.99.1: `sh scripts/check.sh` exit 0, 86 groups,
  1,451 passed / 0 failed / 28 ignored, elapsed 247,940 ms. Background shell
  completion was observed using `shellboy wait`, not inferred from a receipt.
- Final local format/strict all-target Clippy passed again. Focused list tests
  passed (2); work_status passed (14); browser integration passed (6).
- Synthetic performance observations, debug build: 2,000 background Threads
  across 40 pages: 267 ms (earlier 279 ms); first 50-row status page from
  1,001 shells: 62 ms. These are local observations, not cross-machine SLAs or
  a rigorous before/after performance comparison. Query plans require the
  existing latest-Run/ownership indexes and output_chunk primary key.
- [CI 40951871499](https://github.com/tya5/shellboy/actions/runs/40951871499)
  checks exact head `68f4f5dfaec20b120ea2eaa70f97002aa03be091`: all jobs
  successful. Tests-step totals: Linux 86 groups / 1,452 passed / 0 failed /
  27 ignored; macOS 86 / 1,451 / 0 / 28; Windows 86 / 1,340 / 0 / 20.
  This CI does not attest later documentation commits; final exact-SHA CI is
  observed separately and reported in the completion message.

## Requests, usage and time

Source: a read-only query of this task's model_attempt usage records. Only
aggregate numeric measurements are exported. Each stored request attempt is
counted once, not each tool call or Turn. There are 49 attempts:
49 completed, 0 other states;
49 have usage records. No transport retry is inferred from tool failures.

| Measurement | Observed value |
| --- | ---: |
| Input tokens | 8,305,231 |
| Cached input tokens (included in input) | 4,740,608 |
| Input minus cached input | 3,564,623 |
| Output tokens | 26,384 |
| Wall-clock seconds in accounting window | 1655.926 |
| Sum of ended model-attempt intervals, seconds | 730.057 |
| Explicit wait-containing shell intervals, seconds | 896.666 |

Wait-containing shell intervals can include small inspection-command overhead;
model intervals are request latency, not CPU or cognitive active time. Build
and test execution overlaps model work. These categories must not be summed
as disjoint active/wait totals. Precise active-work time and monetary cost are
not measured. Recorded usage is provider-reported, not independently billed.

The issue's supplied #40 comparison is 242 requests, 13.44M input (1.93M
uncached), 49.0k output, with approximately 51% coordinator-role tokens. At
this cutoff this single-conversation run has fewer requests, total input and
output, but **more uncached input** than the supplied #40 figure. Task scope,
starting context, accounting endpoints and review independence differ, so this
is not evidence of universally lower cost or equal defect-detection quality.

## Interruptions, review feedback and limits

- Before implementation, the human clarified solo work and updated the role
  policy after the agent initially refused under the former leader-only rule.
  This was policy intervention, not debugging or code repair. During the
  implementation window no human code intervention was needed.
- Local feedback: two library runs failed on the same UI color fixture that
  changed native state without updating the new activity field. A focused
  integration build then caught one ambiguous generic `.into()` in the new
  test. Both were corrected; the final standard check has no failures/warnings.
- Self-review identified one query-efficiency improvement (the separate #44
  follow-up commit) and clarified live wall-clock versus terminal monotonic
  duration in the adopted design. No unaddressed self-review defect remains;
  this is not an independent review finding count.
- No trim/summary was requested, no context-loss interruption was observed,
  and 0 replacements were recorded for this task's output nodes.
- The initial worktree was created at an unintended staging name and moved
  with `git worktree move` before editing; no user files were overwritten.
  An early telemetry inspection printed excessive attribution metadata locally;
  subsequent queries exported only aggregate counters. No raw attribution,
  conversation or credentials are committed.
- Existing ignored tests, real providers, physical terminal behavior and
  unavailable OS environments are not represented by local mock success.
  Three-OS CI is distinct from physical-terminal testing. The installed owner
  and real user data were not changed. No PR, release or issue close is implied.

## Post-cutoff external review of #44

The public review subsequently found one Medium issue: long activity labels
were clipped in the narrow table column, making background work ambiguous.
This was missed by the original self-review/tests, which could see full labels
in selected details. Fix 99982d7 adds bounded column labels and directly tests
the actual state-column cells for every phase at responsive widths; see the
review follow-up in acceptance. One Low timestamp-ordering observation is
recorded without a change, as requested by the reviewer. This follow-up is
outside the fixed request/token/time accounting above and demonstrates why
passing tests alone did not establish independent-review-equivalent quality.
