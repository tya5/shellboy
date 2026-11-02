# Stage C Language Audit (#152)

Status: initial inventory, not completion or permission to translate source data.
Baseline: main `b12a8291b1950a5f7db58de4dccbb12101a2a074` (W6 task 1 landed).
B-5/B-6 are not yet included. No third-party production source was edited.

## Post-0.6.0 Follow-Up Review

The first two product batches were rebased onto main
`358ed63b1b3627675d8ac55f2253307539cc142a` after the 0.6.0 release.
These are post-release candidates, not changes to the released 0.6.0 binary.

| File / Flow | Reviewed Candidates | Result | Disposition |
| --- | ---: | --- | --- |
| `src/frontend/copying.rs` | 6 literals | Confirmed application-generated errors; codes, source reasons and copy semantics preserved | `issue-152/product-copy-review-1-v060`, tip `6335def7585fc38a0f09b84a00412d587a6edc91`, CI `41369858817` successful |
| `src/ipc/server/endpoint/namespace.rs` | 2 literals | Confirmed application-owned diagnostic and recovery hint; dynamic JSON details unchanged | `issue-152/product-ipc-review-2-v060`, tip `4580a4fb06d91397e4ad6de2fa807e2c22d60fa9`, CI `41370110532` successful |
| `src/ui/threads.rs` | 56 literals | Intentional Ja resources in `BrowserText::render` / `BrowserMessage::render`, with En counterparts | Reread on this base; no resource or glyph changes; no manifest decisions adopted in this follow-up |
| `src/activity.rs` -> `src/frontend/live_work.rs` -> `src/ui/live_work.rs` | 17 column-label variants | One confirmed projection gap: prepared child states use canonical Ja column labels, but the En renderer only projects machine codes and otherwise passes them through | W8 confirmed no #157 overlap; this branch adds a UI-only child-state projection, retaining canonical labels/state and glyph selection |
| `src/ui/watch.rs` | 27 literals | Intentional Ja arms in `WatchText::render` / `WatchMessage::render`, with En counterparts | Resource definitions reread; interpolated source labels/reasons remain a separate review, not a whole-screen English guarantee |
| `src/ui/composer.rs` | 53 inventoried literals | No classification or English-defect count adopted | Consumer/projection review deferred with #150; do not infer resource, protected, or defect status from literal presence |

Both product CI runs succeeded on attempt 1 with actual Linux/macOS `Tests`
steps. Artifact `source_sha` values match the tips above, and the new regressions
ran nonignored on both OSes. The copy integration uses an injected clipboard;
it is not OS-clipboard or physical-terminal acceptance. These two candidates
have been handed to the integrator, not declared landed by this report.

The initial local IPC check reused a test executable from the other worktree's
shared target and did not execute its new regression. That run is not acceptance.
After invalidating this worktree's lib input inside `rust-slot`, fmt/check and
all six namespace tests passed, including the new regression. Fresh exact-tip
CI independently confirms it. Copy worker tests (3) and the original-body
integration (1) also passed locally on macOS.

On the unmodified 358ed63 source, the reviewed manifest reports
`a=304/b=1196/c=1888/protected=0`. This remains a conservative inventory, not
1,888 confirmed language defects. The 83 thread/watch resources have not been
reclassified in the manifest, and checking those resource definitions does not
establish the language of interpolated dynamic/saved values. The phase issue
is one projection defect, not 17 approved source-literal replacements. No
canonical labels, saved values, user/provider/shell text or selected symbols may
be rewritten as a shortcut. #149/#150 owned ranges remain deferred until their
exact changes land. Stage C and combined three-OS acceptance remain incomplete.

## UI Status And Detail Follow-Up

The reported cache, attachment hint/status and `/attach` footer literals were
already inventoried as `(c)`, not excluded by the scanner. The consistency check
validates reviewed fingerprints but intentionally does not enforce a zero-`(c)`
gate, so successful CI did not establish English UI completeness. A disposable
English PTY reproduced `/attach パス` on the input border and Japanese welcome
copy. These are renderer-owned messages; user/provider text and saved titles are
not evidence for translation.

On base `f1a87139`, `--ui-pending` enumerates every remaining `(c)` record in
`src/ui.rs` and `src/ui/**`, with paths, lines, IDs and fingerprints. After this
batch the inventory reports 475 candidates in 21 files, including frozen-header
labels (`src/ui/chrome.rs`), draft/recovery notices (`src/ui/view.rs`) and
command/operation surfaces. The list is conservative and includes canonical,
source-owned or already localized cases; each needs provenance review. It is not
a defect count or permission to rewrite values. The flag makes pending UI work
visible without silently turning the separate Stage C zero gate on.

Against main `87d80574297dcde5e3c95db58a430f103a1e1c7a`, an inventory
comparison found 17 existing `(c)` to `(a)` changes, 23 test-only `(b)`
additions, and four removed `(c)` literals: the two input-border hints removed
by user decision, and `キャッシュ{}%` and `作業場所: {cwd}`. The cache literal was replaced by an explicit Japanese
language arm, `キャッシュ{percent}%` (new `(a)`); the unchanged Japanese working
directory prefix is now selected independently of the opaque path. The six
field-group records are caller-dependent: the renderer must continue to use
`group_label`, not the Japanese compatibility label returned by `group`.

## Reproduce

```sh
python3 scripts/dev/language-audit.py --classifications scripts/dev/language-classifications.json > inventory.json
python3 scripts/dev/language-audit.py --classifications scripts/dev/language-classifications.json --summary
python3 scripts/dev/language-audit.py --classifications scripts/dev/language-classifications.json --ui-pending > ui-pending.json
python3 scripts/dev/language-audit.py --classifications scripts/dev/language-classifications.json --check --summary
python3 scripts/dev/language-audit.py --classifications scripts/dev/language-classifications.json --table --ownership scripts/dev/language-ownership.json
python3 -m unittest discover -s scripts/tests -p test_language_audit.py
```

The tool is read-only: it does not contact providers, inspect user data, write
files, access a database or re-index knowledge. It scans every `src/**/*.rs`
string literal, including raw/multiline and escaped Unicode text, while
excluding comments and character literals. Macro token strings are retained.
Literal IDs use path, literal hash and duplicate ordinal, not line numbers.

Categories: (a) explicit Japanese `local!` argument/language arm; (b) explicit
test-only item/module; (c) needs review or English cleanup. Category (c) is
conservative: it includes typed Message branches not proven by the lexical
classifier, source-recognition constants and pending B-5/B-6 work. It is not
a count of confirmed defects and is never authorization to translate stored,
user, model, provider or shell content. Reviewed ID decisions require a reason;
stale IDs fail instead of silently hiding changed literals. Reviewed entries also
pin a source-token fingerprint: changing an En predicate or surrounding code
requires re-review even when the literal ID is unchanged. Whitespace/comment-only
changes do not invalidate a decision. Unknown contexts
remain (c); there are no whole-production-file exemptions.

The reviewer approved an independent `protected` canonical-token category.
Each such decision must individually record both `canonical_evidence` (saved,
comparison or classifier basis) and `projection_evidence` (actual UI projection
reference), plus the reason and source fingerprint. Both evidence fields are
mandatory and retained in the JSON report. There are currently zero protected
entries: approval of the category does not by itself classify any unknown token.
Ownership labels never exempt a literal from review or the zero-(c) gate.

Current raw scan: 353 Rust files; 3384 literals; a=279, b=1192, c=1913.
Individual review of 25 `Message::render` Ja branches is recorded in
`scripts/dev/language-classifications.json`; the table applies those decisions:
a=304, b=1192, protected=0, c=1888. `Message::render` was reread against the
actual current source after rebase; its source-token fingerprint is unchanged,
so no fingerprint was blindly refreshed. No production source was modified to
reduce the count. The earlier main13d5ac86/f5c9b31a checkpoint had3339 literals;
#148 increased that to3398. W6's landed non-UI English change removes14 remaining
production literals, without changing the reviewed Message fingerprints. W6's
three-OS integration evidence is run41359768326, exact mainb12a8291; final Stage C
acceptance is still separate from that task-1 result.
The zero-(c) gate intentionally fails at this stage. Final classification,
fresh En/Ja rendering/plural checks and three-OS CI remain outstanding.

## File Classification Table

| Source | (a) | (b) | Protected Canonical | (c), Review Pending | Editing Boundary |
| --- | ---: | ---: | ---: | ---: | --- |
| `src/activity.rs` | 0 | 0 | 0 | 34 | review in C |
| `src/attachments/draft.rs` | 0 | 10 | 0 | 0 | review in C |
| `src/compaction.rs` | 0 | 8 | 0 | 0 | W6 task 1 landed; read-only here |
| `src/compaction/context_usage.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/compaction/edit/bounded_tests.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/compaction/edit/tests.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/compaction/keys.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/config_setup.rs` | 0 | 0 | 0 | 10 | review in C |
| `src/control/driver/request.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/control/output/preview/live.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/control/receipt.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/control/replay.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/control/wait.rs` | 0 | 4 | 0 | 0 | review in C |
| `src/docs.rs` | 0 | 4 | 0 | 0 | review in C |
| `src/exec_client/schema.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/frontend.rs` | 0 | 91 | 0 | 16 | defer B-5 |
| `src/frontend/activity.rs` | 0 | 2 | 0 | 4 | review in C |
| `src/frontend/activity/reduction.rs` | 0 | 0 | 0 | 1 | review in C |
| `src/frontend/browser.rs` | 0 | 0 | 0 | 1 | review in C |
| `src/frontend/control_tests.rs` | 0 | 20 | 0 | 0 | review in C |
| `src/frontend/control_tests/answer_details.rs` | 0 | 7 | 0 | 0 | review in C |
| `src/frontend/copying.rs` | 0 | 2 | 0 | 6 | review in C |
| `src/frontend/draft_saver.rs` | 0 | 2 | 0 | 3 | review in C |
| `src/frontend/header.rs` | 0 | 4 | 0 | 4 | review in C |
| `src/frontend/history_reader.rs` | 0 | 2 | 0 | 19 | review in C |
| `src/frontend/input_history.rs` | 0 | 4 | 0 | 0 | review in C |
| `src/frontend/inspection_attachments.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/frontend/inspection_lifecycle.rs` | 0 | 0 | 0 | 1 | review in C |
| `src/frontend/inspection_presentation.rs` | 0 | 5 | 0 | 40 | review in C |
| `src/frontend/inspection_read.rs` | 0 | 0 | 0 | 1 | review in C |
| `src/frontend/inspection_tests.rs` | 0 | 12 | 0 | 0 | review in C |
| `src/frontend/live_work.rs` | 0 | 6 | 0 | 11 | review in C |
| `src/frontend/management.rs` | 0 | 24 | 0 | 40 | review in C |
| `src/frontend/message_queue.rs` | 0 | 3 | 0 | 14 | review in C |
| `src/frontend/notices.rs` | 0 | 4 | 0 | 10 | defer B-6 |
| `src/frontend/preview_reads.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/frontend/receipt_delivery.rs` | 0 | 0 | 0 | 1 | review in C |
| `src/frontend/received.rs` | 0 | 25 | 0 | 4 | review in C |
| `src/frontend/received/list.rs` | 0 | 11 | 0 | 12 | review in C |
| `src/frontend/recovery.rs` | 0 | 8 | 0 | 9 | review in C |
| `src/frontend/refresh.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/frontend/selection.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/frontend/streaming.rs` | 0 | 28 | 0 | 8 | review in C |
| `src/frontend/streams.rs` | 0 | 9 | 0 | 0 | review in C |
| `src/frontend/thread_deletion.rs` | 0 | 6 | 0 | 2 | review in C |
| `src/frontend/tools.rs` | 0 | 0 | 0 | 2 | review in C |
| `src/frontend/tools/tests.rs` | 0 | 10 | 0 | 0 | review in C |
| `src/frontend/usage.rs` | 0 | 0 | 0 | 5 | review in C |
| `src/frontend/usage/tests.rs` | 0 | 5 | 0 | 0 | review in C |
| `src/frontend/watch_page.rs` | 0 | 3 | 0 | 2 | review in C |
| `src/frontend/watch_read.rs` | 0 | 6 | 0 | 11 | review in C |
| `src/gc/usage.rs` | 0 | 3 | 0 | 0 | review in C |
| `src/history/node_row.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/history/payload.rs` | 0 | 8 | 0 | 0 | review in C |
| `src/history/search_bodies.rs` | 0 | 7 | 0 | 0 | review in C |
| `src/index/keys.rs` | 0 | 13 | 0 | 0 | review in C |
| `src/index/store.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/instructions.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/ipc/server/endpoint/namespace.rs` | 0 | 1 | 0 | 2 | review in C |
| `src/ipc/server/tests.rs` | 0 | 3 | 0 | 0 | review in C |
| `src/json_pointer.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/lib.rs` | 0 | 4 | 0 | 0 | review in C |
| `src/model/opaque.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/operations.rs` | 0 | 0 | 0 | 320 | review in C |
| `src/operations/catalogue_tests.rs` | 0 | 5 | 0 | 0 | review in C |
| `src/presentation.rs` | 0 | 2 | 0 | 42 | defer B-6 |
| `src/presentation/events.rs` | 0 | 4 | 0 | 86 | review in C |
| `src/presentation/fields.rs` | 0 | 6 | 0 | 221 | review in C |
| `src/presentation/messages.rs` | 95 | 1 | 0 | 0 | defer B-6 |
| `src/presentation/projection.rs` | 0 | 0 | 0 | 12 | defer B-6 |
| `src/presentation/projection/envelopes.rs` | 0 | 0 | 0 | 11 | defer B-6 |
| `src/presentation/projection/limits.rs` | 0 | 1 | 0 | 0 | defer B-6 |
| `src/presentation/rows.rs` | 0 | 1 | 0 | 18 | review in C |
| `src/presentation/time_text.rs` | 0 | 12 | 0 | 4 | review in C |
| `src/program_status.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/protocol/sender.rs` | 0 | 3 | 0 | 0 | review in C |
| `src/rag/history/raw.rs` | 0 | 9 | 0 | 0 | review in C |
| `src/rag/mod.rs` | 0 | 5 | 0 | 0 | review in C |
| `src/thread_observation/latest.rs` | 0 | 4 | 0 | 0 | review in C |
| `src/tool_description.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/ui.rs` | 1 | 22 | 0 | 136 | defer B-5/B-6 |
| `src/ui/activity.rs` | 4 | 6 | 0 | 6 | defer B-6 |
| `src/ui/activity/i18n_tests.rs` | 0 | 5 | 0 | 0 | defer B-6 |
| `src/ui/activity/timing.rs` | 1 | 8 | 0 | 0 | defer B-6 |
| `src/ui/activity/wait.rs` | 0 | 2 | 0 | 5 | defer B-6 |
| `src/ui/autosave.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/ui/chrome.rs` | 1 | 33 | 0 | 18 | defer B-6 |
| `src/ui/code_copy.rs` | 11 | 19 | 0 | 0 | review in C |
| `src/ui/composer.rs` | 0 | 1 | 0 | 53 | review in C |
| `src/ui/details.rs` | 0 | 65 | 0 | 58 | defer B-5 |
| `src/ui/draft.rs` | 0 | 31 | 0 | 0 | review in C |
| `src/ui/help.rs` | 148 | 24 | 0 | 0 | review in C |
| `src/ui/history_interaction.rs` | 0 | 1 | 0 | 7 | defer B-6 |
| `src/ui/history_items.rs` | 0 | 4 | 0 | 0 | review in C |
| `src/ui/i18n.rs` | 5 | 10 | 0 | 4 | defer B-5 |
| `src/ui/input.rs` | 3 | 26 | 0 | 0 | review in C |
| `src/ui/inspection.rs` | 0 | 5 | 0 | 11 | defer B-5 |
| `src/ui/key_hints.rs` | 0 | 13 | 0 | 33 | defer B-5 |
| `src/ui/live_work.rs` | 0 | 18 | 0 | 17 | W8, coordinate #156 before edits |
| `src/ui/management.rs` | 0 | 1 | 0 | 44 | defer B-5 |
| `src/ui/management_tests.rs` | 0 | 11 | 0 | 0 | defer B-5 |
| `src/ui/message_queue.rs` | 31 | 41 | 0 | 0 | review in C |
| `src/ui/models.rs` | 0 | 2 | 0 | 13 | defer B-5 |
| `src/ui/motion.rs` | 0 | 3 | 0 | 0 | review in C |
| `src/ui/observation_notice.rs` | 0 | 0 | 0 | 16 | review in C |
| `src/ui/operations.rs` | 0 | 27 | 0 | 172 | defer B-5 |
| `src/ui/operations/client_forms.rs` | 0 | 1 | 0 | 72 | defer B-5 |
| `src/ui/operations/metadata.rs` | 0 | 0 | 0 | 76 | defer B-5 |
| `src/ui/operations/owner_tests.rs` | 0 | 21 | 0 | 0 | review in C |
| `src/ui/overlay.rs` | 0 | 10 | 0 | 4 | defer B-5 |
| `src/ui/performance_stream.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/ui/popup.rs` | 0 | 7 | 0 | 0 | review in C |
| `src/ui/popup/reader.rs` | 0 | 2 | 0 | 1 | defer B-5 |
| `src/ui/presentation.rs` | 3 | 92 | 0 | 13 | defer B-6 |
| `src/ui/presentation/i18n_tests.rs` | 0 | 34 | 0 | 0 | defer B-6 |
| `src/ui/reader_tests.rs` | 0 | 6 | 0 | 0 | defer B-5 |
| `src/ui/redraw.rs` | 0 | 4 | 0 | 0 | review in C |
| `src/ui/send_tests.rs` | 0 | 18 | 0 | 0 | defer B-6 |
| `src/ui/startup.rs` | 0 | 6 | 0 | 14 | review in C |
| `src/ui/stream_performance.rs` | 0 | 5 | 0 | 0 | review in C |
| `src/ui/text_width.rs` | 0 | 3 | 0 | 0 | review in C |
| `src/ui/thread_deletion.rs` | 0 | 2 | 0 | 28 | defer B-5 |
| `src/ui/threads.rs` | 0 | 105 | 0 | 56 | review in C |
| `src/ui/update_batch.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/ui/view.rs` | 0 | 16 | 0 | 17 | defer B-6 |
| `src/ui/watch.rs` | 1 | 32 | 0 | 27 | review in C |

## Product Copy Review, Batch 1 (Next Release, Not 0.6.0)

The user deferred Stage C product changes to the release after 0.6.0; only the
inventory tooling is approved for the current integration queue. This review
counts confirmed untranslated production literals, not conservative (c) totals.
Test/fixture strings and intentional Japanese resources are not confirmed leaks.
B-5/#149 and B-6/#150 owner files are deferred until their exact changes land.

| Production Source Reviewed | Confirmed Untranslated Literals | Status / Evidence |
| --- | ---: | --- |
| `src/frontend/copying.rs` | 6 | Batch 1: retrieval limit, changed reference, unavailable source, stalled pagination, changed source hash, missing code block |

These six diagnostics are generated by the copy worker, not user/provider text
or persisted comparison tokens. `LoadCopyChoices` forwards the error's string
and `ui::code_copy::Picker` retains/displays it without a language projection;
copy failures from `copying::text` likewise retain the worker's message. All six
original literals have no other production references/recognition branches.

The English-only candidate `6335def7` was returned by review and is withdrawn:
these errors contain TUI key guidance and must retain Japanese when Ja is
selected. The corrected batch uses `i18n::local!` / typed language arms, captures
the loaded UI language at each worker entry, and carries it through bounded
source and presentation work. All six Ja messages exactly match their pre-batch
sentences. English uses ASCII parentheses; Ja retains the original full-width
parentheses. Each ErrorCode, key `o`/`n`/`b`, bound, hash/pagination guard and copied
byte is unchanged. Source reasons remain verbatim in both locales.

The [copy UI boundary design](../design/ui-language-stage-a-draft.md#copy-worker-ui-boundary-152-review-correction)
records inspected callers: these CopyError values reach TUI updates, not CLI or
wire dispatch. The public Rust copy_source helper shares this TUI worker and is
used by injected tests. Future CLI/wire callers need an explicit English boundary,
not localized TUI key guidance. Underlying History/API errors remain unchanged.

Local macOS fmt/check passed, all four copy unit tests passed, and the injected
clipboard/original-body integration passed. Tests compare both languages for all
six errors and exercise real worker entries with omitted language (default En)
and explicit Ja, retrieval/code limits, hash-change notices and Unicode source
copying. CI qualification of the corrected tip is separate from the withdrawn
candidate's CI. No OS clipboard, physical-terminal or live-provider acceptance
is claimed.

Review is partial: no missing-copy count has been asserted for deferred owner
files or for other unreviewed (c) literals. In particular, named Japanese setup
resources require tracing the default-language caller after B-6 lands; the
resource's mere presence is not an untranslated-copy count.

## Next Work

W6 owns the independent Core, pre-configuration startup and model-facing
instruction changes in `src/control/db.rs`, `src/app.rs`, `src/ipc/server.rs`
and `src/compaction.rs`. W1 must not edit those four files. W1 owns inventory,
classification/remaining mixing cleanup and final acceptance after #138.
Preserve stored titles and all saved summary bodies. Use W6's compaction test
evidence from task-1 integration; do not infer it from this scanner. Do not edit
B-5/B-6 owner files until those tasks land, preserving #149 then #150 priority.
`src/ui/live_work.rs` is also coordinated with W8/#156 before any edit; its
separator-symbol restoration is W8's work. User-selected marks, branches and
separators must not be changed without approval; this audit has no symbol edits.
Regenerate this table after their exact commits reach main; current counts
do not measure those branches and do not claim zero mixed-language output.

Local script fixtures: 13 tests pass (134 in the complete Python suite), including
protected-token evidence requirements and deterministic owner-aware table output.
No Rust/product behavior change or
full Stage C CI result is claimed by the inventory commit.

## Product Copy Review, Batch 2 (Next Release Only)

Confirmed production literals, excluding tests/fixtures and intentional Japanese
resources. Batch 1 is a separate copy-worker branch; neither batch is for 0.6.0.
B-5/#149 and B-6/#150 owner files remain deferred until they land.

| Production Source | Confirmed Untranslated Literals | Disposition |
| --- | ---: | --- |
| `src/frontend/copying.rs` | 6 | Independent batch 1; not included in this branch |
| `src/ipc/server/endpoint/namespace.rs` | 2 | Batch 2: `runtime_error` message and recovery hint |

`runtime_error` constructs an application-owned startup diagnostic before an
interactive locale can apply. Its message/hint are Japanese regardless of the
English-default configuration; they have no other production recognizers. Only
these two complete strings change. The `IoFailed` code, `component=local_ipc`,
dynamic `reason`, namespace candidates, permissions, leases, filesystem actions
and all UI symbols stay unchanged. A pure unit test verifies exact English text
and preserves Japanese/Unicode in the injected reason. Unknown (c) candidates
remain unreviewed; this table is not a claim that they are all defects.

## Product Review Batch4: Watch Page Notices

The2Japanese notice literals in frontend/watch_page.rs are generated UI text,
not saved titles or canonical values. One reports unavailable descendant scope;
the other explains PageDown continuation through a bounded search range (not a
guarantee of matching rows). Producer/consumer tracing reaches
UiUpdate::Threads.notice and the existing localized browser filter/status line.
They formerly remained Japanese in default En. Both now use the loaded UI
language, retaining the exact original Ja sentences. Only this leaf production
module changes; no lookup-by-source-string translation is introduced.

Regressions exercise the real temp-config/DB read entry in default En and
explicit Ja,0/1/2/10saved rows with Japanese/combining/emoji titles, and a129row
witness-cap continuation. Tests compare source pages and encoded cursors across
locales and consume an En-created cursor with Ja rendering. Notice selection is
separate from bounds, hashes, pagination, IDs and read-sequence behavior.
Focused local and exact-tip CI results are reported separately. No zero-c,
complete-Watch, physical-terminal or final Stage C acceptance is claimed.

Scope correction: the old ownership table omitted draft_saver.rs, but actual
#150 source diff includes its queue-warning path. The unpublished draft-save
prototype is deferred, not pushed or a CI/integration candidate. #149 is now
confirmed on main79166408617e9f3acd71bdeff55930638fb62a3f with3-OS integration
CI41380530706 success; #150 remains deferred. Watch-page notices are outside
both current change sets. Dynamic Watch detail/recovery fallback provenance and
synthetic root-generated draft rows remain separate pending review.

## Scoped Review Fingerprints And CI Consistency

On main `7f5f8897`, all 25 existing reviewed decisions concerned
`src/presentation/messages.rs::Message::render`. Each ID was reread in its
current enclosing branch and the #157 width-restoration diff. `en` remains
`language == UiLanguage::En`: the 25 Japanese literals remain in `else` or
`(false, ...)` paths, with separate English paths; added `glyphs::formatted!`
arguments select glyph width within the same Japanese path. Interpolated source
values are not evidence of generated origin. All 25 remain (a); none are newly
classified protected. Their fingerprints were migrated only after this review.

Reviewed decisions now hash token kinds/text of their enclosing function,
including its signature and immediately attached attributes/modifiers; a
nested constant uses the enclosing function, preserving language predicate
coverage. A recognized standalone `const` or `static` uses the item. When no
such item is recognized, the entire source file remains the conservative
fallback. Sibling function edits no longer invalidate unrelated decisions;
changing a predicate, branch, argument or other token in the same function does.
Whitespace and comments do not affect the hash. This is a lexical heuristic,
not a Rust AST or a proof of semantic correctness. Changes outside the selected
item, including enclosing impl/module attributes, another function or macro
implementation, global constants, type behavior, configuration and callers can
change its meaning without changing the fingerprint. Generated macro contexts
and unusual Rust syntax may cause a wider fallback or ambiguous scope. Duplicate
literal ordinals are file-wide, so insertion/reordering of identical strings can
change an ID or associate an ordinal with a different occurrence; inspect context,
not just fingerprint, during review. Even within a function, language behavior
can change via external helpers or macros without a token change in that function.
Reviewers must re-evaluate such dependencies manually; a matching hash is not a
semantic guarantee. Unresolved and uncertain literals remain (c).

The GitHub CI consistency step runs the summary with reviewed decisions and
without `--check`. A stale/malformed decision or source mismatch fails; a
nonzero (c) count does not. It checks accounting consistency only, not language
correctness, completeness, protected-token provenance, terminal rendering or
final Stage C acceptance. Concurrent branches adding decisions must rebase onto
the migrated baseline and individually re-review their own entries; never refresh
fingerprints mechanically to force CI green.

## Product Review Batch5: Deletion Cleanup Storage Warning

Producer/consumer inspection confirms one fixed-Ja application warning in
frontend/thread_deletion.rs::finish_cleanup reaches DeletedWithWarning and the
already-localized deletion status. Default En therefore mixed English status
with Japanese application guidance. Localize only that complete wrapper using
the loaded UI language, keeping the original Ja sentence and opaque underlying
ApiError display unchanged. The existing SaveFailed classifier remains authoritative;
IoFailed/InvalidArgument and other errors pass through without the storage wrapper.
No code, hint/detail, confirmed deletion, tombstone, rollback, admission/cleanup,
shutdown, limit or filesystem semantics change. This helper has only a TUI caller;
there is no current CLI/wire boundary change.

Only frontend/thread_deletion.rs changes in production. Actual #150/#157 diffs
were checked, not just the stale ownership JSON. The planned Watch recovery
projection is deferred because #157 now also edits ui/watch.rs. Root deletion
panic/queue messages and other unresolved candidates remain separate; no blanket
source translation, manifest reclassification, fingerprint refresh or protected
adoption is made. This product work is after the 0.6.1 cutoff.

Fresh local macOS fmt/check/focused Clippy and10deletion tests passed, plus145
Python tooling tests (including13audit fixtures). New coverage checks exact En/Ja
wrappers, Unicode/combining source text, code/hint/details and absent hints,
non-storage pass-through, actual temporary DB cleanup failure after committed
Core deletion, both loaded UI languages, final UI status, closed mutation
admission and SaveFailed shutdown. Unchanged deletion regressions also pass.
Exact-tip CI is reported separately; no whole-screen, physical-terminal,
provider/clipboard, Windows-local or final combined Stage C acceptance is claimed.

## Product Review Batch6: Draft Worker Stopped Notice

After #150 actually landed at89c570a1 (exact3OS CI41399417402), inspection of
its current draft_saver leaf found the stopped-worker notice still fixed Ja
when delivered via DraftSaveFailed to the TUI. This is independent of #150's
already-localized rejected_message and uses neither the old deferred prototype
nor source-string matching. Only this complete generated notice follows the
loaded UI language; original Ja is byte-identical. #157's actual diff does not
include the changed production leaf. No shared presentation or operations files
are part of this batch.

Mac local fmt/check/focused Clippy and5 draft_saver tests passed on the first
implementation. Added coverage exercises real config default En, explicit En
and Ja,0/1/2/10 retained drafts, full critical outbox and subsequent ordered
release, exactly one warning per Thread, unchanged Unicode and combining text,
IDs/revisions/cursors/anchors/follow/typed-prefix, active UI text/status, no
unexpected persistence, and failed worker join. Existing tests cover coalescing,
closure detection, backpressure, new revisions and shutdown. The final test run
and exact-tip CI are reported separately. Local Python tooling ran146 tests,
including13 audit fixtures; manifest validation passed. Project hybrid RAG
returned no items with an incompatible/degraded index; no index was rebuilt.
No broad classification or fingerprint change is claimed. Exact3OS integration,
physical terminal, clipboard, live provider and final Stage C acceptance remain
separate. This work follows the subsequently selected0.6.1 cutoff.
