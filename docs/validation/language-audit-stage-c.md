# Stage C Language Audit (#152)

Status: initial inventory, not completion or permission to translate source data.
Baseline: main `84dc782ef4c2ef9d42b704220a0db5565f863b5a` (#148 landed).
B-5/B-6 and W6 are not yet included. No third-party production source was edited.

## Reproduce

```sh
python3 scripts/dev/language-audit.py --classifications scripts/dev/language-classifications.json > inventory.json
python3 scripts/dev/language-audit.py --classifications scripts/dev/language-classifications.json --summary
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

Current raw scan: 353 Rust files; 3398 literals; a=279, b=1192, c=1927.
Individual review of 25 `Message::render` Ja branches is recorded in
`scripts/dev/language-classifications.json`; the table applies those decisions:
a=304, b=1192, protected=0, c=1902. `Message::render` was reread against the
actual current source after rebase; its source-token fingerprint is unchanged,
so no fingerprint was blindly refreshed. No production source was modified to
reduce the count. The earlier main13d5ac86/f5c9b31a checkpoint had3339 literals;
#148 accounts for the current source changes and extra fixture strings.
The zero-(c) gate intentionally fails at this stage. Final classification,
fresh En/Ja rendering/plural checks and three-OS CI remain outstanding.

## File Classification Table

| Source | (a) | (b) | Protected Canonical | (c), Review Pending | Editing Boundary |
| --- | ---: | ---: | ---: | ---: | --- |
| `src/activity.rs` | 0 | 0 | 0 | 34 | review in C |
| `src/app.rs` | 0 | 0 | 0 | 3 | W6, task 1 |
| `src/attachments/draft.rs` | 0 | 10 | 0 | 0 | review in C |
| `src/compaction.rs` | 0 | 7 | 0 | 1 | W6, task 1 |
| `src/compaction/context_usage.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/compaction/edit/bounded_tests.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/compaction/edit/tests.rs` | 0 | 2 | 0 | 0 | review in C |
| `src/compaction/keys.rs` | 0 | 1 | 0 | 0 | review in C |
| `src/config_setup.rs` | 0 | 0 | 0 | 10 | review in C |
| `src/control/db.rs` | 0 | 0 | 0 | 2 | W6, task 1 |
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
| `src/ipc/server.rs` | 0 | 0 | 0 | 8 | W6, task 1 |
| `src/ipc/server/endpoint/namespace.rs` | 0 | 1 | 0 | 2 | review in C |
| `src/ipc/server/endpoint/tests.rs` | 0 | 1 | 0 | 0 | review in C |
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

## Next Work

W6 owns the independent Core, pre-configuration startup and model-facing
instruction changes in `src/control/db.rs`, `src/app.rs`, `src/ipc/server.rs`
and `src/compaction.rs`. W1 must not edit those four files. W1 owns inventory,
classification/remaining mixing cleanup and final acceptance after #138.
Preserve stored titles and all saved summary bodies. Use W6's compaction test
evidence after integration; do not infer it from this scanner. Do not edit
B-5/B-6 owner files until those tasks land, preserving #149 then #150 priority.
`src/ui/live_work.rs` is also coordinated with W8/#156 before any edit; its
separator-symbol restoration is W8's work. User-selected marks, branches and
separators must not be changed without approval; this audit has no symbol edits.
Regenerate this table after their exact commits reach main; current counts
do not measure those branches and do not claim zero mixed-language output.

Local script fixtures: 12 tests pass (133 in the complete Python suite), including
protected-token evidence requirements and deterministic owner-aware table output.
No Rust/product behavior change or
full Stage C CI result is claimed by the inventory commit.
