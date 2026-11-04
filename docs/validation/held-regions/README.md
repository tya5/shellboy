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
