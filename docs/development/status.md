# Development status

Snapshot: `origin/main` at `482e7eadb39b0bed4059135842c9b148c162f70d` (2026-11-08). This page describes the checked-out main state, not release certification. The exact base commit and open PR were checked against Git history and GitHub on this date.

## Main

The current main implements the adopted reply-expected lifecycle (#194), protocol 4 with strict requests, tolerant result records, capability metadata and a generated schema (#244/#275), change-hint waits with separately opt-in answer text and reasoning activity (#243/#275), model-grouped and grouped compaction usage (#272), and branch-relative usage with branch provenance (#273). Thread archive/re-parent/tags and the bounded maintenance/recovery work described by the effective spec are also landed. The exact-tip validation for #275 (standard and all-OS CI) remains pending in the current main status record; other environment-specific limits are in [acceptance](../validation/acceptance.md). The design index links adopted contracts; individual designs may include historical proposal material, which is identified in context rather than representing current behavior.

## Branch work

Open pull request #223 (`issue-217/compaction-usage`, head `3c7491264d993441e5f3d35628079fa7f05a4853`) proposes grouped compaction usage totals. Grouped compaction usage is already present on main from `16784db966`; the PR is not treated as product state beyond its reviewed delta. Other local or remote branches are not treated as product state unless an open PR or an explicit current handoff identifies them.

## Not verified by repository CI

CI is not evidence of real-provider behavior, physical-terminal behavior, production-database performance, or release readiness unless a validation record names that environment and exact SHA. Current acceptance limitations and evidence are tracked in [acceptance](../validation/acceptance.md); do not infer these checks from an adopted design or a passing build.

The prior branch-by-branch ledger, including entries added to main after the original snapshot, is retained in [status history](status-history.md); its handoffs and pending-check notes are not current status.
