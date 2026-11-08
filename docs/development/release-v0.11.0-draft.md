# Shellboy v0.11.0 release notes (draft)

## Before upgrading

- The conversation database migrates from schema 32 to 37. Migrations 33-35 add index quantization metadata and lookup indexes, Thread tags and cwd lookup, and bounded reclaim indexes; 36 adds bounded spill provenance and 37 adds branch usage support. Back up data and stop the owner before upgrading; older binaries cannot use the upgraded schema.
- Recovery-wait Turns now hold non-human messages by default. Use `--resume-recovery` to resume intentionally; human replies still resume (#263).
- Several non-safety restrictions on messaging and execution are removed (#259). Live-Run protection and data-integrity checks remain.
- Index removal reports `removed` for the logical removal decision; physical reclamation is bounded and may continue later (#267). Do not interpret it as proof all bytes have already been reclaimed.

## Changes since v0.10.0

- Unsaved model text is exposed through `events wait` with explicit gaps (#243 E3); Thread tags and cwd discovery organize conversations (#222).
- Owner configuration can be read and changed with `config get` and `config set` (#266); capacity and usage inspection also support external clients (#265). Disk free-space admission and the 50 GiB default are updated (#261).
- Connection setup and live connection generations improve configuration and keep each Run on its selected generation (#269). Live defaults do not overwrite saved Thread choices.
- Human reply handling, delivery after rewind, and recovery behavior are more reliable (#262, #263). Editing a request now refuses while its Run is active instead of stopping it (#258).
- Compaction avoids repeated oversized-summary retries and repeated spill near the trigger (#251, #271); Thread inspection labels retained compaction facts as incomplete when an expired restore may be missing (#264).
- `usage get --group-by model` groups saved attempts by model; summary usage without a saved model is not attributed (#272). Capacity reporting shows configured thresholds (#268).
- History exposes execution descriptions, and the core guides no longer point to Studio Apps. Other documentation and bounded-read fixes improve inspection.

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
| #271; `92281d1095`; `acceptance.md`: reference pending | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #267; `f01aee7f76`; `acceptance.md`: #267 bounded index reclamation | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #265; `20b5ea8338`; `acceptance.md`: #265 storage threshold read | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #272; `10a7fa0ec5`; `acceptance.md`: #272 model-grouped bounded usage | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #270; `7aa2ef3dd1`; `acceptance.md`: #270 planned/implemented acceptance | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #273; `c54ed50c98`; `acceptance.md`: #273 branch-relative usage and kind | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #264; `e7a7242d55`; `acceptance.md`: reference pending | Yes | Yes | Unverified; run `42475616229` reused | Unverified |
| #262 reply queue; landing SHA pending; `acceptance.md`: pending | Yes | Awaiting landing | Awaiting landing | Unverified |

Evidence links refer to `docs/validation/acceptance.md` section names. Where no uniquely matching section is identified, the evidence is pending rather than a confirmed acceptance reference. Final release CI and 32-to-37 binary migration validation remain pending.

Synthetic schema 32-to-37 SQL migration: 3.683 ms in one in-memory run on macOS arm64 with SQLite 3.51.0 (two Threads, one node). This excludes binary startup and is not a production-scale estimate. Ledger and original columns were checked.
