# #50: single-conversation first version

One conversation, gpt-6.1-sol, no child delegation. Dedicated issue-50/solo
worktree, base 1fec7f0 (DB v14). No schema migration, dependency, worker or queue.
Design first; implementation removes more code than it adds. The prior #47
worktrees/refs remain preserved; cleanup was optional and not needed.

Scope chosen: remove compact_hint_tokens instead of retaining an override;
use /compact-max rather than a new dialog. Keep the existing short recovery line
and F8 error facts for the upcoming #41/#58 rewrite. User-action history remains
in existing receipts, not a new ledger. Actual provider errors/physical terminals
are not proven by mocks. The next issue #59 is not started.

Self-review/regression found that a marker authored in a Turn could become the
apparent request boundary on another reduction. Maximum-edit request selection
now prefers the selected user request without changing nested summary boundaries.
Legacy estimated-policy tests were replaced by the new factual retry contract;
zero retries now suppress explicit-overflow resends too, so those stage fixtures
explicitly configure two resends. Sources and already-executed tools are preserved.

At the fixed pre-CI checkpoint (2026-10-30T09:26:40Z, based on this task's saved
usage read through the #47 CLI), 71 completed model attempts reported 34,621,445
input tokens, including 25,966,080 cached, and 49,617 output tokens. Model-attempt
latency sum was 2,050,371 ms; ended-shell latency sum 977,583 ms. These overlap,
are not active-work/CPU totals, and omit later record/CI/finish requests. The
existing conversation already had a very large context: this is not a controlled
fresh-context comparison. Monetary cost is unmeasured; no IDs/raw logs/private
attribution data are published. No model summary/paid probe was requested.

Local standard check: 7 Rust groups / 1,588 pass / 0 fail / 31 ignored,
layout 88, Python 6, fmt/strict Clippy. Background plus wait observed completion.
Pre-push Windows and final main CI are observed after this record and reported
in the completion message; later CI failures must not be silently retried away.
