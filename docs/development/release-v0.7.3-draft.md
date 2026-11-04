# Shellboy v0.7.3 (draft; not verified for release)

Read-only startup diagnostics, more focused migration validation, and reliability fixes.

These notes cover changes on `main` after v0.7.2 baseline `22db1a226351123fc5b27309522f85987b301669`. This is a draft only: no release tag or published release exists, and the candidate has not completed release-upgrade validation.

## Breaking changes

- None identified in the changes listed here. Conversation schema advances from 27 to 27 (no migration); standalone RAG remains schema 4 (no migration). Verify these values against the final candidate before release.

## Before upgrading

- Stop the owner, back up the complete data directory, update the owner and CLI together, and restart the owner.
- Current source declares conversation schema 27 (`src/storage.rs`, `MIGRATION_NAMES` has entries 1–27; `src/storage/migrate.rs` applies through migration 27). Standalone RAG schema is 4 (`src/rag/store.rs`, `PRAGMA user_version=4`). From v0.7.2 schema 27, no schema migration is expected; confirm on the release candidate.
- Run the release upgrade check against the exact old and candidate binaries, with a fresh disposable output directory:
  `python3 scripts/check-release-upgrade.py --old OLD_BINARY --new NEW_BINARY --root EMPTY_TEMP_DIR --old-version 0.7.2 --start-schema 27`
  This check has not yet been run for a v0.7.3 candidate. The current script choices accept neither `0.7.2` for `--old-version` nor `27` for `--start-schema`, so this requested invocation is presently rejected by argument parsing; update the checker before release validation.

## Changes since v0.7.2

The landed items below are derived from `git log v0.7.2..origin/main` at `f0f719864f6b726e2efdf1ab68d66394b37fafcf`. Planned items are not on `main` and are listed separately. Each item includes user-visible effect, breaking-change/schema impact, and evidence. CI links establish only the reported CI outcome, not release-candidate verification.

- **macOS shutdown test reliability (#211):** fixes PTY screen synchronization in the shutdown test so completed terminal screens are observed rather than searching raw output diffs; reduces a flaky macOS CI failure. User-facing runtime behavior is unchanged. Breaking change: no; schema migration: none. Evidence: [issue #211](https://github.com/tya5/shellboy/issues/211), [main change](https://github.com/tya5/shellboy/commit/1b02aaa82d8e21671748f286648388eb6527b5bb), [CI](https://github.com/tya5/shellboy/actions/runs/41755980362). Release-level verification: not yet verified.
- **Quota recovery accounting (#205):** recovery retries deferred raw collection and checks durable pinned output rather than a GC-pass snapshot, avoiding incorrect quota assertions/decisions when collection changes during recovery. Breaking change: no; schema migration: none. Evidence: [issue #205](https://github.com/tya5/shellboy/issues/205), [retry change](https://github.com/tya5/shellboy/commit/f4128ad659), [accounting change](https://github.com/tya5/shellboy/commit/fc1c603ec9). Release-level verification: not yet verified. The cause of the Ubuntu failure reported for integrated CI 41751532504 is unresolved, and its relationship to #205 is unknown; no conclusion about that failure is drawn here.
- **Scoped startup foreign-key validation (#210):** startup uses migration impact metadata to skip FK scans for index-only migrations and scope checks for declared data impacts, while unknown impacts retain a full check. On one copy of a 3.18 GiB database, the reported comparison was 19.282 s for the full FK check versus 0.496 s for the three migration-target tables; these are single-copy measurements, not general startup guarantees. Breaking change: no; schema migration: none added (current schema remains 27). Evidence: [issue #210 measured-copy comment](https://github.com/tya5/shellboy/issues/210#issuecomment-6617705969), [implementation](https://github.com/tya5/shellboy/commit/d84720fad2), [CI](https://github.com/tya5/shellboy/actions/runs/41742842471). Release-level verification: not yet verified.
- **Bounded bootstrap recovery (#214):** startup bootstrap recovery seeks from the current cursor and bounds each page, avoiding repeated scans of already-processed records as recovery advances. Breaking change: no; schema migration: none. Evidence: [issue #214](https://github.com/tya5/shellboy/issues/214), [design](https://github.com/tya5/shellboy/commit/3b4cb82a6d), [implementation](https://github.com/tya5/shellboy/commit/197ed18f4b), [main integration](https://github.com/tya5/shellboy/commit/87bb092a06). Release-level verification: not yet verified.
- **Bounded authorization recovery (#203):** startup authorization recovery processes bounded keyset pages and resumes after a cursor, limiting per-page work while preserving committed progress on later errors. Breaking change: no; schema migration: none. Evidence: [issue #203](https://github.com/tya5/shellboy/issues/203), [main implementation](https://github.com/tya5/shellboy/commit/f0f719864f), [default CI](https://github.com/tya5/shellboy/actions/runs/41791980982), [3-OS CI](https://github.com/tya5/shellboy/actions/runs/41793199648). Release-level verification: not yet verified.
- **Conversation layout (#179):** nested details and wrapped conversation content remain indented, and narrow rows preserve wide glyphs instead of exceeding the viewport. Breaking change: no; schema migration: none. Evidence: [issue #179](https://github.com/tya5/shellboy/issues/179), [main change](https://github.com/tya5/shellboy/commit/f5ae5ec78c617015597ff2de205223d954484ab1), [CI](https://github.com/tya5/shellboy/actions/runs/41746802462). Release-level verification: not yet verified.
- **Read-only startup diagnostics (#212):** `shellboy startup [--last N]` reports recent startup stages from bounded current/rotated logs without starting the owner or exposing conversation content; missing/incomplete/corrupt records are signaled. Version increases use SemVer precedence, including RC-to-release transitions. Breaking change: no; schema migration: none. Evidence: [issue #212](https://github.com/tya5/shellboy/issues/212), [implementation](https://github.com/tya5/shellboy/commit/7dc182411c), [SemVer fix](https://github.com/tya5/shellboy/commit/4c39a1fcb3), [CI](https://github.com/tya5/shellboy/actions/runs/41761463223). Release-level verification: not yet verified.

## Planned / pending (not on main)

- **Reply usability (#213):** planned; approved candidate `eb3bdb6308b3478febf3fa8d4e8141fe310c2388` is not an ancestor of `origin/main`. Intended effect: document `reply_received` and expose reply-sender provenance in history. Breaking change: no; schema migration: none. Evidence/status: [issue #213](https://github.com/tya5/shellboy/issues/213), [candidate](https://github.com/tya5/shellboy/commit/eb3bdb6308b3478febf3fa8d4e8141fe310c2388); integration/rebase pending. Not landed; release-level verification not performed.
- **#201:** pending/not landed on `main`; no release-note claim is made. Breaking change and schema impact: not assessed for this draft. Evidence/status: [issue #201](https://github.com/tya5/shellboy/issues/201). Keep unverified until its landing and evidence are confirmed.

## Downloads and validation

No candidate archive or checksum has been verified. The v0.7.2-to-v0.7.3 synthetic upgrade check, final default CI for a release candidate, platform matrix, and release artifact validation remain outstanding. Do not treat this draft as a release or replace an installed binary.
