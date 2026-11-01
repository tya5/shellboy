# UI language foundation (#139 / #35 stage A)

Status: implementation proposal, subject to review and the validation recorded
in [validation](../validation/acceptance.md#ui-language-and-terminal-width-foundation-139-2026-11-01). The effective contract is in
[effective spec](effective-spec.md#display-language-and-terminal-width-foundation-139--35-stage-a).

## Language boundary

`[ui] language` is `en` or `ja`; `en` is the default. It does not consult
`LANG` or `LC_*`, and unsupported values fail configuration validation. Resolve
one complete `Message` per UI sentence with typed parameters: English zero,
one and many can have different forms, and Japanese can use a different word
order. Stage A includes the force-quit confirmation and width diagnostic; the
other screens remain stage B. User/model text, provider errors, CLI and protocol
values, content submitted to models, and persisted content are not translated.

A small exhaustive Rust enum is preferable here to a new catalogue dependency:
two locales and a small initial surface fit compile-time coverage and typed
parameters. It makes translators touch code and requires explicit plural rules;
re-evaluate a structured catalogue such as Fluent before adding many locales or
complex plural/gender rules. Do not add an ambient-locale fallback.

## Terminal width

After raw mode and the alternate screen are active, issue two DSR requests after
one and two `U+00B7` characters at row 11. Row 11 avoids the ordinary modified
F3 `CSI 1;modifier R` form. The event reader filters DSR without consuming or
reordering keyboard/paste input; it restores modified F3 events that crossterm's
Unix parser otherwise treats as cursor reports. The UI loop polls without
blocking. Both replies must agree; after 250 ms, a malformed/contradictory reply,
a terminal smaller than 5x11, or an unsupported input path produces `unknown`.
No terminal name, locale, override, or user's choice of language determines width.
Windows currently returns unknown because its crossterm input path does not
expose these reports; this is an explicit safe fallback, not a claimed
measurement. F1 reports the result, and optional diagnostics records the result
without text, paths or user data.

The `unknown` effective width is two. The policy is established before starting
presentation workers and is shared across the process; tests may use a local
thread override. Both ratatui and application wrapping, clipping, scrolling,
input cursor calculations and worker Markdown layout use that policy. For
unknown emoji sequences, reserve the sum of constituent widths, with at least
two cells for an emoji-presentation selector. The terminal may draw a narrower
ligature; its unused cells are intentional. Raw content and copy/save paths do
not change.

Application-owned ambiguous symbols use ASCII alternatives. Existing semantic
row marks/spinners remain only if their narrow and CJK widths agree. Markdown
borders are renderer-generated decorations: they carry a private style marker
and are converted to ASCII before drawing; a user-supplied identical character
is not converted. Source anchor mapping removes the tagged output guide, rather
than stripping ASCII pipes from user text.

The two vendor patches in `vendor/` are deliberately narrow and include upstream
licenses. Crossterm needs an event-reader DSR filter that keeps input order;
its public `cursor::position()` can block the shared event reader and retry
errors. Ratatui and the application must share `unicode-width` behavior, not
calculate different widths on opposite sides of the buffer. Reconsider both
patches on upstream upgrades. The project's desktop binary uses `std` even
though upstream `unicode-width` is `no_std`.

## Stage B partition proposal

Each slice owns its message submodule and 80/120-column bilingual fixtures;
dynamic values are typed arguments of complete messages. The shared `src/ui.rs`
integration points are owned by the integrator after each slice is reviewed,
rather than edited concurrently by every slice.

1. Help, command reference, shortcuts and key hints: `src/ui/help.rs` and
   `src/ui/key_hints.rs`.
2. Conversation, run, tool and recovery presentation: `src/ui/presentation.rs`,
   `src/presentation/` and `src/ui/history_interaction.rs`.
3. Thread/browser, watcher and live-work views: `src/ui/threads.rs`,
   `src/ui/watch.rs` and `src/ui/live_work.rs`.
4. Composer, draft, queue and copy interactions: `src/ui/input.rs`,
   `src/ui/draft.rs`, `src/ui/message_queue.rs` and `src/ui/code_copy.rs`.
5. Confirmation, deletion, model, settings and management overlays: remaining
   `src/ui/` overlay modules. Force quit is the stage A example and must not be
   remigrated by this slice.
6. Startup, frontend and command diagnostics that are actually application UI,
   followed by bilingual guide and acceptance updates. Protocol, persisted
   content, program output and provider/model text remain unchanged.

Check each split against current main before assigning parallel work; shared
registries, `src/ui.rs`, broad snapshots and guide updates are integration-owned.

## Common layout and test rules

Measure translated complete strings in terminal cells, not bytes or UTF-8 code
points. Derive a screen's label/content width from its actual localized lines;
clamp it to the available viewport and reserve borders, padding and scrollbar
cells separately. `popup::text_viewport` implements this for the stage A sample.
Do not use a fixed English/Japanese label column. Recalculate after resize.

When space is constrained, preserve in order: the consequence/safety warning
and destructive scope; explicit confirm/cancel controls; the selected item or
identity; current state and failure/recovery facts; then secondary context,
counts, timestamps, hints and decoration. Wrap or scroll the body before hiding
an action. Truncation is display-only, on a grapheme boundary, with an ASCII
`.` mark; retain the complete source/copy/execute value. For English text, wrap
long words at grapheme boundaries if necessary; never cut a critical confirmation
word when 80/120 columns are available. Status-bar and section rows use the same
priority: keep state and action, drop decoration and optional detail first, and
keep overflow discoverable in the existing detail surface.

For stage B, register each synthetic screen with `ui::i18n::verify_screen`:
provide setup and the essential complete phrases for both languages. It renders
80/120 columns for measured width one/two and unknown, checks generated symbol
width invariance, emoji-variation bases from the pinned Unicode width table,
popup borders and essential phrases. Fixtures containing user
or model data need separate source/copy-preservation assertions; never sanitize
that data merely to satisfy a symbol test.
