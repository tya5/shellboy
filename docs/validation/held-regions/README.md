# Held-Input Region Staging (#218)

## Stage 1: Structural Extraction

Base: `6a0827def47946948c4475365caeeefa1c2058b1` (v0.8.0).
No held region, glyph change, config setting or key-behavior change is included
in this checkpoint. Those belong to stage 2; old queue painting is removed in
stage 3. The adopted design records all approved decisions, not a claim that
later stages already exist.

`src/ui/region.rs` extracts the existing grapheme-aware fitting/right-column
allocation and fixed region geometry/heading spacer/body padding from live work.
The same formatter continues to serve conversation notice rows. Existing style,
selection caret, blink policy and fallback vocabulary are retained.

The only new production type is `RegionLayout`: an immutable capacity/fold value
used during layout/painting. It computes reserved height, overflow/item slots
and blank body padding. It does not own snapshots, clocks, selected identities,
source reads, queue actions, a worker or domain state. `LiveWorkView` keeps those
live-work responsibilities; held input will reuse the geometry/presentation with
its own existing message-domain actions. No catch-all render context hides
function parameters. `row_columns` is still five parameters, and live-work `draw`
is still six; notice rendering's existing maximum eleven is not reduced here.

## Identical Render Evidence

Before extraction, `region_extraction_full_cell_style_baseline` rendered 864 cases
with the original renderer on macOS arm64. Its fixed-input full-cell Debug
representations (including symbols, colors and modifiers, inside/outside the
nonzero-origin viewport) produced FNV-1a digest `cce04517ab437b25`. The refactored
renderer must match that same pinned digest; the golden is not recomputed from
the new implementation. This is a regression fingerprint, not a cryptographic
proof or a Windows pixel claim.

The matrix covers EN/JA, 18/80/120 columns, unknown/one/two measured width policy,
dark/light/plain palettes, empty/single/folded/focused/overflow/loading/error/
output/stopping content, and both heading blink phases. Relative clocks use one
fixed input Instant, not elapsed test runtime. Plain palette is injected through
capabilities without modifying the process environment. Existing live-work and
notice regressions are run as well. Exhaustive geometry tests cover capacities
1..10, short areas, body/overflow slots and preservation of existing padded lines.

The initial baseline attempt failed to compile due to a test-only incorrect enum
name; the corrected original-renderer run exited 0 and supplied the digest. This
failed attempt is not counted as evidence. After extraction, 33 live-work tests
(including the pinned 864-case golden), two geometry tests and 86 presentation
tests pass. A test-only Clippy range spelling was corrected; the final golden,
locked check --tests and strict all-target Clippy pass. Python 200 and integration
layout 105/watchdog checks pass. Language inventory retains the existing 1215
pending literals, not a zero-pending --check pass. Current-main light revalidation
and pushed-commit CI are run separately; this is not a full-feature handoff.

## Measurements

`before.json` and `after-extraction.json` list every function declaration and its
parameter count (including `self`), with cfg(test) classification. Counts are
lexical, not a Rust AST complexity measure. Physical source lines include tests,
comments and blank lines. `scripts/dev/region-metrics.py --help` describes the
read-only tool; nested generic/function-pointer/tuple arguments and comments/raw
strings have helper regressions.

| file | before lines | after lines | production functions before/after | max parameters before/after |
| --- | ---: | ---: | ---: | ---: |
| live_work.rs | 2482 | 2498 | 38 / 36 | 6 / 6 |
| message_queue.rs | 1761 | 1761 | 20 / 20 | 5 / 5 |
| presentation.rs | 6260 | 6260 | 58 / 58 | 11 / 11 |
| ui.rs | 9711 | 9712 | 131 / 131 | 7 / 7 |
| region.rs | 0 | 173 | 0 / 7 | 0 / 5 |

Live work plus the new framework grows from 2482 to 2671 physical lines (+189),
including the new 864-case baseline test and geometry tests. This is extraction
for a shared owner-independent layout, not a line-count or maximum-arity reduction
claim. Queue is deliberately untouched. Later behavioral/removal stages must
record their own measurements rather than replacing this checkpoint's evidence.

## Stage 1 Continued: Shared Identity State And Heading

Before held behavior, the structural follow-up also extracts `RegionState<K>`
and `RegionHit`. `RegionState<K>` owns only coherent UI-local focus/selected row
identity, fixed capacity/fold, current frame bounds and visible slots. It computes
bounded navigation and hidden counts without allocating a second row model. The
stable key type belongs to the domain; payloads, snapshots, Run/Thread generation
checks, timers, source bodies, output observers and action construction do not
move into this type. `RegionHit` is only a current-frame hit classification:
outside, heading, row index, padding. A hit is not an authorization or source ID.

The heading mark/clipping/style moves to `region::heading`; a domain supplies
localized facts and an explicit blink phase. Tree rows reuse the already generic
measured-width `numbered_branch` helper through the region framework; no second
glyph fallback implementation is introduced. Live work's selected-row lookup
still resolves its domain target, rather than inferring identity from text/index.
Shared state retains the existing collapse, fallback selection, keyboard and
mouse behavior. Reverse-video selection remains a later behavioral commit.

`before-shared-state.json` records the pushed `07f78f2` checkpoint;
`after-shared-state.json` records the follow-up. They include the additionally
touched chrome/key-hint/overlay clients. Historical first-checkpoint measurements
above are not overwritten.

| file | checkpoint lines | shared-state lines | production functions | max parameters |
| --- | ---: | ---: | ---: | ---: |
| live_work.rs | 2498 | 2466 | 36 / 35 | 6 / 6 |
| region.rs | 173 | 337 | 7 / 16 | 5 / 5 |
| message_queue.rs | 1761 | 1761 | 20 / 20 | 5 / 5 |
| presentation.rs | 6260 | 6260 | 58 / 58 | 11 / 11 |
| ui.rs | 9712 | 9712 | 131 / 131 | 7 / 7 |
| chrome.rs | 1805 | 1805 | 11 / 11 | 7 / 7 |
| key_hints.rs | 696 | 696 | 14 / 14 | 4 / 4 |
| overlay.rs | 1736 | 1736 | 24 / 24 | 4 / 4 |

Combined live-work+framework is now 2803 physical lines, +132 since the initial
checkpoint and +321 versus the original 2482, including all new tests. Maximum
arity is unchanged, not hidden in a broad context object. The same original
864-case golden and existing focus/click tests must still pass before this
follow-up is committed. Validation passes: 33 live-work tests (the same original
864-case golden), three geometry/state tests, 12 key-hint and 26 overlay tests.
A test-only import was cfg-gated after the initial strict Clippy failure; corrected
golden/check --tests/strict all-target Clippy pass. The first pushed checkpoint's
default CI `41868419181` passes at `07f78f2`; this follow-up has separate exact-tip
validation and is not final held-feature or main landing evidence.

## Stage 2: Held Input Behavior

This stage intentionally changes the layout and focus presentation. It is not
covered by the stage-1 claim of unchanged visuals. Live work and held input now
share `RegionRow` clipping/alignment/palette/focus painting in addition to region
geometry, heading and stable-identity state. Held input retains only borrowed
saved/accepting rows while drawing and copies at most the displayed capacity of
stable IDs into frame state. Domain receipt facts, source requests, status and
confirmation actions stay in the saved-input path; no new worker or persistence
is added. The bounded worker projection classifies the saved sender and current
completion reservation/delivery context, rather than guessing from null Thread
IDs. Existing history completion-context logic and its provenance are unchanged.

The behavior keeps the original fingerprint value. For focused matrix cases the
test first asserts the approved blank caret slot and reverse/bold visible cells,
then reverses exactly those changes. Ratatui's non-emitted wide-glyph continuation
cells are excluded from leading-cell style assertions. Fully default spaces are
restored to `Cell::EMPTY`: a selected fresh TestBackend draw emits `Some(" ")`
where the original renderer skips the equal `None` cell, and their derived Debug
representations differ despite identical symbols/colors/modifiers. This is not
permission to normalize any nonblank or nondefault cell. A temporary copy of
the checkpoint renderer was used locally to compare every normalized Cell and
Debug representation; it was removed after the original `cce04517ab437b25`
passed. The pinned golden was never regenerated. The standalone test still
checks all 864 cases; new held tests separately check the intentionally new
layout and controls. Neither establishes Windows font pixels.

`after-held-behavior.json` is separate from every stage-1 measurement. Physical
lines include tests, comments and blanks; parameters include `self` and remain
lexical, not a complexity or performance measure.

| file | shared-state lines | behavior lines | production functions before/after | max parameters before/after |
| --- | ---: | ---: | ---: | ---: |
| live_work.rs | 2466 | 2491 | 35 / 35 | 6 / 6 |
| region.rs | 337 | 377 | 16 / 18 | 5 / 5 |
| held_input.rs | 0 | 637 | 0 / 13 | 0 / 4 |
| message_queue.rs | 1761 | 1793 | 20 / 27 | 5 / 5 |
| ui.rs | 9712 | 9724 | 131 / 131 | 7 / 7 |

Live work plus shared region plus new held input totals 3505 lines, +702 from
the shared-state checkpoint's 2803 and +1023 from original live work's 2482.
This includes 11 held regression tests and focus-delta golden assertions, not a
claim of code shrinkage or maximum-arity reduction. The old compact height and
painting functions are still present but disconnected; their separate removal
stage must record its own measurements rather than overwriting this stage.

At this checkpoint the standalone live-work 33, held 11, region 3, queue 22 and
worker 10 tests, strict held-row config integration test and locked full test
compilation pass locally. Five disconnected old compact helpers warn as unused
until their separate removal stage. No behavior/final-tip CI result or landing
is claimed.
The final branch must pass default then all-OS CI at one identical SHA and be
reviewed/integrated by W8. Existing stage-1 CI success is not final-feature CI.

## Stage 3: Remove Disconnected Compact Painting

The behavior checkpoint is `553a426af334a8f4cdb1e3a05ca8be8e1541272a`.
This later cleanup deletes only the old compact count, variable height, label,
body and painter. Saved queue/popups, source/status workers, replies, archive,
cancellation and bounded accepting excerpts stay with their existing owners.
The layout variable names now say held region, without changing its position.

Control-text and 4 MiB accepting-draft tests render the held region. The former
compact label tests exercise shared region fitting, retaining source identity,
measured ellipsis, width and grapheme checks. The obsolete two-line compact
indentation/count contract is not an acceptance criterion for the new layout.
Archive/sender/cancellation wording and exact-ID protections are retained.

`after-compact-removal.json` records this stage independently: message_queue.rs
is 1690 physical lines versus 1793 at behavior, and 22 production functions
versus 27; its lexical maximum parameter count remains 5. The other measured
files retain the behavior-stage counts. Live/shared/held still total 3505 lines;
this is not evidence of an overall reduction or performance improvement.
Cleanup validation passes 22 queue and 11 held cases, formatting, strict
all-target locked Clippy, and integration-layout/watchdog policy checks on
macOS arm64. Final-tip CI is still pending.

## Final Palette Follow-Up

Self-review found held metadata used the terminal default rather than the
approved muted palette color. A separate follow-up uses `Palette::muted()`;
Dark/Light, selected/unselected and `NO_COLOR` combinations have a dedicated
cell-color/modifier regression. Live Work's non-selection painting is unchanged.
`final-held-regions.json` preserves this later measurement separately: held is
664 physical lines / 13 production functions / 4 maximum parameters, including
its new test; live/shared/held total 3532, not a code-size reduction claim.

One attempted local final validation reused a shared-target test executable from
another worktree: held and region filters ran zero tests and Live Work ran 32,
not the required 12/3/33. Exit status 0 is not acceptance for that attempt.
Validation was restarted after touching this worktree's library root to force
recompilation under the same rust-slot lock. Only the rebuilt run with the actual
expected tests can establish local feature evidence. That rebuilt run passes
held 12, live-work 33, region 3, queue 22, worker 10, key-hint 12 and overlay 26,
plus locked full test compilation and strict all-target Clippy with warnings
denied. No installed binary changes or dependency changes are involved.

## First Final-Tip CI Failure: Viewport Fixtures

Default CI `41884515961` on `1ba45e4` fails in actual Ubuntu test assertions.
It is not a runner-capacity cancellation and does not authorize all-OS acceptance.
The old scroll screen helpers still sliced terminal height minus 11, accidentally
including fixed-region rows. Their fixtures now add the five held-region rows
and slice height minus 16, preserving the original history viewport and every
source-anchor, row equality, page overlap, preview and resize assertion. The
folded-input test keeps its 24-row frame and explicitly accounts for the new
five-row region (history area 13/10 becomes 8/5); no composer sizing changes.
Small cached-history fixtures gain the same five rows. No production history,
scrolling or shell behavior is changed to satisfy these tests.

That CI also fails two shell-output tests: leading-dash stdout is empty, and
the slow-database binary-tail fixture saves 835584 rather than 1048576 stdout
bytes. Their relationship to this UI change is unestablished; they are tracked
separately and no shell source is changed here. Local reproduction and the next
corrected-tip CI are still required. A passing focused palette/held suite does
not establish the untested full UI/scroll suite.

Corrected-fixture local run (macOS arm64, locked dependencies, rust-slot) passes
19 library scroll-characterization, 4 input, 6 integration scrolling and
88 integration UI tests (1 UI test remains ignored). Both separately failing
shell cases also pass individually locally. This does not reproduce or explain
their Linux failure and is not a claim of flakiness. No shell source changes.
The next default CI must validate the corrected exact tip before all-OS dispatch.
The measured region files are unchanged by this test-only correction; existing
stage metrics remain historical evidence, not regenerated baselines.
