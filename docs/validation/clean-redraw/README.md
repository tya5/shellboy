# Terminal residue investigation (#216)

## Scope and confidence

The owner reports help residue cleared by Ctrl+L. The additional scroll report is
from memory and uncertain; marks at **both row edges** were prominent. Width
mismatch and fallback-font pixel overhang are hypotheses, not an established
Windows root cause. No Windows hardware is available. Synthetic tests establish
cell/sequence behavior only, not Windows pixel cleanliness.

## Symbol inventory (step 1, before implementation)

[ui-symbols.tsv](ui-symbols.tsv) records per-file **source code point occurrences**
(not number of simultaneously painted glyphs). It covers UI and presentation Rust
files. `source_count` includes comments/tests; `production_literal_count` and
`test_literal_count` use the existing language-audit Rust lexer and explicit test
attributes (or test-file names) to separate literal occurrences. Escaped Unicode
code points in literals are decoded. These are static occurrence counts, not
execution counts; dynamic labels may be supplied by other subsystems and
arbitrary content. Comments and synthetic examples are not claimed as UI output;
letters/numbers and whitespace are excluded. Japanese punctuation, combining
marks, and format characters are retained. Neutral is an EAW property, not a
promise of one-cell/pixel width; combining and ZWJ characters need context.
`inventory.py REPOSITORY_ROOT` reproduces the inventory using Python's Unicode
15.0.0 tables. Arbitrary user/model content is unbounded and cannot be listed.

At base `36b70a58f9b64ca66b27b21eda5f2dd05e988589`, help.rs contains six U+2192
right arrows, not the first-pass issue's 42. It also has U+2191 (10), U+2193 (14),
U+00B7 (4), U+2026 (1), U+2315 (3), U+2500 (1), U+2502 (1), U+2588 (1),
and the Japanese punctuation listed in the TSV. Excluding test literals, the
actual help source has `→` 5, `↑` 9, `↓` 13, `⌕` 2, `·` 4 and `…` 1;
its other production symbols are the Japanese punctuation in the table.
Help key hints generate some
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

The passing `src/ui/redraw/clean_tests.rs` reproduction exercises the actual help
and draw path. The VT model has
fixed Unicode widths, so a two-cell CJK proxy replaces only outgoing help arrows
to model a terminal advancing two cells for the app's one-cell arrow. This is not
a claim to reproduce a particular Windows font. Final differing cells are emitted
by the test. A second model contaminates unchanged edge cells to represent
neighbor overhang; it checks scroll repair without claiming to simulate pixels.

## Scroll path and both physical edges

Source references in the implementation branch:

- `src/ui.rs::scroll` delegates to `ThreadView::scroll` in
  `src/ui/view/navigation.rs`, resolving visual-row/source anchors. PageUp uses
  viewport height minus one in `src/ui/overlay.rs`. There is no terminal scroll
  region: `src/ui.rs::draw_conversation` slices visible styled history rows and
  renders a Paragraph; `redraw::draw_terminal` submits the ordinary cell diff.
- Left: `src/ui/theme.rs::conversation_content` leaves a one-column margin.
  Physical column 0 is normally blank. Content starts in column 1; app-owned
  heading/notice/tool marks (`◉`, `⁂`, `$`, `↳`, `⋯`) and `├─`/`└─` branches are
  defined in `src/ui/text_width.rs`, `src/presentation/rows.rs`, and the history
  presentation functions. Which mark is displayed depends on row kind (not all
  these marks are the first symbol of every history row). Equal marks/spaces at
  the same destination are omitted by the backend diff, despite each frame
  rendering them into the next buffer.
- Right: ordinary conversation history has **no scrollbar or border widget**.
  The last physical column is the one-column content margin, normally blank;
  `paint_conversation_edges` resets the last two **content** columns' background
  on non-user rows or paints the user band. It does not force physical writes
  when those cells remain equal. Content can end in the last content column
  (physical width minus two); popup scrollbars, in contrast, are drawn by
  `src/ui/popup.rs::scrollbar` and share the normal diff. A full-width row repair
  deliberately writes through the final physical column. It follows with a
  cursor-position command (and before every subsequent row) to cancel pending
  wrap rather than emitting a newline at the screen bottom.

An unchanged edge skipped by the diff is consistent with the suggested
neighbor-overhang mechanism, but code inspection cannot establish the owner's
actual Windows pixel mechanism. The owner remains unsure of the scroll symptom.

## Prepared scroll repair (separate commit)

The optional scroll pass compares successful painted projection identities,
uses the existing link-snapshot buffer, and erases/replays only visible history
content rows. It rewrites the whole physical width, not a guessed glyph subset,
and restores the composer cursor without DSR. Overlay/selection frames and idle
positions do not trigger it. Full-clear events already subsume it. Bounds are
viewport rows times columns, with no loaded-history-sized copy.

An initial experiment appended a second row pass (80 columns: line 2160 -> 4644
bytes; page 1818 -> 4324 with plain Crossterm output). That was rejected as too
costly. The final implementation expands moved rows **inside the existing OSC 8
backend before output**, merging with the diff so cells are written only once.
Final measurements use that actual HyperlinkBackend, including all URL,
style/cursor, row-erase and restoration bytes. The earlier plain-Crossterm numbers
are not the final byte-cost evidence.

## Observed final cells and sequences

Zero-based `(column,row)` coordinates, 80x30 help fixture, measured width one:

| Cell | Baseline after help close | Fresh draw / enabled close |
| --- | --- | --- |
| (26,19) | y | blank |
| (28,20) | t | blank |
| (26,22) | n | blank |

No other displayed cell/style/width differences remain after normalizing the VT
model's empty cell and printed space representation. Enabled close emits ED2
(`ESC [ 2 J`) and has zero differing cells. Nested F1 help above the F8 operations
popup also clears once on dismissal while preserving the underlying popup;
the initial test incorrectly tried F1 above Threads, where that key is deliberately
owned by the browser, and was corrected to use an actual supported nested path.
Ctrl+L clears with either setting and idle frames do not repeat the clear.

For width-mismatched history, one-line scrolling differs from a fresh draw at
columns 45 through 52 on rows 2 and 8 (shifted arrow/separator text), and at column
79 on rows 13 and 14 (blank-cell style/width metadata). PageUp differs at columns
45 through 52 on row 5 and column 79 on row 13. The test reports every differing
cell; this terminal-advance proxy does not simulate font pixels. Independently,
edge contamination at `(0,2)` and `(width-1,2)` survives baseline scrolling for
both one-line and PageUp at 80/100/120 columns; enabled row repair matches a fresh
draw in all six cases. EL is asserted once per moved content row. The backend
regression checks the first/last physical columns, CJK continuation handling,
OSC 8 URL preservation, row bounds and one-shot consumption.

## Exact output measurements and default

Environment: macOS arm64 synthetic 30-row terminal; **production
HyperlinkBackend over Crossterm**, fixed dimensions, no terminal hardware or
provider calls. Each measurement starts at the same painted history tail;
one-line uses the real history scroll path, PageUp uses the input event, and
help-close excludes help-open bytes but includes the entire closing frame.
Byte counts include all styles, OSC 8 boundaries, cursor commands and EL/ED.

The realistic mixed fixture contains 40 user/assistant messages, Markdown,
wrapping, CJK, arrows and the unchanged decided symbols. Full raw case counts are
in [bytes.csv](bytes.csv); measured-width two equals unknown on this fixture.

| Width | Action | Unknown OFF | Unknown ON | Width-one OFF | Width-one ON |
| ---: | --- | ---: | ---: | ---: | ---: |
| 80 | line up | 5005 | 2836 | 5164 | 2720 |
| 80 | PageUp | 3525 | 2863 | 3488 | 2742 |
| 80 | help close | 6645 | 4249 | 7012 | 4491 |
| 100 | line up | 5353 | 3245 | 5512 | 3124 |
| 100 | PageUp | 5353 | 3245 | 5512 | 3124 |
| 100 | help close | 7891 | 4946 | 8210 | 5231 |
| 120 | line up | 5433 | 3585 | 5592 | 3464 |
| 120 | PageUp | 5433 | 3585 | 5592 | 3464 |
| 120 | help close | 8305 | 5546 | 8697 | 5911 |

Expanded contiguous rows can emit *fewer* bytes than fragmented diffs because
there are fewer cursor/style/OSC 8 run boundaries. That is not universal: the
200-line fenced log countercheck has repeated timestamps/status fields with
changing digits. Its unknown-policy counts, in [log-bytes.csv](log-bytes.csv), are:

| Width | Action | OFF | ON | Added |
| ---: | --- | ---: | ---: | ---: |
| 80 | line up | 1513 | 2754 | 1241 |
| 80 | PageUp | 1815 | 2790 | 975 |
| 100 | line up | 1513 | 3094 | 1581 |
| 100 | PageUp | 1815 | 3130 | 1315 |
| 120 | line up | 1513 | 3434 | 1921 |
| 120 | PageUp | 1815 | 3470 | 1655 |

The sparse-scroll increase is +53.7% to +127.0%, **not a small share**. Therefore
the final proposal is **default OFF**, opt in with `ui.clean_redraw = true`.
This supersedes the dense-only provisional ON proposal in the issue; the owner
retains the final decision. These are byte measurements, not measured CPU time,
latency, terminal throughput, font-pixel correctness or Windows success.

Reproduce through the sandbox's Rust slot:
`cargo test --locked --lib ui::redraw::clean_tests -- --nocapture`.

## Source locations

- `src/ui/help.rs:32`: show-help visibility; `src/ui/overlay.rs:530` and
  `src/ui/overlay.rs:602`: toggle/Esc dismissal. `src/ui/popup.rs:295`: buffer
  Clear on open, not a terminal invalidation on close.
- `src/ui/redraw.rs:58`: Ctrl+L request; `src/ui/redraw.rs:85`: shared full redraw
  decision; `src/ui/redraw.rs:93`: same-size resize clear without DSR.
- `src/ui.rs:6030` and `src/ui/view/navigation.rs:420`: visual-row scroll;
  `src/ui/overlay.rs:478`: PageUp. `src/ui.rs:6413` and `src/ui.rs:6450`: sliced
  history Paragraph, no terminal scroll regions.
- `src/ui/theme.rs:44`: one-column physical margins; `src/ui/presentation.rs:198`,
  `src/ui/presentation.rs:2215`, `src/presentation/rows.rs:73`, and
  `src/ui/text_width.rs:129`: marks and branches. `src/ui.rs:6459`: user band and
  last content columns. These buffer writes do not force an unchanged cell diff.
- `src/ui/popup.rs:444`: popup scrollbar (track and thumb), absent from normal
  history. Physical history edges remain equal blank cells and can be skipped.
- `src/ui/redraw.rs:99` and `src/terminal_links.rs:168`: movement plan and row
  expansion; `src/terminal_links.rs:207`: physical EL before ordered full-row
  output. Ratatui's normal cursor restoration follows backend draw.
- Pinned dependency `ratatui-core-0.1.2/src/buffer/diff.rs:143` and `:158` handles
  known wide/VS16 trailing cells; `:178` handles revealed prior wide-cell style.
  This is not a general repair for one-cell glyph pixel overhang.

The app's terminal-width observation is one aggregate capability, not a per-font
glyph raster measurement. `src/ui/text_width.rs:51` applies the app policy;
Ratatui's own buffer/widget widths retain upstream behavior. Neutral EAW symbols
and CJK correctness do not establish that every glyph's pixels fit those cells.
