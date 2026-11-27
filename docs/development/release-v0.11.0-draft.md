# Shellboy v0.11.0 release notes (draft)

## Breaking: Protocol 5

Protocol 5 requires an exact-version handshake: protocol-4 clients and owners cannot communicate with protocol-5 peers. Upgrade the owner, CLI and external clients together; there is no silent downgrade. Human reply counts now represent 17 or more as a lower bound, oldest can be unknown, and old Awaiting snapshot cursors expire under the live cursor contract.

## Before upgrading

- The conversation database migrates from schema 32 to 39. Migrations 33-35 add index quantization metadata and lookup indexes, Thread tags and cwd lookup, and bounded reclaim indexes; 36 adds bounded spill provenance and 37 records branch kinds, and 38 adds a partial index for pending human replies; 39 removes Run ownership and migrates independent Runs. Back up data and stop the owner before upgrading; older binaries cannot use the upgraded schema.
- Recovery-wait Turns now hold non-human messages by default. Use `--resume-recovery` to resume intentionally; human replies still resume (#263).
- Several non-safety restrictions on messaging and execution are removed (#259). Live-Run protection and data-integrity checks remain.
- Index removal reports `removed` for the logical removal decision; physical reclamation is bounded and may continue later (#267). Do not interpret it as proof all bytes have already been reclaimed.

## Changes since v0.10.0

- Unsaved model text is exposed through `events wait` with explicit gaps (#243 E3); Thread tags and cwd discovery organize conversations (#222).
- Owner configuration can be read and changed with `config get` and `config set` (#266); capacity and usage inspection also support external clients (#265). Disk free-space admission and the 50 GiB default are updated (#261).
- Connection setup and live connection generations improve configuration and keep each Run on its selected generation (#269). Live defaults do not overwrite saved Thread choices.
- Recovery-wait behavior is safer (#263). Editing a request now refuses while its Run is active instead of stopping it (#258). The #262 Awaiting inbox lists Threads needing a human reply. A recipient may have at most 16 pending reply duties; a new one is rejected at the limit. Legacy counts of 17 or more are reported as a lower bound. Thread deletion processes at most 16 duties per batch; garbage collection uses batches of 128.
- Runs are independent rather than owned by another Run; Awaiting combines pending human and Run replies with recipient tags (#276). Terminal processing continues in bounded batches of 128 candidates.
- Compaction avoids repeated oversized-summary retries and repeated spill near the trigger (#251, #271); Thread inspection labels retained compaction facts as incomplete when an expired restore may be missing (#264).
- Durable status attempts expose ordered `response_nodes` references for saved assistant responses (#279). Each ref has a node ID and nullable provider item ID and phase; `history.window` can read the saved node while it remains on the selected path. At most 256 candidates and 64 refs are examined/published. `response_nodes_truncated=true` means coverage is incomplete: an empty truncated list does not prove there was no saved response. There is no continuation cursor.
- `usage get --group-by model` groups saved attempts by model; summary usage without a saved model is not attributed (#272). Capacity reporting shows configured thresholds (#268).
- Global instructions can be configured for all Threads (#270). History exposes execution descriptions, and the core guides no longer point to Studio Apps. Other documentation and bounded-read fixes improve inspection.

## Release Scope

#279 is accepted for this candidate; #278 remains excluded. Final candidate validation is pending.


The candidate consists of main including the reviewed #262 16-item rejection variant. #276 is included in the release train; #280 is deferred to the next release. #278 and continuing-failure prompt improvements are also deferred unless a separate scope decision approves them. The exact release train requires final independent review and all-OS CI.

## Validation and limitations

A synthetic in-memory SQLite fixture (two Threads, one node) migrated 32 to 35 in 3.017 ms on macOS arm64, SQLite 3.51.0, one run. This measures SQL migration only, not a public-binary upgrade or production-sized database. The exact release candidate upgrade, installation and release artifacts remain to be qualified. Native Windows hardware, real provider behavior and production data were not tested in this preparation. CI results must be checked on the final candidate SHA.

## Feature evidence matrix

Each row names its issue, landed implementation SHA and acceptance section. CI `42475616229` reports success on main `dc98c52c`, but its check job was skipped (reuse); this alone does not establish 3-OS coverage. "Unverified" is not a claim of failure. Real-provider verification is not established by local or mock-provider tests.

| Feature / evidence | Designed | Implemented | CI (3 OS) | Real provider |
| --- | --- | --- | --- | --- |
| #243 E3; `28e2c9abb3`; `acceptance.md`: Chat on events.wait; Live model text on events.wait | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #222 tags/cwd; `2fca25962f`; `acceptance.md`: Thread tags and cwd filter | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #261; `facc05669c`; `acceptance.md`: Free-space guard | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #266; `48cdd9e59e`; `acceptance.md`: Configuration read and write | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #268; `8bdba5a27b`; `acceptance.md`: reference pending | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #269; `21e09ac19d`; `acceptance.md`: Stored keys and connection test | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #259; `dfffde451f`; `acceptance.md`: #259 small non-boundary restrictions | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #263; `45ba400bce`; `acceptance.md`: Recovery-wait message sends | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #258; `007f788f58`; `acceptance.md`: reference pending | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #251; `c987d76427`; `acceptance.md`: reference pending | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #271; `92281d1095`; `acceptance.md`: reference pending | Yes | Yes | Prior branch SHA `a302b7be6d`: run `42413703815` passed Linux/macOS/Windows; final tip pending | Unverified |
| #267; `f01aee7f76`; `acceptance.md`: #267 bounded index reclamation | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #265; `20b5ea8338`; `acceptance.md`: #265 storage threshold read | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #272; `10a7fa0ec5`; `acceptance.md`: #272 model-grouped bounded usage | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #279; `292ccca74560e56227f76ed4271dae70e1392926`; `acceptance.md`: #279 saved attempt response references | Yes | Yes | Prior landed SHA: normal `42491328721`, all-OS `42491337302` success; final tip pending | Unverified |
| #270; `7aa2ef3dd1`; `acceptance.md`: #270 planned/implemented acceptance | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #273; `c54ed50c98`; `acceptance.md`: #273 branch-relative usage and kind | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #264; `e7a7242d55`; `acceptance.md`: reference pending | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #276 independent Runs and unified replies; `dd88933623cb6b50fde0c07b139b604f613d65a7`; `acceptance.md`: #276 schema-39 follow-up after #262 | Yes | Yes | Pending final all-OS train CI | Unverified |
| #262; `65dc4574c1d22053f98708ca9f32e0ef5267042a`; `acceptance.md`: #262 per-recipient reply-duty admission cap; TUI Awaiting form follow-up | Yes | Yes | Prior landed SHA: `42525515734` and `42525519060` success; final tip pending | Unverified |

Evidence links refer to `docs/validation/acceptance.md` section names. Where no uniquely matching section is identified, the evidence is pending rather than a confirmed acceptance reference. Final release CI and 32-to-39 binary migration validation remain pending.

Synthetic schema 32-to-37 SQL migration: 3.683 ms in one in-memory run on macOS arm64 with SQLite 3.51.0 (two Threads, one node). This excludes binary startup and is not a production-scale estimate. Ledger and original columns were checked.

Synthetic schema 32-to-38 SQL migration: 5.101 ms in one in-memory run on macOS arm64 with SQLite 3.51.0 (two Threads, one node). This excludes binary startup and is not a production-scale estimate. Ledger, original columns and migration checks passed.



#276 synthetic schema-39 measurement on base 65dc with 3,303 Runs, 22,537 events and 49 recovery waits: Run DDL 0.018501916 s; full migration SQL 0.038415334 s; foreign-key violations 0. This is synthetic, not production timing.
