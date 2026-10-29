# Issues #36–#39 review follow-up (2026-10-30)

This round follows the reviews of `546102d`. Changes remain local and are split
by issue. Earlier test results are historical; current verification is recorded
in [acceptance](../validation/acceptance.md).

| Issue / finding | Disposition | Source |
| --- | --- | --- |
| #36 L1: Quit gate changes completion without a reservation | Gate only pending Revisit delivery; ordinary completion and child completion keep their normal path. | `ce2f101` |
| #36 L2: malformed Revisit metadata blocks unrelated lifecycle operations | Strict explicit reads remain errors; optional summaries omit malformed state, stop/startup remove it. SQL errors still propagate. No fabricated serde defaults. | `ce2f101`, `b1979b6` |
| #36 L3: delivery-capacity recovery can need an empty-input Turn | Document the existing recovery/retry consequence; no new retry engine. | `ce2f101` |
| #36 protocol version | Keep additive pre-1.0 operations at protocol 2; no old-owner operation fallback. | Existing adopted contract |
| #36 child completion / developer-role semantics | Document child Turn versus Run completion and ordinary authenticated developer-event delivery. | `ce2f101` |
| #37 L1: restart hint cannot repair structural history | Inspect the incomplete group, fork before it, or delete the Thread; test the real CLI paths and rejection before HTTP. | `b1979b6` |
| #37 L2: unrelated group identity / malformed capture metadata | Thread-level capture diagnostics have no invented group ID; validate typed Revisit data before capture writes and isolate that Thread. | `b1979b6` |
| #37 suggested isolation of CHECK-violating recovery JSON | Not adopted: current schema forbids it; SQLite quick_check remains fatal for actual integrity corruption. Separate real-owner test verifies this boundary. | `b1979b6` |
| #38 M1: duplicate HUP forces premature zsh exit | HUP is idempotent, including mixed TERM/HUP; only repeated explicit TERM escalates. | `d245d5c` |
| #38 M2: normal-close orphan | Withdrawn by the reviewer after correcting pgrep argument parsing. Baseline bash cleanup already succeeds; do not count this as a new defect fixed. | [correction](https://github.com/tya5/shellboy/issues/38#issuecomment-6501975229) |
| #38 test startup readiness | Require a read-only IPC round trip before creating the scenario; do not infer readiness from locator publication or retry mutations. | `9e4f8ea` |
| #38 L1: fixed deadline shorter than native cleanup | Derive Unix budget from configured cleanup and persistence bounds; Windows retains the OS callback limit. | `d245d5c` |
| #38 L2: terminal EOF without HUP | Nonblocking native terminal-health probe; real noncontrolling PTY test. | `d245d5c` |
| #38 L3: restoration may fail during forced exit | Explicit best-effort boundary and recovery instruction; bounded force must still work when terminal writes stall. | `d245d5c` |
| #38 force-path owned-process cleanup / diagnostic outcome | Best-effort KILL of live owned groups before force; nonblocking UI access, bounded ownership and fixed error code in Control end trace. | `d245d5c` |
| #38 unused event-stream / inherited ignored HUP / macOS high FD | Remove unused feature, observe inherited ignored HUP, document select descriptor limitation. | `d245d5c` |
| #38 tool stopping is acceptance rather than completion | Preserve existing lifecycle semantics; verify final durable shell/Run state separately. | `d245d5c` tests |
| #39 L1: exit marker hides ordinary state | Preserve ordinary state in selected detail and append the exit marker. | `f5769f0`, Clippy-only `0288bc1` |
| #39 L2: snapshot overwrites explicit stop reason | Match durable COALESCE semantics in startup and shutdown captures. | `f5769f0` |
| #39 L3: CLI factual fields undocumented | Document latest_run_stop_reason and Revisit summaries. | `f5769f0` |
| #39 latest-Run disagreement / repeated subqueries | Both views use created_at/id order; resolve one latest Run and test the actual query plan. No new sequence or index. | `f5769f0` |
| #36/#39 wording and TUI grouping polish | Retain scope in #35/#41 rather than enlarging lifecycle fixes. | Deferred |

The exact reviewer-provided `winclose2.py` (`pty.fork`, interactive `zsh -f -i`
or bash without rc files, TERM-ignoring shell) was run with fresh synthetic
stores. Baseline `546102d` zsh forces in about 0.1 seconds and leaves Run stopping /
shell running. Current zsh and bash finish in about 2.1 seconds with Run
stopped/app_shutdown, shell stopped and no orphan matches. Current zsh still
receives duplicate HUP, proving the change handles rather than suppresses it.
No private script path, synthetic database or raw diagnostic log is published.

Native Windows/Linux execution and three-OS CI are not implied by the macOS
results. No installed binary replacement, push or release is part of this round.
