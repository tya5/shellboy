# Documentation language and UI language evaluation

2026-10-29. Documentation policy is adopted and implemented in this change. UI language support is evaluated here only; no UI strings, runtime settings, key bindings, or rendering behavior change in this task.

## Documentation policy

- English is the default for new documentation and development instructions. `README.md`, root `AGENTS.md`, the entry points `docs/README.md`, `docs/guides/README.md`, `docs/design/README.md` and `docs/validation/README.md`, and all unsuffixed files under `docs/guides/` are English.
- Only the User Guide has a Japanese translation, `user-guide.ja.md`. Its feature-guide links target the English guides. Remove the other eleven translations; no Japanese Agent Guide or Japanese guide index is maintained.
- `agent-guide.md` is English only. The root development `AGENTS.md` and the model-facing Agent Guide remain distinct documents with different audiences.
- Feature guides are shared by people and models and are English only. Their complete examples, limits, errors, ownership rules, and recovery distinctions remain in the English document.
- Current Japanese TUI captions can appear as literal examples in English guides, with English explanations. User content, commands supplied by users, model answers, paths, and quoted data are not forced into English.
- Update the English User Guide and its Japanese translation together when changing their behavior. Other guides have no translation obligation. Keep language-independent commands and JSON field names identical; translate example natural-language requests when useful. Existing design/development/verification history is not bulk-translated or rewritten. New design and record entries use English.

## Bundled docs and RAG

Keep the existing 13 logical guide IDs and three categories. `docs list/get/search` and the `docs` RAG scope use English bodies, titles, summaries, and category introductions. The system prompt already directs models to these stable IDs in English and does not require a new prompt or language state.

The Japanese User Guide remains readable from the source and release archives. It is not an additional bundled catalogue entry. Links to `.ja.md` are repository links, not falsely reported as resolvable bundled IDs. No `--lang` flag, per-Thread locale, dual-language search index, schema, worker, or dependency is added. `docs search` performs literal search, not query translation. Unicode matching and UTF-8-safe pagination remain supported for quoted text and user-provided RAG/history content.

Changing guide text changes the existing documentation version/content hash. Old cursors expire normally. Previously indexed bundled docs can require an explicit `rag index --source docs --mode semantic`; search must not silently rebuild or make extra embedding calls. This is the existing version/readiness contract, not a new migration or automatic indexing job.

## UI options

The current implementation has Japanese presentation strings in the UI modules, Japanese frontend status messages, shared popup/layout primitives, Unicode-width-aware rendering, and strict `[ui]` configuration with no language field. CLI syntax, JSON field names, and most protocol errors are already English. A label in `docs list` is guide metadata, not a change to TUI presentation strings.

| Criterion | English UI only | English default with Japanese selection |
| --- | --- | --- |
| Initial implementation | Audit the review estimate of approximately 1,220 Japanese literals / 1,090 unique strings across 40 non-test files, including about 130 formatted messages. Translate one presentation set. | Audit the same estimated scope, extract presentation messages, maintain two bounded sets, and add startup selection. |
| Maintenance | One presentation copy; review estimated 43 integration-test files containing Japanese strings need an audit. | Two presentation copies and both layouts; the same estimated 43 integration-test files need an audit. |
| Existing Japanese users | All familiar labels change with no way to retain them. | Existing wording remains available without keeping a separate UI implementation. |
| Core complexity | No locale state needed. | Locale can remain entirely in UI/frontend presentation; Core state enums, schema, events, and wire field names need no locale state; product-authored natural-language values must follow the English default. |
| Layout risk | English labels can wrap differently, despite often using fewer cells. | Both Latin and Japanese widths must be covered; shared layout fixes benefit both. |
| Future languages | A later localization pass would revisit the strings again. | A small presentation boundary is available; broader language support still needs its own justification. |

## Scope and policy consequences

The inventory above is the reviewer's approximate source audit, not a fresh measured count or a verified implementation estimate. The largest reported concentrations are `presentation/fields.rs` (204), `ui/operations.rs` (152), `ui.rs` (141), `ui/help.rs` (108), and `frontend.rs` (101). Recount and classify literals before implementation: user data, quoted source, tests, and product-authored prose have different obligations. Both UI options require this audit; bilingual presentation additionally needs a second message set and two-language layout coverage.

English default fixes three boundaries independently of the UI option:

- Core product-authored values visible through CLI/wire are English. Current unnamed/unsent Thread fallbacks in `src/control/db.rs` are exceptions to correct in the future language implementation. Japanese UI can render localized display equivalents without changing stored user titles, IDs, state enums, or schema.
- Startup errors and hints emitted before UI configuration can be read are English, including the current exceptions in `src/app.rs` and `src/ipc/server.rs`. An unreadable configuration cannot supply a reliable selected language.
- Model-facing instructions are English regardless of UI locale, including the summary instructions/headings in `src/compaction.rs`. Choosing Japanese presentation must not alter model instructions.

These are future implementation requirements, not claims that those source strings changed in this documentation task.

## Recommendation

Prefer **English by default with an explicit Japanese UI option** in a future task. Existing Japanese copy and layout work are useful assets, and an English-only UI would discard them while still requiring a complete string audit and layout regression pass. The extra responsibility can remain a small presentation concern; it does not justify a localization service, new Core state, or a second renderer.

The recommended first implementation is startup selection, applying to the entire app, rather than per-Thread or live switching. Use an explicit English default, independent of OS locale, so behavior is reproducible. A future field in the existing UI settings is the likely place, but **no such field is accepted by the current parser**. Do not add one to a configuration file yet. A hot language switch, if later requested, needs explicit layout/cache invalidation and is not part of this recommendation's first stage.

Use one UI tree and the existing popup/list/reader framework. Put complete presentation messages behind a small typed Rust language boundary; pass dynamic values as data rather than concatenating translated fragments. Keep domain enums, error codes, operation names, persistence structure, provenance, and raw output language-independent. English product-authored Core values are a separate boundary described above. Translate frontend-generated display statuses and help as well as widget labels; changing widget headings alone would leave a mixed-language interface. This task adds no such runtime abstraction. English already needs singular/plural handling. Use small typed Rust helpers selecting `count == 1` versus all other counts (including zero), returning complete messages such as “Deleted 1 conversation” and “Deleted 2 conversations”. Pass counts as numbers; do not concatenate translated noun fragments. Test 0, 1, 2, and larger counts in both languages. This requires no grammar DSL. For additional languages or richer plural/gender rules, evaluate an established localization library rather than extending a custom grammar engine.

Do not translate stored conversations, model replies, shell output, source text, user titles, or provider error bodies. Optional translated display explanations must retain exact error codes and access to the original. Choosing Japanese UI must not change model instructions or silently choose Japanese docs search/indexing.

## Future implementation checklist

1. Inventory UI, frontend display messages, help, slash descriptions, confirmations, placeholders, and progress/recovery text; separate literal source data from product-authored labels.
2. Add the finite presentation lookup and an explicit startup choice with English default/Japanese opt-in. No database migration, per-Thread language, locale probing, network call, or background worker is needed.
3. Keep current Japanese presentation text as Japanese resources, with deliberate corrections recorded separately. Route all presentation paths through the same boundary. Apply the English Core fallback, pre-configuration diagnostic, and model-instruction requirements above without UI-locale coupling.
4. Verify narrow and wide terminals, wrapping, physical rows, scrolling anchors, all list/popup types, disabled states, errors, and help in both languages. Use existing reactive updates; do not add a language poller or change acceptance/cancellation behavior.
5. Update every guide quotation of UI captions to match the English default, retaining exact Japanese captions only where explicitly describing the Japanese option. Keep the Japanese User Guide aligned. Extend the guide-label regression to all quoted operation, management, and received-record labels against actual output; reject invented labels instead of checking only a selected set exists somewhere.
6. Keep Unicode editing and Japanese IME capability regardless of display language. Check a real terminal only where hardware is available; unit rendering tests do not establish IME behavior.
7. Document the eventual supported setting only after implementation. Revisit the English-only option if maintaining both languages measurably impedes releases; no evidence currently justifies dropping Japanese.

## Verification for this change

Check complete English guide coverage, exactly one Japanese User Guide, absence of other Japanese translations and a Japanese Agent Guide, relative links and heading fragments, offline catalogue content and metadata, literal search, cursor/UTF-8 behavior, and bundled-doc RAG version/readiness behavior. Confirm that no runtime UI/configuration behavior changed; a UI-module regression test alone does not implement language selection. Record commands and observations in [acceptance](../validation/acceptance.md), separately from the future UI checklist above.
