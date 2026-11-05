# Shellboy v0.8.3

This draft tracks changes after [v0.8.2](https://github.com/tya5/shellboy/releases/tag/v0.8.2). It is not a release announcement.

## Breaking changes

None. Version 0.8.3 is appropriate because the changes are additive or compatible, including migration 0029 (an index only). New CLI output fields and the `history get --parts` flag are additions; no setting or option is removed or changed.

## Before upgrading

- Stop the owner, back up the complete data directory, update the owner and CLI together, and restart the owner.
- Conversation schema upgrades from 28 to 29 through migration `0029_active_attempt_recovery_index.sql` (a partial index; no row is rewritten). Standalone RAG schema remains 4.
- Verify with `python3 scripts/check-release-upgrade.py --old OLD_BINARY --new NEW_BINARY --root EMPTY_TEMP_DIR --old-version 0.8.2 --start-schema 28 --expected-version 0.8.3 --expected-new-schema 29`.

## Changes since v0.8.2

| Change | User-visible effect | Breaking | Schema migration | Evidence |
| --- | --- | --- | --- | --- |
| Rate limits across all requests of a connection ([#224](https://github.com/tya5/shellboy/issues/224)) | After a 429, requests of the same connection profile wait behind one probe in FIFO order instead of all retrying; a usage-limit response (including the wrapped LiteLLM form) is not retried before its reset. Summary requests retry 429 and transient failures within the existing request budget through the same gate, and all map requests of a summary and all batches of a reduce round are sent together, one at a time with a yield so other Threads' requests can interleave. On one synthetic fixture, a summary took 337 ms instead of 1,350 ms serially; this is not a real-provider measurement. | No | None | Commits `ca2505f7f1`, `711b61bf68` |
| Resumable history GC ([#225](https://github.com/tya5/shellboy/issues/225)) | History GC continues within its budget across steps instead of stalling on a large batch; candidates over budget are kept and recorded. F1 "DB / GC" shows GC diagnostics. | No | None | Commit `20cd2444` |
| Recoverable automatic compaction records ([#226](https://github.com/tya5/shellboy/issues/226)) | An automatic compaction attempt is recorded before its stages run, so a failure after commit leaves an inspectable record instead of none. | No | None | Commit `5540f44438` |
| Faster startup recovery ([#219](https://github.com/tya5/shellboy/issues/219)) | The startup cleanup of interrupted model attempts uses a partial index instead of scanning every attempt. On a copy of one real database (70,494 attempts) the statement took 1–2 ms instead of 47–573 ms; the real cold-start effect is not yet measured. | No | `0029_active_attempt_recovery_index` | Commit `46ce2f068b` |
| Welcome logo ([#228](https://github.com/tya5/shellboy/issues/228)) | A new Thread's empty screen shows an ASCII logo with the version, model, working directory, config path and data directory, in the live-work accent color; narrow or short terminals keep the previous text. | No | None | Commit `a65421c74b` |
| Compaction transparency ([#230](https://github.com/tya5/shellboy/issues/230)) | `compact list` items for adopted summaries report the provider-reported input of the first request after adoption (`post_compaction_input_tokens`, `post_compaction_attempt_id`). The conversation shows a collapsed marker at each adopted summary (expand to read the summary) and a one-line note for automatic attempts that were not adopted. | No | None | Commit `57421d618b` |
| Summary parts reader and GC ([#232](https://github.com/tya5/shellboy/issues/232), [#233](https://github.com/tya5/shellboy/issues/233)) | `history get --node SUMMARY --parts` returns each summary part's stage, source range, size, provider-reported usage, reusability and body (paged). Reduce parts now record their sources, so GC collects them with their deleted conversations; parts without traceable sources are collected as stale cache. | No | None | Commit `7cb82d3923` |
| macOS shell start with a high open-file limit ([#234](https://github.com/tya5/shellboy/issues/234)) | With a large `ulimit -n` (for example 1,048,576), the shell guardian closed inherited descriptors one number at a time, delaying shell start and holding the owner lock long enough that an immediate owner restart failed. It now closes only the descriptors that are open. | No | None | Commit `b278088a23` |
| Documentation | The proactive compaction retry rule is described as it is implemented (new eligible content, not a 10% input increase). | No | None | Commit `57421d618b` |

## Unverified / pending

Synthetic compaction measurements do not establish real-provider latency, retention quality, or physical terminal behavior.
