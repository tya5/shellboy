# 開発判断

## Confirmations are not invalidated by other Threads; queue cancel needs no Y (#257, 2026-11-07)

Basis: personal use. Before the change any `EngineEvent::Changed` (any Thread) or a lagged
event bumped `confirmation_revision`, cleared `accepted_index` / `accepted_deletion`, and sent
`ConfirmationSourcesChanged`, which turned an open deletion dialog into `Phase::Stale` (Y
disabled), an index confirmation into `Confirmation::StaleIndex`, and a ready index preview into
`IndexPreviewState::Stale`; a Changed during a read also dropped the in-flight preview. State
tests (`changed_deletion_preserves_evidence...`, `stale_deletion_review_disables_destructive_action`,
`changed_confirmation_retains_preview...`) showed Y was a no-op after any change; the TUI itself was
not driven (code and state tests only). Chosen: remove the whole mechanism instead of adding a
variant. Kept: the one-shot `accepted_*` fence (a Y must follow the preview with the matching
request id), now cleared only by a new preview or its use. The guard that matters stays in the
owner: the target-marker comparison and live/stopping Run check at `src/control/db.rs`
(delete transaction) refuse a truly stale delete, and the dialog shows the refusal.
Lost: counts and cost estimates on screen may be slightly old (the dialog's `Reviewed at`
time says when they were taken). Reconfirm if a delete ever removes a target the user did not
review, or if index cost previews are found to mislead.

Queued-input cancel (`/queue` D/A, held rows) no longer asks for Y: the text is archived and
restorable with `/restore`, and the status line reports each outcome. A reply duty is not
restored (documented in the TUI guide). Agent Guide is unaffected: the model-side
`message cancel` CLI is unchanged. Reconfirm if mis-key cancellations are reported as lost work.

## Search speed: single KNN scan and two-stage visibility (#240, 2026-11-06)

Basis: the measurement at
https://github.com/tya5/shellboy/issues/240#issuecomment-6674241038 on the
owner's DB copy (44,038 eligible chunks, 6,723 nodes, 667 MB vec0 table). The
KNN over-fetch loop (k 80, 160, 320, 640) rescanned the whole table 3-4 times
(about 70% of slow-state time); `searchable` materialized the full `consumers`
closure (about 20%). Chosen, no other part (eligibility step, witness checks,
JSON decode are each at most about 5%, skipped by perf-by-contribution):

- `searchable` runs a root-only statement first and the unchanged full rule
  only when it is false. Equivalence test over random synthetic graphs.
- One vec0 scan of `min(count, 4096)`. The measured k=4096 trial needed one
  scan, and its extra 0.35-0.41 s was hit assembly for every distinct node;
  that cost is removed by building provenance/thread_scope only for the
  returned nodes. The old doubling schedule is replayed over the ordered rows
  (spans per node come from the satisfying stage only), so results equal the
  old loop; checked by a unit test against the old loop and by comparing result
  digests on the owner-DB copy before and after. After the change, on the same
  copy (warm, load about 7, 2 passes x 3 runs interleaved with the old build):
  all-history ranked_hits 2.6-3.1 s to 0.66-0.86 s, largest Thread 1.17-1.71 s to
  0.41-0.52 s; result digests (node, distance bits, span count) identical.
  Recheck if vec0 per-k cost grows with k on other DBs (not measured).
- Not changed, recorded as a latent risk: the `DELETED_OWNER` branch when roots
  are false (about 25 ms/node, not triggered on this DB).

## Automatic compaction target window and spill evidence (#215)

The owner approved proposals 1 and 2: bound automatic prefix selection by the
existing calibrated removal target, preserving whole groups and safety fences,
and record anonymous spill candidate and skip counts. Preserve manual and
overflow semantics, cancellation and atomic adoption; do not parallelize map
requests. Source reduction and observed token target are separate from semantic
retention. Historical slow-case spill skip reasons were not recorded and are
not inferred. Revisit if calibrated projection or fresh usage shows shortfall.

## End-message rename, step 1 details (#194, 2026-11-03)

Judgements made while landing step 1 of the
[reply-expected messages](../design/reply-expected-messages.md) plan:

- The `status` item field is shared by all item kinds, so it is renamed to
  `end_message_node_id` for message items too (same value: the assigned Turn's
  end message). Step 2 replaces it for message items with `reply_node_id`.
  Recheck if step 2 keeps a Turn end message on message items.
- The TUI operation form field for the renamed flag follows it:
  `最終回答のみ` / `Final answer only` becomes `終了メッセージのみ` /
  `End message only`. The `回答` row label is unchanged. Recheck if the user
  wants the old form wording kept.
- The exec contract prompt sent to the model ("Final answer contract") and the
  schema-validation messages keep their wording, since changing model-facing
  text is a behaviour change outside a rename.
## F6 request size display (#186, 2026-11-03)

The user approved option 3: F6 plus `usage get`. F6 shows, for a model answer,
`入力のバイト数（UTF-8）` with the total and nonzero sources, `入力トークン（提供元の
報告）` and the renamed `キャッシュ済み入力トークン（提供元の報告）` (previously
`入力キャッシュ（cached_tokens）`), in Japanese and English, with no new glyph.
Unrecorded bytes show `未記録`; unreported tokens keep `未報告`, since a missing
provider count is not a missing record. Numbers use ASCII thousands separators.
Bytes and tokens are never converted or compared as a ratio.

## Request input byte accounting attribution (#186, 2026-11-03)

Attribution rules for bytes that do not obviously belong to one item, chosen
so the partition is exact and deterministic: a merged message's wrapper framing
goes with the item that created it (for Messages, often a thinking block or a
tool result); a member's key, colon and one comma go with the member's label,
and a labelled first member takes the comma after it so cache markers change
only `other`; tool definitions, model and settings are `other`. All current
merges split by source, so no `merged` category is emitted. `latest_request`
searches the newest 33 Turns, mirroring the cache-comparison bound. Impact: small
framing shifts between categories, never between sent and unsent bytes.
Revisit if the TUI display (pending approval) needs tools as their own
category, or a new conversion merges content that cannot be split.

## DB worker activity display (#190, 2026-11-03)

The user approved option 1: place `DB <operation> <elapsed>` at the statusbar
right edge only while execution exceeds one second. Show `DB! <operation> no progress`
when the existing no-progress state is present, and show nothing otherwise.
Provide Japanese and English text/help, keep the existing footer row count, and
do not add a setting or persisted state. The indicator is observational only.
## Reply-expected messages: finalization judgements (#194, 2026-11-03)

The user answered Q2, Q11 and Q14 of the
[reply-expected messages](../design/reply-expected-messages.md) memo, accepted
the recommendations for the rest, chose glyph option B, and ruled that an empty
end message is a normal response (D10, refined): a successful empty result,
exit 0 in every mode even with `--schema`; Schema validation and repairs apply
only to non-empty text. The user also ruled
(correcting an earlier judgement here) that a Turn ended by Esc produced no end
message: it is not empty text but is treated like a stopped Turn, exit 4, in
every `--result` mode. These points were decided while finalizing, without an
explicit user answer:

- `history get --final-answer-only` is kept and renamed `--end-message-only`
  instead of removed (Q12): `exec --result turn-end|run-end` needs the
  end-message projection, which ordinary history reads do not provide. Recheck
  if exec stops needing it.
- `exec --result turn-end|run-end` keep exit 4 for held, recovery wait,
  stopped, failed and unavailable, as today; only the reply outcomes cannot
  occur. The user's "never exit 4" is read as "never exit 4 for an ignored
  reply". Recheck if the literal reading was meant.
- exec `reply` mode waits on `run_ended` of the assigned Run, not
  `thread_settled`, which without a cursor matches an earlier Run's state.
- Per-mode usage diagnostics: Turn usage (`turn-end`), Run usage (`run-end`),
  observed Run usage (`reply`).
- The TUI reply mode (Q7) is in scope but lands last; the `回答` row label is
  unchanged (Q10) while the `回答node ID` field label follows its field.
- Reply errors use existing codes (`target_unavailable`, `invalid_state` with
  `details.reason`); no `forbidden`/`not_found` error code is added.

## Measured proactive retry gate (#153, 2026-11-02)

Replace #125's permanent per-Run failure flag with a provider-usage growth gate.
After any non-adopted attempt require ceil(threshold/10) additional reported
input tokens. Ten percent is a fixed retry interval, not a capacity estimate:
with the default threshold it permits one retry per 20,000 measured tokens of
growth. No new setting or persisted state is needed. This avoids both a tight
retry loop and a multi-hour Run being disabled by one rejection. Keep unknown
and stale usage ineligible, the 200,000 default, and zero opt-out unchanged.
Revisit the fixed interval if bounded retries are still materially expensive.

Investigation: ordinary summarize(false) uses normal_prefix, ending before the
current and latest completed Turns. argument_free_compact first spills older
safe tool groups, then summarizes only that prefix. A long Turn can therefore
have a tiny summary source despite huge reported input, including when its old
tool groups are already spilled. Keep the safety contract; this change does
not make current-Turn text eligible or guarantee input shrinks below threshold.
Review follow-up: read-only W3 compaction and usage records confirm 44 spill
replacements were committed before the summary was rejected as `invalid_state`;
the old automatic result nevertheless reported only `rejected`. The trigger
reported 202,622 input tokens and the next completed conversation request
151,601, a 51,021-token difference; the interval can include other changes, so
this is not a controlled measurement of spill alone. The rejected summary
reported one map request, input 2,895, cached 0, output 1,051, and unknown
cache-creation tokens. No private conversation text is included here.

## Retained-output fixture clock (#41, 2026-11-01)

The latest-main standard exposed a real calendar dependency in the existing
work-overview navigation test. Its raw output ended2026-10-25T00:01Z; at the
2026-11-01T00:10Z diagnostic run, the unchanged seven-day policy legitimately
reclaimed it. The UI received OutputExpired, but the test discarded unmatched
updates and eventually hit the unchanged60s watchdog. Earlier G CI ran before
expiry, so its success is not evidence against this diagnosis.

Use one captured UTC instant for the synthetic end/output timestamps and a
one-minute-earlier start in this navigation fixture. Keep normal GC, retention,
assertions and watchdog limits unchanged; expiry tests own expired-source cases.
Temporary trace instrumentation is removed. Focused native verification passes
1/0 atfbf0d5c; v32/v33 failures and private trace remain recorded. This is a
fixture correction, not a product GC workaround or an unproven flaky-test retry.
Revisit other fixtures if another retained-output navigation test fails after
crossing a literal calendar date; do not globally disable retention to hide it.

## Scoped resource envelope for latest-compaction display (#41, 2026-11-01)

Use the existing canonical readback queries under a private readonly display
envelope rather than duplicating receipt/automatic/replacement merge semantics
in a second reader or adding stored summary state. Limit1 bounds returned rows;
SQLite's value-length limit, a counted progress allowance and pre-copy cell/
aggregate text limits bound additional resource exposure. Over budget means an
explicitly unknown compaction field; the rest of Thread metadata stays live.
CLI output and saved sources stay intact. This does not replace actual query-plan
and full G9 evidence, and no latency threshold is promised.

The budget is a conservative initial display policy, not a statement about
typical user data. Revisit if ordinary synthetic/observed histories frequently
hit it, if SQLite behavior changes, or if snapshot ownership installs its own
progress callback. SQLite permits one callback per connection; current ownership
is one finite presentation worker/readonly connection, with RAII cleanup before
the caller continues its shared snapshot. A new owner-level hook requires
composition, not replacement. No generic observation engine or persistent cache.

公開版の判断を記録します。日付、理由、影響、代替案、再確認条件を必要な範囲で残します。過去の私的作業記録を利用の前提にしません。

## Coalesced Snapshot Reads And Slow-Only Diagnostics (#47, 2026-10-30, Active)

Use one scoped UI snapshot with one inflight read. Changed bursts leave one dirty
pending flag, eligible three seconds after the previous result. Reuse the existing
500ms tick, including when no progress animation is active; do not add a timer,
worker, queue, setting or DB-worker sleep. Normal eligibility is within3.5s plus
existing worker/DB backlog, not a hard completion guarantee. Close/new-generation
protect against late application. Esc and F8 closure stop refresh.

The parent-approved small-version surface distinction applies this Changed cache
only to explicit F8 usage/get detail forms. Existing reference navigation
(`FollowManagement`) remains a one-shot query and F6 pages inspect cached fields;
neither registers a new Changed subscription. Explicitly re-query the form or
follow a reference again for newer values, with no freshness deadline promised.
Each actual reference aggregation uses one `open` slow diagnostic only at>=100ms.
Close/draft switch leaves no hidden refresh, ordinary Thread selection remains
metadata-only, and index confirmation semantics are unchanged. Do not expand
reference lifecycle registration as part of this clarification.

The latest user instruction withdraws all-execution logging. Only monotonic DB
aggregation durations >=100ms enter existing shellboy.log as usage_totals_slow.
Its typed payload is trigger(open/changed/cli), scope and elapsed_ms, no content or
IDs. Faster aggregations emit no aggregation event at any level. A small read-local
context follows the existing request task and DB job to preserve the trigger;
there is no protocol flag or new logger. Pure/injected duration/state-clock tests
cover the exact boundary, burst query counts, zero fast logs and content exclusion.


## Measure Saved Totals First (#47, 2026-10-30, Active)

[Latest comment 6515082156](https://github.com/tya5/shellboy/issues/47#issuecomment-6515082156)
withdraws the earlier arbitrary-period/write-time additions (comments6513644118/
6513653608) and the complex approved design through308dfef. Preserve those sources
in archive refs; do not carry their schema/observer/ledger/period gates forward.
The started, untested schema-only WIP is isolated at protected archive commit
ff2d0cd and is not part of the current branch or an integration candidate.

Use existing saved state/standard usage/timing and measure indexed SQL first.
100ms is advisory, not a CI threshold. The subsequent user/reviewer clarification
accepts roughly500ms at100k requests after removing obvious read-query waste.
Keep read-time aggregation; only substantially slower measured behavior warrants
a new report with one minimal alternative, never an unapproved persistence change.
This limits scope without pretending missing usage is zero. No period filters,
client-invocation observation, summary accounting or new GC policy in version1.


## Standard Usage Scalar Boundary (#49, 2026-10-30, Active)

The leader adopts five independent optional u64 measurements, restricted to
`0..=i64::MAX` so Rust saves and SQLite reads have identical exact-number
semantics. Check JSON integer type and SQLite extraction type before accepting
legacy data; never cast REAL overflow to integer. Missing/invalid fields are
unknown, including when all fields are unknown; explicit zero is known. Do not
repair totals, cache or reasoning by inference. Keep the normalized Rust value
available to future #47 writers rather than reparsing saved JSON.

Normalize only known provider-envelope usage members, including summary and
failure/stream audit response copies. This closes the second persistence path
for attribution; do not recursively remove user-content keys named usage or
alter output, error diagnostics or HTTP request bodies. Historical raw and usage
rows remain unchanged, with SQL scalar reads supporting large legacy usage.
The alternative full-document cap loses reported numbers and the alternative
unbounded document load retains provider appendices. SQLite must still parse old
large documents internally. No special rewrite migration is justified. Revisit
only if the provider standard fields or exact numeric storage contract changes.


## Measured CI test layout and failure bounds (#46)

Use the [adopted performance design](../design/ci-test-performance.md) and
[investigation record](issue-46-ci-performance.md). Historical Windows logs
separate substantial repeated linking from native fixture execution; the first
candidate reduces links and fixture-only commits without changing production
behavior. Keep subprocess suites standalone and enforce complete registration
and normalized identity/ignored-set equality. The initial native comparison
retained two Windows test threads. After fixing
shared runtime_features 5s watchdog/poll helpers, source `8fa0429` passes both two
and four threads with identical test/ignored sets. Harness sums 350.876s versus
237.908s favor a bounded four-thread CI default; verify it in the normal three-OS
workflow and retain explicit two-thread diagnosis. Its first normal verification
found three response_retries 5s fixture guard failures; repair those with existing
notifications and bounded saved-phase diagnosis, preserving the failed run and
all actual product timeout/retry/absence/responsiveness assertions. The repaired
ordinary four-thread source `03ceac1` then passes all three OSes: Windows Tests/job
285/379s versus 640 / 888s at `0469ca0`. Keep the measured four-thread default, while
preserving source-specific failures and runner/cache variability in the evidence. These independent-runner
samples are not a latency guarantee. Discard the 405-source index rewrite:
isolated measurements did not support it, unlike historical-DDL seed batching.

A short mock-arrival or waiter-completion watchdog is not a product latency
contract. Raise deadlock guards, retain phase diagnostics and require all 24 IPC
wait receipts to remain pending when status succeeds, before stop. Their expiry
is longer than the watchdog, so expiration cannot make a blocked dispatcher pass.
Do not blanket-convert native upload/termination tests to 1ms: those verify real
phase/ownership wiring, whereas existing pure tests cover deadline and retry
policy. Do not add test-only production clocks, new dependencies or filesystem
security exclusions without evidence. Reconsider higher concurrency and further timer
refactoring when repeated native measurements or changed failure profiles justify it.

## Review follow-up: bounded lifecycle fixes (2026-10-30)

Keep lifecycle fixes in the existing Control, capture and supervisor ownership
boundaries; no new persistent table, worker, queue, generic recovery engine or
compatibility route is needed. Findings and commit mapping are tracked in the
[follow-up record](issue-36-39-follow-up.md).

- Revisit remains a strict current-format state. Optional projection may omit
  malformed state and stop/startup may discard it; explicit reads report the
  error. Defaulting absent counts would manufacture facts. SQL failures still
  propagate. Revisit capacity recovery keeps the existing retry behavior,
  including its possible empty-input model Turn.
- Thread-local malformed capture metadata can be isolated, but a recovery JSON
  value violating the schema CHECK is database-integrity corruption. Do not
  weaken startup quick_check to make a synthetic corrupt database look usable.
- Latest Run retains the adopted created_at/id order and existing covering
  index. Activity and list must agree. No migration/sequence is justified by
  this review; wall-clock rollback remains a timestamp-order limitation.
- HUP is a terminal/session-loss notification and cannot reliably express a
  second human request. Make it idempotent; repeated explicit TERM retains force.
  The reviewer's normal-close orphan finding was withdrawn after its process-wait
  script was corrected. Exact baseline/current zsh comparison confirms the
  duplicate-HUP defect, not a new bash cleanup defect.
- Forced exit best-effort signals only groups owned by live supervisor tasks.
  One set entry per active native task is removed on confirmed disappearance or
  task destruction. UI access uses try_lock and a racing spawn sees an atomic
  force flag. No persisted numeric process identity is trusted. A stalled spawn,
  OS process-group reuse, blocked terminal restoration and Windows callback
  deadlines remain explicit limits rather than durable-success promises.

Reconsider these choices if current-format state evolves, native OS evidence
contradicts the bounds, or product requirements require non-timestamp Run order.
See [acceptance](../validation/acceptance.md) for executed checks and remaining
platform limits.

## Shellboyへの改名と公開構成（2026-10-26）

利用者の指示により製品名、Cargo package、実行名、環境変数、既定保存先をShellboyへ統一する。旧名の互換は不要とし、別名launcherや旧保存先の自動探索・移行は追加しない。旧データの移動も行わない。影響は起動名・設定参照・新しい既定保存先であり、旧環境を変更しない。保存先と検証は [setup](setup.md)、[検証状況](../validation/acceptance.md) を参照する。

非公開資料を含む旧リポジトリの履歴は保管し、公開用には別ディレクトリで新しいGit履歴を用意する。履歴の書換えだけで非公開資料を隠す方法は採らず、公開用ファイル集合を明示する。利用ガイド・採用設計・ソース・テスト・fixture・配布migration・検証ツールは保つ。非公開原文、過去レビュー台帳、個人環境記録、私的な画像・ログは公開構成に含めず、公開版のリンクやテストをそれらへ依存させない。

改名前の実行結果は改名後の合格に流用しない。新しいsourceと公開用構成でチェック結果を記録する。公開準備の段階ではpushを行わず、ライセンスは利用者指定のMITを採用した。後続の公開許可は本書の「公開後の保守と初版配布」に記録する。公開方針や互換要件が変わった場合は、この判断と入口文書を更新する。

## コアの責務

UI投影・module分割・既存資源の所有整理を、新しい業務状態や独立workerの追加と混同しない。新しいCore型・永続表・event・queue・依存が必要な場合は、既存責務では不足する理由、上限、所有者、寿命、終了時回収、互換を対象設計に記録する。数値を減らすためだけの削除や汎用frameworkの追加は行わない。

## 非同梱DDLへのテスト依存を外す

公開対象から原仕様snapshotを除くため、依存smoke testが読み込むDDLを原仕様の論理候補から配布中の `migrations/0001_initial.sql` へ変更した。初期migrationの35表（core 33表、採用経路、migration ledger）とintegrity/foreign-key検査を維持する。元DDLをコピーして別のschema正本を作らず、後続migrationの検証は既存のstorage試験へ残す。これにより非公開資料なしで結合試験をコンパイルできる。

## 未統合だった復旧表示修正の扱い

改名前に用意されていた修正は、履歴やerror JSONを変更せずtyped errorのmessage部分を短い表示へ投影する内容だった。名称変更とは別のcommitとして取り込み、完全fieldのコピーと利用者本文中の `provider:` は保持した。変更順序を再構成するための旧履歴のmergeや書換えはせず、公開用履歴には修正済みソースだけを反映する。

## 公開後の保守と初版配布

利用者の追加指示によりpushと初版Releaseを許可された。ローカル標準checkの成功をGitHub上のWindows/Linux成功として扱わず、GitHub CIの結果を確認してからversion tagを作る。tagに対応するnative release binary、公開資料、SHA-256をReleaseへ掲載する。接続済みGitHubツールにはRelease asset upload操作がないため、Git CLIと認証済みGitHub CLIでpush・Release操作を実施する。

レビュー指摘PR2-1〜3はコード経路を確認し、公開文書に条件・回避策・再現未確認の範囲を記載した。ここでgoalを再開したり、指摘の全是正完了とは扱わない。製品作業は公開リポジトリ、非公開台帳はGit管理外の `.local/review/` 内で独立したローカルGitとする。旧Gitを公開履歴へmergeしない。

実proxyのURLとモデル名を持つ2つのignored試験は、合成データ専用・明示実行のみであり、秘密情報ではないため保持する。製品既定値や利用可能モデルの保証ではなく、通常CIでは外部APIを呼ばない。

## 初回CIで判明したOS固有の境界

LinuxとmacOSでopenptyのwinsize引数のconst性が異なるため、共通試験の4箇所はraw pointerで渡す。OSを跨ぐ広いlint抑制は追加しない。WindowsのSECURITY_ATTRIBUTESはCreateFileWだけが借りる同期blockへ閉じ、非SendなC構造体をasync状態へ残さない。unsafe Sendの追加、worker変更、Win32呼出し順の変更は不要と判断した。

Windowsの標準検証で、Unix専用テストのjson macro importにもcfgが必要と判明し、importだけを同じcfgで制限した。CIはClippyのkeep-goingで全targetの診断を集め、失敗時も依存cacheを保存して同じ依存の再ビルドを減らす。通常CIのpushはbranchに限定し、version tagでは専用release workflowを実行する。コンパイルやテストの失敗を成功扱いにはしない。

## モデルAPIのTLS検証設定（2026-10-26）

利用者の依頼により初版0.1.0へ接続ごとの `ssl_verify`（既定true）を追加する。reqwest 0.13.5/rustlsに環境変数だけで検証を無効化する機能はなく、既存ConnectionとHttpModelClientの生成箇所で完結させる。設定省略で無効にならないようRust DefaultとSerdeの既定値を揃える。会話/要約/embedding/モデル一覧で同じ設定を使い、ownerのhot reloadや新規環境変数は追加しない。証明書チェーンだけでなくホスト名なども検証対象外になることをガイドへ明記する。追加CAや接続時の対話確認は今回の範囲に含めない。

確認は合成証明書のローカルHTTPSで行い、HTTPだけのmockをTLS成功の根拠にしない。既存リリース候補のCI結果と変更後の確認を区別し、変更後の候補でCIを確認してからタグを作る。

TLSのローカルfixtureに限り、既存lockにある `tokio-rustls = 0.26.6` を固定のdev dependencyとして直接参照する。実行時のOpenSSLやOS trust storeへの書込みを不要にし、合成の公開テスト鍵で全OSのHTTP要求を検査するためである。製品依存・worker・永続表は増やさない。HTTP(S)プロキシとNO_PROXYは既存ライブラリの環境変数対応を案内し、独自の設定schema/除外規則は実装しない。

Windows CIで判明した管理UI試験のUnix絶対パス前提は、fixtureをOS別絶対パスへ変更して解消する。製品の起動準備完了判定は緩めない。CIのTestsにはno-fail-fastを付け、一つのtargetが失敗しても後続targetの診断を集める。

Windows全target試験で判明した添付fixtureのUnix絶対パス11件と、AGENTS参照pathの区切り期待値1件はfixtureだけを修正する。残るoverflow回復試験の5秒timeoutは段階が不明であり、PowerShell起動遅延や製品障害と断定しない。期限/製品動作は変えず、失敗時の段階と有限の保存状態を記録する。Windows CIでは当該回帰を全target testの前に実行し、不要な全実行ファイルのリンク待ちを挟まず診断する。成功後は従来どおり全targetを実行する。

初回配布workflowのUbuntu 22.04ではPython 3.10にtomllibがなく、build前のversion検査が失敗した。Python/action依存は増やさず、固定Cargoのmetadata JSONからpackage versionを読む形へ変更する。Rust製品ソースは3 OS CI合格時のまま。Releaseも配布assetも未公開のため、初期配布前のv0.1.0タグだけをこのworkflow修正へ更新する。公開済みReleaseのtag/assetはこの方法で置換しない。失敗runと旧候補hashは検証記録に保持する。

配布tagの再CIではWindows overflow試験の5秒待ちが再発し、追加診断で「最初のshell command echoがrunning・raw未到着、overflow開始前」と判明した。単独実行2回と前回fullは成功しており、製品側に5秒完了契約はない。Windowsに限りこの実プロセスを含むfixtureの待機上限を既存native shell fixtureと同じ15秒へ揃える。Unixは5秒、診断・全ての意味検証・再実行回数のassertを維持する。OS内部の遅延原因までは断定しない。

この追補はtest harnessと判断記録だけで、製品ソース・同梱guide・Cargo依存は配布tag96e65bfから変更しない。タグを再度更新して同一製品を再ビルドすることはせず、配布binaryとそのsourceを保持し、追補後CIを別hashの検証として記録する。公開済みのtagやassetの置換は行わない。

## ストリーム差分と失敗後の受信本文（2026-10-26）

会社Windowsの社内proxyで複数モデルが最終JSON不一致による復旧待ちになる報告を受けた。当初はmetadata以外の意味項目だけを照合する案としたが、利用者からAPIがその一致を保証するのか指摘を受け、途中と最終の差分自体を失敗条件から外す方針を採用した。最終responseそのものの既存検証を行い、最終データだけを採用・実行する。途中の差分はAPI違反と断定せず、固定分類の診断と保存済み原文から調査する。

成功時も差分があれば既存node表にpath外のmodel_stream_observation監査nodeを同じ採用transaction内で保存し、通常モデル再送と索引に入れない。新しい業務state・永続表・worker・依存は不要。16件までの差分分類と総数、既存64MiB stream上限内の受信回答/平文推論/確定itemと最終responseを保持する。DBの既存payload容量計数とGCのTurn所属rootを共用する。履歴のために会社の実データやAPI資格情報をGitへ入れない。

不正な最終応答等で復旧待ちになっても受信した有料本文をUIから消さず、保存済み証跡から再表示する。最新窓の表示上限は維持し、過去の受信データはUI一覧と既存原文cursorで発見・閲覧可能にする。Thread削除・GC時の既存回収責務には平文推論も含める。

受信deltaと最終本文の差も観測対象とし、item.doneのない応答でも途中本文を失わないようにする。response全体の回答/平文推論差には架空のoutput_indexを付けない。同一HTTP chunk内で後続JSONの解析が失敗した場合も、先に解析済みのeventを受信本文へ反映する。成功時のmetadataだけの差は原文・診断へ残し、通常会話へ同じ回答を重ねない。

## Windows継承環境の隠し変数（Issue #1、2026-10-26）

`environment_block`が`=C:`だけを例外にしていたため、既存の`inherited_environment`が読み取れる`=::`、`=ExitCode`等を再構築時に拒否していた。ドライブ名の列挙を増やさず、先頭以外の`=`を区切りとする既存の読込み規則へ検査を揃える。[MicrosoftのMSBuild実装](https://source.dot.net/Microsoft.Build.Framework/BackEnd/CommunicationsUtilities.cs.html)にも隠し変数と`=ExitCode`の説明があり、継承環境を通常設定の環境変数名制約と同一視しない。値のNUL、名前のNUL・空名・途中の`=`は拒否し、設定envの英数字/underscore制約は変更しない。

不正entryの診断は変数名だけを先頭128文字に制限して引用・escapeし、値はコピーしない。UTF-16の環境block本体はlossy変換せず、値中の`=`や非Unicodeのcode unitも保持する。新しい状態・依存・worker・設定は不要。親processの環境を変更しない合成fixtureで混在環境と注入拒否を試験し、Windows専用の起動回帰は実機確認と分けて扱う。

## WSL/LinuxのIPC起動条件（Issue #2、2026-10-26）

利用者にXDG_RUNTIME_DIRの作成や祖先権限の調整を求める旧方式を廃止する。XDGが使えなければmacOSではOSのユーザー一時領域を検証し、それも利用不能ならcanonical /tmpのsticky領域へランダム0700 directoryを原子的に作る。以前の固定hash由来socket配置を、既存endpoint.jsonのaddressにランダムpathを保存する方針へ変更する。pathは認証情報ではなく配置のlocatorであり、instance handshakeと接続両側のpeer uid照合を併用する。[Linux unix(7)](https://man7.org/linux/man-pages/man7/unix.7.html)と固定Tokioのpeer_cred実装を確認し、独自OS syscall wrapperや新依存は追加しない。

ディレクトリ作成後・socket bind後の公開前クラッシュでlocatorを失わないよう、予定pathをmkdir前に既存publisherで原子的に保存する。通常のCLI接続はbind完了後に可能になる。owner/Control間にはpublish済みEndpointの小さなメモリsnapshotを共有し、shellへSHELLBOY_SOCKETを渡すための毎回file読込みや業務stateを増やさない。別data-dir指定では環境pathを使わず、指定環境pathが失敗しても他ownerへfallbackしない。

清掃は現在認められる安全なbase・専用名・所有・0700・内容・file identity・生存probeで限定する。別XDGへ変更した場合や改変されたlocatorを根拠に探索範囲を広げず、削除対象と検証できない残骸は保全する。旧固定名の自動移行清掃や/tmp全域走査は行わない。保存先はsocket配置から独立して新規0700、既存の自分のdirectoryも0700へ限定し、symlink/他uidへのchmodや親の権限変更をしない。実行UI thread、Core状態・表・worker・キューは追加しない。

## Enter直後の送信表示と障害切り分け（2026-10-26）

既存のdraft ID/revision・operation ID・message IDを使い、UI内のpendingだけに送信開始時刻と反映確認を持つ。Coreの状態・表・event・workerは増やさない。受付済みと画面反映済みを分け、source IDと履歴node IDが一致した事実を一方向に保持する。高速完了でsource通知が遅着した場合は結び付けまで保持する。Completedとsource IDが揃ったら現在の履歴窓外でも解放し、画面が表示待ちに固定されることを避ける。実際のRun/Turnが動いていれば保存後の表示待ちは補足へ回す。アニメーション時計も主表示に従い、復旧・異常等の静止状態で送信のためだけに10fps描画を続けない。

固まった場合の切り分けには、既存の有界診断writerへfrontend受信・処理開始・保存/失敗を記録する。operation IDをfrontend受信時に作り、その同じIDでControlへ渡す。保存成功はUI通知の前に記録し、保存前と通知後の問題を区別できるようにする。本文や任意エラー文字列の記録、経過時間だけによるtimeout判定・自動再送は追加しない。UI描画完了の永続的な追跡は今回の範囲外であり、ログ欠落だけから未受付と断定しない。

## レビューgoal再開時の検索境界（2026-10-26）

> Historical decision: earlier compatibility choices are superseded by Issue #33
> and the latest current-format decision at the end of this log.

> Historical choice; compatibility portions are superseded by Issue #33.

PR2-2は採用する。旧splitter identityの選択空間を、新規sourceの索引成功だけで非選択へ変えない。旧vectorへ異なる分割を混在させず、既存candidate/group pointerへ新規分を保管し、全job成功後の明示selectまたはrebuildで切り替える。自動の全履歴再索引は行わない。新しい表・worker・Core状態は不要。

PR2-3は採用する。巨大rawは既存decoder/SQLite BLOB page読取りでspan全体をincremental hash検証し、返す原文referenceを最大64K文字rangeに限定する。小rawの全文referenceとhybrid融合は保持する。正常な巨大hitをinput_limitでscope全体失敗にせず、実際のsource不整合は区別する。

PR2-14の可変400文面によるpause回避は採用する。provider codeがない場合はkind/statusの障害クラスで連続計数し、文面中のrequest ID等でresetさせない。同じ根本原因と断定せず、成功時resetと既存の有限counterを維持する。読取identityと分割設定の分離は続けて検証する。

PR2-6は既存CLI契約に沿い、末尾のstatus.next_cursorを省略する。非末尾のcursorと、旧null/新省略の読込みは保持する。

PR2-8の『elapsed_msを直接の子終了時までへ変更』は採用しない。既存shell設計のS6は配下確認・drainを含む監視期間と明示しており、実装も両OSで一致する。子終了後の回収待ちが含まれるという指摘自体は正しいが、保存済み値の意味を変えず、利用ガイドへ期間を明記する。CPU時間や直接子だけのdurationが必要となった場合は、別の測定事実として検討する。

PR2-1は採用する。通常256MiBのreceipt上限は維持し、停止2枠に必要な最大outcome＋request frame/管理metadataの固定余白を追加する。RunStop/ShellStopだけその有界上限を使い、同じreceipt/counter/再送経路を共有する。追加は内部のAdmission分類だけで、Core lifecycle・表・workerは増やさない。停止自身が余白を埋めれば新IDは拒否し、元ID照合とreceipt-free TUI停止は維持する。

PR2-14の読取identityには、分割設定と独立したendpoint/model hashを既存app_metaへ1space1件保存する。旧spaceは現在の完全keyまたはlegacy keyを検証できる場合だけ補完する。既存markerが一致する読取では書き直さない。RAGは読み取り専用を維持する。元splitが分からず検証できない旧spaceを推測再束縛せず、元の設定へ戻して明示selectする導線を残す。新しい永続表・workerは不要で、markerはspaceと寿命を合わせる。

PR2-7は採用する。観測経過を適用仕様へ明記し、背景移行の日本語文面をFrontendへ移す。既存の公開Rust型EngineEvent::Noticeのin-process payloadを対象execution・既存ForegroundRelease・取消件数へ置き換える。既存enumの可視性だけをadapterへ広げ、新しい状態・表・event種別・worker・依存は追加しない。EngineEventはCLI/保存wireではなく、公開CLI・DB契約は維持する。Coreはgroup保存成功後だけ事実を通知し、Frontendで現在と同じ文言へ投影する。将来の表示変更もCoreの翻訳変更を不要にする。

PR2-9は対象を調整して採用する。現行macOSはユーザー別TMPDIRではなくcanonical `/tmp`を使う。Linuxでは新規namespace directoryへの既存fs2 flockをSocketFileの生存期間保持し、協調するage清掃から保護する。[systemdの推奨](https://systemd.io/TEMPORARY_DIRECTORIES/)に沿う限定対策で、既存listenerごとにFDを一つ持つ。新しい表・worker・poller・依存は不要。stale metadata捕捉はlockを取らず、既存のlive socket probeを維持する。XDG最終logout・reboot・明示削除は別の寿命境界であり、防止や自動再bindは加えない。macOSのcleanerが同lockを尊重する根拠は未確認なので、不要なlockを追加して消失防止を保証しない。Linuxの実清掃試験は実行環境がある場合だけ実施し、macOSの合格で代替しない。

PR2-10/11は部分採用する。Turn作成の共通保存と復旧TurnのCASを共有し、認可・wake・context actorは各受付に残す。receiptは復旧したTurnのものだけを解放する。LiveRegistryはmapを非公開にして原子的な登録/通知/slot照合削除を所有する。一つのRunの停止と所有木の停止は対象・戻り値・計算量が異なるため統合しない。DB・spawn・Changed・再kickをRegistryへ移さず、新しいengineや遷移DSLは不要。構造整理と復旧範囲の是正を別commitへ分ける。

PR2-12/13/16〜18は採用する。受信済み本文は保存IDの通知だけで消さず、同じapplyで本文を取り込んでから解放する。eviction後の遅着応答では仮本文を維持する。確定待ちの対応表は既存provisional/cache予算内に限る。表示groupは保存済みの中断参照、または隣接する同executionのcall/resultだけに限定し、全memberの原文・anchor・完全URLを保持する。離れた結果やmetadataだけの結果を根拠に呼出しを隠さない。Coreの履歴や状態を表示都合で変更しない。

PR2-5は競合を作る待ちと、保存状態を観測するpollを分けて対処する。RAGの3競合は要求ごとのarrival/releaseへ変更する。添付の条件pollは残し、失敗時だけ有限の状態診断を追加する。過去のtimeoutやWindows cleanup待ちの原因を、この修正だけで証明したとはしない。新しい汎用fixture engineや製品の遅延hookは追加しない。

PR2-15は既存65,536のgraph work境界を維持して、seedの走査起点を狭める。LIMITだけでは無関係なreplacement走査を防げないため、Turn node/legacy receipt起点のqueryとcovering index2本をschema v9へ追加する。新tableや探索engineを作るより小さく、追加disk・書込保守・初回構築費用を負う。旧SQLとの集合一致とbundled SQLiteのVM命令数で利益を検証し、当該Turn自身の候補数やDB待ちまで一定時間とは約束しない。移行の原子性・既存本文/割当/予約の保持も確認対象とする。

PR2-19は採用する。重複admissionの競合を既存callへ保存する新しいtool errorへ変換せず、競合として返す。failed attemptの再分類も保存済みtyped context-limitだけへ限定し、入力側の原因を信じて失敗原文を置き換えない。新しい状態や再実行機構を足さず、既存の保存事実とCASを強化する。

表示の所属と原文位置は別に扱う。group本文は実際のresult/summaryのnodeとUnicode位置へ結び、生成headerで本文位置を消費しない。複数callの依頼表示と結果表示を分担し、結果はcallが画面外でもcommandと成否を読める形にする。索引の初回利用可能通知は簡潔にするが、既存の選択spaceが別spaceへ変わる通知を通常のreadinessに分類して隠さない。

F4の再測定は製品と同じ32件/4msの更新経路を使用する。測定用の旧256件pumpを廃止し、pump後の表示source・入力からTestBackendまでの時間・DB往復を観測する。中間値の統合も観測間のskipに含むため、queue待ち時間や通知配送件数とは呼ばない。保存確定は仮本文を除いて確認する。既存の測定用コードだけを修正し、製品のhook・workerは追加しない。条件の異なる旧baselineとの性能非退行や、実端末の描画・IME性能をこの測定から主張しない。

PR2-17/18の実端末再確認で、正式な親子終了記録のkind・詳細参照が一般fieldsの後方で切り捨てられ、要約と詳細が重複する条件を確認した。中断envelopeの関係情報を既存fields予算内で優先する。照合条件や上限を緩めず、Coreの保存形式・原文・表・workerは変更しない。小さな表示用fixtureだけでなく、実保存形式とmetadataの多い場合を回帰対象にする。

## v0.1.2タグ前の再レビュー（2026-10-27）

外部レビューは採否の入力とし、コード・保存契約・再現条件で判断する。受信本文の有無と最終本文との差を表示の根拠にし、監査記録の存在だけで会話本文があるとは表示しない。巨大rawの複数spanは同じwitnessの検証走査を共有する。起動エラーの次操作は起動段階に合わせる。いずれも既存の保存形式・worker・Core状態を増やさず対処する。

> Historical choice; compatibility portions are superseded by Issue #33.

索引更新時の旧active維持は継続採用する。新candidateの履歴はselectまで既定semantic検索から外れ、select後に新旧を統合するには明示的な有料再索引が必要という制限を明示する。pause/失敗理由と切替案内は併記し、一方を隠さない。自動selectや暗黙の有料backfillは追加しない。

最初のv0.1.2候補のWindows CIはUnix専用テストの補助accessorがWindowsのtest buildにも含まれ、strict Clippyのdead_codeで停止した。全呼出しがUnix限定であるため同じcfgへ揃える。警告抑制やWindows jobの成功扱いには変更しない。macOS/Linuxは同候補で成功したが、後続修正は改めて3 OS CIで確認する。

受信previewを元のTurnへ戻す際は、表示用の項目をcanonical ordinalへ数えない。履歴の次窓・前後端・cache再利用・trimの位置は採用履歴だけで計算する。遅着した監査読取りは既存の履歴世代と保存revisionへ束縛し、新しいpreviewを古い結果で消さない。新しい永続状態やCore状態機械は不要。

追加レビュー後の最初のnative CIで、WindowsはUnix専用importのcfg漏れ、Linuxは既存lease回帰の古い英語message期待が失敗した。importを対象OSへ限定し、テストは新しい機械reasonへ照合する。lease保持やlive socket拒否のassert自体は維持する。ローカル標準checkは履歴ordinalの追加是正を優先してClippy中に中断し、成功には計上しない。

## 判断記録の再レビューと長時間稼働（2026-10-27）

Issue #3は採用する。観測は同IDでも再計算しreceiptを作らない。waitはTurn待機/解除を保存するため従来のintentと再送保護を維持する。semantic検索の再観測で外部費用が再発生する場合があり、read-onlyを無課金や完全なSQLite query_onlyとは扱わない。変更とwaitの終端結果はcreated_atから24時間経過後の既存GC保守、又は圧迫時に回収するが、ID/hash/origin等を同じrowの期限切れ記録として保持する。古い結果を失った同IDで変更を再実行せずoperation_expiredを返す。pendingと未反映参照は保護し、無制限のID tombstone削除はしない。任意IDを安全に失効させる新しいticket発行機構より、既存表の小さい結果縮退を選ぶ。墓標のmetadataも有限予算内なので、無限稼働は保証しない。

> Historical choice; compatibility portions are superseded by Issue #33.

Issue #3の追記を採用し、受付記録の正規入口はoperation getへ統合する。status --operationはv0.1.2だけ非推奨aliasとして残し、同じ処理へ正規化してstderrで移行先を案内する。古いwireのstatus wrapperも同じ詳細投影を共有する。次のリリースでaliasを削除する。別pool・表・worker・queue・依存や新error codeは追加しない。読取りの保存済み観測再取得を廃止する契約変更は、このIssue対応と利用者の実装指示に基づく。

macOSのユーザー一時領域優先案を採用する。環境変数TMPDIRを盲信せず、既存libcのconfstrでOSの選択を取得し、canonical化・uid/0700・祖先保護・103byteのsocket長を検証する。XDGを優先し、その後OSユーザー領域、最後に/tmpを使う。再起動cleanupも同じ候補集合で認識し、LinuxだけのflockをmacOSのage清掃保証とは扱わない。

Historical interpretation, superseded by Issue #33 on 2026-10-29: startup
schema upgrades and credential-removal transforms stay only inside removable
migration modules. Normal runtime readers accept the current format only; legacy
index reads, marker inference and old-owner response defaults are removed.
General validation and current witness checks remain, without a release-specific
compatibility promise. Launcher/env/data-directory fallbacks remain absent.

index previewは無課金・有限の候補観測、operation getは結果不明の副作用を重複させない観測として採用を維持する。通常の追加設計は自律判断の委任範囲として進める。Quit後の保存完了までは所有Runが短時間動き得ること、Unix PGID再利用の検査/送信間競合を原子的には排除できないことは、対象設計と利用ガイドに限界として明記する。

metadataだけが毎回変わるproxyでも、差分監査nodeには受信本文・完了item・最終responseを保存する。本文保全を優先して現契約を維持するが、頻度とDB増分の実測は未実施である。64MiBは受信stream上限で、再serializeや複数表への保存を含むDB増分の上限ではなく、常に約2倍とも断定しない。論理容量を圧迫する場合は、本文/原文到達性を維持する監査表現を別途検討する。会社proxyの最終JSON差分報告を、reasoning item再送拒否が再現した証拠にはしない。他provider・社内環境は未検証として公開する。

65,536件の履歴探索と128件batchは内部の安全上限で、最適値や処理時間の保証ではない。上限到達やDB占有が問題になる場合に対象を絞って再測定する。u64 ms最大値のtimer overflowは過去のmacOS試験で再現せず、推測だけでclampを加えない。他OSや時間境界で再現した場合に再評価する。過去のcheckpoint標本は読取64件のPASSIVE呼出65→0、decoder依存だけのdebug最適化標本は抜粋4.444秒→75.7ms・release34.4msだった。これは古いfixture/負荷条件の記録で、現sourceの性能保証や非退行測定とはしない。現時点の限定測定と環境は [検証状況](../validation/acceptance.md) に記録する。過去の添付timeout/owner lock競合の原因未確定と、fixture安定化や後続の成功は別の主張として保持する。

今回の増分はschema9のcovering index2本、既存app_metaのspaceごとのquery fingerprint、同じreceipt表の停止余白/期限切れmarker、既存listener当たりLinux directory FD一つ、およびUIの有限対応表/revisionである。新しい永続entity・独立worker・queue・汎用job engine・依存は増やさない。EngineEvent::Noticeの内部Rust payloadは文面から事実へ変わり、既存Rust adapterへの影響はある。公開wire版を変えていないことを全API不変とは言わない。既存body/metadata上限とowner終了時の回収は対象設計で維持する。

判断レビュー追記の利用者条件に従い、Quit開始後は新しいtool実行を開始しない。既存処理の保存と終了待ち、強制終了の別経路を保ち、Quit開始から新規spawnまでの受付境界を回帰で検証する。PGID再利用の残余競合は今回の制限として維持するが、将来shell監視を作り直す際は先頭プロセスをcleanupまで回収しないwaitid/WNOWAIT方式を採ることを再確認条件とする。nohup/setsid等で所有追跡外へ切り離した処理の完全回収・デーモン化維持は今回に含めず、OSごとの所有機構を含む別の設計対象とする。

追補レビューのLowは、容量拒否hintを圧迫回収実施済み・pending/保護/ID metadataの観測・必要時の明示再起動へ更新する。保持時間はRESULT_RETENTION_SECONDSからSQL修飾子も生成し、status表示と別々の24時間定義を持たない。墓標の増加量は仮定を置いた計算例として公開し、固定稼働期間の保証とはしない。

## HTTP期限の共通化（Issue #8）

接続後に無期限で残るembeddingをHTTP層で制限する。全呼出しでApiEndpoint別の同じ小さな期限helperを使い、呼出し側のRAG90秒・モデル一覧20秒wrapperは撤去する。Responsesは全体を切らず受信停滞600秒、embeddingは全体60秒、モデル一覧は同設定と20秒の短い方を採用した。生成の誤中断は料金・時間・受信本文の保全に影響するため長め、前面で待つ検索は有限とし、接続ごとの設定で変更可能にする。接続確立30秒は現契約を維持する。時計範囲外の加算はconfig_invalidとして拒否し、0以外の値を暗黙に無期限へしない。

2026-10-27に指定のローカルproxyへ合成入力・high effortで1要求を送ったが、HTTP400（header待機約0.893秒）で拒否され正常streamの無通信時間は測定できなかった。600秒は安全性を実測確認した値ではなく、変更可能な暫定既定値である。proxyのhigh設定が正常受理された時点で、複数回の受信間隔と十分な余裕を再確認する。本文・鍵・利用者DBは測定記録へ保存しない。この1要求からproviderの遅延上限を推定しない。

期限切れはkind timeout、header/body段階・観測ms・設定keyをローカル生成し、HTTPエラーの既知statusも保持する。401等の本文が止まっても認証/creditのpause判断をtransientだけへ変えない。索引のretry/pauseを再利用し、検索は呼出し元切断とowner停止でHTTPをdropする。Control観測の外側には既に切断取消しがあり、これを失われていたと断定しない。IndexServiceの直接呼出しにも所有tokenを渡す形へ揃え、実IPC切断の回帰で既存の保護も維持する。会話の部分出力は既存AttemptFailure/復旧待ちへ保全する。未受信時の自動retryは別Issue #7として分離し、今回暗黙に料金が増える再送は追加しない。

受信の時刻だけを同じ有限channelの型付き通知で渡し、保存状態やprovider JSONへ追加しない。通知飽和時はmodel eventに添えた元の単調受信時刻で補完し、UIへの追加通知は最大1回/秒に抑える。表示は1秒未満の観測差を許す。UIの既存時計で30秒閾値を投影し、DB周期照会・新worker・新table・新queue・依存追加は行わない。

## Windows CIの起動取消し試験の切分け

公開commit211750eのCI40420705214はWindowsの起動取消し試験1件だけがSaveFailed / SQLite database is lockedで失敗した。同じ製品sourceの先行CI40418072576は成功しており、失敗commitの変更は文書だけである。旧試験は別SQLite接続でwrite lockを保持し、Database通知を受けてから解除していたが、通知はDB worker受付前、watchは観測用で、製品のbusy timeoutは2秒である。試験側に2.3秒の解除遅延を一時的に加えるとmacOSでも同じ失敗を再現した。Windowsで実際にどのscheduler待ちが何秒生じたかまでは断定しないが、この試験は正当な有限競合失敗を誤って不合格にする。

busy timeout延長や無制限retryは採用しない。既存の起動進捗callbackをprivate helperへ通し、同じDatabase通知の呼出しで取消すことで、移行完了・DB close後の返却・endpoint未公開を検証する。実際に受付済みwriterが呼出し元drop後もlockを持つ条件は既存storageのchannel同期試験で維持する。外部writerを取り除いてscheduler依存だけを排除し、成功条件や公開契約を弱めない。globalな試験registryや本番用の待機hookは追加しない。

## 未受信時の自動再試行（Issue #7）

利用者の採用指示により、#8の有限HTTP期限と統合した会話Driver再試行を採用する。HTTP層は事実（受信byte有無、対象HTTP status、Retry-After）だけを渡し、料金を伴う再要求の回数・永続証跡・取消しは既存Driver/attemptで管理する。何も表示していなくてもmetadata/commentを受け取った場合は再送しない。HTTPエラー本文はモデル出力とは区別する。timeoutは受信前でも対象外とし、#8の停滞保全/明示retryを維持する。

予算は要求ごとに復活させずTurn/generationあたり最大2回（設定最大5）とする。context圧縮の2段階予算とは独立し、成功したtool要求の後も同じ予算を共有する。待機は指数1秒から、Retry-Afterは秒/HTTP date対応、既定上限30秒。取消し可能なsleepのみで専用schedulerは追加しない。既存failed attemptのerror_jsonとChanged通知を利用し、表示のための新しい状態・表・eventを追加しない。上流処理済みによる重複課金は許容される残余リスクとして保存する。要約・索引・独立RAGへ再送を拡張しない。

automatic_retryのnumberは保存後の待機を含む予約回数であり、HTTPが送られた回数の証明にはしない。待機中に停止すると次のattemptは作成しない。実要求の追跡は後続のmodel_attemptと診断記録を併せて確認する。

上限到達時のTUI復旧理由では、元ModelErrorと回数の接尾辞を別々に保存する。Frontendは同generationのincomplete attemptの型付き回数と、構成した診断文全体の完全一致が成立する場合だけ「モデル要求」と表示する。単なるprovider接頭辞の推測へ緩めず、欠落/不正/不一致/古い記録は従来の段階不明表示を保ち、保存JSONは変更しない。全体check後に見つかったこの表示投影だけの補正は、対象回帰と最終Clippy/fmtで別に検証し、先行実装の全体成功と区別する。

## 解決済み受信記録の注意表示（Issue #6）

受信記録そのものと現在の対処要否を分け、元Turnのrecovery_waitだけを注意色の根拠にする。後の別Turnが完了したことを古い失敗の解決とみなさず、各sourceの元Turnを参照する。途中本文は確定回答へ変更せず、原文・同Turn対応・再送対象外を維持する。現在のTurn状態は既存DB workerで読み、Frontend/既存UiUpdateで色を投影する。UIがanchor保持のため古い本文windowを残す場合も表示分類は更新する。

一覧は同Turn割当receiptの最初の依頼を有限抜粋として使い、無ければ不明を示す。続く追加指示すべてを1行に列挙せず、Turn短縮IDを補助に残す。通常のRust子moduleへ一覧SQL/投影を分けるだけで、汎用一覧frameworkや新しい永続状態は足さない。

## 2026-10-27: EOFの監査をtransport推測より先に行う

- 判断: BOM/CR単独で受信済みcompletedを取りこぼす自前SSE decoderを修正する。空行未到達・DONEのみ・JSON type欠落を自動成功扱いにはしない。
- 理由: 修正前テストでBOMは実報告と同文言、CR単独はpartial EOFを再現した。HTTP受信順序にEOF追い越しは見つからず、Windows専用decoderも存在しない。BOM/CRは仕様上有効だが、会社Windowsの実wireが未観測なので頻発原因の確定とは分ける。
- 調査性: 受信/解析/残留byte数と固定分類を既存errorに記録し、実データを新たに自動ログしない。Coreの状態・保存schemaや完成条件を緩めず、小さなtransport修正に限定する。
- 再確認条件: 修正版でも頻発する場合、完了を示す受信データがどの形式で到着したかを診断結果から絞り、SSE event名や非標準終端の扱いを別途判断する。

## 2026-10-27: Ctrl+Lは同一viewportの再描画へ集約する（Issue #9）

- 判断: UIのbool予約一つで重複をまとめ、既存FPS制限内でfullscreen Terminal::resizeを同じareaへ呼ぶ。下書きやpopupを変更せず、Coreへ操作を送らない。
- 理由: 初回の実PTY試験で、固定Ratatui版のTerminal::clearがget_cursor_positionのDSR応答を待つと確認した。実際のサイズ変更と同じ内部clear_viewportを使えば、端末への往復照会を増やさず、画面と差分bufferとリンクmetadataを無効化できる。
- 影響: 選択固定中も手動再描画だけを許可する。送信/改行へのCtrl+L設定は優先し、ヘルプで無効理由を示す。周期的repaintや文字幅方針の変更は含めない。
- 再確認条件: Ratatui更新時にclear/resizeのcursor照会・buffer無効化契約を見直す。Windows実端末での表示ずれ原因は別途確認する。

## 2026-10-27: 環境別の起動案内は実装と確認範囲を分ける（Issue #4）

- 判断: 端末・起動元shell・モデル用shellを分け、Windows/PowerShell、Windows/Git Bash、mintty、WSL、macOS/Linuxの構成と設定例をガイドへ追加する。モデル用PATHはcurrent_exe親を自動追加する実装へ記述を合わせる。
- 理由: 提案のmintty一律非対応やWSLからWindows版起動不可能という一般化は採用しない。mintty公式はConPTY対応を説明しており、Windows/WSL相互運用自体と製品が採用している構成は別である。未確認を保証/非対応と取り違えず、まずWindows Terminal等からWindows版、WSL内はLinux版へ案内する。
- 検証: macOSの実binary/PTYとloopback mockで、親PATHをOS標準directoryだけにして絶対パス起動し、モデルのBashが短い名前で同じCLIのversion/docsを実行できた。Windows/Git Bash/pwsh/mintty/WSL実機へ結果を読み替えない。
- 影響: ドキュメントだけを変更し、shell adapter、PATH優先順位、保存schema、依存は変更しない。Windows実機での組合せごとの受入は環境が得られた時点で別途記録する。


## 2026-10-27: provider固有の項目IDは送信用copyから除く（Issue #10）

- 判断: replay adapterを2へ上げ、message（type省略のrole形式を含む）とfunction_callの直下idを共通変換で外す。IDのprefixを判定する方式にはしない。call_id、phase、status、未知フィールド、reasoningは変更しない。
- 理由: 接続先で有効なitem ID形式が異なり、特定prefixだけの許可/拒否ではproxy変換に追従できない。toolの対応はcall_idで維持する。元JSONをDBへ残し、送信copyのみに適用すれば監査と再送を両立できる。OpenAI Docsの[function calling](https://developers.openai.com/api/docs/guides/function-calling)もcall_idで結果を関連付け、reasoning項目の再送を案内している。
- 影響: 通常履歴・item wrapper・spill内outputへ同じ変換を適用する。spillの保存時点ではIDを除去せず、古いspillを含めAPIへ送る際に正規化する。新しいschema、設定、worker、自動HTTP400再試行は追加しない。
- 境界: 既存のowner再起動は復旧待ちを含む旧Runを停止するため、停止済みRunへの/retryを新たに許可しない。再オープンした同じThreadで新しい依頼を送る場合と、生存owner内の復旧待ちへの明示/retryを確認する。
- 再確認条件: reasoningのID/暗号化本文に別provider互換問題が確認された場合は、そのprovider入力契約とepochを別途検討する。今回のmessage/function_callの処置をreasoningへ拡張しない。


## 2026-10-27: 長時間の会話を連続失敗予算で再試行する（Issue #11）

- 判断: Issue #7の「Turn全体の予算」「metadata/timeoutは対象外」を更新する。検証済み最終応答の採用後に連続失敗回数を0へ戻し、既定8回、指数待機上限60秒とする。正常終端前のEOF/transport/停滞timeoutを対象にし、生成出力ありでも既定で再試行する。response_retry_partial=falseで後者を止められる。
- 理由: 数時間のtoolループで独立した一時失敗が累積し、前半の失敗によって後の復旧機会がなくなるため。出力のある途中応答も、未採用toolは実行されず、同じ応答の副作用を増やさずに作り直せる。ただし上流料金と生成時間は重複し得るため、証跡・表示・optoutを備える。OpenAI Docsの[streaming events](https://developers.openai.com/api/docs/guides/streaming-responses)に照合し、生存通知と生成出力を区別する。
- 影響: 失敗attemptと部分受信監査を同じtransactionで保存し、監査本文を次のモデル入力へ再送しない。通常の採用/CAS/停止・GC境界は維持する。Retry-Afterは600秒まで指定どおり待ち、超過は復旧へ進める。指数上限で切り詰めて早期再送しない。要約/embedding/RAGの再試行、DB schema、依存、worker/queueは増やさない。
- 再確認条件: 実proxyで連続的に復旧できない場合、固定予算と待機の実測を見直す。出力を持つ新eventや別providerのterminal契約は明示的に確認し、任意protocol失敗を通信一時失敗へ広げない。

## 2026-10-27: 復旧のベルには既存Changedのcommit後の事実を使う（Issue #12）

- 判断: 既存Changedに実際の復旧遷移を示すboolを付ける。UIのpending boolと単調時刻だけで2秒集約し、描画と同じ端末backendからBELを出す。
- 理由: 選択Threadのsnapshotだけでは背景/子の復旧と速い再開を見逃す。全Threadの永続状態監視・無限の重複排除集合・専用worker/queueは増やさず、既存の復旧CASのcommit後に通知する。
- 影響: 永続状態・wire・event種別は増やさない。既存broadcastの過負荷欠落、端末の音/点滅の無効化は到達保証の対象外とする。起動済み状態・停止・自動再試行待機では鳴らさない。
- 再確認条件: 通知の永続配達が必要になった際は、今回のbest effortと区別して目的と費用を再検討する。


## 2026-10-27: HTTP層は事実を返しDriverへ再試行判定を集める（Issue #13）

- 判断: AttemptFailureの再送許可を受信事実ResponseFactsへ置き換え、対象判定をDriverのclassify_retryへ集約する。期限・有界JSON本文・Responses受信を通常のRust子moduleへ分ける。
- 理由: Issue #11の動作を維持しつつ、EOF・HTTPエラー・transportごとの読取経路に再試行ポリシーを重複させない。完全なJSON responseの検証失敗と未終端EOFを別の事実として扱い、ModelError.kindの永続形式を変更しない。
- 影響: 新しいworker/queue/依存/永続状態を追加しない。分類関数はDriverとResponseStreamで共有し、Deadlineは時間だけを持つ。HTTP methodとJSON本文の共通手順を抽出する。clientの再利用という挙動変更は別commitに分ける。
- 再確認条件: 新event形式や別のHTTP要求を足す場合、受信の事実とポリシーを混同せず、同じ表形式の判定試験と既存の期限/原文保全回帰を通す。


## 2026-10-27: ownerの接続別HTTP clientを共有する（Issue #14）

- 判断: Control配下のStoreが、設定接続数に固定されたHttpClients表を構築し、会話・要約・索引・意味検索・モデル一覧へ共有する。各entryはclient又は初期化エラーで、mutex/動的登録/worker/queueを増やさない。独立テスト/RAGの従来constructorは維持する。
- 理由: reqwestの内部poolを要求ごとに捨てず、TCP/TLS初期化の繰返しを減らせるため。静的な接続設定と、要求ごとに読む資格情報を分ける。起動時に全体を失敗させず、使わない接続のbuilder失敗も要求時のエラーとして保持する。
- 影響: pool_idle_timeoutは45秒（提案30〜60秒の中間）を初期値とし、実会社proxyの最適値とはしない。API key/headerは送信ごと、HTTP(S) proxy/no-proxyはclient構築時なので後者の変更にはowner再起動が必要。SSEのterminal時点終了を維持し、再利用できない接続のためのdrainを追加しない。再試行は#11のDriverのみが判断する。
- 再確認条件: 会社proxyで古い接続の失敗や接続再利用率を実測できたらidle値を見直す。小さな実proxy標本から速度向上や原因解消を断定しない。全profile数/同時要求数は既存設定と所有に従い、pool保持をプロセスRSS上限とは扱わない。


## 2026-10-27: Issue #15と追加レビューの採用判断

- 会話と要約のThread UUIDは要求引数として渡す。共有clientにsessionを固定すると子Thread・forkが混同されるため、設定とpoolは共有し識別情報だけを要求単位で付与する。header既定true、prompt_cache_key既定falseで接続先依存の本文拡張をopt-inにする。同名固定headerは自動付与時だけ上書きし、opt-outで明示headerを消さない。
- 保存済みusageは同Turn最新attemptで推測せず、output_itemのnode関係から正確なattemptを参照する。既存DB workerの有限page読取だけを使い、schema・永続event・workerは追加しない。値0と未報告・不正・対応不明を区別し、UI付加情報を原文へ書き戻さない。
- #4/#9/#10のLowは文書と固定中再描画試験へ反映する。Windows端末の利用者報告は受入成功へ読み替えず、WSLENVと再起動後の新規依頼の条件を明記する。
- #11の再試行対象HTTP statusはエラー本文がtimeout/切断しても失わせない。認証・非対象status・終端のprovider失敗は引き続き自動再送しない。推論項目開始の部分出力扱いと、既定無通信例で約93分になることをガイドへ明記する。生成全体期限としては扱わない。
- #13の共通要求文脈を局所型へまとめる提案を採用する。引数の重複とpanic前提を除き、再試行方針をHTTP層へ戻さない。
- #14のSSE terminal即終了はHTTP/1.1接続をpoolへ返せない場合がある。正常terminalの検証後だけ最大1秒・64KiBの本文読捨てを採用し、終端境界と取消しを維持する。末尾エラーは完成済み応答を無効にせず、上限到達で接続再利用を諦める。過去のdrain不採用判断はこの条件付き判断へ更新する。会社proxyでの最適値は未測定で、再確認条件は遅延・接続再利用率の実測報告。
- キャッシュによる切断低減や全provider共通のroutingは保証しない。実API確認はローカルLiteLLMへの合成入力4要求に限定し、15万tokenの負荷や会社の実会話は使わない。Windows実機なしの端末操作は対象外のまま、3 OS CIを別に確認して公開する。


## 2026-10-27: Windows CIのcheckpoint観測競合

v0.1.4候補の初回CIで、Linux/macOS成功に対しWindowsの既存idle checkpoint試験が1件timeoutしました。DB workerは返信を送ってからidle処理へ進むため、別jobでtracerを登録すると更新直後のcheckpointを観測できない場合がある。またcheckpointはperiodic workerではなくjob後のidle opportunityなので、counterを待つだけでは再試行を駆動できない。tracer登録と更新を一つのfixture jobにし、意図的にeligibleな更新境界を観測する。待機helperはread jobで有限に機会を作り、callback後はactor barrierで同期する。timeoutだけを伸ばして不明な状態を隠さず、expected/actual counterを診断へ残す。製品の保存・throttle・schemaは変更しない。


## 2026-10-27: frontend fixtureの終了待ちとWindows CIの負荷

次のWindows CIでcheckpoint観測は成功し、既存management_scope試験のcleanupが10秒でtimeoutした。mutationの永続化と遅着拒否は成功済みで、終了猶予5秒は完了SLAではない。多数のfixtureの同時実行でWindows lib全体が100.72秒まで伸びた状況と整合するが、遅延原因の全てを断定しない。12か所のcleanupを直接JoinHandle awaitと5msの有限更新排出へ共通化し、busy-pollを避ける。watchdogはWindows30秒/その他15秒で失敗診断に最終ShutdownProgressを残す。forceによる成功偽装や製品shutdownの変更は行わず、猶予・forceの明示試験は保持する。

Windows CIのRust harnessは同時2件へ制限し、独立DB/IPC/file worker fixtureの同時起動負荷を抑える。除外・ignore追加ではなく全件を実行し、個々の試験内で作るTokio並行処理は維持する。再確認条件は同条件での継続的なtimeoutや最終進捗診断で、製品deadlockをfixture対策と混同しない。


## 2026-10-28: 範囲編集と切断後の縮小（Issue #16〜#18）

- #16の構造移動は`5ee28ba`へ分離し、旧手動/overflow規則と新規則を混ぜない。更新コメントの「途中範囲の省略」も取り込み、prefix専用の新状態を作らず共通omit部品で扱う。新規table/worker/queue/event/依存は不要。引数は既存receiptに付随させ、既存pending保護と回収を使う。
- 切断と大きさの関係は相関であり、会社proxyの約100秒上限は未確認。未指定の起動目安を75,000推定tokens、目標をその時点の半分とする。明示compact_hint_tokensは起動目安と目標の両方。75kは報告標本に合わせた暫定値で、context上限や全providerの最適値ではない。attemptの縮小記録と次の成功/失敗で効果を測り、別provider/多言語の精度や失敗率で見直す。
- 不十分な縮小を残さないため、新自動縮小のspill/omitは計画後に原子的に採用する。旧manual compactのspill/summary別採用は維持する。現在の依頼が途中にあればそのgroupを残し、markerに原順序と保持anchorを記録する。明示omitは範囲中の依頼を黙って除外せず拒否する。
- restore後もthread_replacementを監査/GC rootとして保持する。添付は自動復活させない。外部ファイルの現在状態が変わっている可能性があり、文脈の復元とファイル送信の再許可は別の判断であるため。通知・CLI結果・TUIに注意を残し、必要なら新しい添付IDで追加する。
- 隠れた範囲のsummaryは最初は一つのmarkerの元全体に限定する。部分省略の再分割は別markerの作り直しとanchor管理を増やすため、正確な候補を示して拒否する。需要が確認されたら既存出典/CASを維持して検討する。終点巻き戻し、focus引数、モデルの自作要約採用は今回の対象外。
- TUIの`/restore 番号`は失敗下書き復元に既存利用があるため維持し、引数なしだけ入力範囲復元に使う。特定markerはCLIで明示する。補完とhelpで二つの対象を明記する。

- 入力推定はoutbound replay変換と共有し、provenanceや内部event metadata、旧epoch reasoningを数えない。full inputとhistoryの実測文字数差で固定分を分け、同接続・モデルの直近64成功attemptから補正可能な最新値を使う。補正根拠と未測定対象を返し、部分的な推定を実測tokensとして表示しない。大きい添付の除外後は固定分の推定が過大になり得るため、縮小の採用可否は保守的になる。
- TUI概要は同Turn/generationの直近256 attemptの数値投影に限る。常時監視・新しい状態/表を増やさず、長いtool-loopで範囲外になった場合は未取得として扱う。markerと元attemptの保存情報は保持する。

- 最終レビューで2点を修正する。引数なしcompactの連続受付は従来どおり一度の処理へまとめ、明示編集は必ず独立実行する（明示編集を越えてまとめない）。余計な要約API要求を増やさず、旧downstreamを維持するため。引数なしrestoreは経路の後方ではなく、選択中で最後に作成されたmarkerを選ぶ。途中範囲を後方→前方の順に省略したときも直前の編集を戻せるため。nodeの既存挿入順を参照し、専用履歴stackや状態を追加しない。


## 2026-10-28: Issue #16〜#18のレビュー対応

- #16のコミット再分割は行わない。純粋な構造移動は`5ee28ba`で既に分離済みで、共通validator・Snapshot採用fence・後続2機能とその試験は`e8153f6`で一体として検証した。レビュー記録・Issueコメント・検証結果がこのhashを参照しているため、その対応を保持する。共通検証を先行commitへ抽出して履歴を書き換えるより、レビュー修正を別commitにし、今回の境界共通化と後続の差分を追跡できる形を採る。これは#16レビューMediumが認める「分けない理由の記録」で解消する判断。
- #16 Lowの最新group/現在依頼の保護規則は、計画と検証が同じ`trim_group_protected`を使う。#18 Lowの端点変換失敗でも、omit/trimの候補はそのmodeで実行できるものだけを返す。
- #17の通知には、保持した現在依頼より後ろの省略node数を明示する。位置に印を増やす代わりに、同じmarkerへ作業途中も省略した事実と副作用未取消し・再実行前の現状確認を記す。新しいnode/stateを増やさず、復元の順序を維持する。
- #17の旧attemptの文字数不足は、未計測として扱い続ける。過去のprovider入力を現在の経路から推測復元したり、tokenizer/言語判定/移行処理を追加したりしない。日本語中心の旧会話は文字数/4で過小評価し得るため、最新の実測input_tokensも確認し、必要なら明示compactまたは小さいcompact_hint_tokensを設定する。新しい成功要求の計測が揃ってから補正へ移る。旧会話の推定を正確と主張せず、実proxyでの効果は引き続き未実施とする。
- #18 Lowの明示summaryは、反映済みの現在依頼も指定可能な契約を維持する。ユーザーまたはモデルが正確な範囲を選んで意味を要約へ保存する明示操作であり、自動の省略とは区別する。未反映receipt・未完結tool groupの共通保護は維持し、自動compactの現在Turn保護は変更しない。通常は現在依頼を範囲外にするようガイドで案内する。
- 復元の通知は特定markerを指すよう、既存`--from`に省略元の最初のIDも受け付ける。選択marker IDを優先し、元IDの複数候補は拒否する。引数なしは最新作成markerのまま。添付警告は実際にactive参照が除外されたときだけ出す。


## 2026-10-28: 一覧からの会話削除（Issue #19）

- DeleteとCtrl+Dを採用する。macOSの通常deleteはBackspaceであり絞り込み編集に既存利用されているため維持し、fn+deleteかCtrl+Dを使えるようにする。通常の文字dはフィルター用のまま。
- 既存ThreadDeletePreview/Deleteと確認popupを使う。子の会話も削除する指定は既定falseで、切替え後は最新previewが届くまでYを無効にする。子の存在だけをUIで独自blockにしない。Coreは親だけを削除し子を残せるため、その実際の意味を変えない。
- Core削除結果はAcceptedでID一覧を持たない。Core契約を増やす代わりに、削除成功後、UI既知のIDだけをDB workerで照合して削除済みview/下書きを破棄する。新しい全子孫一覧APIや永続削除jobは不要。UI-localの要求/返答には対象・descendants・要求IDを持たせ、既存runtimeの寿命と有限admissionを使う。
- 削除された現在会話からは新しい下書きへ移る。背景会話では一覧を開いたまま隣行へ移す。確認を閉じても受理済み削除は取消したことにせず、成功のcleanupは適用し、閉じたpopupは再表示しない。保存途中の古い下書きや画像所有が会話を復活させないよう、既存所有/世代とbacklog回収を再利用する。

## 2026-10-28: Issue #19〜#23の対応範囲

- #19は削除確定とFrontend後片付けの結果を分ける。後片付けの失敗でCoreの削除事実を否定せず、確定済みrootと照会できた子だけを整理し、未観測の子を推測しない。UI-localの有限inflight IDで重複操作を防ぐ。historical generation fenceを全て削除観測の候補に足すことはやめ、現在UIが所有する有限候補へ限る。Coreの操作/表/worker/eventは増やさない。
- #20は既定除外とunsupportedのskipped分類を不具合修正として採用する。完全scanで新たに対象外となる旧active資料はremovedへ移すため検索から減り得るが、原本を消す処理ではなく現在の対象定義へ同期するための判断。除外忘れによる復活を避けるためpatternはsourceに保存し、解除は明示する。新しい独立RAG列とatomic migrationだけが必要で、会話schemaは変えない。glob構文を自作せず [globset 0.4.20](https://docs.rs/globset/0.4.20/globset/) を固定追加する。これはstandalone RAGでだけ使う有限pattern照合で、Core機構は増やさない。gitignoreのネスト・否定は今回範囲外。
- #21は明示hybridの準備不足だけを成功したkeywordへのdegraded結果として返す。従来の全てのscope_errorで非0終了という契約を、確認済み未準備warningだけに限って変更する。利用者とモデルが結果を使えることを優先し、scopeごとに使用手段・理由・次の索引操作を示す。本当の実行失敗や設定不正は隠さない。別の人間向け出力modeはJSON統一の方針に反するため採用しない。
- #22の新しい操作結果保存表やCore receipt連携は採用しない。既存shell出力と既存RAGの永続状態で取得・追跡できるため、status投影とガイドを補う。最終走査時刻と内容更新時刻は区別し、embedding作成時刻の新列は作らず現在設定の不足件数を示す。
- #23は提案1と3を採用し、2のJSON重複除去は今回は見送る。共有nodeの生成元context.thread_idとfork先selection.thread_idは同じとは限らず、ExecutionContextとPageは他の公開操作/構造表示にも利用されている。一覧だけの派生型と後方互換投影を増やすより、既存形を保ち抜粋制限とSQL長さだけを追加する。0指定は本文の転送/解析を避けるが、provenanceや実行リンクが残るので16KiB/50件を保証しない。実測と意味の異なる長さの説明をガイドへ残す。

## 2026-10-28: 0.1.6へ対象拡大（Issue #24〜#30）

- 利用者が#19〜#30完了、push、0.1.6公開、常用binary更新をgoalとして明示。初期5件を検証済みcommitへ分け、#24は#20の対象選択へ重ねて内部形式変換を削除する。外部変換の主経路とモデル解釈の代替経路を分け、原本hashと生成側申告を混同しない。原本の大きさは保存量ではないため固定bufferの逐次hashで16MiB制限を外し、保存する本文/metadataの上限は保持する。
- #25〜#30のレビューの推定値を性能の証拠とは扱わず、同じ合成fixtureで変更前/後を測定する。#25/#26（経路と索引）、#27/#28（読取り隔離と通知）、#29/#30（描画と配送）を並行準備し、Cargo/Gitはrootで直列管理する。新しい表・汎用pool・worker engineを作らず、追加する有限admissionや通知の所有/終了を各設計へ記録する。
- #26コメントの未起票項目（全DB使用量counter、retention/VACUUM、履歴keyword FTS、下書き/一覧全体の再設計）は今回の範囲へ広げない。必要な索引はEXPLAINと測定に根拠があるものだけ採用する。

## 2026-10-29: #24〜#30の採用境界と最終検証

- #25は既存node採用の末尾追加だけを共通化し、任意経路の置換APIを増やさない。revision/CAS、末尾とcursorの整合、順序、既採用nodeのno-opを維持する。既存edgeのINSERT前triggerを再実行しないSQLにし、新規cycleの拒否は保持する。
- #26は会話DBの3索引と独立RAGの2索引を採用する。実際のEXPLAINと同一fixtureの比較を根拠にし、app_metaの不正JSONに対する認可の意味を変える部分索引は採用しない。schemaは会話10/RAG3で、起動前バックアップと旧版への直接復帰不可を公開時にも明記する。
- #27はIndexService所有の1検索slotと既存runtimeのreadonly blocking処理に限定する。取消し後も実処理終了までpermitを保持し、owner終了でdrainする。ランキング後に新しいsnapshotで出典/可視性を確認するため、検索自体の高速化や長期snapshot保証とは扱わない。GCの集計分離は同じtransactionの回収判定と一緒に別途検討する。
- #28は出力Notifyをownerに1個追加する。終了待ちをraw出力ごとに起こす負荷を除き、ShellOutput待ちのexecution別分配mapや専用workerは作らない。
- #29は既存16MiBのlayout予算内でrender unitを共有する。高さ変更は再描画のcache keyにしない。全可視nodeのresize取得と、本文window再取得のpreflightを保持する。最終試験が検出したwindow再読込の退行は期待値を緩めず修正した。debug測定のp95は約52msであり、常時30fpsの保証とはしない。
- #30はfrontend所有の有限outboxとUI専用の有限broadcastを採用する。通常受付を抑制する閾値と既受理結果の予備枠を設け、重要な結果をcoalesceしない。例外的な上限ではbackpressureを残し、停止/終了のpriorityと強制終了を維持する。詳細な所有・上限・切断・遅着・終了は採用設計へ記録した。
- 性能はdebug build・合成DB・loopback HTTPで測る。会社proxyや外部モデル、Windows端末の体感を推測しない。3 OS native CIと配布物の実取得はローカル検証から分離して記録する。

## 2026-10-29: 配布runnerの更新と既存tagの再ビルド

- 0.1.6の3 OS CIはすべて成功したが、配布用macos-14 jobが10分以上queuedになった。GitHubの公式告知 [runner-images #13518](https://github.com/actions/runner-images/issues/13518) で、2026-11-02の廃止と、それに先立つ待ち時間増加を確認した。待機の唯一の原因とは断定しないが、4日後に使えなくなるrunnerを維持しない。
- 配布runnerをmacos-15へ更新し、macOS deployment target 14.0を明示する。実行OSを対応最低OSと混同せず、配布物のMach-O load commandも確認する。14実機の動作確認とは扱わない。
- 公開済みtagを書き換えず、既存の安定tagをcheckoutするworkflow_dispatchを追加する。v0.1.6の製品sourceは3 OS CI済み025388cのままで、mainのworkflowだけを直す。tag/version/native hostの照合、全3 archive/checksumが揃ってから公開する規則、公開済みreleaseの上書き拒否を維持する。再実行でもdraftのrelease notesを保持する。

## 2026-10-29: English defaults and Japanese user guides

- Interpret the English default across all product guides, not just `user-guide.md`: sending English readers to Japanese-only feature guides would leave the requested navigation incomplete. Preserve all twelve user-facing originals as `.ja.md`, sharing one English-only Agent Guide. Root README/AGENTS and documentation entry points are English. Historical design/development/verification records are not bulk-translated; new records are English.
- Keep the existing thirteen bundled IDs and three categories, with English text and metadata. Do not add a docs locale API or duplicate every guide in semantic indexing. Japanese copies are source/archive files; mark links as repository references and explain literal English search in both languages. Existing version hashes invalidate old cursors/index readiness normally; no automatic paid rebuild is authorized.
- Recommend English-default UI with explicit Japanese startup selection, preserving one renderer and the current Japanese copy. This needs only presentation-layer resources/settings in a future task. Do not implement it, add an unsupported config field, change stored error/domain values, or force a response language in this task. Live switching, OS locale detection, per-Thread locale, and more languages have no current requirement. The [comparison](../design/documentation-language.md) records costs and later acceptance criteria.
- Translation exposed an existing shell-guide typo: ASCII `SHELLBOY_BG_READ_V1` is 19 bytes, not 15. Correct both languages. Also remove the contradictory claim that CLI has no preview-only command immediately before its `index preview` example; CLI lacks the TUI confirmation screen, not the preview command. Remove the fixed operation total in the User Guide while retaining access to the full catalogue. (Correction from Issue #31: the TUI total of 36 was accurate; removing a duplicated count was a maintenance choice, not correction of a stale value. The CLI count of 37 describes a different catalogue.) Update a previously stale Control-to-HTTP heading link and translation-affected heading links.
- No UI implementation/configuration files, Core API, migration, dependency, model prompt, or user data change. Validate guide fidelity, links, TOML examples, actual offline catalogue, and docs/RAG behavior; do not run paid discovery APIs or claim real-terminal/other-OS verification for a documentation change. Local commits only; the preceding release authorization covered the completed 0.1.6 release, not publishing this new task.


## 2026-10-29: Reopened #19/#20/#27/#30 boundaries

Adopt all four Medium findings against the existing contracts. Confirmed deletion
is independent of cleanup success, but a cleanup write failure still stops the
owner. A per-file extraction failure is independent of complete-scan removal of
other keys. A readonly search failure is independent of writer durability.
Transient UI state and worker results need one ordering boundary.

The [reopened-issue plan](reopened-19-20-27-30.md) records every Low disposition.
Use one shared, finite UI outbox rather than adding a dispatcher task or Core
state. Keep the semantic admission slot across embedding to avoid charging for
queries that cannot be admitted; document parallel-child contention and retries.
Keep incomplete RAG walks conservative for all updates. Preserve Windows legacy
file-key identities where possible without changing POSIX backslash semantics.
Serialize UI deletions rather than querying an unbounded parent/descendant graph.

Defer replay of a live prefix filtered before a queued thread-selection action
is handled: offset gaps and canonical terminal refresh remain the recovery path.
A separate Core replay buffer/handshake adds ownership and retention rules for
transient presentation. Revisit if canonical history cannot be recovered. Force
quit may abandon UI delivery, but must not silently enter another grace or wait
for UI queue capacity after it has already been accepted.


Cross-review also identified inherited exceptional backpressure at all 512
critical entries: one frontend handler can wait before polling priority controls.
Preserve finite FIFO rather than dropping a result or bypassing the shared queue.
Document the responsiveness limitation; removing it needs both a finite main
reserve and incremental batch continuation, not a larger queue. The ordinary
pressure test is not evidence for every exhausted-reserve case. This follow-up
is tracked in the reopened-issue plan and known limitations.


## 2026-10-29: Remove #20 Windows legacy-key compatibility for 0.2.0

Issue #20 comment `6480908316` clarifies that the next release is 0.2.0 and does
not carry forward old-version migration/compatibility. This overrides the initial
Windows-key preservation decision above. Remove the native-key lookup and its
two preservation tests rather than carrying compatibility-only database queries
and identity rules. Keep current slash normalization, failure/removal separation,
root-glob protection and case-sensitive matching unchanged. Legacy key identities are not preserved; ordinary complete synchronization registers
current keys under new identities. Database recreation is unnecessary (clarified
by the subsequent #20 re-review); slash normalization shipped in 0.1.6. Do not
open, reset or convert user data automatically. No version
bump or release is performed as part of this targeted follow-up.


## 2026-10-29: Issue #31 documentation review

Accept all four Medium findings. English literal search needs English discovery
terms in recovery hints, Agent Guide and RAG guidance; Japanese quoted labels
remain searchable without query translation. Keep the system prompt unchanged:
it already routes models to Agent Guide, whose explicit English-search instruction
fixes discovery without a new prompt or language state. Restore the full strength
of Windows unverified-status and cache-invalidation instructions. Restore the
AGENTS requirement to reflect discovered design discrepancies, including existing
ones beyond intentional contract changes, before changing code or fixtures.

Quote the actual current Japanese TUI labels in both languages, with English
explanations, rather than translating navigational targets into nonexistent UI.
A narrow regression compares documented operation labels with the real OpKind
labels; an actual-binary regression follows the InvalidArgument recovery search
and requires useful shell examples. No runtime UI language implementation changes.

Resolve all eight Low findings in this task. Correct the earlier count-removal
rationale: F8 has 36 operations and CLI has 37; the former was accurate. Keep the
User Guide free of a duplicated total for maintenance, without calling it stale.
Add all missing user-guide sibling links, state busy InvalidState and rejection
before charge separately from cancellation, remove change-relative product prose,
restore version/catalogue qualifiers, correct English and zero-budget/expiry
wording. Correct the existing deprecated alias help without changing alias behavior.
Define documentation entry points explicitly and translate the small design and
validation indices; existing design/validation records are not bulk-translated.


## 2026-10-29: Follow-up to reopened-issue re-review

Adopt the small boundary fixes: explicit storage-stop wording, local deletion
unwind-to-uncertain-result handling, unavailable symlink root, readonly-specific
repair/charge hints, and one shared UI Notify. Keep existing workers, permits,
result channel, cleanup ownership and Core contracts. Add the missing complete
frontend cleanup-failure test and actual direct-file removal test. Label the lock
fixture as rollback journal, not WAL. Retain current #31 guide fixes when updating
search and RAG wording. AGENTS now records the requested pre-1.0.0 no-shim policy;
general migration/protocol-version mechanisms are allowed. Existing one-off code
inventory is a separate forthcoming review, not a reason to add more shims now.

Correct the earlier #20 “recreate indexes” phrasing: users need not recreate the
DB. Complete sync deactivates old keys and creates new identities, without
preserving legacy IDs/keys; slash normalization is already in 0.1.6. A persistent
file failure still returns partial_failure/nonzero even with other applied work.

Accept best-effort ForceShutdown presentation when explicitly forcing termination:
nonzero forced result/error remains reliable, while waiting to guarantee the UI
marker would break the one-request force contract. Create Issue #32 for exceptional
full-reserve priority responsiveness. This remains separate from the Notify wakeup
fix; neither ordinary pressure tests nor FIFO exhaustion tests prove its solution.


## 2026-10-29: latest agreements and #32 / #33 supersede compatibility choices

Adopt the [complete tracking plan](issue-32-33-plan.md). This supersedes earlier
entries preserving pre-G5 keys, inferred query markers, deprecated status aliases,
and runtime old-owner optional fields. Those entries describe earlier decisions,
not the current contract. Keep the general schema runner, immutable historical
SQL, ledger/user_version/sqlite-vec checks and strict protocol validation. Isolate
all startup transforms in removable migration modules; initialize fresh databases
from the current schema independently. Old readonly schemas are rejected without
conversion. Conversation schema stays 10, RAG stays 3; wire protocol becomes 2.

Only User Guide Japanese is maintained. The earlier twelve-pair language scope
and prescribed English search queries are superseded by the full #31 comment
agreements. Catalogue language and query-language choice are separate concerns.
UI language work remains an evaluated future task. The intended next release is
0.2.0; source package version stays at the last release until release preparation.
No push/release or user-data mutation is part of this work.


## 2026-10-29: review round 4 persistence and current-format upgrade

The [round-4 tracking record](review-round-4.md) supersedes remaining pre-upgrade
assumptions in the earlier #32/#33 plan. The new reviews confirm #19/#20/#27/#30/
#31 fixes; no change is justified solely to rewrite already accepted behavior.

Use schema 11 for the derived failure-counter format boundary. A startup-only
migration removes malformed/non-object/wrongly typed counters, preserves current
objects and existing pauses, and drops the unused receipt_turn_source index.
Fresh schema and historical upgrades must converge. This is preferable to a Core
scalar fallback: public v0.1.x wrote integer reset values while remaining schema
10, so accepting that schema without normalizing derived metadata stranded work.
No content, vectors, authorizations or immutable interruption nodes are rewritten.
Current malformed counters still fail strict decoding; a separate transaction
preserves the provider failure and fixed input, marks work failed and pauses
spending. General worker failures also acquire an existing durable pause; if even
that diagnostic cannot be saved, stop the index workers rather than keep billing.
If both normal and diagnostic DB writes fail, preservation of the original
provider failure is not guaranteed. Avoid adding another diagnostic queue/service
for that exceptional storage-failure boundary; revisit if concrete operational
failures show the existing error/pause reporting is insufficient.

Keep immutable versioned SQL in conventional root migrations/. Only the isolated
storage::migrate imports historical SQL in src; ordinary Core tests use current
schema. Explicit migration integration tests retain old SQL intentionally. Moving
all SQL under a custom module directory adds path churn without improving runtime
isolation. Removing the runner in future requires removing its declaration/call;
obsolete SQL can then be removed alongside the explicit migration fixtures.

The metadata audit finds no further required startup conversion: public Work
writers already supplied the now-required fields and public draft writers supplied
positions. Invalid versioned GC cursors restart safely. A missing query-profile
marker cannot be inferred without configured endpoint identity and remains an
explicit pre-HTTP error. Saved mutation receipts remain scoped to their original
owner instance, so changed draft fingerprints do not authorize old-instance replay.
The old interruption payload key remains an intentional display-format break;
known limitations describe raw/F6 access rather than a fallback reader.

For #32, persist accepted closing sends and drafts before presentation-only
shutdown waits. Omit closing Accepting projection: after Quit the main loop no
longer handles its completion, and this transient label must not gate persistence.
Normal sends retain immediate Accepting. Keep autosave pumping independent of UI
reserve; retain only its failed-thread projection while snapshot ownership stays
in the existing backlog. A repeat bit on existing pending receipts preserves a
fair scan before starting a finite follow-up pass, including lag recovery. Clone
always yields worker admission; shutdown borrows the unique main handle. No new
Core operation, table, channel, task service, dependency or background poller.

Use rag init as the explicit writer-owned upgrade for an existing RAG store;
it does not register a source, index documents or call a model API. Protocol
mismatch reports both versions and same-release recovery; only endpoint/instance
change recommends refreshing/reconnecting. Preserve uncertainty after a request
was sent. Earlier guide old-record notes are replaced with actual unknown-state
conditions. The pre-1.0 policy permits isolated schema upgrades and forbids Core
compatibility shims, rather than forbidding migration itself.

Validation fixture decisions: stalled mock HTTP was also reproduced on baseline
`e977dfe`, before this follow-up. In the index fixture, the seed request stalled
before the migration/workers under test, with a small queued server response.
Use TCP_NODELAY on accepted local mock sockets and write JSON response headers
and body together in the index/shell fixtures. Apply TCP_NODELAY to the two
summary fixtures as well. Keep assertions, deadlines and production HTTP clients
unchanged. The index worker regressions pass with these fixture changes. The later #33 review supplied independent raw TCP tests proving a roughly
16KB loopback transfer stall and recovery after Mac restart. Root repeated
successful transfers up to 10MB after restart and the exact 6c577c8 standard
check/HTTP-PTY now pass. The original packet-timing explanation for 8368874 is
not established; compare the unchanged baseline without those fixture changes. Record unsuccessful
whole-suite/manual attempts rather than treating earlier runs as evidence for
the current source.

The saturated shutdown test sends forty messages to forty independent existing
Threads held in recovery_wait. This isolates UI delivery from valid same-Thread
selection-revision CAS conflicts. Assert exactly forty durable receipt/user-node
pairs and draft revision 99 before forcing the blocked presentation to exit;
do not reduce the expected count to accommodate rejected concurrent requests.

RAG migration assertions use the new actionable rag init hint and preserve the
original source count (including built-in sources). Existing readers must still
reject without modifying the old store. Both v1 and v2 writer-only migration
regressions pass, without registering material sources or indexing.


## 2026-10-29: early maximum reduction and truthful HTTP observations (#34)

A failed large input is reduced at its first eligible failure rather than after
eight more attempts. Reduction removes every eligible group outside existing
protection, not an arbitrary half-size target. Keeping a particular token count
has no established benefit; immutable originals, replacement provenance, scoped
keyword search, selected summaries and explicit restore provide recovery paths.
The latest completed tool group, current request, incomplete work and pending
user instructions remain protected. One adopted reduction/restart belongs to a
Turn/generation; another failure goes to recovery rather than consuming another
paid retry cycle. Only an explicit compact_hint_tokens enables pre-POST reduction;
it is trigger guidance, not capacity inference or a reduction target.

Transport observations must distinguish facts from inference. A Body stream
reports how much source reqwest consumed, not a TCP write acknowledgement or
proof that the peer read/processed it. The public client API does not expose
exact socket reuse. Record known incompleteness, unknown delivery while waiting
for headers, and response-body reception separately. First use of a fresh pool
is a pool fact, not a promise about every later connection. Replacing a failed
pool generation prevents subsequent requests from borrowing its idle sockets;
it cannot promise immediate closure of an HTTP/2 connection still used by other
requests. Keep those limitations explicit rather than add another transport,
dependency, worker or per-connection lifecycle engine.

Issue #11 retry-count reassessment: retain the default of eight consecutive
retries provisionally. The supplied incident demonstrates that retrying the same
large input eight times before trimming is wrong; it does not establish the
optimal retry count for smaller transient failures after this change. No
representative successful production retry dataset is available in this task,
and routine verification must not call paid APIs or inspect real conversation
content. Loopback tests validate retry mechanics and bounded request counts, not
provider recovery rates. Revisit with anonymized attempt number, phase, error
category, header latency and elapsed waiting after these observations have been
deployed. The production-data part of item 7 remains explicitly unmeasured.

The maximum-omission rule is structural: omit all eligible groups, including
when verbose provenance/notification overhead means a tiny removed prefix does
not lower the estimated token count. Report the actual before/after estimates;
do not claim a numeric reduction in that corner case or restore the removed
content automatically. The once-only resumption bound still prevents a loop.
Revisit marker size if measurements show its overhead defeats recovery on
realistic large inputs; do not reintroduce an arbitrary target/all-or-nothing
planner to address a synthetic tiny-prefix case.

A failed shared HTTP pool generation gains only two bounded counters and its
existing client owner; the lock is held for snapshot/replacement, never across
await. Retired clients are retained only by already in-flight requests, bounded
by existing request concurrency, not a new registry or background reaper. Proxy
environment variables are read from the owner process when a pool is built or
rebuilt. Editing a different shell's environment still cannot reconfigure a
running owner; the documented practical procedure remains owner restart.

Additional #32/#33 review decisions: stop pumping a known-closed draft worker
after its first critical notice while retaining newest unsaved snapshots for
shutdown failure reporting. Reset receipt repeat bits only at a fresh scan, not
during an unfinished pass. Expose a bounded in-memory index storage-stop reason
when the diagnostic itself cannot persist, and report actual RAG migration
versions. Keep the already conservative uncertain reply-ID diagnostic and the
existing historical-language sections; the reviewer marks both cosmetic points
as optional. Same-Thread concurrent-send CAS rejection is not covered by the
independent-Thread shutdown persistence claim and is outside this delivery fix.

Header deadline decision: default response_header_timeout_ms is a positive
120,000ms, separate from the 600,000ms body-idle deadline. The opt-in local
HTTP/1.1 measurement sent eight 1,024-character requests and eight
3,480,000-character requests successfully; the latter had p50 2ms / max 3ms
from send initiation to headers. Full samples are retained only in ignored
synthetic evidence. These loopback measurements establish instrumentation and
large-body delivery after restart, not provider queue/prefill behavior. The
issue reports successful generation around five seconds and possible header
latencies of tens of seconds. Choose two minutes as conservative initial
headroom for that reported range while cutting the previous ten-minute header
wait; no statistical optimum or provider capacity is inferred. Reassess this
provisional default with representative deployed successful/failing header
observations. An explicit longer positive value remains available.


## 2026-10-29: native CI and portable bounded-delivery fixtures

Initial run 40795744408 on df6aafb found Linux/Windows Clippy's while-let-loop
lint; 6f19b89 applies the equivalent while-let form. Its local strict Clippy and
14 delivery regressions pass. The first macOS job separately hit two five-second
attachment preparation watchdogs; post-timeout state was recovery_wait with no
model attempt. Local eight-way parallel reproduction (200 cases) did not fail,
and the next native macOS run passed with deadlines/assertions unchanged. No
cause is established for that isolated event; it is distinct from the earlier
local raw-TCP loopback fault.

Run 40796272377 on 6f19b89 passed Linux and macOS but found three Windows FIFO
fixtures timing out. Each drains a synthetic one-slot raw channel over 512 items
with a one-millisecond timer wait per iteration and a five-second watchdog. The
pinned Tokio Sleep documentation explicitly permits resolution larger than one
millisecond on Windows. Those timing assumptions are unnecessary to prove FIFO,
capacity or permit ownership; the runner's exact timer quantum was not measured.
Replace the three per-item sleeps with cooperative yields. For completed
shutdown, require the first resumed critical transfer to occur without fixture
flush, then explicitly flush after capacity is released for the remaining bulk
transfer. Preserve the five-second deadlines, all IDs/counts, ordering and permit
assertions. Dedicated producer wake/Notify regressions remain unchanged. No
production queue, capacity, interval, dependency or global timer setting changes.


## 2026-10-29: explicit standalone RAG writer lease release

The third native run passed Linux and Windows, including the corrected FIFO
fixtures, but found two RAG-selection failures on macOS. Local 14-way parallel
reproduction confirmed source add/re-registration could return busy immediately
after an awaited preceding command. Temporary probes established flock
WouldBlock/errno 35, not Interrupted/EBADF. All prior File destructors had
returned before API completion. A matching-inode FD scan and subsequent lsof
found no other retained user FD/process; that does not establish the underlying
kernel mechanism. This is distinct from the old loopback fault and from the
first isolated attachment watchdog observation.

Use a conventional private WriterLock(File) owner guard, one existing handle
per write command. Its Drop explicitly unlocks before closing the handle; Store
drops its SQLite connection before this last field. Thus logical lease release
does not rely only on the lifetime of every reference to an open file
description. No retries, sleeps, global mutex/registry, table, queue, worker or
dependency are added. A deterministic regression retains a duplicate handle,
checks live-writer exclusion, drops Store and reacquires while that duplicate
remains open, then verifies closing the old duplicate cannot release the new
owner's lease.

Old uninstrumented parallel tests failed three of the first six rounds; after
the explicit unlock, 20 rounds x14 cases passed without retries or relaxed
assertions/deadlines. Initial registration and status fixtures now assert their
prerequisite successes and report useful diagnostics instead of hiding a busy
error behind a later null unwrap. All temporary logging, FD probes, lsof calls
and diagnostic guard types are removed. Revisit with exact OS/unlock/holder
evidence if contention recurs; do not label an unobserved descriptor leak or
precise kernel cause as proven.


## 2026-10-29: IPC failure notification versus owner-cleanup watchdog

Windows CI run 40802346281, job 123213135953, failed the permanent-listener
frontend fixture only at its final two-second bridge join. Its earlier
two-second failure-notice check, local database mutation and Quit submission
completed. LocalServer publishes the failure watch after listener termination
and endpoint cleanup, so that notice establishes the listener task has already
finished; the final join additionally drains Control and closes SQLite, diagnostic
logging and owner filesystem resources. DataOwner's diagnostic shutdown may
legitimately wait the default five-second grace by itself. A two-second whole-owner
cleanup bound is therefore not the IPC-notification contract being tested.

Keep the two-second notification deadline and original liveness/error assertions.
Use the existing frontend fixture's 15-second (30 on Windows) owner-cleanup
watchdog only for the final join, drain bounded updates as a real UI does, retain
a duplicate-notice count across that drain and report last shutdown progress if
the watchdog expires. Also assert the database worker is actually closed before
return. No product timeout, grace, lifecycle ordering or native Windows success
is inferred or changed. This diagnosis does not identify which individual
Windows cleanup operation exceeded two seconds; revisit with the recorded
shutdown progress if the lifecycle-sized watchdog also fails.


## 2026-10-29: attachment fixture preconditions and native watchdog diagnostics

Two attachment cases in Windows run 40802346281 exceeded the shared incoming
request watchdog. The old panic identifies neither the caller nor whether the
Driver is preparing, recovering or awaiting HTTP. No production transport,
retry or attachment cause is established. Keep the five-second watchdog and
all semantic assertions; do not add retries or alter product code on that
evidence alone.

The synthetic ordered-content setup now confirms the exact accepted Turn
entered recovery for the intended invalid-UTF-8 AGENTS error, with no saved
instruction snapshot or model attempt, before editing synthetic source content
and issuing RunRetry. Generic absence of active Turns is insufficient evidence
for that particular setup condition. Each incoming watchdog now captures its
real caller and, only after failure, obtains a separately bounded one-second
saved-state diagnostic including synthetic recovery/attempt errors. No success
path wait or product logging is added. If native failure recurs, use that phase
evidence before deciding whether a production fix or fixture change is needed.


## 2026-10-29: completed-pass identity, per-Thread save failure, index stop evidence

Review #32/6486175965 identifies a completed receipt pass whose retained checked
bits become indistinguishable from an actual partial pass after another send.
Clear checked bits when continue_scan returns idle; retain repeat/fairness during
actual partial scans. A completed scan plus a later accepted receipt/Changed is
one fresh pass, not a partial pass and a duplicate full pass.

The stopped autosave worker still has one lifetime closed bit, but its existing
bounded backlog now stores one notification bit with each retained snapshot.
Warn every affected Thread once, including a Thread edited after closure; newer
revisions retain that bit. Do not add a separate Thread registry or notification
queue. Backlog ownership/limits and final SaveFailed remain unchanged.

Review #33/6486176260 requests more useful local stop evidence. Replace the
boolean with a once-published pair of existing ErrorCode values for processing
and diagnostic-pause failures, before cancelling workers. Two enums are bounded
and contain no provider/SQL/user text. Preserve any existing durable reason
verbatim and append local cause codes for readable status; search error details
carry those codes. The once cell has the existing service lifetime; no durable
state, queue or worker is introduced.

## 2026-10-29: recurring attachment watchdogs are unresolved preparation evidence

The same compacted-source and corrupt-source cases exceeded watchdogs on macOS
and Windows in separate native runs. A shared Driver/preparation issue remains
a credible hypothesis; successful local probes and one green native run do not
resolve it. The original Windows incoming helper did not identify which POST
was missing, so neither the precise common stage nor separate causes are proven.
Earlier statements separating this from the old raw-loopback fault must not be
read as establishing independent causes for the two attachment occurrences.

Retain the five-second attachment watchdog and exact accepted-Turn preparation
precondition from b8f1f84. Run multiple native gates on the current fixes, count
them as observations, and inspect caller/state/reason/attempt evidence before
changing production behavior on recurrence. Never retry until green and call
that a fix. Bounded shutdown stage timestamps separately address the IPC cleanup
measurement gap; a five-second configured diagnostic grace is an upper bound,
not evidence that diagnostics caused the earlier two-second timeout.


## 2026-10-29: retry and scope review boundaries for v0.2.0

Review #34 separates the once-per-Turn/generation trim allowance from its
immediate resumption guard. Spend the allowance only after an adopted edit;
clear the guard after the first durable response adoption, preserving ordinary
later retry and context-overflow recovery. An actual partial body source may
trigger one safe reduction for large input; before-body/connect failures follow
ordinary retries. A second known partial failure still stops without another
trim, even if the first edit was rejected. Keep the pre-header deadline inclusive
of upload time and explain this combination rather than imply a separate upload
delivery guarantee.

Optional proactive edits are not prerequisites for a valid request. Ignore
non-storage cancellation/rejection and let stop and existing generation/profile/
selection admission fences decide; preserve storage failures. Use actual prepared
input counts with bounded calibration before planning, and retain typed planning
outcomes so changed path estimates cannot rewrite the activation reason. Adopt a
bounded oldest whole-group provenance window when the automatic restore metadata
cap permits some reduction; never split a group or exceed protected boundaries.

Scoped keyword membership now walks indexed reverse replacement consumers per
candidate rather than rebuilding the full scope. It still scans candidate IDs
up to the current global watermark and may traverse deep ancestor chains; do not
claim constant-time or a latency SLA. There is no materialized ID cache, temp
write, new index or persistent search state. Overlapping history scopes preserve
all memberships but contribute the best single rank vote per retrieval method;
keyword and semantic remain separate evidence. Revalidate live Thread existence
after embedding before classifying a selection change.

Only appropriate failed Responses requests retire their pool generation. Build
the next client on the existing blocking executor, install under a short lock,
and never cache a failed build as a permanent pool value. Existing request
admission bounds concurrent rebuilds; a racing rebuild installs only into a
still-retired pool, while generation checks reject stale failure retirement.
Exact TCP delivery/reuse remains unknown. Shutdown observations reuse the
existing bounded writer/event with fixed enum facts and per-scope elapsed time;
keep that writer alive through endpoint cleanup, close it last, and preserve
Control/DB/endpoint/owner-lock ordering and error precedence. The writer cannot
log its own completed join; its last begin record is intentionally not an End.


The former public trim_disconnect wrapper had only four unit-fixture callers
and kept obsolete snapshot-before-threshold logic. Remove it and its dedicated
failed-input estimate field rather than retain an unused Core compatibility
path. Existing maximum/no-candidate fixtures use trim_activated; actual prepared
input calibration is tested directly. The duplicate wrapper-only small-context
case is covered by real Driver retry tests, which now assert unchanged original
content/path, no replacement and no irrelevant trim error annotation.


## 2026-10-29: bounded native repetition instead of repeating all-target builds

For the unresolved #33 attachment symptom, run the full native gate and then
exactly two additional attachment-provenance test processes on every native OS.
Each process gets fresh temporary owners/stores; keep all cases, assertions and
five-second watchdogs. A failure stops the step immediately, so this is fixed
sampling, not retry-until-green. The existing three-job matrix/queue/production
code is unchanged. This concentrates repetition on the actual recurring cases
instead of spending another full all-target link/build cycle merely to repeat
unrelated tests. Earlier/new full native jobs provide separate-run observations;
repeat-process evidence within a runner is explicitly identified and does not
prove the original cause or absence of a rare failure. Report standard totals
separately from the repeated executions.


## 2026-10-29: a three-attempt connectivity fixture is not a five-second SLA

Windows run 40810247300 passes all production/other test cases but times out
while waiting for three saved closed-endpoint failures within five seconds.
The fixture explicitly permits 3,000ms per request and 100ms retry delay, so
three legal attempts alone can consume 9,300ms. The original aggregate watchdog
cannot cover its own configured behavior. Keep all per-request/product deadlines,
retry limits, immediate recovery-state rejection, minimum attempt count, retry
numbers, no-trim and final-completion assertions. Derive observation time as
5,000ms fixture preparation allowance plus three times (3,000+100ms), totaling
14,300ms. After listen, allow one pre-existing failed attempt and the next
successful request: 5,000+2*3,000+100 = 11,100ms for accept/completion observation.
These are fixture observation budgets, not latency requirements or new product
settings. No OS-specific timeout relaxation is introduced.

Record at most nine saved states/classifications after watchdog failure, with a
separate one-second diagnostic bound. On success, bind immediately without a
new observation await that could consume retries; print bounded kind/phase/source
consumption/approximate wall-duration facts only after recovery. Do not assume
closed loopback endpoints fail immediately on Windows or label a pre-body timeout
as a proven OS connection-refused exception. A dedicated native CI execution
prints these facts; it is a repeated observation of an existing case, not a new
distinct test. The old exact delay cause remains unproven until observed.

## Complete interrupted current-format tools without replay (#37, 2026-10-29)

Startup must restore the call/result invariant even when a previous owner has
already terminalized its Turn. This is repair of the current format, not a
compatibility reader or migration. Keep the existing startup transaction and
original interruption snapshot ordering. Distinguish pending/no admission
(`not_started`) from an uncommitted result with possible side effects
(`interrupted`). Preserve actual result nodes and report saved execution facts
separately; never repeat a shell automatically or infer success from lost output.

Cancel unapplied authenticated shell-origin attachments before completing the
source group; completing a group must not release uncertain side effects into
another Thread. Retain applied sources, refs and external files. Keep existing
DAG edges immutable, splice only newly created repair nodes, and normalize model
input using source-preserving replay frames. Current spills need their converted
items normalized once before caching because interleaved context and results
can share one source node. Cache entries retain canonical path order for reuse;
validate complete pairs before pin acquisition and before HTTP.

Only explicitly collected, unreachable GC tombstones belonging to a deleted
Thread are skipped, without restoring payloads or fabricating completion.
Retained inconsistent source bindings remain errors. Normal live admission
conflicts keep their existing guard: do not globally repair groups while the
current owner may still be collecting a shell. Missing pairs instead fail input
preparation locally; startup repairs the interrupted durable state.

The DB worker owns repair; keyset pages hold at most 32 groups/calls/Threads,
while SQL splices selected paths. Temporary replay maps are bounded by the
existing input frames/items and released during request preparation. No table,
worker, queue, dependency, schema or protocol bump is needed. Revisit if a new
source format changes grouping, attachment eligibility, or GC ownership rules.
See [design](../design/tool-group-recovery.md) and
[validation](../validation/acceptance.md) for executed tests and remaining limits.

## Revisit at normal completion (#36, 2026-10-29)

Adopt [Revisit](../design/revisit.md) after #37: reserve one bounded instruction
per live Run and consume it at the existing transactional completion decision.
The same event/delivery and ordinary Turn path handles the instruction. It is a
developer event, not a synthetic human message. Re-registration is deliberately
unlimited by default; an optional per-Run cap, visible count/pending facts and
existing stopping/recovery controls bound operation without a goal engine.
Direct-parent authority follows retry's one-Run mutation scope; cancellation
still propagates through descendants under the existing stop contract.

Use one existing app_meta row per registered Run. Schema 12 extends the existing
history-byte covering index to include that prefix, so reservation/count metadata
is not hidden from logical capacity. Repeated firings overwrite that state;
ordinary events/history account for their own growth. No table, scheduler, worker
or dependency is added. Protocol 2 gains additive operations and genuinely
optional summaries (Runs without registration have none). Historical conversion
remains isolated in startup migrations.

Explicit stop clears pending state; shutdown/startup capture original text and
setter/count before clearing it. Keep compaction protection on the current event
source. Do not infer task success or classify questions in Core: the Agent Guide
explains cancelling/rewriting while awaiting the user. Mechanical mock tests
prove lifecycle/delivery behavior, not better real-model task judgement. Revisit
that evaluation separately with original requests, changed constraints and
question false triggers; no paid evaluation is silently added to routine checks.

## Isolate unresolved tool repairs (#37 review, 2026-10-29)

Adopt M1 isolation and L1 consistent saved execution facts. A current-format
structural inconsistency rolls back only its tool group's savepoint, keeps the
unresolved original history and continues other conversations. Preserve fatal
handling of actual SQL/persistence failure and missing global root metadata.
Bounded fixed-reason diagnostics identify affected Threads/groups; they contain
no command, instruction or arbitrary source text. Startup capture must also
avoid appending to a structurally inconsistent selected cursor before that
isolation can run. A skipped snapshot/group is not successful repair.

Original interruption observations precede terminalization; synthetic results
then refer to the post-startup stopped execution state. Keep L2's existing live
admission guard: completing a possibly live collector's call would risk racing
real results or repeating effects. An unreproduced live conflict still requires
inspection/restart; do not add speculative live repair. The candidate scan pages
RAM but examines the tool_group PK range at startup; it is not proportional only
to unfinished groups. Keep indexed per-group lookups and document that cost
rather than adding a partial index which cannot cover all missing-result cases.

## Bounded OS termination and exit markers (#38–#39, 2026-10-29)

Route delivered TERM/HUP and Windows console-close events through the existing
draft-preserving shutdown. One owner-lifetime async observer on the shared
runtime holds bounded atomic facts; the OS callback never owns DB/UI work. A
fixed cleanup deadline and second delivered signal force exit, relying on #37
for durable repair when cleanup cannot finish. Terminal loss disables further
render/input attempts and leaves backend drain alive. The existing windows-sys
Console feature is enabled without a new crate or version change.

Windows logoff/shutdown notification is best effort under OS delivery rules;
user32/gdi32 can suppress those console events. A hidden-window message pump is
not introduced for this change. Native Windows desktop/console behavior is a
separate unverified boundary, not established by a mock. Revisit if actual
Windows operation demonstrates the need for that extra lifecycle owner.

Derive exit markers from the latest Run's saved stop_reason only; no new state
or auto-resume. Preserve explicit_stop during interrupted startup as normal
shutdown already does. A new Run naturally clears the marker. Bulk resumption is
an explicit message operation with batching and existing ownership rules; the
Guide adds no extra restriction on the model's judgement about uncertain effects.

### Quit admission is not ordinary replay cancellation

The Revisit shutdown guard applies only at the idle completion decision. Closing
new tool starts must still allow already accepted tool groups to save cancelled
results and follow existing request/drain behavior until real Control shutdown
cancels them. An early blanket next() return regressed two existing Quit tests;
it was removed rather than weakening those tests. A pending Revisit stays
unconsumed so the shutdown snapshot can record it.

### Select the bounded Crossterm Unix polling backend (#38, 2026-10-30)

A real PTY-master-close test failed with exit 1 after 5.1 seconds: diagnostics
contained the first HUP and force deadline, no frontend shutdown stage, and the
persisted Run, tool group, call and shell remained active. Inspection of pinned
Crossterm 0.29.0 `event/source/unix/mio.rs` found an inner readiness/read loop
that neither exits on zero bytes nor propagates EIO; it cannot honor the caller's
poll timeout while inside that loop. A pre-poll HUP check would retain a race,
and moving the same backend to EventStream would retain the spinning reader.

Enable the library's existing `use-dev-tty` feature instead of adding a parser,
vendored library fork, emergency Quit path, or longer shutdown deadline. Its
`tty.rs` source breaks the zero-byte read loop, propagates EIO and checks its
outer poll deadline. Both sources call the same `tty_fd()` and retain the same
terminal-stdin versus `/dev/tty` selection; Windows input is unchanged. The
filedescriptor 0.8.3 package is already locked through termwiz; only the
Crossterm dependency edge and existing Rustix process feature are activated.
Keep the five/four-second cleanup budgets and actual successful PTY-loss
assertions. Re-evaluate this feature choice on Crossterm upgrades against the
source behavior and the same physical PTY regression. Source inspection is not
a substitute for running that regression or native Windows verification.

## Issues #44/#45: read projections without new lifecycle state (2026-10-29 UTC)

Status: adopted. The list retains native state and adds typed activity through
one shared pure classifier. Running-only hints are gated so completed-answer
background pages avoid historical tool-group scans. Existing indexed ownership
EXISTS predicates distinguish shell/child work from finalization. No new schema
is needed. Revisit and app-exit facts remain separate.

Live shell elapsed_ms is explicitly a read-time wall-clock observation, not
the terminal supervisor's monotonic duration. Highest retained output sequence
provides last_output_at; reclaimed or absent output is null. Dynamic timing is
added only to the bounded status page after fingerprint/cursor derivation.
Revisit this choice if a future contract requires output-driven ordering or
monotonic live timing; that would require a separate persistence/ownership
design, not an implicit change to the present read projection.


## 2026-10-30: bounded index operations (#42/#43/#55)

- Preserve singleton embedding requests by default (`batch_max_inputs=1`).
  No portable proxy batching limit is known; configurable count/Unicode caps
  are explicit bounds rather than a provider support claim. Revisit the default
  only with evidence across supported endpoint configurations.
- Keep requested dimension separate from observed dimension. Native spaces omit
  the wire option; an explicit rebuild freezes its mode with the space and all
  query/ingest consumers reuse it. No provider catalogue or silent truncation or
  fallback is permitted. The first authorized input validates a candidate once;
  rejection stops that group while keeping the old active search available.
- Use bounded app_meta observations (32 probe tuples, one candidate mode per
  profile, one optional mode per retained space). Existing DB/HTTP workers and
  cancellation own them; add no background engine or polling notification.
- Expose float32 payload arithmetic only, not claimed file reclamation or
  retrieval quality. Explicit GC may collect inactive derived spaces, protecting
  current active/candidate pointers and running work. SQLite file/WAL shrink
  remains separate. Paid operations require the guide's user consent, without
  adding an approval state machine to Core.
- Historical pause observations remain immutable. Preserve the first saved
  timestamp for the same pause cause, expose unknown time honestly, and clear
  current UI attention only on fresh state. Existing Turn-boundary observation
  markers deduplicate notices; do not introduce a poller for immediate delivery.

These decisions are provisional only where endpoint support or retrieval
quality is unknown. A failing endpoint or measured quality regression warrants
reconsidering the user's chosen caps/dimension, not inventing a provider default.

## #51: explicit AGENTS references (adopted)

Explicit selection is strict and has no clearing operation. Relative paths use
the effective target cwd, lexical normalization and revision-checked preflight.
Only reference-change events defer to the next new Turn. This preserves existing
settings authority and snapshots without adding a worker, queue or table.
Revisit clearing/composition only under a separate user requirement.

### #51 review: persistence and unsaved TUI selection boundaries (adopted)

Keep History fork helpers DB-only: explicit references require Control, and are
rejected rather than silently ignored by the helpers. Return inherited paths
truthfully. Unsaved TUI selection must adopt the created Thread into the original
draft; a generic management result is not adoption. A draft/generation-bound
UI action/result uses the existing command slots and ordered outbox; one local
pending request identity blocks a racing first send. No new Core event, persistent state,
worker or queue is added. Later edits are retained; replies cannot select an
unrelated active draft. The existing worker/shutdown owns completion.

UI-to-frontend admission rejection unwraps submitted/management actions and
releases only the matching draft/generation/request selection. Submitted slash
text retains its normal restoration behavior. A local UUID identifies each
pending selection (one bounded value per view) so a stale rejection or result
cannot release a later selection in the same draft generation.

## #51 external review: deterministic idle checks and deferred Low items

Status: adopted follow-up to public comments
[6511989087](https://github.com/tya5/shellboy/issues/51#issuecomment-6511989087)
and [6512018897](https://github.com/tya5/shellboy/issues/51#issuecomment-6512018897).

Idle correctness must not be inferred from a quiet receive timeout. Use the
existing durable RunEnded boundary, compare target-scoped saved work identities
and states before/after operation completion, and independently verify the
settings MutationResult cannot wake a driver/retry. Keep reference-change audit
pending without delivery/history adoption until a new Turn. No production
behavior, table, worker, queue or test binary is added for this correction.

The following are record-only Low observations, not closing conditions:

- agents-only and same-path selection still increments profile_revision through
  the shared settings update, so an admitted preparation may be discarded and
  rebuilt before POST. Preserve this existing behavior for #51; revisit separate
  revision domains only in a future scoped change with admission-race coverage.
- create/fork override audit reports the default/inherited reference as before,
  even if this new Thread never used it. This is the adopted audit contract;
  revisit initial-reference wording/delivery only with an explicit UX decision.
- optional argument fields require changes across constructor literals because
  not every argument struct implements Default. Defer that ergonomics refactor;
  it is outside #51's production scope and must not obscure behavioral changes.

Publication dependency: #46 lands before #51 is rebased onto origin/main.
Adapt existing regressions to #46's resulting layout, then repeat standard
checks. Do not preemptively transplant or resolve conflicts while #46 is absent.
This records ordering, not permission to push, post to GitHub, update main or
replace installed binaries. Physical-terminal verification remains unclaimed.

## #48: attach provenance to the actual instruction message (adopted)

Choose proposal (a): prepend one source/snapshot metadata line to the AGENTS
developer message, not another system instruction or authority/permission layer.
This keeps provenance adjacent to the body even when the Agent Guide is unread.
JSON-quote the fixed Turn snapshot path and escape controls/line separators;
append one LF and the unmodified body. Preserve all snapshot storage and roles.
The guide explains configured-reference inheritance/override independently of
cwd and requires explicit permission for edits, without promising automatic
exceptions to other higher-priority instructions. Model behavior improvement
requires later dogfood/real-model observation; mocks prove request contents only.


## 2026-10-30: recover input without rewriting ownership (#53)

Use explicit-resume path adoption of the same immutable node, rather than a
second receipt for that message. Existing message/context readers assume one
receipt per message; duplicating it would expand identity and cursor semantics.
Keep original assignment and add one persisted admission-fact column instead.
Normal acknowledgement still follows response adoption; terminal repair can
settle admitted input without inventing an answer.

Fresh known-unadmitted input follows proposal A. For historical rows lacking
positive path/attempt admission evidence, prefer explicit restoration (proposal
B) to silently resurrecting a rewound instruction. The old format cannot prove
never delivered. Record unknown honestly and expose it in #52's queue. Revisit
only with additional positive historical evidence, not an absence-based guess.
A bounded 128-ID resume rejects excess input atomically instead of overtaking
old messages or adding a recovery scheduler.
## Pending-message implementation boundaries (#52, 2026-10-30)

- Use a frontend read-order stamp assigned inside the serialized DB read, rather
  than a new durable queue version. History and pending rows are one transaction
  and one update; existing generation/view checks still apply. Revisit if reads
  leave this shared DB lane or a second UI producer bypasses paired delivery.
- Freeze a submission in a separate row of the existing draft table. One editable
  row cannot preserve revision R while accepting a newer autosave of R+1. Bind by
  operation/source draft/revision, never text; preserve newer edits and make
  failed precommit snapshots explicitly restorable. Original source references
  serve cancellation archives, retaining attachments and provenance without
  copying conversation bodies into another store.
- A nullable per-Turn interrupt timestamp records durable human intent without
  adding a lifecycle state. Keep the Turn active until adopted call/result pairs
  finish, because attachment admission relies on that boundary. The existing
  Run-wide cancellation token cannot represent a light Turn interrupt. One
  Turn-bound token/native-start gate is owned by each driver and released with
  it; DB callbacks never acquire the gate. Native Windows behavior needs fresh
  CI evidence; passing a loopback test on macOS will not establish it.
- Ordinary sends no longer release foreground waits. Immediate delivery is an
  explicit persisted fact and never interrupts response generation. Existing
  send-key configuration wins over the Ctrl+Enter convenience; the two-key chord
  remains available. Timed automatic backgrounding stays deferred by the latest
  #56 decision, avoiding an unrelated timer/configuration mechanism.


## Published Message Recovery Review (#53, 2026-10-30)

Review of main b686ec1 reports no Medium issue and approves closing the Core
work after exact-SHA three-OS CI. Track unknown historical admission explicitly
in #52 with synthetic list/cancel/restore verification; do not auto-replay it.
Retain the two factual existing reason codes: Thread deletion says
target_unavailable, while general terminal repair may report target or source
unavailable. No alias/compatibility layer is needed merely to unify wording.
The five-millisecond polling in a few isolated recovery fixtures observes saved
state under a sixty-second watchdog; it is not product polling or elapsed-time
correctness. Prefer existing Changed notifications when next editing those
fixtures, rather than broadening this fix into an unrelated test rewrite.


## Compaction owner-scope absence is a saved fence (#52, 2026-10-30)

Adopt exact absence checks for the existing optional Run/Turn snapshot fences.
A snapshot taken before a first Run or between Turns must not adopt after a new
owner scope starts. The prior validator checked captured Some values only,
leaving None-to-Some transitions unchecked. Reuse indexed Run/Turn existence
checks; do not add a counter or bump selection_revision at message acceptance.

A same-active-Turn queued message remains off-path and cannot be erased by the
captured selected-source edit. Existing path/source/profile/generation validation
therefore remains sufficient in that case. A general thread.revision fence would
also reject unrelated metadata and routine queued reports, potentially discarding
paid automatic summaries without improving this source-safety boundary. Revisit
only if a concrete change alters selected/protected source semantics without
advancing an existing fence. Deterministic Store-only regressions distinguish
new Run, new Turn and unchanged active Turn instead of racing driver adoption.


## Shutdown registration readiness (#52, 2026-10-30)

Main CI41016702414 passes Linux/macOS but fails Windows at the three-second
wait.pending registration barrier in the shutdown capture-failure fixture,
before prepare_shutdown runs. The identical product/test tree passed manual
Windows41015832578. This supports a fixture readiness investigation, not a
claim that shutdown failed or that runner scheduling is proven responsible.

Start Control explicitly before installing synthetic owner facts; use one
shared60-second saved-registration watchdog with premature-result diagnostics
and a120-second observed wait. Initial human wait registration has no Changed
notification, so use a bounded5ms fixture poll instead of a tight yield/DB loop.
Keep all failure caching, receipt, Run-stop and drain assertions. No product
timer, retry, worker or native lifecycle changes. Revisit if the expanded
diagnostics expose a real request failure or a missing durable registration.


## Pending-message review dispositions (#52, 2026-10-30)

Review6517077748 identifies a reachable busy loop: an interrupted active or
recovery-wait Turn with an unfinished adopted group cannot be acknowledged,
but next previously returned Recheck unconditionally. Use Idle only while that
same interrupted Turn has an unfinished group. Keep Recheck for an intent
committed after acknowledgement when no pair blocks progress. Preserve saved
results and uncertainty; do not call startup repair in a live owner or reject
previously allowed human interruptions. Existing nudge and Run-stop ownership
remain responsible for further progress. No timer, event, queue or worker is added.

Correct the review's failure location using its exact log: tests/recovery_paths.rs
429 is the registration guard before prepare_shutdown, not the later shutdown
watchdog. Retain both failed native run and separately rerun evidence.

Adopt60-second deadlock watchdogs and notification-before-read in the new native
Turn scenarios where Changed is guaranteed. A reserved native-start row has no
guaranteed notification while its start gate is held; retain bounded5ms fixture
polling there. Product timers and side-effect assertions do not change.

Historical #52 deferral: the submitted-copy decision in this paragraph is
superseded by the active #60 receipt-watermark decision below.
Defer submitted-draft retention cleanup (review Low3). Those rows also preserve
crash/replay correlation and the source-revision watermark; deleting them at
admission would revive stale autosaves or lose replay provenance. They already
count toward the existing logical storage cap and are removed with their Thread.
Revisit with measured draft-byte pressure and a design that retains a bounded
watermark/replay guarantee without an independent archive engine. The registry
lock versus long DB work observation (Low5) remains a documented ordering tradeoff,
not a newly discovered deadlock; revisit through the separate DB-query work.
Fix the Agent Guide's "up to128" typo without changing its cancellation contract.


## Large-upload fixture observation (#52 Windows follow-up, 2026-10-30)

The shutdown-fixture correction run41018286041 passes that earlier failed test
but fails the existing response_upload_recovery arrival watchdog after15seconds.
The observed phase is a real1024-byte socket prefix, not a simulated source;
between requests the fixture also reduces/rebuilds16MiB of history. The old
message did not identify prefix ordinal or saved attempt phase, so causality
remains unknown. Raise only arrival/settlement watchdogs to60seconds and report
bounded saved Turn/attempt diagnostics if they fail. Keep the product3-second
header deadline, actual backpressure, partial consumption and exact attempt-count
assertions. Retain the failure and require fresh native Windows qualification.
The reviewer-run unchanged-main second attempt succeeding is independent evidence,
not proof of the cause of either failed attempt.


## Finite visible history retention (#58, 2026-10-30)

Retain actual painted node identities, capped by the existing128-node projection,
plus the current finite source neighborhood. Do not pin every node of a Turn.
An existing global body budget applies across views; zero budget still exempts
this finite active working set. If a transient lease and current neighborhood
protect both window endpoints, preserve them rather than trimming a protected
edge; the next paint replaces the lease. No sparse ordinal cache is introduced.

NodePresentation carries two nonserialized scalar facts: eviction and the existing
serialized-DB read order. This avoids adding fields to every UiNode constructor
or introducing an ID-indexed request engine. Semantic projection hashes exclude
those cache facts. Per-node hydration order is independent of queue age; a stale
matching navigation response is retried, not allowed to overwrite newer output.
Rejected rebase snapshots retain ownership/pending wheel movement and use the
existing rebase action again, avoiding a permanently true rebase-pending flag.

An offscreen committed preview is evicted as a whole when pressure requires it:
its attempt ID cannot be fetched by history.get, but its saved canonical node
remains in the original DAG. Never keep a blank attempt-ID stub and ask the DB
for that nonexistent source. Keep visible previews and the active uncommitted
stream until a saved source exists. This avoids an unbounded all-preview exemption.
Revisit if a concrete case creates more than one uncommitted current stream after
the existing settle-other-streams boundary, or if measured metadata pressure
requires a separate bounded lease snapshot; do not assume all prior paid text
must remain resident forever.

Audit received_response previews retain their applied-window/anchor scope and
remain visible when the same saved predecessor is still in a new history window.
Do not clear them merely while the separate audit read is in flight; a fresh
accepted audit snapshot decides membership. Old-revision audit replies still
cannot update metadata or remove retained text. Anchors outside the new window
are not appended to an unrelated tail, and audit positions never become canonical
ordinals. Current paid streaming text and committed preview bindings survive
background rebases until actual canonical body adoption.
Only move cached transient-failure preview blocks when adopting the replacement;
a discarded or rebase-pending response must not consume the retained frame.

Diagnostics reuse the current best-effort nonblocking writer. Fixed enums,
counts/booleans and source owner IDs describe follow/window/eviction boundaries;
body/title/command/path/provider text are not accepted by the trace type.


## Tool result visibility before the next response (#58)

The80-column native PTY candidate showed an exited shell result absent35seconds
while request2 was held before its first response byte. DB ordinal10 existed but
the last UI page contained10 nodes;140columns sometimes passed. Source tracing
shows shell collection emits Changed before save_tool_result commits its new
canonical node, which emitted no later notification. Frontend dirty/refresh
coalescing retains later Changed correctly. Add that missing postcommit Changed,
not periodic history reloads or a new event. Require deterministic commit/rollback
notification tests and repeat both native PTY widths against the final binary.

## #50: remove inferred input-size policy (2026-10-30)

Adopt [failure-driven reduction](../design/failure-driven-reduction.md). Delete
compact_hint_tokens rather than preserve an optional override; use saved usage
and character/byte counts for facts only. Retain the existing finite retry budget
and compaction safety machinery, not a new recovery engine. Choose /compact-max
for the API-free action instead of a new dialog. Minimal recovery text precedes
#41/#58 UI work. The old 75k provisional activation decision is superseded.


## #41 staged preview API (2026-10-30)

B+C uses bounded, independent read-only tails for completed tools. Keep only
facts with concrete callers in its internal preview DTO; defer continuation
cursors and live timing to D, which owns their actual lifecycle. This avoids
unused speculative API fields and an unnecessary timing query on each completed
preview. The adopted design describes that stage boundary. Raw byte and row
limits remain checked in production, not only in tests. Revisit when D adds
incremental live output; no protocol or saved format changes are involved.

## #50 review: explicit overflow requires an adopted reduction

An explicit capacity rejection must not spend remaining retries on the same
input. If maximum omission has no target, try the existing final spill/summary
stage immediately; if neither adopts a reduction, or stages are spent/rejected,
recover without another POST. Keep ordinary transient/no-candidate retries.
Expose the unavailable automatic-reduction fact without a cause recommendation.

## #61: explain the actual safety boundary, without adding one

Align public guides with the existing shell/Origin/ownership, persistence, stop
and retention contracts. No per-command shell approval or command-name denylist
is offered; existing TUI confirmations and CLI validation still exist and must
not be described as absent. Explain the bypass/habituation rationale and OS
isolation alternative, not a promise that logging or stopping prevents harm.
User Guide EN/JA carry the same five-part safety explanation; the English Agent
Guide adds scope/injection/secret-handling reminders. Saved usage is three user
points, with implementation detail retained in tui.md/usage-aggregates.md.
Defer OS sandbox command/profile examples until each platform is actually tested;
list tools only and note owner-mediated access. WezTerm Shift+click is a reported
macOS observation from issue61, not new developer/other-terminal qualification.
Only documentation changes; no new runtime policy, schema, worker or dependency.


## #41 repeated quiet observations (2026-10-30)

The large coalesced-event wheel regression exposed an information hierarchy bug:
limiting source references by splitting every two event nodes produced hundreds
of identical quiet rows. Group at most two distinct concrete facts instead,
with exact repetition counts inside the finite loaded projection and latest
representative source references. Preserve the full canonical frontier and all
immutable original records; F6 DAG navigation reaches individual observations.
Counts are displayed-window facts, not invented whole-Thread totals. Model text,
received paid previews and actionable failures are not quiet observations.


## #41 projection bounds and preview admission (2026-10-30)

Painted quiet summaries can follow later prose, so their physical first row does
not identify the canonical beginning of the bounded projection. Reuse the same
128-node/192KiB range calculation for backward navigation; preserve actual
Unicode row anchors for body hydration. Do not infer source order from paint
order or introduce a second navigation cache.

Inline preview limits include key/entry/block/vector metadata and conservative
map overhead. Retire a matching in-flight preview when its source/outcome is no
longer current; do not leave its finite admission slot permanently occupied.
Token checks still prevent that old response from cancelling a newer request.

## #62: validate release candidates without publishing

Prepare 0.3.0 on a fixed, green main SHA. Add optional candidate_ref to the existing
release workflow: candidate builds check out that exact SHA, build the three native
archives, extract and launch them, and assemble SHA256SUMS as Actions artifacts.
Build/assembly have read-only repository permission; publish is skipped whenever
candidate_ref is supplied. release_tag is only a package/version label in this
mode, not a Git tag creation. Do not use tag/publication mode without fresh user
approval. No installed binary, user DB, tag or GitHub Release is changed here.
Keep a release-specific notes draft separate from the version-independent README.


## #41 quiet footer ownership (2026-10-30)

Send/receipt progress belongs to its typed reactive state, not a copied operation
notice. Input events promote only status text changed by that event, preventing
old receipts from reappearing on caret movement. A missing activity snapshot is
shown as checking, never known idle. History/send failures remain explicit until
the next deliberate action; an unrelated older success cannot erase a failure.
Both new readonly UI actions retire their loading state on UI queue rejection as
well as reader-slot rejection. No new Core lifecycle, queue or producer is added.


## #41 first-unit review: preserve B+C freshness ownership (2026-10-30)

Review6523314574 is accepted. The shipped b0e3c29 unit covers conversation
structure and inline interaction, not every adopted B+C freshness inventory row.
Record BC-R1–R6 explicitly in plan/status/acceptance and the issue before further
claims of completion. Prioritize that usable follow-up before D; do not relabel
F6/F7 and snapshot timestamps as general G work simply to mark B+C complete.
Reason: successful standard checks and structural PTYs do not prove external
mutation-to-paint latency or a missing Changed/read path. Revisit completion only
with implementation and new route-specific evidence, preserving bounded readers,
stale-response guards and the no-polling/no-new-worker boundary.


## #41 detail availability notifications and preserved previews (2026-10-30)

The B+C freshness audit found raw/history GC commits with no Thread Changed
notification. A source-owner subscription alone therefore cannot fix availability.
Use one private coalesced Notify in the existing GcService, signalled only after
successful availability-changing commits. The existing frontend forwards only a
selected-detail invalidation; hidden/F2/closed detail consumers do not read or
repaint. It has one permit, no payload/task/queue, and existing owner lifetime.
A global GC invalidation is justified because its selected exact source set is
at most2 and can include another Thread's source; it does not make unrelated
shell-output notifications repaint the view. Validate the actual collector path,
including raw retention, protected complete fork sources and loss after deletion
of the final referencing Thread.

Explicit preview close/reopen must perform the advertised bounded reread. Keep
the last known paid output and its original read-time notice while that read is
pending or fails. A separate at-most512-character refresh error is counted with
the retained preview payload, rather than replacing the timestamp or silently
hiding known output. Existing cache-pressure eviction still applies. An explicit
R during a pending detail read queues one dirty follow-up, not a concurrent read.

## 2026-10-30: #59 sender facts and existing source presentation

Active. Source: issue #59 body and mandatory comment
[6517220821](https://github.com/tya5/shellboy/issues/59#issuecomment-6517220821),
plus implementation-scope approval during review. Store explicit sender kind and
optional self-asserted label in the existing flat receipt sender JSON, retaining
top-level Origin IDs for cancellation authorization. Expose an allowlisted,
credential-free observation; incomplete old evidence is unknown, not repaired.
No schema migration or Core legacy request-format shim is introduced.

The reviewer identified that the existing conversation metadata classifier would
map a newly explicit human object to Unknown. Preserve its established Human
classification with a data-boundary check and regression, ignoring label. Do not
change render/layout/index refresh or add conversation badges. Historical null
receipts keep the existing conversation display convention only; CLI/F6 sender
facts correctly remain unknown. Revisit that historical presentation only under
separate conversation-design scope, not by inventing identity in sender facts.


## #41 D0: separate helper extraction from shared-planner adoption (2026-10-30)

First extract existing connection reads and off-DB preparation behind unchanged
async wrappers. This is not yet the one-job refresh optimization and does not
complete D. Fully implemented preprojection/aggregate helper APIs are tested
under cfg(test) until the shared planner adds their production callers; promote
those helpers in that caller commit rather than suppress dead-code warnings or
add redundant runtime queries merely to satisfy lint. The existing projected
read path remains fast and behavior-preserving in this foundation commit.

Move Revisit summary decoding off the DB worker through one pure helper sharing
the existing private State and summary conversion. Malformed summary evidence
remains unavailable as before; no state/schema/worker/queue or Core policy changes.
The later shared-read commit must preserve raw legacy-null band evidence separately
from typed unknown sender inspection and keep F6 immutable sender facts intact.

## 2026-10-30: D-006 legacy sender display clarification (#68 / #59)

Active. Source: issue68 D-006 and the issue59 review follow-up. Preserve old null
senders' human conversation band to avoid reclassifying an entire existing
conversation's appearance. It is a display convention, not sender evidence.
CLI/message/history provenance and F6 continue to report unknown, with F6
explaining absent evidence including old records. Newly recorded external_cli
uses the existing generic input category. Correct the overly broad unchanged-
rendering claim; do not change band code, source classification or #41 styling.
Revisit only on an explicit user decision to restyle historical null records.

## 2026-10-30: bounded first-run setup (#69)

Adopt `config init` / `config check`: noun/verb naming matches existing CLI groups
and limits the promise to configuration, unlike a general `doctor`. Reuse strict
loading and model-catalog HTTP bounds rather than add provider templates, login,
interactive setup or billed chat probes. Keep AGENTS existence display deferred:
it requires worker-fed filesystem observations and overlaps #41; this task only
changes empty-state/notification text. Revisit after that UI work is stable.


## #41 shared-read measurement and remaining SQL work (2026-10-31)

A single DB-worker closure is necessary for the coherent current-view witness,
but it is not evidence of one batched SQL plan. The actual128-node/50-tool fixture
changes7→1 worker jobs; p95 worker time changes3.675→3.940ms. Preserve that slight
increase rather than describing a SQL speedup. Existing per-node tool/source
queries and empty-title fallback parsing on the DB worker are explicit remaining
work before final SQL-only/global batching qualification. Start with exact modern
node/tool PK batches; preserve bounded receipt collision/null evidence and sparse
call ordinals rather than replacing row limits with assumed ordinal ranges.

This keeps Core small: source/execution validation remains read-only frontend
projection, without a new worker/table/queue or compatibility engine. Separate a
bounded UI raw-title observation from off-worker preparation; do not silently
change the CLI's existing title policy as a side effect. Revisit this choice if
bounded witness exhaustion demonstrably hides essential labels, and record that
case before changing the fallback contract.

Private cross-worktree measurement reused a before binary once because of a shared
Cargo target cache. Exclude that sample; force package compilation and require an
explicit source witness plus per-sample expected job assertion in future private
comparisons. Instrumentation stays ignored and must never enter shipped code.


## #41 D1: bounded title evidence belongs in the frontend (2026-10-31)

Use the existing pure title policy with a bounded SQL-only frontend witness,
leaving CLI lookup unchanged. Prefix caps cover inspected path/receipt entries,
not merely returned matching users; source bytes share one512KiB budget. A missing
earlier candidate yields an explicit limited-evidence title, never a confidently
selected later request or an unsent claim. Pending-prefix exhaustion conservatively
suppresses derivation because created_at ties can reorder by node rowid beyond the
receipt-index prefix. This trades a rare generic title for finite read work and
correct source order without Core indexes or persisted derived titles. Revisit if
real bounded-witness exhaustion affects ordinary conversations; measure before
raising caps or changing CLI contracts.


## #41 D1: retain the existing source index; reject an unnecessary migration (2026-10-31)

The initial Python minimal-schema probe omitted tool_result_node(node_id), which
has shipped since migration2. Its rising VM-step curve therefore does not model
the current store and must not justify a migration. Before any code/schema change,
full-schema inspection found the existing index. With that index, the corrected
probe remains3,486 VM steps for one missing-payload source plus49 known sources
as unrelated results grow1,000→10,000; the proposed covering index saves only3
VM operations in that fixture. Require a bundled-SQLite/full-current-schema
regression rather than treating this Python result as native proof.

Retain schema14 and the existing index. The briefly considered, uncommitted
migration15 design is rejected; no migration/source-index code was written or
user data changed. Current payload GC can still use exact saved result relations
without an old-format shim or a new authority policy. Revisit a covering index
only with measured same-node fanout or native lookup evidence demonstrating a
material benefit beyond the current index. Check the full schema and existing
migration history before using a minimal prototype to infer a missing index.

## 2026-10-31: catalog absence is advisory (#68 D-009 / #69)

Active, from the user's issue68 decision and issue69 review: an unadvertised
model is a warning with exit success, because wildcard/alias proxy models need
not appear in a catalog. No chat probe is substituted to guess availability.
Syntax, required settings, credentials and catalog HTTP failures remain errors.
Revisit only if actual naming mistakes justify an explicit stricter option.

## 2026-10-31: finished submission copies and receipt watermarks (#60)

Active; supersedes the submitted-copy retention deferral in the #52 review.
User approval requires body-free, bounded facts in existing receipt JSON, not a
new archive engine/table/migration. Keep only operation ID, source draft ID and
revision, and message ID, each identifier <=4096 UTF-8 bytes and the escaped
object <=64KiB. Both acceptance and legacy GC use the same binding shape.

The copy is collected only for admitted or known-admission cancelled input;
unknown/submitting copies and editable/archived drafts remain. Cancellation
archives retain original source references and restore text/attachments without
sending. A terminal Run alone is not proof that held unadmitted input is gone.
Receipt-based fences keep old autosaves/restore suppressed after copy disposal.

Use existing DB-worker transactions and history/all GC, at most128 candidate
copies per pass. No schema or index is added: exact source-revision checks can
scan receipt scalar JSON (including across edit-fork Threads). This is a bounded
payload, not an O(1) query-cost claim. A new index/migration needs measured need;
do not trade away cross-Thread correctness to pretend a target-only seek suffices.
No user data or installed executable is edited as part of validation.


## 2026-10-31: Thread-bounded submission fences (#60 Medium follow-up)

Active, superseding the initial #60 global receipt-scan tradeoff. Per user/reviewer
instruction, pass the receiving Thread at every autosave/selection/archive fence
and constrain binding.target_thread_id before JSON checks. Reuse receipt_target;
no migration/new index is needed. EXPLAIN QUERY PLAN must show SEARCH binding
USING INDEX receipt_target(target_thread_id=?), not SCAN binding. A receipt in
another Thread is not a watermark for the selected Thread's editable draft.

## 2026-10-31: one conversation wire boundary (#64)

Adopt a connection Api enum and one stateless HTTP-edge adapter, normalizing Chat
to the current Core response/event/replay contract. Keep lifecycle, storage,
retry/reduction/recovery and tools common; no new table/worker/queue or provider
fallback. Developer messages map to system; all consecutive assistant messages
and calls share one Chat message before their tool results. Skip Responses-only
reasoning replay; vendor reasoning_content/opaque tool metadata is unused, not
silently claimed supported. Reject Chat prompt_cache_key; no extra_body/cache
provider knobs in this first version. Disclose limits in catalog/check output.

Per #68 D-003, real providers including Gemini stay unverified, no paid test.
Use recorded synthetic fixtures, loopback and a test-only Tokio test-util feature
(same pinned Tokio version) for a controlled inactivity clock instead of short
wall-time assertions. No dependency version or production worker is added.
Core input-character diagnostics ask the common client for actual dialect input
characters, so discarded Responses-only items are not mislabelled as Chat wire
input; HTTP body-source byte observations remain actual transmitted source facts.

## 2026-10-31: exec and deliberately limited local Schema validation (#65)

Active. Name the client `exec`, not the reserved `run`: it communicates
noninteractive saved work, unlike tool-free ask or shell start. Default to a new
saved Thread with caller cwd; keep history and do not auto-delete it. Existing
Threads reject new-Thread configuration overrides. Send the final contract as
ordinary user text, preserving AGENTS and owner/model configuration.

No validator dependency is added. Use existing serde_json for a documented
bounded shape subset; recursively reject every unsupported keyword/shape before
owner contact, with no remote $ref resolver. Supporting full drafts, combinators,
patterns or numeric constraints later requires a separate dependency/validation
decision. Bound input, depth/nodes, validation work and diagnostics. Integer-float precision and exhausted-work tails have regression coverage.
Use a bounded local scalar-span index after existing serde_json syntax parsing
to preserve exact numeric lexemes for integer checks. No dependency feature or
package is changed; Core Value parsing/storage semantics remain unchanged.

stdout is validated JSON only; stderr carries IDs/repair metadata or raw invalid
answer/errors. Exit0/2/3/4/5/6 distinguish success/input/schema/recovery/timeout/
owner failures. Default two corrections (cap five) are ordinary saved user messages,
not transport replay. A read probe obtains the existing instance so every later
RPC has a known, reported operation ID even if its reply is lost. Deadline expiry
never issues stop/retry. A new read-only history projection is necessary because
ordinary history text includes reasoning; isolate its cursor scope and preserve
all ordinary defaults/storage. There is no new Core execution mechanism.


JSON instance indexing is limited to100000 nodes/depth128. Numeric parsing uses
the existing serde_json supported range; out-of-range numeric inputs fail explicitly.

## 2026-10-31: fact-preserving Chat tolerances (#68 D-010 / #64 review)

Active, superseding the strict stop-with-calls/name/id/choices handling in the
initial #64 adapter. User/reviewer explicitly authorizes four bounded tolerances:
complete calls with stop; repeated complete declared names; stable derived call
ids from response id/index; omitted choices on usage chunks. Persist original
finish reason and normalization facts in output-item metadata because successful
Core persistence retains output items, not the entire HTTP envelope. Missing
both identities and incomplete arguments are not repaired. Keep capacity-error
classification unchanged: string codes only, no vendor-message inference. No
paid/live-provider validation is authorized; no new Core table/worker/queue.


## 2026-10-31: generic terminal status without renderer changes (#54 / D-011)

Adopt default on, maximum32 child records plus root, no feature query/progress/
heartbeat. The only activity classifier is existing activity::classify. Use the
existing bounded read admission, DB worker, coalescing UI outbox and tick; add no
worker/poller/process/queue/table/dependency. A UI acknowledgment marks a selected
completion seen; session-start UTC excludes historical completions from new done
signals. Seen entries are pruned when deleted/replaced and never persisted.
The full-thread scalar scan is an event-coalesced aggregate cost, not an O(1)
claim; titles/history are projected only for retained children. Wire diffing
compares final encoded bytes; root clear is normal-exit only and off emits none.
Only UI metadata admission and frame-boundary IO are changed, not #41 draw/layout
or status-row code. Keep real receiver qualification separate from mock success.

## #73 bounded explicit summary decisions (2026-10-31)

Active, implementing user-approved [D-015 in issue #68](https://github.com/tya5/shellboy/issues/68)
and [issue #73](https://github.com/tya5/shellboy/issues/73). Defaults are 20,000
characters and 5% (smaller, effective floor 2,000). A present configuration section
may select just one limit; each CLI override replaces its matching configured
value, not the other limit. Source size uses replay-projected original characters.

Use a fixed 64-request per-invocation budget across map/split/reduce and four
reduce rounds. This bounds paid requests and nonconverging reductions without
adding a table/worker/queue. Existing validated part caches allow explicit retry;
revisit these bounds only with concrete workloads, never automatically run past
them. Reduction prompts increasingly favor shorter output and latest state;
semantic accuracy still needs source inspection. Reject over-limit or nonshrinking
source replacements without truncation. An explicitly summarized omission may
reintroduce information and increase selected context; report that caveat.
Automatic overflow behavior is unchanged. No actual provider call is authorized
or required by this implementation's loopback tests.

## #41 D1: verify physical query shape, not statement count (2026-10-31)

The first integrated P3 trace46963c9 reduced176→26 SQL executions but increased
VM work16,602→61,255. A kind filter let SQLite drive the modern selector from
nodes_retention and repeatedly scan the requested JSON IDs. Force requested IDs
outermost with an ordinary CROSS JOIN and existing node PK lookup; retain the
kind filter and add a bundled-SQLite unrelated-node scaling regression. Python
full-schema isolation measures selector30,146→4,037 VM steps; this guides the
change but does not substitute for native requalification.

Also filter user candidates only after the materialized128-member title prefix,
then run the byte-budget window over those candidates. Retain a singleton status
witness for zero users so path/pending exhaustion is still explicit. The synthetic
128-nonuser case drops15,953→3,336 VM steps in the Python probe. This changes no
source ordering, copy budget, title authority or public contract. Requalify native
before/after results rather than reporting fewer SQL statements as a speedup.


## #41 D2: reuse exact registered wait sources (2026-10-31)

Copy existing operation/source-execution identities, then join their saved shell
owner and pending key in the activity transaction. Do not add duplicated Origin
scope fields to the registry or infer ownership from OS-handle liveness. Handles
can be removed before pending-wait cleanup; driver identity can race a replacement
Run. The exact saved-owner join preserves that boundary. Snapshot128 is above the
normal CLI transport's24 concurrent ordinary requests; direct Control calls can
exceed that, so overflow is explicitly unavailable instead of an arbitrary prefix
that might hide the selected Run's oldest waits. Revisit the cap only with a real
admitted workload, and keep unavailable distinct from zero.

Background wait registration/cleanup currently lack a guaranteed Changed when
Turn state stays unchanged. Add existing factual notifications after registration
and committed deletion; retain existing CAS/stop/disconnect semantics. No new
state, worker, queue, table, protocol operation or registry lifetime is introduced.

### #41 D2 incremental output ownership

Keep one display-only decoder/cursor for the open live shell in the existing
frontend observer. Initial observation and retention/encoding changes use the
finite tail; contiguous updates carry each stream's decoder state. If saved
bytes outrun the4KiB read allowance or the128-row metadata window, explicitly
reset to a tail and mark skipped/uncertain instead of replaying an unbounded
backlog. Preserve incomplete code units until a committed complete terminal
snapshot. The cursor owns no CLI pin, durable offset or public read cursor.
Selection/visibility/shell closure cancels and drops it; stale replies cannot
install it in a new scope. Decoder state and display text share one open-shell
lifetime, with4KiB total retained text and4KiB total new bytes/read. These are
bounded active display working data, separate from retained history previews.

### #41 D2 validation findings before admission

The first full standard run failed three assertions. Two were real product
regressions: initial live-read admission recomputed an immediate Instant after
its caller's `now`, keeping the first read forever in the future; the new live
clock used the draft-touching UI accessor and scheduled redundant autosave
signature checks. Fix admission against the saved optional deadline and use a
presentation-only mutable accessor. Keep the original assertions.

The remaining overflow assertion was a test harness defect: concatenating every
Ratatui cell inserted continuation blanks inside wide Japanese text. The last
Wait row's original label already fit its40-column allocation. Reconstruct rows
using Unicode display widths, preserve real spaces/newlines and retain the exact
visible overflow/command and hidden-ID assertions. The separately added width
reservation protects overflow when secondary timing grows; it was not a third
reproduced product bug. The second run's886/1/13 isolated this harness failure;
the first run's884/3/13 and neither partial run are passing qualification.
Record the later exact source and full standard result separately.

## D2: 親への停止通知をDriverなしの経路にも揃える

復旧待ちの子RunはliveのままDriverが終了できるため、後続の停止は通常のowner nudgeを通らない。表示中3件の依存追跡だけでは、範囲外の子を停止した際に親の件数が更新されない。既存stop target取得にlive/stoppingの親Threadを含め、Driverなしのdrainがcommitした後だけChangedを送る。親モデルは起動せず、失敗時は既存の子Errorを維持する。新しい照会・状態・通知型は増やさない。再確認条件は停止処理の所有境界またはDriver終了時の親通知責任が変わった場合。

D2's third standard run passed888 library tests but stopped at one integration
assertion expecting answered-background progress to request periodic polling.
The adopted reactive design deliberately returns false for this cadence while
preserving the factual background label and actual-spawn count. Update that
single boolean expectation and document the helper's return meaning; the
stopping-Run expectation remains true. This is an intentional cadence contract
change, unlike the initial-admission and autosave defects above.

A subsequent audit found a genuine snapshot-time race: deriving the live-work
monotonic base in delayed presentation could discard a newer output marker as
if it belonged to an older DB observation. Capture the paired monotonic/wall
base before returning raw SQL facts and prepare ages against that captured base.
Also reconstruct trimmed cursor strings when retained capacity exceeds the
allowance, because trimming length alone does not release allocation. Neither
finding is qualified by earlier green library tests; repeat current-source checks.


D2 output observation/preview delivery uses the existing latest-value outbox
instead of ordered critical-result slots. A stalled UI must not retain one
critical record for each output marker. Marker updates therefore carry all
currently observed displayed execution scalars (maximum3), not just the changed
subset; replacing a pending update preserves a different earlier shell's known
marker. Full preview snapshots are replaceable by Thread/generation as well.
Receipt/business results keep ordered delivery. Pressure tests exercise1,000
updates per kind with a full raw UI channel and require two latest entries,
zero critical entries and the newest values after draining. No channel/queue or
capacity is added. The previously qualified45b8b15 source predates this follow-up.

## #73 review / D-017 (2026-10-31, active)

Supersede the shared 64-request and automatic-path exclusion decisions above.
Use independent map 128/reduce 16 default request budgets, configurable in 1..4096;
reject a known excessive uncached map plan before sending any model request.
Keep legacy map prompt/key identity independent of final-size policy; reduce
caches remain policy-specific. Apply identical limits to automatic stage 2 and
argument-free compact, leaving #50 recovery handling unchanged. Size-free omission
commands honor user configuration. A request-budget-only configuration section
uses D-015's default size policy; single size keys retain their existing meaning.
Source: reviewer request, issue #73 review, issue #68 D-017. No new persistence or worker.


### #41 E admission: readonly display snapshots and #77 test boundaries

Decision: E uses short-lived READ_ONLY/query_only WAL connections on the existing
finite presentation pool for explicit browser pages, bounded ID patches and the
paired visible conversation observation. Saved-summary meanings and title
extraction remain shared with canonical CLI readers; dirty sets, UI admission,
ordering and formatting belong to Frontend/UI. No new Core worker/table/queue,
protocol operation, schema migration or polling loop is introduced.

Reason: coalescing writer tasks still delays persistence; finite display work
can instead pin one consistent WAL snapshot without waiting for that queue.
Admission-ordered read tokens conservatively prevent older snapshots from
superseding newer history reads. Busy/locked/interrupted display reads remain
retryable rather than triggering the owner's persistence failure latch.

Limits and revisit conditions: canonical initial/filter/pagination still uses
its existing per-row title/excerpt path on the readonly connection. This is a
writer-ownership improvement, not proof of constant page cost or improved total
CPU. P3/P5 before/after measurements remain required evidence; change the page
query only if measured cost justifies a separate bounded refactor. E/F/G remain
separate admissions, superseding the earlier design's optional E+F grouping.

The E fixture uses #77's exact shared60s watchdog helper at crate harness scope.
Its bounded update forwarder waits on actual UI notifications and the native
loop's pure visible-action delay. Latencies are recorded, never used as a speed
assertion. #77's independent broad timeout cleanup remains on its own branch;
its scanner is used against the E fixture files without copying its unrelated
product/test changes or granting file-wide exceptions.

## #75 saved readback placement (2026-10-31, active)

The user authorizes these core interfaces as generic bounded reads of existing
facts, not a supervisor-specific engine. Reuse finite presentation admission and
read-only WAL connections, retaining writer ownership for mutations. Select whole
usage lifetimes by saved starts (Run creation) in half-open periods; never invent
unknown times or costs. Bound shell candidates and snippets, redact obvious
credentials conservatively, and expose continuation even for empty filtered pages.

Schema15 adds four justified access paths: global/owner shell start time, sparse
compaction receipt target and sparse automatic attempt. Existing model/Run indexes
serve periods. The measured index sizes/write cost and legacy/retention gaps are
explicit in validation/storage docs. No new tables, workers, queues or denormalized
source copies. Revisit only if real scoped-query workloads justify another access
path; mocks/query plans are not real-provider or general latency guarantees.
Source: issue75 and user-approved query-plan/read-only/watchdog/redaction constraints.


### E paired-observation fixture admission (provisional diagnosis)

The same product tree passed once, then its integration observer saw List77 and
current-view78 after a title edit. The fixture's initial readiness accepted any
browser patch, including metadata-only work, rather than confirming the selected
row's observation. Require the actual selected excerpt result before editing;
retain the strict one-observation, same-sequence and no-reload checks. The
separately gated real combined-read test still proves one shared WAL snapshot.

Initial-work overlap is an inference from the insufficient readiness condition,
not a traced cause. Focusedc19e5c1 now passes. If the full check or native CI fails
again after this stronger gate, capture ticket IDs/admission and result sequences
instead of relaxing assertions or adding sleeps/time budgets. No product cadence,
queue, state or sequence policy changes on this evidence alone.


### E CI follow-up: simultaneous deadline admission

The selected-excerpt gate alone did not remove CBC's Linux/Windows sequence
mismatch. Source inspection identifies a separate counterexample: both deadlines
can be ready in the same select, whose browser arm immediately dispatches an
independent read and advances the shared deadline before the selected view's
outer admission loop runs. Yield that arm to the existing loop whenever its
selected refresh deadline is already due. Browser-only reads remain unchanged;
no new scheduler, queue, worker, event or timer is added.

A real separate-CLI integration now performs16 consecutive mutations while
preserving exact one-observation, paired-sequence and no-reload assertions under
one shared60s watchdog. Focused macOS execution passes all16 changes. This
supersedes treating readiness alone as a sufficient repair; the earlier failures
remain visible. The full check and new native CI are still required.

The unrelated raw-save stop case replaces8s/250ms/5s test guards with #77's
shared watchdog and its75ms absence assertion with post-drain request and durable
attempt counts. The saved stopped state and SaveFailed shutdown remain required.
The existing short polling sleep stays inside the test-only saved-state waiter;
there is no new product polling. Passing a60s deadlock guard does not explain the
original eight-second delay or create a product shutdown-latency guarantee.


### E integration with #77 and managed-path main (2026-10-31)

Merge main0d4f63a, preserving #77's canonical harness modules, count-after helper,
policy checker and all unrelated test conversions, plus #79's managed-path read /
file-observation boundaries. Keep the E raw-save post-drain request/attempt checks
and its descriptive watchdog labels when resolving overlapping guards. Register
the shared lib helper once; no duplicate module or local watchdog constant.

Remove9 exact obsolete frontend/frontend_reactivity guard allowances. Add6 exact
allowances for three UI tests that pass sampled future Instants to pure debounce /
coverage methods. These are input data, with no sleeping, elapsed-speed assertion
or real-time acceptance deadline. Retaining the source and explicitly explaining
each fingerprint is preferable to disguising it to evade the lexical checker.
There is no file-wide exemption or new short asynchronous guard. The merged
layout/watchdog policy and Python18 tests pass; Rust standard qualification is
performed separately. F/G and migration changes remain excluded.

## #41 historical P3 fixture state correction (2026-10-31)

While preparing E's P3, inspected the retained instrumented frontend modules in
both original detached baseline1fec7f0 and candidate216b213 worktrees. Each SQL
seed inserts measure-run as completed with ended_at, contradicting the B+C
qualification document's phrase saved live Run. Correct that phrase and retain
the historical counts/times with their actual completed-Run scope. Do not infer
live/background or E/F combined-consumer performance from those values. New P3
must assert actual saved lifecycle states along with node/tool population and
consumer scope. This is an evidence correction, not a change to run semantics
or an invalidation of the observed completed-Run query-cost measurements.

## 2026-10-31: Unix lifeline ownership (#82)

Use one native guardian per live Unix execution. Its PID reserves the model PGID
while it waits outside that group, so existing normal TERM/grace/KILL probes do
not count the guardian as surviving work. A short-lived setup child creates its
private group, preventing an owner-group SIGKILL from killing the guardian too.
Steady owner/guardian overhead is two FDs each (lifeline and status), not just
the proposal's one owner FD. The status acknowledgment preserves the existing
natural-exit/late-stop contract; exec-error acknowledgment is startup-only.
All use the existing supervisor task; no runtime worker, queue, table or package
is added. The low-level fork branches perform native operations only.

Do not add a persistent PID registry or attempt startup kills by old numbers.
Startup records OS-unverified Unix shells as failed rather than claiming stopped.
Escaped groups, intentional guardian tampering, kernel-stop latency and observing
old-version survivors are outside this minimum implementation. Windows Job and
normal owner shutdown policies are not changed. Revisit the helper layout only
if native tests or a future shell runtime change invalidate these bounds.

## 2026-10-31: Minimal command-prefix provenance (#84)

Treat the whole leading slash name as manual only when it lies in the contiguous
manually entered prefix. Paste can truncate trust but cannot grant it; a typed
slash alone cannot authorize a pasted name. This bounded local counter is
skipped by serialization, keeping saved formats/Core/protocol unchanged.
Snapshots inside the UI retain trust; reloaded/recalled source text does not.
Choose an inline keyed warning instead of an execution/submit dialog.
Conservative edits can require retyping the name after an insertion/deletion;
do not add per-character tracking merely to recover trust. Revisit if concrete
editing workflows show the minimal prefix rule is insufficient.

## Local E reconciliation with main2327636 (2026-10-31)

Prepare the E admission branch against current main including #79 worker release,
#82 Unix lifeline and #84 typed slash/pasted-argument fixes. Preserve both append-
only development/validation histories; Rust merges automatically. This is local
preparation, not E main admission or another push approval. Frozen published
measurement7c6f4d6 stays unchanged. Original P2/P5 and P3 source witnesses remain
0d4/7c and0b respectively; do not relabel them as measurements of this merge.
Recheck the combined local source with the standard suite. Keep #71 admission,
#54 conditional tick, canonical #77 guards and #84 provenance behavior intact.


## F Watch witness admission (2026-10-31)

Treat the16KiB row limit as the sum of eligible command JSON, recovery and answer
sources, rather than allowing16KiB separately for each. A row exceeding that
sum leaves its source fields explicitly unavailable; oversized individual
sources do not prevent an unrelated small field from being considered. Rows
are admitted deterministically by Thread ID under the64KiB aggregate ceiling.
Charging the complete command JSON avoids admitting128 large commands merely
because their512-character labels are short; UTF-8 labels may use2048 bytes.
This conservative row-level policy may show less preview than a field-by-field
packing algorithm, but avoids partial body copies and a new scheduling policy.
Originals remain available through existing navigation. Revisit only if measured
real usage shows too many unavailable previews, retaining pre-copy byte bounds
and explicit unavailable reasons. Stored parent title copies separately cap at
4096 bytes; normal public titles are already limited to512 Unicode characters.
The source budget does not describe total Watch cache or RSS.

## #80 minimal on-demand memory (2026-10-31, active)

Source: issue #80 and the user's scoped implementation request. Choose the status
--memory flag, existing cache budget accounting and native process APIs. UI cache
figures use the existing event/update queues and a per-request completion responder;
that responder is reply plumbing, not an additional work/admission queue. Native
reads reuse the existing control-file slots, after UI counter capture and outside
the DB lane. There is no periodic sampler or new production counter. OS failures
and absent UI ownership remain null with reasons. No allocator/SQLite estimation,
leak diagnosis or request-copy optimization is included. F8 display is delegated
to #41 because its published branch overlaps the diagnostic/rendering files.

## 2026-10-31: minimal conversation row vocabulary (#83)

Adopt a single display table with EAW=N marks rather than ambiguous `●`/`◇`/
`◐`; reuse palette roles and named copy keys, without a translation framework.
Keep spacing in display/source-row maps, not storage or item counts. Live
seconds repaint cached rows from a saved-start/monotonic observation, with no
DB poller or Markdown relayout. Correlate notification sources only in the
bounded loaded view; do not hydrate shell output merely by selecting a notice.
Choose Enter navigation because baseline F7 is HTTP-only. Do not alter #86's
region rendering. Defer missing command/child names and off-window source
lookup to a bounded read-projection follow-up, rather than changing Core events.
No dialog, new worker, queue, schema, protocol or
theme setting is required. Existing pending DTO work in #74 is not modified.

## #89: Observe committed boundaries, not scheduler or diagnostic admission (2026-10-31)

Status: active. Issue #89 and the #77/ad6c54b test policy apply. The three
failures have concrete test-side counterexamples: independent context metadata
commit, valid acceptance before native handle registration, and permitted
diagnostic drops. Preserve production ordering and grace; add only a test-only
registration gate, saved-fact/notification observation, and durable/native
shutdown gates. Listener teardown is the repeated-signal acknowledgment, not
its best-effort diagnostic line. This does not claim every native lifecycle
race is impossible or attribute the missing handle to lifeline termination.

## 2026-10-31: bounded saved Thread observations (#74)

Use indexed terminal Run rows plus existing child_recovery_wait facts, rather
than a new subscription service or clock-only cursor. Root recovery also saves
that existing fact without a delivery because resume clears the current reason;
otherwise an unobserved recovery/resume cannot be reconstructed. This is one
small additional root-recovery write, not a new lifecycle or event type.
Query-scoped per-stream row positions/ID anchors avoid wall-clock reordering;
round-robin consumption preserves prefixes/fairness under the64-change cap.
Source rows are retained while Threads exist; removal/replacement rejects anchors.

Latest answer uses the own final Turn pointer, not arbitrary assistant tool steps
or fork-only ancestors. Latest input uses received receipt order, including held/
cancelled, with existing safe sender facts. This is the smaller first version;
no new UI watcher, idle-without-Run condition or fork-answer expansion is added.
Revisit these cuts only with a concrete consumer. Four derived indexes/schema16
support bounded seeks. Body excerpts are bounded in fragments/returned characters,
not a claim of zero source-JSON parsing cost. No table/worker/queue/dependency is
added. #80 and #83 were integrated before shared files were edited; UI changes
are optional DTO initialization only, not rendering/row/composer behavior.

## #76: Minimal fact-only usage-limit cooldown (2026-10-31, active)

Use only explicit usage_limit_reached plus a future valid integer reset, or
HTTP 429 Retry-After above the existing 600-second automatic-wait ceiling.
Generic 429 and other HTTP statuses are not inferred to be plan quotas.
Keep one fixed optional fact/monotonic observation/duration per configured
shared client, never a growing map of arbitrary provider keys or persistent
state. Reuse owner lifetime; expiry is lazy and restart forgets memory.
Retain the later concurrently observed reset. Fail locally before HTTP but
preserve the existing attempt/recovery path and mark the absence of sending.
No auto-switch/resume or input-preparation optimization. Header facts suffice
without a possibly stalled error body. Existing reason text/run get are the
initial UI boundary; bottom-line presentation belongs to #85 to avoid #41 F/G.


## 2026-10-31: Reuse-only CI integration (#92)

- Status: active; reviewer-approved scope replaces the proposed Windows split.
- Decision: reuse first-attempt successful three-OS checks on the exact main-push
  SHA, with read-only API evidence, logged source run and full-check fallback.
  Preserve manual execution and the original unsplit four-thread Windows job.
- Reason: repeating build/link work in two Windows jobs gave little wall-time
  benefit. The two surfaced test failures move to #95; their assertions remain.
- Validation: qualify the final unsplit source on all three OSes, then verify
  actual same-SHA main-push reuse. Mock API fixtures are not live verification.
- Revisit only with measured build-sharing savings and separately fixed tests;
  do not reintroduce split execution as part of this reuse-only change.



## F parent-index renumbering after latest-main integration

Main a0f7022 already owns schema16/0016_thread_observation. The unpublished F
parent index is renumbered to17/0017_thread_parent, preserving all main lookup
indexes and migration identities. Bootstrap, atomic upgrade and rollback fixtures
follow that numbering; there is no compatibility alias for the unpublished
local candidate. Recheck the next free version before F admission. E remains
independent and uses main schema16. Back up the complete stopped data directory
before opening any release containing the F upgrade.


## Shared visible-read cancellation remains in Frontend

A captured plan has at most the existing browser and descendant-count consumers.
Use their existing cancellation tokens directly, without spawning a listener:
wait for both to close when both exist, and for the single token otherwise.
Closing one must preserve work for the other. The existing readonly snapshot
helper accepts a captured ownership predicate checked before admission and
immediately before SQLite open on the finite presentation worker. An already
executing read may finish and still passes its existing stale-result fences.
This closes the queued-read gap without Core state, a new event/table/worker,
separate cancellation queue, or an unbounded family of subscriptions. Revisit
only if another actual bounded consumer joins this same snapshot.


## F shortcut mode memory without scope persistence

The final F requirement audit found Ctrl+T shared /threads' explicit-List route,
so the previous-mode contract was missing. Reuse the existing browser enum to
choose the existing global Watch or List entry path; do not add a preference,
configuration key or Core state. Only mode is retained. A previous descendant
root/filter is intentionally cleared, because after switching conversations it
could silently monitor an unrelated old root; explicit /threads descendants
or Alt+D selects the current saved Thread. Revisit if a separate scope-memory
requirement is adopted. Slash commands remain explicit and a new UI starts List.
## 2026-10-31: Separate input-band decoration and isolate clipboard checks (#91)

Use three explicit display parts, with non-RGB layout-only decoration markers
excluded from source matching and stripped before terminal rendering. Preserve
body wrapping and foreground/link styles; omit the whole metadata group before
sacrificing body. Sender labels are unverified receipt facts, not authority.
Keep existing source glyphs and move their labels to optional right metadata;
external/unknown input uses a provisional distinct width-one shared-table glyph.
User selection among ⤷, ⇥ and ⤳ blocks main integration. No per-character source
or copy API changes, schema/protocol changes, additional query or worker is needed.

After a test-side Auto-backend clipboard write, require explicit OSC52 in
synthetic fixture configs, preflight shared PTY startup, and inspect OSC52 bytes
only inside pipe-drained PTYs. GUI capture is unsuitable for clipboard checks
because it can forward OSC52 to the host; reject clipboard-related GUI input.
Routine tests use scratch config/data and injected clipboard backends where
appropriate. This is test isolation, not a guarantee about arbitrary programs.

## #91 external mark selected by the user

2026-10-31, active: the user selected candidate B, ⇥, from the recorded real-PTY
comparison. The common External row remains labeled 外から, uses the existing
user-marker palette and has width_cjk()==1. Candidate A was provisional only.
The reviewer states this visual choice is approved; qualification/integration
remain delegated to worker4. No confirmation dialog, color or protocol is added.


## Adopt #41 count-based performance acceptance6543044236

The user/reviewer explicitly removes p95/CPU/DB-work-duration and strict-bin
acceptance thresholds, with one available measure run sufficient. Retain zero
UI/display jobs/reads/draws, indexed SQL/query/batch/cache and total-frame counts;
record timing/CPU and alert review only to an obvious measured regression.
All32 run41193740385 windows satisfy the retained scope counts;601 frames are
within ceil(actual elapsed seconds), including partial endpoint opportunities.
Unrelated Core maintenance is separately classified, not silently zeroed.

Do not adopt strict-bin-only clock-pacing prototype5847799 or dispatch the
optional Watch follow-up81c6f37. Both remain preserved; the already qualified
f286c5b feature source is the admission base. This avoids adding UI pacing state
for a superseded guarantee. Remove the optional P4 p95 assertion while keeping
its cardinality/one-statement assertions and observation output. Functional
configured deadlines/coalescing and watchdogs remain. Revisit only if a new
explicit count/behavior requirement or obvious observed regression warrants it.
## #96: publication and post-commit observation boundaries (2026-10-31)

Counterexamples are separated from original CI attribution. The old Ubuntu
status is reproducible without #91 rendering; close its foreground receiver
publication window, rather than retrying a rejected Background or weakening
end-to-end output assertions. Publish only after a collector owns the process;
clear receiver ownership if the running-state write fails. Existing sticky
per-call release and no-later-call delivery remain authoritative.

On Quit, accepted sends must finish independently of optional display reads.
Cancel only post-commit HistoryReader::saved observation with the existing
frontend token. Do not enlarge the send/presentation pools, discard durable
submissions, or add another worker/queue. Keep ordinary UI result retention and
Core shutdown order unchanged. The old Windows trace lacks a pool witness;
report the controlled deadlock class and that qualification gap explicitly.
## #95 ordering boundaries (2026-10-31, review required)

The Windows split failure recorded three saved attempts, not three proven HTTP
sends; its assertion omitted per-attempt phase/status and actual JSONL. Do not
infer the original CI timeline or duplicate provider charges from that count.
A separate deterministic counterexample is real: a ready permanent header loses
to an expired header timer, fabricating a status-free retryable failure. Preserve
ready response facts after cancellation, before the local header deadline. Keep
pending-header timeout, unknown-transport retry policy and strict total deadlines.

For E, the prior simultaneous-deadline guard misses pending selected changes
while an older read has no refresh deadline. Reserve the existing next joint read
and wait for an older browser ticket before selected admission; otherwise its
pending IDs can be consumed independently. This uses existing dirty/inflight
state and notification wakeups, no new queue, worker, epoch or timer. Codex was
notified in #41 comment 6541664736. Exact CI ticket traces are unavailable; source
counterexamples and red/green virtual-clock tests, not rerun success, justify the
changes. Require user Core review and delegated Windows split repeats before
claiming qualification. No #5 implementation or CI-sharding change is included.

### #95 third observation: backward-window intent (fixture-side)

The pinned-tool fixture dispatched an ordinal0 non-tail window while still
following the newest page, and only disabled follow after waiting for that old
node. That is a circular prerequisite when the initial tail is not yet in the
cache: on_history_window correctly requests a real tail rather than replacing
a following frame with a non-tail slice. When the tail is already cached, merge
can cover the tail and hide the fixture mistake. Establish paused navigation and
its anchor before dispatch, matching actual backward navigation. Keep the exact
old guards and body/metadata assertions. A pure two-order regression checks both
states without time; no UI product policy changes for this third case.

## #95 return: periodic work is not another Changed event (2026-10-31)

Use the existing selected-refresh in-flight set to suppress redundant periodic
poll requests while a snapshot is already being read/projected. Do not clear
real dirty work, suppress a real Changed event, deduplicate delivered facts to
hide extra reads, or invent a new epoch. Idle activity can stop polling without
a tick echo left behind; live activity resumes the existing cadence after the
read completes. Real Changed during a read retains epoch invalidation and its
next read. Keep the strict E observation/sequence/reload contract unchanged.

An old-rule red fixed-order test and new-rule green test establish this source
class; three local original integration runs are separate regression evidence,
not historical Windows scheduling proof. The old product's three green split
runs do not cancel its failed ordinary Windows run. This source change requires
Core review and fresh exact-source native CI by the integration worker.

## #95 post-F alignment: preserve incorporated guards (2026-10-31)

Use F as the source of truth for browser observation. Its admitted8677807 and
307e531 already retain the approved periodic/pending-selected rules. Remaining
approved work is HTTP classification plus navigation tests, not a replay of the
old E frontend. Restrict the source delta to four independently byte-compared
files and retain all F guard/state modules exactly. #100 is a separate review;
prior0ce qualification is not reusable as proof for this new product source.
## #100: fitting offset coherence on F (2026-10-31)

After F landed, reproduce before changing production: existing membership
coverage and the preserved painted-row regression are complementary, not reasons
to weaken either test. Prefer showing every row when the visible list fits over
preserving an unnecessary blank bottom. Keep the selected ID and ordinary
overflow/collapse behavior. Reconcile Watch's actual TableState before its
bounded formatting bands and visible-ID scope; do not broaden formatting work.
The patch is confined to presentation, with no new state/epoch/table/index/
worker/queue/pool/timer and no replay of the unreviewed E browser draft. User
binary, owner and databases remain unchanged; Core/UI review is required.

## G saved Run/Turn bounded reads and latest-Turn tie ordering

Retain initial saved usage totals and their canonical unavailable-scope marker
when moving explicit Run/Turn details to readonly snapshots. Reuse the existing
saved-usage helper, lifetime calculation and pure context-reason decoder; these
are internal domain semantics, not a new Core operation or read-model engine.
Bound JSON copies before decoding, and make oversized lifecycle scalars explicitly
unavailable instead of silently truncating identities/timestamps. Owned ID lists
use finite per-state indexed prefixes and explicit128-row coverage limits.

Do not infer bounded latest-Turn work from a point result or query count. Current
main17 has turns_by_run(run_id,created_at), but canonical ordering also uses id.
Local Python SQLite3.53.0 diagnosis at100/10000 equal-time rows showed1319/130016
VM steps and a temporary tie sort; extending the existing index with id showed
17/14 steps and a covering seek. This preflight is not bundled-Rust qualification.
G9 must confirm the complete query and migration/rollback using bundled SQLite;
recheck the schema slot and backup requirement before adoption. No production
schema/index is changed by this checkpoint. Revisit if current main supplies a
suitable covering index or canonical ordering changes.
## #5 stage 2: startup-only adapter and failure observation (2026-10-31)

Use the shared owner_session assembly and cleanup, keeping TUI in-process. serve
requires explicit nonempty data-dir, performs ordinary config resolution and
emits a flushed ready JSON only for an assembled IPC/dispatcher. It never obtains
owner identity from inherited CLI endpoint/origin variables. Existing per-tool
identity assembly is reused; Windows clears an inapplicable inherited Unix
socket. API/proxy environment and runtime global environment are not rewritten.

Readiness JSON requires a UTF-8 absolute path; reject a nonrepresentable path
with cleanup rather than fabricate a lossy directory identity. Startup errors
remain stderr, distinct from normal CLI Response stdout. The ready-write failure
fixture uses an inherited read-only file, not scheduling sleeps or a production
fault-injection hook. Tokio output() would replace that handle, so use explicit
spawn/wait_with_output. No new table, worker, queue or owner mode-specific Core
engine is introduced. Graceful shutdown, signal ordering and final lifecycle
rows are stage 3, not a claimed property of this startup-only increment. Core
review and integration approval remain required for the final stage 2 source.

## #5 scope boundary after F (2026-10-31)

Keep owner replacement without interrupted work out of this implementation.
No exec-in-place transfer, inherited live shell handover, reconnect protocol or
late TUI attach is introduced. #82 cleanup ownership remains unchanged. Stage 2
only provides explicit terminal-free startup and existing CLI access; stage 3
will separately define shutdown acceptance/reply ordering before implementation.

### #5 stdout failure observation correction

The read-only handle fixture is superseded: Rust stdout may discard EBADF as a
successful write. Use the standard portable pipe writer with its reader already
closed to obtain a real BrokenPipe, without a production injection hook or
scheduling sleep. Verify cleanup at the real failed-write boundary, retaining
the existing shared watchdog and assertions. This changes the fixture, not
startup/shutdown behavior, and does not broaden stage 2 into handover/reconnect.

## #99: Separate disclosure outcome from output progress

A tool's stable group/call identity owns disclosure. Its outcome class (running,
success, failure, cancellation, unknown) decides when an explicit choice can be
reset; equivalent saved/current terminal spellings and numeric exit metadata do
not. Saved output sequence/completeness decides whether an open snapshot can be
refreshed. This preserves explicit closes during metadata-only observations and
prevents completed-output timestamps from becoming a redraw clock. Revisit if a
future contract permits one group/call to acquire a genuinely different execution;
such a change needs an explicit identity transition, not a display-string guess.
## #5 stage 3 shutdown ownership

The shutdown handler commits acceptance and closes admission, but must not drain
its own active-request scope. Reuse existing reserved stop admission and retain
one optional transport-completion token only for shutdown invocations. Release
request/transport admission before signalling the adapter. The adapter owns the
existing snapshot/cancel/drain/DB close sequence. A fixed optional Control token
marks serve capability; TUI does not enable it. Saved acceptance is not restart
intent and expected instance identity prevents stale shutdown of a new owner.
No new table, queue, worker, timer or capacity is needed. Tests count committed
facts and side effects at named boundaries, not elapsed shutdown seconds.

## G reconciliation with #5 headless lifecycle controls

Main66d2746 introduces serve/shutdown and OwnerShutdown. Core's new tests require
TUI shutdown requests to be rejected before receipts, while the headless adapter
owns admission/reply/drain. Treat these CLI paths as explicit lifecycle inventory
alongside launch controls, not fake F8 actions on the current TUI owner. Preserve
all new Unix-only signal conversion, preview lifetime (#99), owner admission and
shutdown semantics. Catalogue matching remains exhaustive with an explicit None
for the no-form OwnerShutdown; command captions remain defined, so an externally
constructed UI request is rejected by Core instead of panicking in the caption.
Reason: exposing these as normal current-owner forms would contradict main's
adopted transport boundary. Revisit if supported TUI/other-owner lifecycle
semantics are added; this is a reviewer-visible integration judgment, not a new
Core capability or silent coverage omission.

## Retained history current facts: separate presented metadata, no second body cache

G reuses the original management-detail Arc unchanged and retains one small,
last-presented metadata Arc for `/current_source` pointers. Resolving against the
latest read before presentation would pair a displayed request ID with future
facts, so publication and detail adoption share the existing presentation fence.
Metadata is bounded by200 source IDs,128KiB encoded input, scalar/copy/channel
limits and the existing detail retention budget. The UI retains at most256
original field descriptors, never another original body. New-result/owner exit
releases these references; hiding cancels reads through the existing visibility
scope. The one RetainedInspectionStarted delivery carries only request identity
and the finite typed scope on the existing bounded update channel; no new queue,
worker, persisted state or Core operation is introduced.

HistoryGet supplies no selection Thread. Origin provenance and the current UI
owner are not authority to invent one; current selection remains unscoped.
HistoryList retains its literal Thread. Wire list snippets do not supply a
selection revision, so observation reports the actual current revision without
claiming the old list cursor is still valid. Node revision matches likewise do
not authorize raw output or prove external file availability.

Current history observations conservatively coalesce existing Thread changes
while visible, plus existing GC availability invalidation. This avoids a new
persistent node-to-observer index or per-node notification protocol. The cost is
one bounded metadata refresh on unrelated committed changes, at the existing
500ms cadence; quiet/hidden views must still admit zero work. Revisit this choice
if actual combined-consumer P3/count tests show excessive admission. Narrow it
using already-known exact dependencies before adding infrastructure. Initial
explicit payload/list reads and the other saved routes still require their own
readonly/source-cost qualification; this helper is not a blanket completion.


## #41 count policy: remove invented universal flat-VM gate

Independent audit against user/reviewer6543044236 and6520522032 confirms that
constant VM work under own-Thread cardinality is not a universal requirement.
The explicit gates are bounded targets, indexed SQL, queries/request, cache and
frame counts. Descendant depth8/256 and other adopted numeric caps still apply.
Earlier notes demanding constant total SQL work for every exact aggregate were
stronger than the request. Keep scope isolation/source-copy evidence and record
own-cardinality growth honestly; do not add denormalized counters or discard
known values solely for that invented gate. Current Run tie-order index tests
compare a useful optional optimization, not a mandatory migration. Revisit if
actual scoped work starves the finite pool or a concrete new requirement needs
stronger bounds; timings remain observations, not current acceptance thresholds.

Header's display-only128KiB path allowance avoids treating512-byte identity limits
as a valid path limit. Oversized settings fail explicitly with a field witness;
canonical CLI values remain intact. Native synthetic tests can qualify these
copies/semantics, not Windows filesystem/path execution. No schema/worker/queue.
## Disable Development Debug Information (2026-11-04)

Set `[profile.dev] debug = false`; test inherits this profile. The target is the
large set of unpacked macOS codegen object files, not panic source locations:
those remain available in panic text without debug info. CI and workers do not
set `RUST_BACKTRACE`. For a one-off build needing line-number backtraces, run
`CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test --locked --no-run`.

Measured on this Apple Silicon Mac using Rust 1.99.1, fresh throwaway
`CARGO_TARGET_DIR` under `/private/tmp`, `cargo test --locked --no-run`, and
`du -sk target/debug`; three cold builds and one-file incremental rebuilds per
profile. Before `line-tables-only`: clean 489.12/423.61/293.30s (median
423.61s, range 293.30-489.12); incremental 2.90/4.15/2.65s (median 2.90s,
range 2.65-4.15); target size 5,245,940/5,260,688/5,279,620 KiB (median
5,260,688 KiB). After `debug=false`: clean 156.33/139.28/292.69s (median
156.33s, range 139.28-292.69); incremental 2.86/2.76/3.90s (median 2.86s,
range 2.76-3.90); target size 4,182,880/4,174,832/4,205,828 KiB (median
4,182,880 KiB). Local clean and incremental medians did not increase; target
size fell about 20.5%. The wide cold-build spread overlaps, so interpret the
median, not each run in isolation.

Three exact-SHA default CI runs for each profile on main-equivalent source:
before runs 41931953415, 41931957475, 41932009257 had Ubuntu check durations
652/538/623s (median 623s, range 538-652). After runs on candidate SHA
`a91dc4ad96abc3732469c0b03f89f5c552791b6a` were 41936385665 (547s),
41936389385 (418s), and 41936393572 (637s), all successful (median 547s,
range 418-637). These fall within the measured baseline range. The durations
are GitHub's Ubuntu job `startedAt` to `completedAt` intervals. No
schema/migration or product behavior changes.

## #105: Line Tables For Development And Tests (2026-11-01)

This was the initial development profile choice, adopted on 2026-11-01 and
superseded on 2026-11-04 by [Disable Development Debug Information](#disable-development-debug-information-2026-11-04).
The original rationale and measurements below remain historical evidence. To
obtain source-line backtraces for a one-off test build, use
`CARGO_PROFILE_DEV_DEBUG=line-tables-only cargo test --locked --no-run`.
Release settings, the encoding_rs optimization and the existing #46 test
aggregation remain unchanged.

One frozen macOS/aarch64 cold pair (main5ecbebc versus profile-only7a4bfc88)
passed2125 Rust tests/35 ignored and71 Python tests on each side. Allocated target
size fell5773640 to4680264 KiB (18.94%; `du -sh`:5.5G to4.5G); the main executable
fell111329272 to107014568 bytes. A post-snapshot Cargo example panic retained
stack locations at source lines2 and3. Both temporary targets/probe were deleted.
Elapsed440.95/427.87 seconds are observations, not gates. Details and provenance
are recorded on issue105; latest-main checks/3-OS CI are separate qualification.
## #72: Optional External Error Catalogue (2026-11-01)

Use a Python-standard-library script and an independently replaceable derived
SQLite file, not Rust Core, an owner query/worker, automatic guidance or UI/G.
The current conversation schema already provides exact shell/call/result/Turn
references and indexed output prefixes through a read-only SQLite connection.
Requiring explicit data-dir/build gives non-users zero startup/owner cost.
Bound the source set and prefix reads, sanitize before persistence, and label
recovery pairs as temporal associations rather than causal fixes. Prefer a
bounded rescan refresh over a rowid watermark: completions, GC and deletion can
change existing rows. No schema/migration change, background subscription, LLM
or new dependency is justified. Revisit a generic Core reader only if an external
current-schema reader proves insufficient, after documenting need on issue72
and obtaining the user's decision.

## #101 bounded grouped usage decisions (2026-10-31)

Adopt the issue's group-by-thread alternative to repeated Thread flags. Bound
periods to31days, scan windows to10,000+1 and response groups to168. Apply scope
filters after the candidate cap so selective scopes cannot cause an unbounded
scan. Cursor pages can therefore be empty or repeat groups across distinct scan
windows; mark page-local coverage and do not imply whole-period percentiles.
These choices prioritize countable DB/memory work; revisit pagination only with
an approved contract, not persistent aggregates. A single schema18 normalized
start index enables the otherwise missing global period seek. The index is
Core-review-required; no schema/table/worker expansion beyond it is proposed.
Use local/UTC/fixed offsets without a new timezone crate; IANA-name support is
outside this implementation and can be reconsidered with a separate dependency
review. Max attempt references include Turn/Run/Thread so history is reachable.

### #101 index proposal superseded by reviewer (2026-10-31)

The schema18 expression-index proposal above is rejected. Retain schema17 and
all existing indexes unchanged. Grouped reads instead page existing primary
rowids before filtering, cap candidates at10,000+1, and compare UTC started_at
strings to whole-second period bounds. The finite pass validates current UTC
format and exact half-open fractional boundaries. Non-current/missing/malformed
times are counted as excluded, never inferred. Only measured evidence may justify
a separate proposal for a plain started_at index; no such index is implemented.
## #119: Provider extras are opaque, validated by shape only (2026-11-01)

Add `extra_body` and `extra_headers` per connection. Shellboy does not interpret
them; it checks JSON representability, size (64 KiB body, 32 headers of 8 KiB)
and collisions with fields it owns, and passes the rest through. Reject a
colliding key instead of choosing a winner, so `reasoning`/`reasoning_effort`
stay owned by `default_effort` and no precedence rule has to be learned.
Reserved lists are per dialect and are the only place that must track Shellboy's
own request shape. Credential-like and transport-owned header names are
rejected; credentials keep one path (`api_key_env`/`headers`). The attempt
manifest stores only a SHA-256 of the canonical extras, never the contents, so
secrets accidentally placed there are not copied into the database (the config
file remains their only home). Extras apply to conversation and summary
requests, not to model-list or embedding requests. Revisit if a provider needs a
non-top-level merge, per-model extras, or a header that the credential filter
wrongly rejects.

## #126: Three tiers of provider settings; effort reaches Messages (2026-11-01)

Provider settings are (1) Shellboy concepts converted per dialect, (2) opaque
`extra_body`/`extra_headers`, (3) opaque response blocks returned unchanged. A
setting is promoted from tier 2 to tier 1 only when (a) correct placement needs
structure only Shellboy knows, or (b) it varies per Thread/Turn and Shellboy
already holds that state. Thread effort meets (b), so Messages now sends it as
`output_config.effort` (reserved key, rejected in `extra_body`); unset sends
nothing. One mapping function in the encoder: `minimal` -> `low`, other strings
unchanged, because there is no model catalog and the endpoint is the judge.
Thinking on/off stays tier 2. Real-API confirmation is pending approval and
balance. Revisit if a provider rejects `minimal`-style names we pass through or
a second per-Thread knob appears.

## #121: context_role maps inserted context at send time (2026-11-01)

Add `connections.<name>.context_role = "developer" | "user"` (default
`developer`). With `user`, `developer` items after the leading system message are
sent as `user` with unchanged text, in the request just before dialect encoding,
so every dialect and the summary path get it and saved nodes are never rewritten.
A per-connection setting was chosen over a global switch because OpenAI-style
APIs give developer a real authority meaning and only some gateways need the
change. The marker text (`Shellboy event v1:`) stays so the model can tell a
notification from user text. Considered and rejected: appending a synthetic user
turn only when the tail is not user (hides the real order and still changes
saved-vs-sent equivalence), and dropping the notices (loses delivered facts).
Known limits: two adjacent user messages after mapping are left to the provider
or gateway to merge. Revisit if a provider rejects consecutive user messages.

## D-018: API-free compact preview (#113, 2026-11-01)

Active. The reviewer authorizes #113 per its issue, after G main. Adopt a narrow
read-only compact.preview route rather than admitting/rolling back a mutation or
forking a conversation. Share canonical pure policies; bound collection and label
coverage failure separately from a predicted execution refusal. Summary after-size
is unknown without model text. The fixed collection envelope is documented in
compaction.md and can be revisited with row/byte/query evidence, not timing bins.
## #120 stage 2: opaque provider items (2026-11-01)

Keep provider-specific, uninterpreted content (Anthropic thinking with
signature, redacted_thinking, future block types) as one envelope item
`provider_opaque{dialect,kind,block}` inside the existing assistant output array.
Considered and rejected: a new side table (new bounds, owner and GC path for no
gain), per-dialect item types in the core (the core would track every provider),
and storing raw bytes (the whole conversation path is `serde_json::Value`).
Consequence accepted: object key order inside `block` is not preserved; strings
(text, signature) are byte-exact, which is what providers verify. Replay uses the
existing reasoning-epoch rule for all opaque items, because signatures are bound
to the model and a connection or model change must not resend them. Provider
tools cannot be enabled (`tools` is a reserved `extra_body` key), so provider
executed blocks that must be returned are not expected. Revisit if a provider
requires blocks to survive a model change, or key order.

## D-019: Proactive threshold compaction on reported tokens (#125, 2026-11-01)

Active. User decision: compact before the provider limit, enabled by default
(opt-out). Positioning (user): normal operation keeps the conversation small; a
single huge input is sent, not blocked or pre-checked; only a response reporting
input tokens >= `auto_at_tokens` (default 200,000, connection override, 0
disables) triggers the argument-free compact at the next safe boundary, one step
late. Explicit-overflow reduction (#50) remains the last-resort fallback;
persistent large-context users set 0. This narrows #50's "no token activation":
there is still no pre-POST check or estimate, only a post-response reported
count. Spec change (user): no character estimate; unknown usage never triggers.

Choices: the trigger reads the same latest-completed-attempt usage as
`context_usage` and ignores usage older than an adopted replacement, which bounds
repeats without new state. Failure cap: one non-adopted proactive attempt per Run
(in-memory per Driver; restart creates a new Run anyway). The record reuses
`automatic_trim_attempt` on the triggering completed attempt rather than a new
table/event. Measured motivation: workers averaging 440–570k input tokens per
request and $100 in about two hours via LiteLLM. Revisit if Runs are long enough
that one failed attempt per Run leaves them large, if a provider omits usage
(feature then inert), or if recording on completed attempts confuses consumers
that assume error_json implies failure.

## #127: Shellboy-placed cache markers; no top-level automatic caching (2026-11-01)

Active. The common cache hint (tier 1 under the #126 policy, condition (a)) is
on by default; connection `prompt_caching = false` opts out and sends nothing.

Tail placement deviates from the issue text, which asked for top-level automatic
caching for the conversation tail. Every Driver request ends with a
per-request `Shellboy context v1` line (Turn ID, cwd, model) that is not saved
in history, and attachment/summary notices are appended the same way. Automatic
caching marks the last block, so each request would write an entry ending on
that line, and the next request (whose conversation has grown past that
position) could never read it: only system and tools would hit, while every
request paid a cache write. Shellboy instead puts an explicit marker on the
last cacheable block before that trailing context, which is the structure the
issue says only Shellboy knows. Three explicit markers (tools, system,
conversation tail), all 5-minute; one slot stays free. The trailing context is
recognized structurally (the trailing run of system/developer items), so a saved
event notice that is the very last item is also left after the breakpoint;
the cost is that item's tokens on one request. Revisit if the per-request
context moves, if Anthropic changes lookback/breakpoint semantics, or if a
live check shows cache reads below expectation.

`cache_control` is reserved in `extra_body` for Messages even when
`prompt_caching = false`, so there is no precedence rule; a user wanting a
different TTL has no override today. `prompt_cache_key = true` with
`prompt_caching = false` is rejected as contradictory. Responses and Chat send
nothing new.

cache_creation in saved usage: added. Anthropic reports
`cache_creation_input_tokens` and writes cost more than reads (1.25x vs 0.1x at
5 minutes), so the read count alone cannot explain a bill such as the
2026-11-01 $100-in-two-hours case. It is stored as one more optional integer,
`input_tokens_details.cache_creation_tokens`, inside the existing saved
`model_attempt.usage_json` whitelist (`StandardUsage`): one counter per attempt,
bounded to the signed-64-bit range like the others, no new table or column.
It stays included in `input_tokens`. SQL readers, totals and the UI do not
project or aggregate it yet; revisit if usage totals need a cache-write column.

## #122: Optional Bounded Deletion Target Marker

Adopt a stateless count plus domain-separated SHA-256 digest of sorted,
length-prefixed selected Thread IDs. Reuse the #109 bounded observation subtree
collector and its256 physical descendants/depth8 limits rather than inventing
a second traversal or sending IDs. Keep absent-marker behavior intact for old
clients and CLI; protocol2 gains an optional field and typed preview_stale error.
No Core historical-format shim or schema migration is involved. Compare inside
the mutation transaction and mutate the exact compared vector, not a later
unbounded recomputation. Incomplete scope has no marker and fails marked
execution closed. Reference completeness is not target identity completeness.
The dedicated UI attaches the marker and refreshes after stale rejection; a new
Y is always required. Revisit only if preview admission limits or traversal
semantics change, in which case both consumers must change together. Verification
is synthetic database/protocol/UI tests plus exact-SHA native three-OS CI; no
user data, provider calls, installed binaries or live deletion are used.
## D-020: Summary request usage in app_meta records (#133, 2026-11-01)

Active. Compaction summary requests record provider-reported input, cached,
cache_creation and output tokens; unreported values stay unknown (no estimate).
`usage get` shows them as a separate `compaction` section and `compact list`
per event (`metrics.summary_usage`).

Storage choice: no new table. Considered and rejected: (a) only the existing
compaction results: `compact.result.*` expires with its operation receipt
(24 h), coalesced queued operations share one body (double counting), a proactive
record can be overwritten by a later Run on the same attempt, and failed manual
compactions leave no durable trace, so lifetime totals would silently shrink;
(b) `model_attempt` rows: they require a Turn and would mix compaction into
conversation totals; (c) a dedicated table: possible, but `app_meta` with a
Thread-keyed prefix already gives an indexed range read and has precedent
(`revisit.*` metadata, migration 0012). Chosen: one `summary.usage.<thread>.<uuid>`
record per summarizing compaction (owner: Compactor via DbWorker; bound: one
record per compaction, a few hundred bytes, readers skip >16 KiB; GC: retained
like attempt usage and counted in logical history bytes by extending the
`gc_bytes_app_meta_history` partial index in migration 0018). The results and
summary node payload also carry the same counters for per-event display.

Attribution: Turn/Run scope uses the Turn/Run that was current in the compaction
snapshot; a manual compaction between Runs belongs to Thread scope only.
Revisit if per-request (not per-compaction) rows are needed for grouped
breakdowns, if record counts per Thread grow enough that the range read matters,
or if a provider reports summary usage only in a form `StandardUsage` ignores.
## #132: Automatic Compaction Metrics Reason

Mirror the already bounded saved automatic_trim_attempt.reason into metrics.reason
using the same <=64-byte value and failure fallback as top-level reason. Keep the
existing top-level field for consumers. This repairs #125 readback only: no SQL,
row/text limit, provider path, trigger, persistence or schema change. Verify the
actual mocked model-response -> request-boundary -> adopted path, not just a
hand-inserted proactive record, plus absent-reason failure records.

## D-116: Stage-1 Resource Benchmark Workload (2026-11-01)

Adopt the explicit #116/reviewer list/create-file/grep/count-lines/done workload
for the deterministic Shellboy-only fixture. The named upstream jcode script
at `12a0dd30e4ad9d6351ca83bb7795d9f8aae9d522` (and its first revision) instead
uses read-title/summarize in rounds 2/4. Preserve provenance and disclose that
difference rather than claiming byte-identical prompts or cross-agent validity.
Revisit only when stage 2 is separately authorized and one exact common workload
is agreed. No real provider or comparative performance claim is authorized by
stage 1. The optional CI job records observations, not performance thresholds.
## #137: Request inspection as a standalone read-only client

`shellboy thread request` reads the conversation database read-only and never
starts an owner, so it works on a copied database and cannot disturb live
Threads. It is not a protocol operation or TUI form; the catalogue test tracks it
like `serve`/`shutdown`. Assembly reuses the driver's leading items, context line
and `HttpModelClient::wire_request` (the same function `send` serializes), so a
fully known rebuild is byte-identical. Attempt manifests gain `replay`
(path-node count plus SHA-256 over id/revision/tombstone) and `cwd`; older
attempts report history as unknown instead of guessing from timestamps. The wire
form uses the current configuration; a changed connection shows up as a
`request_body_bytes` mismatch, not silently. Revisit if attachments need
byte-exact rebuilding or if a TUI view is wanted.
## 2026-11-01 cache stall on the ChatGPT backend is provider-side

Through LiteLLM to the ChatGPT backend, cached_tokens only grows to the end of
the most recent user-role input; tool-loop items and developer-role notices
after it are never served from cache. Today's data: cache grew in 50 of 54
requests that followed a request carrying a new user message, and in 19 of 919
otherwise (yesterday 377/416 versus 29/2067). Rebuilding all consecutive
requests of the two affected Threads with `thread request` showed each request
repeats the previous one's items except the per-request context line at the
end, with body sizes equal to the saved byte counts. No Shellboy change between
811852f and main alters Responses request assembly. The lower ratio follows from
long tool loops with few user messages plus proactive compaction shrinking the
cached part below the last user message. A possible mitigation, connection
`context_role = "user"`, would make the trailing context line the last user
item but also changes how the model reads notices and may drop in-loop
reasoning items; it needs a measured single-Thread trial before any default
changes. Revisit if a provider documents its cache boundary or the trial shows
a clear gain.


## D-116b: Stage-2 Cross-Agent Fake-Model Comparison (2026-11-01)

The user decided stage 2 on #116: compare jcode, Codex CLI, Claude Code and
opencode with Shellboy on one CI Linux runner against a loopback fake model,
with no real provider, cost or thresholds. Adopt each agent's own headless
shape (as in jcode's `bench_headless_memory.py`) rather than forcing one
process model, keep the stage-1 workload and Shellboy runner unchanged, and
turn off permission/sandbox systems and jcode memory so every agent can run the
file-writing workload unattended. Measure the native Codex binary rather than
its npm Node launcher. Uncertain: whether disabled features or the launcher
choice materially change PSS. Revisit if stage 3 publishes these numbers, if a
pinned version changes its headless protocol, or if the user wants
default-install variants (e.g. jcode memory on) recorded alongside.

## #143: Thread Session Header Names (2026-11-01)

Add per-connection `session_header_names` (list, max 8, default
`["X-Shellboy-Session-Id", "x-litellm-session-id"]`); with `session_header=true`
the Thread UUID is sent under every name. Reason: LiteLLM reads only
`x-litellm-session-id`; otherwise it makes a new session per request and the
ChatGPT backend does not route a Thread consistently (measured 97-98% cached
with it vs 0 with a new session per request). The LiteLLM name lives only in
the default value; no product branching in code. Unknown headers are ignored by
other providers. Empty list sends nothing (chosen over rejecting it, so users
can opt out without touching `session_header`). Validation reuses the
`extra_headers` rules (credential-like words, transport-owned names plus
`anthropic-version`, case-insensitive duplicates in the list); collisions with
`headers`/`extra_headers`/the credential header are rejected only while
`session_header=true`. `X-Shellboy-Session-Id` keeps its old exemption so a
static header of that name is still overridden. Consequence: an existing
static `x-litellm-session-id` in `headers` now fails `config check` until the
list is overridden (accepted pre-1.0; the message says how). No new table,
worker, queue or dependency. Revisit if a gateway needs a per-request name.

## D-141: Context Display Latest Is Exact Per Turn (2026-11-01)

The status-bar read treated any full 33-Turn or 33-Attempt window as incomplete,
so every Thread with 33 or more Turns showed `↑—`. Migration 0019 adds the
derived partial index `attempts_completed_scope(turn_id,profile_id,model_id)`;
the display read takes the exact newest matching Attempt per recent Turn and is
incomplete only when 33 Turns hold no match. Assumes attempts of an older Turn do
not outrank a newer Turn's (one open Turn per Thread); revisit if Attempts can be
added to closed Turns.
## #118 doubtful-case disposition (2026-11-01)

Retain intentional Tab pause, derived frame-checkpoint publication, and true-EOF
full-page projection. Focus should not drift under streamed growth; input needs
actual last-painted geometry; EOF should not waste a viewport while the durable
source anchor remains unchanged. These are explicit specifications, not bugs
hidden by dropping assertions. Tests require source/fence/owner stability where
applicable and distinguish partial windows from true EOF. Revisit if item-focus
interaction changes or projection publication is moved into a separate preflight;
do not infer that all render-time domain mutations are permitted.

Fix tail no-op anchor/fence writes, lost reverse input, and failed-read replay
separately. Their reasons, acceptance conditions and measurements are in the
adopted scrolling design. No I/O, worker, protocol, budget or deadline change.

## D-142: Opt-in cache diagnostic facts, not routing or inferred usage

2026-11-01, adopted for issue #142 part 1 per reviewer task and issue comment
6558350298. Use one derived Thread-to-adopted-attempt pointer (schema 20) for
bounded latest-ID lookup; store small diagnostic observations/request witnesses
in existing attempt manifest metadata, not a new counter ledger/table/worker.
A single-attempt `usage get --attempt` selector avoids adding lifetime-growing
body lists or new work to aggregate/UI usage reads. Bound/whitelist provider
fields independently of usage; missing is null. The comparison is same-Thread,
without invented provider/model identity filters; it spans Turn/restart changes.
Reserve the dynamic options object rather than silently override a static
extra_body value. Sticky
headers, provider/cache efficacy tests and real API calls remain outside scope.
Revisit only with an explicitly approved routing or options-extension request.

- D-142 revision (reviewer, 2026-11-01; supersedes the schema20 pointer above): reuse #141 current-scope selection over 33 recent Turns with one completed-index seek per Turn; no witness means no comparison/options, no older-history search, migration, column or derived copy.
## UI Language Foundation (#139, 2026-11-01, Proposed)

Use one typed complete-message boundary, explicit `[ui] language = "en" | "ja"`
(default English), and no locale-based inference. The initial force-quit
confirmation and width diagnostic are examples, not a promise that the rest
of the UI is already translated. Keep stored/model/user text, API and CLI output
unchanged. The design and non-overlapping stage B proposal are in
[UI language foundation](../design/ui-language-stage-a-draft.md).

Observe ambiguous width using two bounded startup cursor-position responses;
unknown reserves two cells, and user settings do not override it. Apply the same
policy to UI and asynchronous presentation workers. Emoji sequences are bounded
conservatively, not asserted to have one font-independent width. Generated UI
symbols, borders and Markdown table borders must have invariant widths; user
content is never substituted. Review revision: use upstream crossterm cursor
position in a bounded isolated startup helper; do not fork dependencies or alter
Ratatui's internal width. Report a measured mismatch with Ratatui in TUI status
and diagnostics. Consider a translation catalogue if the locale/plural surface
grows substantially.

## #158 Unsupported attachment handling (approved)

Status: approved by the user for #158. For a file whose current bytes match
neither a supported signature nor valid UTF-8 text, do not send an octet-stream
input.
At request preparation, expand the attachment at its original source position
into an explicit text notice saying the format was not sent and must be
converted or removed. Record the existing unavailable state and reason in the
attempt manifest; do not record a successful binary send or update its sent-hash
baseline. Make the same outcome discoverable to the user, not just the model,
through a localized TUI activity warning and a guide to explicit reload. Reload
inspects current bytes, not a historical snapshot.
Retain the original reference, per-Thread selection and fork semantics; do not
delete or modify the file. As for other unavailable content, a successful notice
consumes initial protection without establishing a sent hash baseline. Re-read
current bytes on retry, so conversion or replacement can make the file sendable
later. No new configuration or automatic provider fallback.

Alternative: reject before sending. That avoids a model proceeding without
bytes, but a previously saved attachment can block every subsequent request,
including recovery, until explicitly excluded. A text notice better preserves
continuity provided the user can tell that the model did not receive the data;
it must never imply that the file was successfully analyzed. This proposal was
approved on 2026-11-02. File signatures take precedence over text detection;
image-kind and control-byte handling require explicit acceptance tests and must not mislabel arbitrary binary as text.

## D-143: Turn context line is saved history (#183, 2026-11-03)

The `Shellboy context v1` line moved from a per-request tail item to a `context`
history node, appended by `next()` only when the latest one on the selected path
differs from the current values. One rule covers the Turn start, a mid-Turn
cwd/connection/model/effort change (after its `thread_settings` event) and a
replacement that removed the line. Chosen over protecting the line from
compaction ranges: protection would add a third protected class to every range
policy, while re-emission adds no rule to compaction and costs no extra cache
(the replacement already changes the prefix). Correctness over cache: a change is
always announced at the next request, which is also the first request whose
shell calls use the new cwd. The node is hidden from TUI transcript rows (model
input, like the system/instruction items) and excluded from search. Revisit if a
mid-request setting must reach an already-running request, or if the UI should
show settings per Turn.
## #181 Mid-Turn summary: adapter gating (2026-11-03, Proposed)

Status: proposed with the [mid-Turn design](../design/midturn-compaction.md),
pending leader review. B proceeds only when no current-epoch reasoning is
replayed after the replacement point, or the adapter's contract is established.
Chat never replays reasoning and is supported. Anthropic documents a prefix
binding of thinking signatures on Claude 5.1+ (keep-tail client compaction
fails it), so B skips there whenever retained thinking exists. OpenAI documents
passing items since the last user message untouched when truncating, but not
whether retained reasoning is rejected after a removed segment; Responses skips
until a bounded paid validation passes. Impact: until validated, the measured
long-Turn growth on Responses connections is not reduced. Revisit when the
validation is recorded, when OpenAI documents the case, or if Anthropic exposes
a binding-preserving client path Shellboy can use without a model catalog.
The same Anthropic binding may also affect existing prefix summary, explicit
middle summary and #50 omission with thinking enabled; tracked as a separate
question, not changed here.
## #181 Mid-Turn summary: drop reasoning by new epoch (2026-11-03)

Status: decided in review with the user; implementation on hold until the
coordinator confirms after the survey. An adopted B replacement advances the
Thread's reasoning epoch in the same transaction, so no reasoning or opaque
block saved before it is sent again; the decision applies to every adapter
without per-adapter retain/skip branches. Basis: OpenAI documents reasoning
items as recommended, not required, and the user's GPT route accepted the
shape (acceptance, 2026-11-03). Anthropic's preserved-thinking rules allow
removing thinking blocks from the start, the end or all of them. Codex, OpenCode
and Goose compact mid-Turn without carrying reasoning. Impact: earlier-Turn
reasoning is dropped too, and the cache effect is unmeasured. Claude is not
live-validated. Revisit if a provider rejects the shape (fallback: skip
predicate on Anthropic thinking in the active Turn), if measured cache or
quality loss warrants a positional drop, or to keep Responses reasoning later.
#182 tracks the separate risk to the existing edits.
## D-189: Signed reasoning and history edits (#182, 2026-11-03)

User decisions on #182 (issue comments, 2026-11-03), applied to every adapter:
edit history only while preparing the next request; the active-Turn summary
keeps earlier Turns and the request byte-identical and replaces all tool rounds
with one summary carrying the latest round as text; selective edits (trim,
spill) wait for a boundary without a tool call, falling back to the active-Turn
summary on overflow; no `drop_block`; one recorded resend after the documented
binding rejection; no paid verification. Follow-up decisions the same day: the
proactive order is the ordinary summary of earlier Turns first, the active-Turn
summary only when that prefix has no target left or a fresh measurement after
it is still at or above the threshold (reverses #181's "instead of"); and the
general positional rule (reasoning before the earliest edit kept, after it
dropped) applies to every replacement.

Implementation choices with lower confidence, with recheck conditions:
- The positional rule is a new epoch plus a per-Thread kept-node set in
  `app_meta` (`reasoning.keep.<thread>`, bounded by the output nodes on the
  path, rewritten only at an epoch advance), not a schema column. Recheck if
  the set's size shows in write cost. Owner and end (coordinator review): the
  Thread; Thread deletion removes the key in its transaction.
  `compact.pending.*`/`inflight.*` is not the same leak: queued intents carry
  operation receipts, are drained to terminal outcomes by the compaction queue
  and are cancelled with recorded results at startup, so deletion does not
  remove them.
- User decision B, simplified (2026-11-03, final; replaces moving the
  messages before or after the summary, per-segment summaries and code-quoted
  messages): mid-Turn user messages are summarized with the tool rounds. The
  summarizer sees them interleaved in original order, and every summary prompt
  (ordinary, active-Turn, nested) asks for a "User instructions to keep"
  section. Meaning, not byte-exactness, is required. Message IDs are derived on
  demand from the ordered replacement sources (`history::summary_message_ids`,
  bounded by depth 8 and 65,536 rows on indexed lookups), so no ID list,
  original-order list or reply-expected list is stored on summary nodes. The
  path keeps the ordinary summary shape and no original node gains an edge. A
  span holding a protected node is not a target. Covered messages'
  attachments leave send selection like any replaced source. Recheck if
  models drop instructions in practice.
- The ordinary prefix summary runs while the tool loop is open (required by the
  order decision); its edit stops the open loop's thinking too. Anthropic
  documents this as degrading, not failing; recheck on any provider rejection.
- Explicit user edits are not deferred; they rely on the positional rule.
  Recheck if users expect them to wait.
- A binding rejection is detected from the provider message text (the
  documented sentence), with status 400 or unknown. Recheck if proxies reword it.

## D-187: Revisit is a delivery timing of an ordinary message (2026-11-03)

Approved by the user. `message send --deliver now|complete` replaces `--now`
and the `revisit set/get/cancel` commands. `complete` holds the receipt without
assignment (`hold_reason='run_completion'`) while a Run is live; normal Run
completion assigns every waiting one, in send order, to one new Turn.
Ordering is per timing class: later default/`now` messages overtake waiting
`complete` messages.

A waiting message never outlives its Run (user rule, 2026-11-03; replaces the
earlier choice to keep messages waiting after Esc). At normal completion,
including completion after a human Esc, it is delivered into a new Turn. On
stop, shutdown or entry into recovery wait it becomes, at that moment, the
held input an ordinary send would have produced (`stopping`,
`previous_instance_interrupted`, or assigned `recovery_wait`), and only the
existing message rules decide what follows. Startup applies the shutdown
hand-off after a forced exit.

Less certain choices, with recheck conditions:
- A message waiting when its Run enters recovery wait is held for an explicit
  retry even if a human sent it (the hand-off happens without a caller). A
  `complete` send while the Run is already in recovery wait is an ordinary send,
  so a human's resumes recovery. Recheck if the two feel inconsistent.
- `limits.revisit_max_per_run` is removed rather than redefined: ordinary
  messages already allow unbounded self-continuation, so the cap guarded nothing
  unique. Recheck if real runs show runaway self-Revisit loops that stop and
  Esc do not handle.
- Stored #36 reservations are dropped by migration 24, not moved: they had no
  message node or receipt to move into. Old `revisit` events stay in history as
  generic events.
- The Revisit count is derived from receipts through a new partial index
  instead of a stored counter. Recheck if the index shows up in write cost.

## D-188: honor explicit usage-limit reset facts

2026-11-03, user-approved for #188. When a provider explicitly identifies `usage_limit_reached` and supplies a valid reset time, skip automatic retries only if that reset is later than the complete remaining retry schedule; keep the ordinary retry policy for shorter resets or absent/invalid facts. Use existing recovery reason wording/symbols, preserve the fact and reset time, add no guessed values, settings, or provider fallback. Validate with synthetic responses and three-OS CI.

### Startup integrity scan policy (#192)

The 2026-11-03 decision supersedes earlier unconditional startup quick_check
claims: quick_check runs on unclean or unknown previous shutdown, and
foreign_key_check runs after migrations. Clean startup skips these scans.
`storage check` explicitly runs both; a clean marker is not a corruption proof.
## D-189: Reduce duplicate and queued native CI work (2026-11-03)

User-approved. On `main` pushes, the `Rust checks` workflow asks the Actions API
for completed runs on the exact `github.sha` and reuses only a same-repository,
first-attempt successful run whose Ubuntu, macOS and Windows check jobs all
succeeded. The small gate uses the existing `actions: read` `GITHUB_TOKEN`
permission. Missing evidence, API errors, reruns, partial matrices, or reuse-only
runs do not qualify; they run all three checks. The gate applies to main pushes,
not manual dispatch.
## D-189: Reduce task-branch CI queueing (2026-11-03)

User-approved.
Workflow dispatch on task branches defaults to Linux only. `all_oses=true` opts a
task branch into all three OSes for Windows, PTY or path-specific work. `land/*`
and `integration/*` always run all three. The existing
`windows_test_threads=4` Windows-only experiment and `=2` all-OS diagnostic
remain unchanged.

Reason: measured macOS queueing on task branches had 4.5 minutes median and
19 minutes maximum. Linux-only default avoids that queue for ordinary task-branch
iteration while preserving three-OS coverage for landing branches.

Validation: dispatch on this task branch and confirm Linux-only default; retain
the existing Windows-only (`=4`) and all-OS (`=2`) modes.

## 2026-11-03: bounded recovery without oversized-queue rejection (#194)

User decision supersedes the earlier proposed InputLimit rejection: first audit
queue invariants; if held receipts can exceed 128, release oldest 128 in ordered,
progressing passes, retain the rest and expose remaining queue through status.
Audit found reply-duty admission capped at128 but no cap on ordinary held sends.
Thus shared send/run.retry recovery uses committed units and retains a
recoverable Turn until drained; no compatibility writer exemption is added.
Source: user instruction in this #194 chain integration task, 2026-11-03.

## 2026-11-03: Observe shutdown PTY cells, not emitted byte substrings (#211)

The shutdown subprocess fixture keeps a persistent VT screen across all reads
and key actions. Ratatui emits changed cells, not complete labels: unchanged
punctuation or letters can be retained at their old coordinates and never
appear in a contiguous output substring. A frame is observed after Ratatui's
cursor visibility command at the end of a draw. Success requires all expected
labels in that same completed screen after the action, not elapsed time or a
label from an earlier screen. Readiness uses Tokio's Unix AsyncFd; the existing
60-second deadlock watchdog and 50 ms application grace are unchanged.

Pin test-only `vt100` 0.16.2 rather than writing another partial VT parser. It
handles cell persistence, cursor movement, styles, OSC and fragmented UTF-8;
no production dependency or terminal input behavior changes. The fixture keeps
a bounded 4 MiB transcript and prints the completed/live screens and JSON VT
capture on observation failure. This is synthetic temporary-data evidence,
not a claim about physical terminal fonts or installed applications.

## 2026-11-03: Scope Post-Migration Foreign-Key Checks (#210)

Adopt explicit typed migration impact metadata, with unknown/missing metadata
falling back to full checking. Skip only explicitly index-only upgrade ranges;
otherwise check declared affected tables and referencing child dependencies.
Persist the combined scope before migration so a crash cannot downgrade pending
validation to a clean reopen or narrower later upgrade. Keep full manual checks
and independent unclean-start quick_check.

W9 measured a full FK scan at 19.282s versus 0.496s for three index-target tables
on a VACUUM INTO snapshot (different warm-cache conditions, not a startup speedup
guarantee). Migrations 25-27 change indexes only, so even those table scans cannot
validate a new data/FK change. Avoid that redundant work rather than sample rows
or infer safety by parsing SQL. Historical migrations remain full unless their
metadata has been explicitly reviewed. Table scopes still scan every selected
child row and are not bounded-latency integrity checks. Index-only skip deliberately
does not discover unrelated pre-existing FK corruption; manual checks cover that.
## D-190: Keep Japanese operation-catalogue source as display data (#152)

Owner decision, 2026-11-03: system-to-model input is English-only; UI display
follows the selected language. Keep the Japanese source strings in
`src/operations.rs` unchanged. They are TUI catalogue display data, not model
input: operation labels/meanings flow through
`src/ui/operations.rs::catalogue_label` / `catalogue_meaning` into
`src/ui/operations/metadata.rs::text_for`, whose operation mappings select
English or Japanese. Field labels/help use `text_for` with
`field_label_for` / `field_help_for`; global argument text uses
`global_argument_text`.

Consumer inspection found no model-request, tool-description, prompt, system
message, CLI JSON, or generated tool-schema consumer of these catalogue fields.
The only non-TUI direct label consumer was the frontend operation title; route it
through the selected-language catalogue label instead. Tests verify the
English projection for every operation, field and global argument, and cover
the frontend title in both languages. The Stage C audit records the Japanese
source as reviewed UI copy with source-token hashes; changes invalidate review.

## 2026-11-04: Accept slow DB diagnostics overhead (#201)

The leader accepts the production instrumentation without opt-in gating on the
measured evidence for `46f9fbc7` (issue #201 comments 6627459223 and
6627587728). On Apple M1, the synthetic 50,000 point-read worker request median
was -2.08% versus the uninstrumented control, with overlapping ranges; its
contribution is not distinguishable from zero. The 1,000-row workload was the
worst measured case: approximately +0.18 ms, or +4.4% of a 4 ms synthetic
worker request. These sequential, uncontrolled measurements compare revisions,
not an isolated trace-only cost. The instrumentation is intended to find slow DB
paths such as #164/#204; gating could hide the diagnosis when needed. The
measured cost is bounded and sub-millisecond per statement in these workloads,
not a general latency guarantee.

The share in real application end-to-end requests is unmeasured and is not
claimed. Re-measure if the real-workload share of DB worker time becomes
available, or if instrumentation gains work per statement.

Source: leader acceptance for #201, 2026-11-04; benchmark details and ranges in
issue #201 comments 6627459223 and 6627587728.


## 2026-11-04: Index interrupted tool-group recovery candidates (#219 proposal 1)

Replace startup's all-group candidate discovery with an indexable marker table
keyed by tool-group ID. The marker predicate exactly preserves legacy behavior:
non-completed group, pending/running call, or call missing its committed result.
SQLite triggers maintain it atomically with group/call/result writes; migration
backfills existing rows once, in its transaction, before the new code reads only
markers. No permanent old-scan fallback or wall-time promise. Keep keyset pages
and all existing repair, savepoint, replay-blocking, and transaction contracts.

The design is motivated by W9's read-only reproduction: the old repair candidate
query returned one candidate from 56,540 groups; its first run took 5.940 s,
versus 6.606 s for the entire observed `interrupted_work` startup stage. This is
not an exact stage attribution or a timing budget. Validate recovery equivalence,
crash atomicity, migration coverage, and sparse candidate workloads; remeasure on
a read-only VACUUM INTO copy and report the observed result without promising a
latency.

Source: W9 #219 comment; user-approved implementation scope, 2026-11-04.

Measurement follow-up (2026-11-04): on one 3.4 GiB read-only `VACUUM INTO`
snapshot (57,465 groups, 59,430 calls/results; zero unresolved candidates), the
legacy candidate keyset loop took 1.031 s cold and 0.238/0.354 s on two warm
repeats (median 0.354 s). After applying migration 28 to that same copy, the
marker keyset loop took 40 microseconds cold and 8-11 microseconds warm (median
9 microseconds). EXPLAIN changed from a primary-key walk over `tool_group` with
correlated call/result probes to a covering primary-key seek over the candidate
marker. This measures candidate enumeration only, not the entire
`interrupted_work` stage or startup; the W9 5.940 s query and 6.606 s stage
remain distinct historical measurements. The snapshot was deleted after use.
A second one-copy measurement used a read-only `VACUUM INTO` snapshot of the
local schema-27 database (59,471 groups). Legacy candidate enumeration returned
zero candidates in 0.598 s on the first pass and 0.206 s warm. Applying migration
28 took 0.141 s; marker enumeration returned zero candidates in 55 microseconds
on the first pass and 12 microseconds warm. The temporary copy was deleted; those values isolate candidate enumeration.
A same-copy rollback-safe
comparison then measured the full `interrupt_previous_work` body: 1.097229 s
using the legacy full-predicate query and 37.912 ms after migration with marker
lookup. Both reported zero repaired, unresolved, and retired groups. Timing
excludes transaction begin/rollback and migration. The local environment was
arm64 macOS 25.3 with SQLite 3.51.0; this observation is not a startup promise.

## 2026-11-04 — Reuse recovery candidates in startup context capture

With proposal 1 applied, the tool-call snapshot still walked completed groups
because SQLite chose the call-history join as its outer loop. Start that query
from `tool_group_recovery_candidate` and preserve that order with `CROSS JOIN`;
use the same exact candidate set for the thread selector's group predicate.
Migration 28's one-time backfill covers older rows and triggers maintain the
set transactionally, so neither query may miss a group that satisfies the old
predicate. Existing snapshot chunk writes remain in place; no latency budget is
introduced. Do not add the tested partial wait or active-attempt indexes: their
one-copy migration costs exceeded the measured per-startup savings for this
observed workload.

Evidence: on one 3.5 GiB read-only-source `VACUUM INTO` copy with migration 28
applied (60,840 groups, 62,903 calls, nine interrupted-work Threads), warm
`TOOL_CALL_SNAPSHOT` query time fell from 249 ms to 0.064 ms with the same empty
result, and the thread selector fell from 70 ms to 8.8 ms with identical
selected Thread IDs. A same-copy measurement of complete context capture plus
`interrupt_previous_work` was 1,796 ms before and 881 ms after, with zero
capture failures and zero repaired, unresolved, or retired groups in both runs.
The separate per-step measurements and omitted-index costs are in the #219
follow-up comment.
This is an observed local comparison, not a startup timing promise. Status:
implementation pending final checks and hosted CI.
## D-215: Automatic spill ordering and summary reasoning (2026-11-04)

The owner revises D-189: selective edits previously "wait for a boundary
without a tool call". Automatic mid-Turn spill may now run while the tool
loop is open, only while preparing the next request, never between a call and
its result. Protect the latest complete tool group, incomplete groups and
unreflected user input. Adoption advances the reasoning epoch and clears the
kept set in the same transaction, dropping all prior reasoning from replay.
Anthropic documents graceful degradation for missing tool-use thinking; this
shape has not been live-validated against Anthropic without paid runs.

Automatic order is closed-Turn spill, closed-Turn summary, active-Turn spill,
active-Turn summary, each only while a calibrated need remains and each with
a fresh measurement and recorded outcome. Uncalibrated fallback preserves the
existing escalation. Summary material includes readable reasoning from older
epochs, but never encrypted or opaque data; replay filtering is unchanged. A
summary request is plain text, so provider signature rules do not apply; no
documented reason was found for the old summary-material epoch filter.

Owner clarification (2026-11-04): #182 no longer requires automatic mid-Turn
summary to replace all rounds. Select oldest-first whole safe groups only until
the remaining calibrated need is met; retain the latest complete tool group,
original request, user-instruction fences, incomplete and unreflected inputs.
Atomically advance the epoch and clear the kept set on adoption, even when no
spill was adopted: no earlier reasoning survives replay, giving the same
drop-all provider shape as the approved full summary. Record the remaining need
before and after. This implements the owner choice to summarize only what is
needed; provider behavior remains unvalidated by paid Anthropic runs.

## D-224: Profile-scoped connection wait (2026-11-05)

The owner adopted #224 stage 2. After an HTTP 429, all requests of one
configured connection profile (the name the user configured, not merged by
provider or credential) pause new sends until the Retry-After time, or a
connection-capped exponential backoff when the header is absent or invalid.
Admission is FIFO with one probe; state is in memory and bounded (1,024 waiters
per profile) with no new worker, queue or table. Restart clears it. Existing
#76 retry rules and #188 usage-limit rules are unchanged. Known limitation:
profiles that share one upstream quota are throttled independently and may each
receive a 429. Stage 3 (summary retry, adaptive map concurrency) is not
included. Revisit when real measurements show cross-profile quota sharing
matters or when a stable provider quota identity is documented.

Usage-limit recognition: a real LiteLLM 429 (`litellm.RateLimitError`) wrapping a
ChatGPT plan `usage_limit_reached` fact was retried eight times because the
embedded JSON was followed by diagnostic text, which the strict parser
rejected. The parser now reads the first JSON value at up to 16 candidate braces
of a `litellm.RateLimitError` message (also when `error` is a plain string). The
format is based on one observed case plus the documented fields; other
providers' wrappers are not guessed. A fact whose reset exceeds the remaining
retry schedule is not auto-retried and its reset time appears in the recovery
reason.

## D-226: Recoverable automatic attempt record (2026-11-05)

The leader adopted option A of the #226 proposal. Each automatic attempt writes a
provisional `unknown` record in its witness transaction; each stage appends a
fixed-size content-free fact (and, when calibrated, the provisional pending
target) in its own adoption transaction; W2/W4 keep the facts. Reason: a
post-commit storage failure previously lost every diagnostic while the
replacements stayed committed, and a second write cannot be made reliable once
the writer fails, so the fact must commit with the replacement. Impact: one
extra primary-key UPDATE (and one app_meta upsert when calibrated) per committed
stage (210-249 us on a synthetic debug fixture); a failing fact write rolls the
stage back, which is the same writer failure that would fail the commit. No new
table, key, worker, queue or migration. Decisions made during implementation:
(1) `removed_characters` in a fact is the projection difference of the replaced
items only; whole-history removal stays with the driver's re-measurement.
(2) The W2 exception body now also carries `reason`, `trigger` and `target_plan`.
(3) The uncalibrated active-Turn summary and late prefix summary are now recorded
stages; their `stages` entries were `not_reached` before. (4) The #180 guard and
the witness need no change; the guard test asserts equality with the pre-attempt
rows only where the attempt's own guard write is not reached or is rejected.
(5) The cost measurement is temporary instrumentation, not a test, because
wall-clock numbers do not belong in the suite. Revisit if a real DB shows the
stage UPDATE matters, if another stage kind is added, or if a record with
non-NULL invalid `error_json` must also carry facts.

## #219 active-attempt partial index accepted (migration 29)

On the owner's real machine v0.8.2 startup stage `interrupted_work` took
4,858 ms with no migration; the #201 diagnostics attribute 3,277 ms to
`UPDATE model_attempt SET ... WHERE state IN ('preparing','inflight')` (70,493
full-scan steps, 281,997 VM steps). The earlier rejection rested on a copy
measurement (343 ms cold / 33 ms warm, index build 380 ms) that did not
reproduce cold-cache startup on a 4 GB database; the real cold cost is about ten
times larger and is paid on every start, while the index is built once and holds
only active rows. So migration 29 adds `model_attempt_active_recovery` on
`state` restricted to `preparing`/`inflight`, with the statement unchanged.
Measured on a fresh copy: 572.6/47.1/50.2 ms before, 1.10/1.85/2.15 ms after;
build 45.5 ms; index 4 KiB. The wait-prefix index stays rejected. Revisit if the
set of interruptible attempt states changes (the index predicate must follow the
statement). Not reproduced: the exact 3.3 s cold figure on the copy (the page
cache was warm for the copy), so the real startup gain is to be confirmed on the
owner's machine after release.

## D-224-3: Summary retry and parallel summary requests (2026-11-05)

Decision: summary map and reduce requests retry transient failures with the
shared conversation classifier inside the existing request budgets, through the
profile gate; usage limits are never retried; successful parts are cached and
never re-requested. All uncached map requests of one summary, and all batches of
one reduce round, are launched together with no concurrency counter; the gate
alone throttles. Reason: one transient failure discarded the whole summary, and
serial requests dominated its wall time. Adaptive growth/halving was considered
and dropped: the budget already bounds the number and the stage-2 gate already
holds every request of a cooling profile. Fairness: the gate stays plain FIFO;
the summary takes one ticket at a time and yields after each launch so another
Thread's ready request can enqueue between them (a round-robin gate was
considered and rejected as more mechanism than needed). Impact: no new setting,
table, worker or queue. The classifier moved to `model::retry_class` unchanged.
The usage ledger records the rate observation by request index instead of one
pending slot. A retry that finds the budget exhausted fails with the original
cause. A budget error while launching a reduce round no longer happens after
earlier batches ran serially; those in flight still finish and are cached.
Unverified: behaviour against a real provider, in particular a provider with a
low concurrency quota (the first 429 queues the rest in the gate, the probe
releases them). Revisit if measured 429s per summary increase.

## D-230-1: Post-compaction input is derived at read time (2026-11-06)

Decision: the link from an adopted summary to the input tokens of the next model
request is computed when `compact list` is read, from the saved replacement
`created_at` and the Thread's attempts (`usage_json`), not stored. Reason: both
facts are already saved and retained for the same lifetime; an index-ordered
lookup ends at the first match, so the read is bounded by one seek per retained
replacement row. Storing would add a write after the next request and a race with
cancellation and late arrival. Impact: no table, column, event or worker. Limit:
ordering rests on timestamp string comparison like `next_request_pending`; a
clock step backwards could pair the wrong attempt. Revisit if a measured read
cost or timestamp anomaly appears.

## D-230-2: Marker next-trigger text states the eligible-content gate, not a 10% rule (2026-11-06)

Decision: the note for a skipped, failed or rejected proactive attempt says
`next: when new eligible content appears`. Reason: #230 mentions a 10% input
growth rule, but the product retries only when the saved history changes so that
the bounded candidate forecast shows new or grown eligible content (a later Turn
alone or more input tokens alone does not retry); showing a percentage would be a
guess. Impact: text only. Revisit if a growth rule is added. The row keeps the
existing `⬦`/`⁂` marks and the separator rule instead of a `──` rule line, to
reuse existing presentation parts; the spilled count is shown only when known and
above zero. Review follow-ups: the render stamp now covers the note and marker
facts; a fork's inherited replacements omit the linkage keys (v3); a spill-only attempt
shows a rejected summary and no later-input part.

## D-231-1: Summary parameter bounds and migration 31 (2026-11-06)

Decision: `segment_chars` accepts 2,000..=1,000,000 and `reduce_rounds` 1..=16, checked at every level (config, connection, `thread set`, `compact`) and by CHECK constraints on the two nullable `thread` columns (migration 31, registered `IndexOnly` for the same reason as D-229-1). Reason: the lower bound equals the existing minimum summary limit, so a part is never required to be smaller than the smallest summary it may produce; it is not a safe value. The real cost of a low value is known and recorded here: a conversation group larger than `segment_chars` cannot be split, so the summary stops before any request (every automatic summary of that Thread fails until the value is raised or the group is spilled/omitted), and a small size multiplies reduce requests and rounds, which can reach `summary_reduce_max_requests` or `reduce_rounds` before the map budget. `compact --dry-run` reports the group refusal before any cost. 1,000,000 keeps one request's source text bounded; 16 rounds is four times the old constant and each round already costs requests under the reduce request budget (default 16). These are bounds, not recommended values; defaults stay 64,000 and 4. Impact: values outside the range are rejected rather than clamped. Revisit if a provider needs a part larger than 1,000,000 characters or the rounds bound is seen to hide a loop.

## D-229-1: Migration 30 is IndexOnly (2026-11-06)

Decision: the nullable `thread.auto_compact_at` column is registered as `IndexOnly`. Reason: it has no foreign key and every existing row is NULL, so the integrity scan has nothing to check. Impact: upgrade skips the full check, as for migrations 25-27. Revisit if a later column gets a foreign key or a non-NULL backfill.

## D-236-1: Rewind keeps branches as Threads and copies the whole path (2026-11-06)

Decision: `thread rewind` (#236) adds no branch tables and no migration. The cut-off
part is a child Thread made by `fork_thread` at the old end of the path; the Thread's
path is rewritten by `select_path` (whole path, the study's R1 `thread_branch` tables
are not used); the boundary is checked by `validate_group_boundary` on the new end.
Reason: the user chose "branches are Threads" (undo = open the child) and reuse of
existing parts. Impact: each rewind inserts one Thread row and about
2 x path-length `thread_path` rows (child copy plus rewrite); no node is copied.
Measured cost (release build, Apple silicon, temp file DB, synthetic message nodes,
one operation = child fork + attachment delete + `select_path` + boundary check),
`cargo test --release --test integration rewind_path_cost -- --ignored --nocapture` (the 1,000,000 row was measured with the limit temporarily raised):

| path nodes | rewind to the middle | second rewind of the new (half) path |
|---|---|---|
| 10,000 | 0.21 s | 0.09 s |
| 100,000 | 1.93 s | 1.01 s |
| 1,000,000 | 22.6 s | 13.2 s |

Cost is linear (about 20 us per path node). It runs in one writer transaction, so
the number is also how long the DB writer is occupied. These are observations, not
promises. Limit: rewind refuses a path above `PATH_LIMIT` = 200,000 nodes
(`input_limit` with `path_nodes`/`limit` details), declared as the writer bound (about 4 s by the table). Revisit if
real Threads approach it or a cheaper truncate (delete only `ordinal>k`, share the
prefix) is wanted; the fork copy is the other half of the cost. Child "started
after NODE" is by time (first own node, or fork node), so a fork extended later is
listed by its first own activity; shell "after NODE" compares `started_at` with the
node's `created_at`.

## D-237-1: Branch view reads fork relations; no storage, bounded family (2026-11-06)

Decision: `thread branches` and the TUI branch marker/panel (#237) add no table,
event, worker, queue or migration (branches are Threads, user decision). The
divergence node is found by bisection over `thread_path` point lookups instead of
storing a fork node; payload bytes are not summed (`bytes` null); the family is
capped at 256 Threads with the ancestor chain always kept; the cursor is an offset
bound to a fingerprint of the listed tree (not `root_revision`, which every append bumps). Reason: the bounded reads are the whole cost of the view,
and a stored fork point would need a migration for data (the shared prefix) that the
paths already hold. Impact: the shown divergence can be the bisection's boundary
after a compaction rewrote a path before it (documented in History; only when both paths were separately compacted over the same range); a family above
256 Threads is shown in part with `family_truncated`; a tip's last request is
searched only 256 path rows back. TUI choices: the inline marker reuses the
`compaction_note` slot pattern rather than a new item kind, so Enter on a node with
a previewed non-current alternative opens that Thread and otherwise keeps its usual
meaning; prune from the panel opens the existing form (dry run selected) rather than
adding a second confirmation. Recheck when: real families approach 256 Threads, a
compaction-heavy Thread shows a wrong point, or the user wants compare.

## D-219-1: Startup app_meta prefix scans become primary-key ranges (2026-11-06)

`interrupted_work` startup recovery replaced `LIKE 'compact.pending.%'`, `LIKE
'compact.inflight.%'` and `LIKE 'wait.pending.%'` with `key>='prefix.' AND key<'prefix/'`.
Equivalence: these keys are written only by `CompactionKeys::pending/inflight` and
`wait.pending.{operation}` as exact lowercase ASCII prefixes, so LIKE's ASCII case folding
and its `%`/`_` wildcards (absent from the literal prefixes) never differ from the byte
range ('/' is '.'+1). EXPLAIN QUERY PLAN changes from `SCAN app_meta` to `SEARCH app_meta
USING INDEX sqlite_autoindex_app_meta_1 (key>? AND key<?)` (MULTI-INDEX OR for the pair).
Share: 660 of 1,207 ms measured on a real startup. No new index or schema. Recheck when a
key with an upper-case or differently written prefix is introduced.
## D-244-1: Reject unknown commands/fields with a typed reply (2026-11-06)

Decision: `serve_connection` parses the request frame as JSON first and decodes it
with `Request::decode_frame`; when the frame has a non-empty `operation_id` but an
unknown command, unknown field or wrong argument type, the owner replies
`invalid_argument` (existing code; no new one) under that ID and closes. Requests
stay strict. Reason: closing without a reply made a client talking to an older owner
(new `chat` sending `human`) report an uncertain mutation although nothing ran, and
protocol.md P5 already requires an own-ID reply for decodable rejected requests.
Names are bounded to 64 printable characters and values are never echoed (keys can
still be caller-chosen; only the key, not its value, is shown). Frames without a
usable ID still close silently. Impact: no version change, no new table/worker/queue.
Recheck when: protocol 3 adds capabilities (study A2), which should let clients avoid
sending unsupported fields at all.
## D-246-1: One-key rewind: `r`, confirmation from `--dry-run`, undo on Alt+U (2026-11-06)

Decision: (1) The history focus gets `r` (Rewind) on a selected user request; no other
key of that focus uses `r` (c, b, o, Space, Enter, Tab/Esc, arrows; F-keys pass through).
`/rewind` takes the same preview and confirmation. (2) The confirmation shows facts
read from the owner, never computed in the UI: `thread rewind --dry-run` runs the real
rewind inside a SAVEPOINT that is rolled back (no second implementation of the facts,
refusals and boundary checks to drift; no write survives, tested by a table dump).
The UI refuses to show a confirmation when one fact is missing. (3) The undo offer is
the footer notice (`operation_notice`, an existing component) plus `Alt+U` in the key
help while the input is empty. A plain letter cannot work there because it is typed
into the draft; Alt chords already own Alt+D/Alt+W in the composer and honour the
configured send/newline bindings the same way. The undo only opens the branch Thread
(always valid), is scoped to the rewound conversation and used once. Impact: one new
`UiAction`/`UiUpdate` pair, no table, worker, queue or glyph. Recheck when: a terminal
where Alt+U is unusable is reported (macOS Terminal without "Option as Meta"), or the
user wants undo reachable without the keyboard enhancement of Alt.

Addendum (review of #246): the preview occupies the DB writer exactly like a real
rewind (about 20 us per path node, D-236-1), because it performs the same writes before
rolling back; the 200,000-node bound applies to it too. The "writes nothing" test now
dumps every table except the executor's own request journal (`operation_receipt`,
`quota_counter`, written for every call, dry run or not), including app_meta
(`root_revision`), edges and thread revisions. The undo offer ends at the next
submission in the Thread, when its selection revision moves beyond the rewind's own
+1, or when the Thread is left; Alt+U also needs an empty draft.

## Unit-length embeddings at the provider boundary (#250)

Decision: every embedding returned by the provider is scaled to unit L2 length in the
model HTTP client (`unit_normalized`), the one place all embeddings pass through (index
ingestion, probe, rebuild candidates, history/RAG queries, exact L2 fallback). No model
name is consulted. Zero-length or non-finite vectors fail as a protocol error. Why:
Gemini `gemini-embedding-001` returns normalized vectors only at 3,072 dimensions;
reducing dimensions for #240 yields unnormalized vectors, and sqlite-vec L2 ranking then
tracks vector length rather than meaning. Impact: already-unit vectors are unchanged
within float tolerance, so existing 3,072-dimension spaces need no rebuild; reduced-
dimension spaces built earlier stay unnormalized until rebuilt. The pre-normalization
length is not recorded (optional in the issue; no consumer). Recheck when: a provider
needs dot/cosine-specific semantics or a vector arrives from a path other than the HTTP client.
## D-222-1: Archive wire shape, caps and indexes (2026-11-06)

Decision: archive is two commands (`thread.archive`, `thread.unarchive`) with the same
args instead of the study's single command with an `archived` flag; a `--descendants`
selection above 256 Threads (explicit IDs included) is refused with `input_limit` and
changes nothing; unknown IDs are listed in `not_found` and only an empty selection is
`thread_not_found`; the partial indexes are `(updated_at DESC, id DESC)` to match the
list `ORDER BY`; migration 32 is `IndexOnly`. Reason: one operation per CLI path and F8
form keeps the operation catalogue one-to-one; a silent partial write over a capped
subtree would be a surprise for a model that tidies by IDs; the study's `id` ascending
index would need a sort for `id DESC`. TUI keys Ctrl+A (archive/unarchive) and Ctrl+G
(archived list) are free in the Thread list (checked against the browser, the overlay
dispatcher and the global keys Ctrl+Q/Ctrl+C/F1/F2). Impact: Watch keeps showing
archived Threads with live work. Recheck when: real users archive subtrees over 256
Threads (a continuation cursor would be needed) or Watch should hide archived work.

Review follow-up to D-222-1 (key choice, not tested): Ctrl+A is the default prefix of GNU
screen and a common tmux remap, and Ctrl+G is the default lock toggle in Zellij (reviewer's
knowledge, not verified here), so either may be swallowed by a multiplexer. The keys are kept
for now and the key choice goes back to the user. The thread list footer now picks the richest
line whose text, measured after the view's labels are substituted, fits (full, two-line, compact,
small); the two-line tier trades Ctrl+R/Fold hints for the archive keys (F1 and the failure
footer still name Ctrl+R).

## D-243-1: In-memory change clock and `events.wait` (2026-11-06)

Decision: `Control` owns a `ChangeClock` advanced from `Control::changed`; `events.wait`
long-polls it. User decisions (#243): the long-poll shares ordinary request slots (the
Web bridge multiplexes one poll for all tabs); no change record is persisted (clients
re-read saved facts); models may use it. State record (AGENTS.md): owner `Control`, dropped
with it; bound 4096 Threads (one latest sequence per Thread, oldest evicted and `floor`
raised; a cursor below the floor or from another owner instance gets `resync_required`,
never a silent gap); at most 64 changes per reply and 64 filter IDs; one `std::sync::Mutex`
held only for map updates, never across `.await`; no task, queue, table or per-client buffer.
Termination: a pending call ends on match, timeout, disconnect (observation wrapper) or
owner stop; nothing outlives the request. Memory is two small map nodes plus one Thread ID
per entry; bytes were not measured. Hint completeness is checked by one test per mutation
family in `tests/events_wait.rs`. Recheck when: measurement shows follow-up reads dominate
(per-Thread digest, study E1), or long-polls crowd out the 24 ordinary permits (study Q6).
Hint-completeness findings (fixed in the same change): `message.send` that is queued or held
(no wake), `thread.prune` (non-dry-run) and `attachment.add` that is not delivered live
committed without `changed()`; each now reports its Thread in `changed_threads`.
## D-248-1: Rejected closed summary is frozen for the current Turn (2026-11-06)
## D-248-1: Rejected summaries are remembered per Turn under unknown coverage (2026-11-07)
## D-248-1: A failed summary stage is not repeated in the same Turn (2026-11-07)

Decision (one rule): under a coverage-limited forecast (`marks:null`), any non-cancelled
failure of a summary stage (closed or active) blocks that stage for the rest of the Turn,
until that stage adopts a summary. State: a Run-local `Blocks` value in `ProactiveBudget`
(turn id plus two booleans). No counter, serde field, table, worker or witness coupling;
restart or a new Turn allows one more attempt, and an overflow-recovery rewrite does not
reset it (accepted: it only suppresses proactive summaries for that Turn). A cancelled
stage never blocks, and the older freeze at the adopted-spill plus rejected-summary save
is also skipped on cancellation. Why one rule: provider 400s on reduce are built as
`invalid_state` (`budget.error`), so classifying "deterministic" misjudged them, and a
provider 400 on every summary gave 10 real closed requests with the active stage never
reached (r2 review). Why a Run-local value instead of guard fields (r2's form, which
needed `mark_refresh`, a witness-extension carry and a keep branch): it survives our own
closed/active adoptions without any of that and is a net deletion; the closed stage also
cannot be rejected a second time after a closed spill clears the guard. Accepted cost: an
outage blocks proactive summaries for that Turn; request-level retries already ran and
overflow recovery still works. A wider closed range later in the same Turn stays blocked.
Visibility: the blocked stage's record in `compact list` says `skipped:
"blocked_by_earlier_rejection"` (the existing `skipped` field); the explicit `compact`
path does not read the block. Bound: at most one rejected attempt per summary stage per
Turn under unknown coverage; spill passes may still start at every boundary.
Prompt-cache note: on the #248 Thread the active stage was never reached, so its cost is
unknown; with the block, an active spill (reasoning-epoch bump) can still run at every
boundary after the rejections. That cost is unmeasured. Recheck when: a real Turn shows
more than one rejected attempt per stage, or finding 2a is fixed.
