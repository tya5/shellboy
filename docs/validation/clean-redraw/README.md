# Terminal residue investigation (#216)

## Scope and confidence

The owner reports help residue cleared by Ctrl+L. The additional scroll report is
from memory and uncertain; marks at **both row edges** were prominent. Width
mismatch and fallback-font pixel overhang are hypotheses, not an established
Windows root cause. No Windows hardware is available. Synthetic tests establish
cell/sequence behavior only, not Windows pixel cleanliness.

## Symbol inventory (step 1, before implementation)

[ui-symbols.tsv](ui-symbols.tsv) records per-file **source code point occurrences**
(not number of simultaneously painted glyphs). It includes all UI Rust files,
including comments, tests, and synthetic emoji examples, to avoid hiding symbols;
letters/numbers and whitespace are excluded. Japanese punctuation, combining
marks, and format characters are retained. Neutral is an EAW property, not a
promise of one-cell/pixel width; combining and ZWJ characters need context.
`inventory.py REPOSITORY_ROOT` reproduces the inventory using Python's Unicode
15.0.0 tables. Arbitrary user/model content is unbounded and cannot be listed.

At base `36b70a58f9b64ca66b27b21eda5f2dd05e988589`, help.rs contains six U+2192
right arrows, not the first-pass issue's 42. It also has U+2191 (10), U+2193 (14),
U+00B7 (4), U+2026 (1), U+2315 (3), U+2500 (1), U+2502 (1), U+2588 (1),
and the Japanese punctuation listed in the TSV. Help key hints generate some
arrows at runtime via text_width::glyphs::controls; source counts are not rendered
counts. Decided marks/glyphs remain unchanged. B (ASCII prose arrows) and C
(measurement-dependent substitutions) remain proposals only, not implemented.

## Existing redraw and closing behavior (step 2)

`src/ui/redraw.rs::request_full_redraw` accepts Ctrl+L before owner dispatch in
`src/ui/overlay.rs::event`. It sets dirty/full_redraw_requested. `draw_due` keeps
it in the ordinary FPS budget. `draw_terminal` calls `Terminal::resize` at the
same viewport, then draws. It does **not** call `Terminal::clear`: that API queries
the physical cursor (DSR), potentially waiting for a response. In pinned
ratatui-core 0.1.2, `terminal/resize.rs:24` calls `clear_viewport` at line 48;
`terminal/buffers.rs:158` clears the fullscreen backend with `ClearType::All` at
line 160 and resets the previous buffer at line 176. Thus Ctrl+L combines a real
clear with buffer invalidation without a cursor query.

Before this change, closing help/popups mutates visibility/selection state and
marks the UI dirty; it does not invalidate the physical covered area. Popup
`Clear` widgets write blank cells to the **next Ratatui buffer**, not a terminal
clear escape. All output still depends on the cell diff. A clear widget on open
therefore does not guarantee terminal cleanup on close.

Ratatui's `buffer/diff.rs:143` handles wide graphemes, skips covered columns,
checks nonblank trailing symbols for wide VS16 emoji (`:158`), and force-emits
previous wide cells' trailing columns when shrinking/revealing visible style
(`:178`). Equal ordinary cells are skipped at `:151`. This helps known Unicode
widths, not an unmeasured overflow of a one-cell arrow or font pixels.

## Synthetic reproduction (step 3)

Pending execution evidence; see `src/ui/redraw/clean_tests.rs`. The VT model has
fixed Unicode widths, so a two-cell CJK proxy replaces only outgoing help arrows
to model a terminal advancing two cells for the app's one-cell arrow. This is not
a claim to reproduce a particular Windows font. Final differing cells are emitted
by the test. A second model contaminates unchanged edge cells to represent
neighbor overhang; it checks scroll repair without claiming to simulate pixels.
