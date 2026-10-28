# Documentation language and UI language evaluation

2026-10-29. Documentation policy is adopted and implemented in this change. UI language support is evaluated here only; no UI strings, runtime settings, key bindings, or rendering behavior change in this task.

## Documentation policy

- English is the default for new documentation and development instructions. `README.md`, root `AGENTS.md`, the entry points `docs/README.md`, `docs/guides/README.md`, `docs/design/README.md` and `docs/validation/README.md`, and all unsuffixed files under `docs/guides/` are English.
- Preserve the Japanese user-facing guides as `name.ja.md`, including `user-guide.ja.md` and `README.ja.md`. Use ordinary sibling links to Japanese translations. They link to `agent-guide.md` when model instructions are needed: there is no Japanese Agent Guide.
- `agent-guide.md` is English only. The root development `AGENTS.md` and the model-facing Agent Guide remain distinct documents with different audiences.
- Feature guides are shared by people and models. Translate the complete guide, examples, limits, errors, ownership rules, and recovery distinctions; do not replace them with a shorter English overview that sends readers back to Japanese prose.
- Current Japanese TUI captions can appear as literal examples in English guides, with English explanations. User content, commands supplied by users, model answers, paths, and quoted data are not forced into English.
- Update the English guide and Japanese translation together when changing behavior. Keep language-independent commands and JSON field names identical; translate example natural-language requests when useful. Existing design/development/verification history is not bulk-translated or rewritten. New design and record entries use English.

## Bundled docs and RAG

Keep the existing 13 logical guide IDs and three categories. `docs list/get/search` and the `docs` RAG scope use English bodies, titles, summaries, and category introductions. The system prompt already directs models to these stable IDs in English and does not require a new prompt or language state.

Japanese guide files remain readable from the source and release archives. They are not additional bundled catalogue entries. Links to `.ja.md` are repository links, not falsely reported as resolvable bundled IDs. No `--lang` flag, per-Thread locale, dual-language search index, schema, worker, or dependency is added. `docs search` is literal search, not query translation: use English search terms for the English catalogue. Unicode matching and UTF-8-safe pagination remain supported for quoted text and user-provided RAG/history content.

Changing guide text changes the existing documentation version/content hash. Old cursors expire normally. Previously indexed bundled docs can require an explicit `rag index --source docs --mode semantic`; search must not silently rebuild or make extra embedding calls. This is the existing version/readiness contract, not a new migration or automatic indexing job.

## UI options

The current implementation has Japanese presentation strings in the UI modules, Japanese frontend status messages, shared popup/layout primitives, Unicode-width-aware rendering, and strict `[ui]` configuration with no language field. CLI syntax, JSON field names, and most protocol errors are already English. A label in `docs list` is guide metadata, not a change to TUI presentation strings.

| Criterion | English UI only | English default with Japanese selection |
| --- | --- | --- |
| Initial implementation | Translate one set of presentation strings and update affected tests. | Extract the same presentation strings and keep two bounded sets; add a startup UI language selection. |
| Maintenance | One copy of presentation prose. | Every changed message needs two translations and layout coverage for both. |
| Existing Japanese users | All familiar labels change with no way to retain them. | Existing wording remains available without keeping a separate UI implementation. |
| Core complexity | No locale state needed. | Locale can remain entirely in UI/frontend presentation; Core states, database, events, and wire values remain unchanged. |
| Layout risk | English labels can wrap differently, despite often using fewer cells. | Both Latin and Japanese widths must be covered; shared layout fixes benefit both. |
| Future languages | A later localization pass would revisit the strings again. | A small presentation boundary is available; broader language support still needs its own justification. |

## Recommendation

Prefer **English by default with an explicit Japanese UI option** in a future task. Existing Japanese copy and layout work are useful assets, and an English-only UI would discard them while still requiring a complete string audit and layout regression pass. The extra responsibility can remain a small presentation concern; it does not justify a localization service, new Core state, or a second renderer.

The recommended first implementation is startup selection, applying to the entire app, rather than per-Thread or live switching. Use an explicit English default, independent of OS locale, so behavior is reproducible. A future field in the existing UI settings is the likely place, but **no such field is accepted by the current parser**. Do not add one to a configuration file yet. A hot language switch, if later requested, needs explicit layout/cache invalidation and is not part of this recommendation's first stage.

Use one UI tree and the existing popup/list/reader framework. Put complete presentation messages behind a small typed Rust language boundary; pass dynamic values as data rather than concatenating translated fragments. Keep domain enums, error codes, operation names, persistence, provenance, and raw output language-independent. Translate frontend-generated display statuses and help as well as widget labels; changing widget headings alone would leave a mixed-language interface. This task adds no such runtime abstraction. For more languages or complex plural/gender rules, evaluate an established localization library rather than growing a custom grammar engine.

Do not translate stored conversations, model replies, shell output, source text, user titles, or provider error bodies. Optional translated display explanations must retain exact error codes and access to the original. Choosing Japanese UI must not change model instructions or silently choose Japanese docs search/indexing.

## Future implementation checklist

1. Inventory UI, frontend display messages, help, slash descriptions, confirmations, placeholders, and progress/recovery text; separate literal source data from product-authored labels.
2. Add the finite presentation lookup and an explicit startup choice with English default/Japanese opt-in. No database migration, per-Thread language, locale probing, network call, or background worker is needed.
3. Keep current Japanese text as the Japanese resources, with deliberate corrections recorded separately. Route all relevant presentation paths through the same language boundary.
4. Verify narrow and wide terminals, wrapping, physical rows, scrolling anchors, all list/popup types, disabled states, errors, and help in both languages. Use existing reactive updates; do not add a language poller or change acceptance/cancellation behavior.
5. Keep Unicode editing and Japanese IME capability regardless of display language. Check a real terminal only where hardware is available; unit rendering tests do not establish IME behavior.
6. Document the eventual supported setting only after implementation. Revisit the English-only option if maintaining both languages measurably impedes releases; no evidence currently justifies dropping Japanese.

## Verification for this change

Check complete English guide coverage, preserved Japanese counterparts, absence of a Japanese Agent Guide, relative links and heading fragments, offline catalogue content and metadata, literal search, cursor/UTF-8 behavior, and bundled-doc RAG version/readiness behavior. Confirm that no UI implementation/configuration files changed. Record commands and observations in [acceptance](../validation/acceptance.md), separately from the future UI checklist above.
