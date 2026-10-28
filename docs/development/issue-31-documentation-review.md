# Issue #31: English guide review

Review baseline: `8f00000`; this follow-up starts at `69be5ad`, including the
reopened-issue fixes and the 0.2.0 no-compatibility clarification. Verify findings
against implementation and the adopted language policy; no UI language feature
or new Core responsibility is needed.

| Finding | Disposition |
| --- | --- |
| Medium 1: Japanese discovery queries for English docs | English recovery-hint query, explicit English literal-search instruction in Agent Guide/RAG, and corrected Japanese docs explanation. Preserve the system prompt: it already links Agent Guide; no prompt/language state is needed. |
| Medium 2: weakened model instructions | State physical Windows examples remain unverified, and restore the prohibition on repeated cache-invalidating edits. |
| Medium 3: narrowed AGENTS design obligation | Require discovered existing discrepancies, as well as intended contract changes, to be reflected/resolved in design before code/fixture changes. |
| Medium 4: absent/mistranslated UI labels | Align both languages with actual Japanese labels; retain Japanese navigation targets with English explanations. Add a cross-component guide/OpKind label regression. |
| Low 1: incorrect operation-count rationale | Correct the decision record: 36 TUI actions was accurate, distinct from 37 CLI operations. Keep duplicated count out of User Guide as a maintenance choice. |
| Low 2: missing sibling links | Add eight missing English-to-Japanese links; retain the English-only Agent Guide. |
| Low 3: search busy details missing | Restore the concurrent-search section with InvalidState, pre-embedding rejection/no charge, separate from cancellation and post-embedding retry cost. |
| Low 4: change-relative product wording | Replace “this change” scope claims with precise current hardware/ownership limitations in both languages. |
| Low 5: omitted qualifiers and wait mistranslation | Explicit English-only catalogue, old records may lack new EOF fields, and literal waiting label with faithful meaning. |
| Low 6: broken English | Correct Run ownership and operation-ID retention wording. |
| Low 7: existing factual errors | Document accepted deprecated status alias without a false removal deadline (also CLI help); document all four zero cache budgets and current-instance receipt expiry in both languages. |
| Low 8: unclear entry-point scope | Enumerate root/docs/guides/design/validation entry points; translate the two remaining small indices. Existing historical records are not bulk-translated. |

## Added compatibility policy

Comment `6480967665` adds the requested Implementation boundaries rule to
AGENTS: before 1.0.0, no one-off old-format/alias/key compatibility shims;
breaking changes are acceptable. Retain general migration runners, user_version
and protocol checks for long-term compatibility, guaranteed from 1.0.0. Existing
one-off-code inventory is a separate upcoming review, not implemented here.

## Validation and closing gate

Run format, all-target strict Clippy and tests against frozen guide/source
contents. Follow the recovery hint through the actual executable and require
useful shell examples. Check offline catalogue, links/fragments and twelve sibling
pairs. Distinguish macOS local results from unavailable native Windows/Linux CI.
Keep the issue open until its requested push and three-OS CI gates are met.
No push, release, installed binary change, paid API or default-data modification
is authorized by this review-follow-up request.


Final local gates passed: format, all-target strict Clippy, 76 groups / 1,322
passed / 0 failed / 24 ignored, offline docs 33, actual RAG CLI 13, operation-label
and recovery-hint regressions, twelve reciprocal sibling pairs and 734 relative
links / 50 fragments. The reopened-issue integration and terminal/pressure checks
also passed on the same source; acceptance records their scope. No push or native
Windows/Linux CI was performed, so the issue remains open.


Implementation is committed locally as `8f7b036`; subsequent tracking-record
edits do not change the checked source or bundled guide contents.
