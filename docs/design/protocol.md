# P1 公開操作・ローカル通信の実装契約

## Current-only contract (Issue #33, 0.2.0)

Before 1.0.0, release-specific backward-compatibility paths are not part of the contract. The general schema migration runner, `user_version`/ledger validation, and protocol-version checks remain. Earlier sections describing an old-format fallback, deprecated alias, metadata repair, or compatibility shim are historical and superseded by this section; they must not be used to justify a runtime fallback.

採用状態: 実装基準。公開wire・型・エラー・永続化境界を本書で具体化する。契約の由来となった初期仕様資料は非同梱であり、現行の採用範囲は [適用仕様](effective-spec.md) と本書を正本とする。

`src/protocol.rs` がTUI/Control/CLIで共有する公開型である。CLIの空白区切り操作名はwireでは `thread.list` のようなドット区切りとする。35操作を `Command` の閉じたenumと各操作専用argsへ対応させる。shell起動はこのenumに含めず、モデル唯一のshellツール `ShellToolArgs` のみで表す。`attach` はCLIで `attachment.add` に正規化する。`--text-stdin` はCLIが読み取り `text` に正規化し、wireへOSのstdinハンドルを送らない。

要求は `protocol_version/operation_id/command/args/origin`。未知のtop-levelとargsキーを拒否する。`Request::validate` で版、空ID、limit、wait空配列、明示index対象を検査する。宛先省略は `Option<String>` のまま有効originをControlで検証して解決する。文字列origin IDだけで所有権を与えない。`Origin` のDebugはtokenを隠す。明示別data-dir接続時のorigin除去はCLIの責務。

`thread.set` のeffort未指定は変更なし、空文字はeffort解除とする。`thread.create` では未指定が継承/既定、空文字が明示的effortなしを表す。モデルへの空文字送信はしない。この区別はCLIの `--effort ''` でも利用できる。cwd/model/parent/対象IDの空文字は拒否する。本文空文字は空テキストと画像添付等の操作を許容し、送信内容の最終妥当性は添付選択も含めてControlが検査する。

成功/失敗は `Response<T>`。`success/failure` を利用し、受信時 `validate` を行う。片側だけを出力し、hint/details/run_id/turn_id/next_cursor等の未確定値は省略する。`MessageAccepted::validate` はpending時のID不在と割当済み時の両ID存在を検査する。各操作のcore結果をThread/Run/Turn/Shell/History/Attachment/Index/Storageのrecordで表し、provider payloadやwait snapshotのみJSON拡張値を許す。状態enumは原文のRun/Turn/attempt/shell/message/managed/index状態に対応し、Turnのfailedは論理DDLのCHECKに合わせて含める（引継ぎの遷移表は網羅列挙ではない）。API/proxy attempt errorはこの操作errorと区別する。

wait条件は `type` tagの `turn_completed/run_ended/shell_ended/shell_output` のみ。timeout既定30,000ms、0を許容、非負u64。any判定、同snapshot成立集合、循環拒否、登録と状態確認の直列化、停止とは独立したRPC取消しはControl実装の責務であり、型fixtureだけで検証完了とは扱わない。

通信frameは4 byte unsigned big-endian長＋UTF-8 JSON、版はenvelopeに置く。1 frameの上限は8MiB。最大1MiBのdraft/stdinがJSONで制御文字ごと6 byteへ展開されてもmetadataと共に収まるための境界であり、入力本文上限を引き上げるものではない。prefix前EOFは正常切断、途中prefix/payload EOFはio_failed、0/超過長はinput_limit、JSON不正はinvalid_argument。長さ検査を割当より先に行う。

一覧は `Page<T>`。既定50/最大200、検索既定10/最大50、本文既定/最大65,536 bytes。抜粋512 Unicode文字、多バイト境界、raw stream独立decoderは各取得実装の責務。`PageCursor` はbase64url(JSON)のopaque version1 token。scope/filter/selection又はbody revision/encoding/watermark/last sort keyを保持する。query/revision変更はcursor_expired、encoding変更はcursor_encoding_mismatch。ページsizeをtokenへ含めず変更可能とする。これは改ざん防止署名ではなく個人ローカル操作の整合境界。shell raw cursorはsequence/stream/offset/retention/decode持越しを必要とするため、PageCursorへ無理に流用せずshell実装の専用型とする。

## External client stability and versioning (Issue #244 A2)

The local owner handshake is the compatibility boundary for external clients, including a Web UI in a separate repository. Protocol 3 added an explicit capability list to `Welcome`; protocol 4 adds independently opt-in `events.reasoning_activity` to `events.wait` (#275). `events.reasoning_activity` advertises support. Clients must compare `protocol_version` first, ignore capability names they do not recognize, and use only known capabilities the owner advertises. This owner advertises `events.reasoning_activity`. There is no silent downgrade across protocol versions. `Endpoint` and `Welcome` tolerate unknown additive fields so metadata readers can ignore future fields, but typed RPC results tolerate additive fields only on the records explicitly marked as open result records. Request args remain strict: unknown operations/fields/types are rejected before dispatch with A1's typed `invalid_argument` and `request_may_have_been_applied: false`.

A wire change that a supported peer cannot safely ignore requires a protocol version bump and coordinated release. Use additive result fields only where the older client uses tolerant decoding; never infer compatibility from syntactic JSON parsing. The dynamic `events.wait` result may carry an additive field only with a protocol-version bump: typed RPC results are strict even when JSON is syntactically extensible. A capability documents the feature but does not make an old client tolerant. External clients may rely on documented commands/arguments, response fields and omission rules, enumerated errors, cursor behavior, sender provenance, capabilities, and documented bounds. They must not depend on SQLite tables, internal logs, undocumented `details`, implementation module names, event timing, or incidental ordering not stated here. Operation receipts make retries safe only with the same operation ID and same request; transport uncertainty is not evidence a mutation was unapplied. Cursor tokens are opaque and scoped to the issuing owner/query; restart or expiry requires resynchronization, not decoding or editing the token.

`protocol schema` exports the handshake, envelope, per-operation args/results, capabilities, and version from Rust types to `docs/reference/protocol.schema.json`. Results still represented by `serde_json::Value` are explicitly opaque. The committed document is exact-compared with generated output in the existing test suite; it is a client aid, not a replacement for these behavioral rules.

## P1 fixture 対応

`tests/protocol.rs` の匿名fixtureが入力/出力契約を固定する。

| テスト | 原文対応・証拠 |
|---|---|
| all_public_commands_have_typed_roundtrip_fixtures | CLI 3節の全35操作、正規化wire、serde roundtrip |
| rejects_unknown_fields_and_invalid_limits_without_echoing_inputs | CLI 1/6/7節、版・limit・空ID・未知field・対象未指定拒否 |
| success_and_error_envelopes_match_contract_and_omit_absent_fields | CLI 1/4節、envelopeと未確定ID省略 |
| assignment_never_exposes_fabricated_or_partial_ids | CLI 4節、pendingと割当IDの整合 |
| wait_conditions_and_timeout_are_typed_and_any | CLI 5節、全4条件・timeout既定/0・未定義条件拒否。実際のany判定は対象外 |
| cursor_binds_query_revision_encoding_and_watermark | CLI 6節、scope/filter/revision/encoding不整合拒否 |
| shell_defaults_and_origin_debug_do_not_leak_token | CLI 2/3節、shell既定とtokenのDebug秘匿 |
| frames_roundtrip_and_reject_truncation_oversize_and_invalid_json | 詳細設計12節、frame往復・EOF/途中切断・割当前上限 |

これらはP1 schema fixtureである。receipt冪等性、送信/削除race、wait循環、self stop、ページ中追加削除、raw文字コード再decodeはP2以降の実装テストが別途必要。

2026-10-24 macOSローカルで `cargo test --test protocol` は9件成功、`cargo clippy --test protocol -- -D warnings` は成功。追加の `fixed_error_codes_and_persisted_state_values_are_closed` はCLI 7節の18固定error codeと永続状態の閉集合を確認する。この結果はAPI/OS/実行制御の受入完了を示さない。

P1照合追記: Thread作成/変更argsの `connection` は名前付きResponses接続プロファイル、`model` はモデルIDとして独立に保持する。どちらも未指定は継承又は現状維持、空文字は拒否。`ThreadDetails.connection` で有効プロファイルを返す（仕様5節と設定契約）。`ShellToolArgs` はOSへ渡せないcommand NULとencoding名NULを事前拒否し、改行commandと初期stdinのNULは許可する。既知encodingの解決はshell runtimeで行う。

frame上限8MiBは固定通信境界である。設定でdraft/stdin上限を増やしてもframe上限を黙って変更せず、超過は `input_limit` として返す。入力本文検査はUTF-8本文byte数と設定値の両方で別途実施し、JSON envelopeのサイズ判定で代用しない。途中切捨ては行わない。追加fixture `connection_profile_changes_are_independent_and_shell_nul_is_preflight_error` がこの型境界を検査する。

P3照合修正: ShellReadArgs.encodingをOptionへ変更し、省略時にexecutionの保存済みdecode設定を維持する。明示指定だけが変更である。ShellOutputへdecode_errorsを追加し、完了端での不完全文字や不正byteの代替を表示と別に確認できる。詳細は [raw output設計](output.md) を参照。

`model.list` の追加契約・接続解決・receipt規則は [models.md](models.md#cliと共有control操作2026-10-25採用) を参照。原文32操作に対する採用済み追加である。


2026-10-25の添付契約是正により、AttachmentDetailsへ必須 `source_node_id` と `delivery_status` を追加した。add/listのresult配列という形は維持する。受付済みと反映済みを混同せず、CLI添付追加の出典IDを人間のmessage_idとして表現しない。所属と状態の扱いは [添付の所属](attachment-provenance.md)。

## 検索filter・出典・直接関係の追加契約

ユーザー採用の拡張であり、この追加時点では保存原文の32操作にmodel.listとhistory.relationsを加えて34操作となった。現行数は末尾の削除影響追加節を参照。詳細は [検索・関係設計](search-and-relations.md)。

`HistorySearchArgs` は既存query/limitへ任意thread_id/from/toと既定空配列kinds/exclude_kindsを追加する。未指定の新fieldはserializationで省略し、従来要求を保つ。queryは非空、limitは1〜50（既定10）。各kind配列は最大32件、各値は非空・最大64 Unicode文字。未知kindは将来拡張を妨げず、完全一致条件として扱う。from/toはoffset付きRFC3339を検査し、from inclusive/to exclusive、両端指定ならfrom < to。kind内はOR、その他はAND。Thread対象は現在pathとcompleted置換の出典閉包。thread_id省略時はglobalで自己対象解決をしない。

検索はfilter後にL2距離で順位を決め、chunkをnode単位にまとめる。同じ内容の異なるnodeは統合しない。各hitはnode_id/excerpt/distance/spans/provenanceを持つ。spansはsource_revision/content_hash/body_kind/start/endを持ち、抽出本文のUnicode scalar位置 `[start,end)` を示す意味検索chunk範囲である。語の一致範囲ではなく、threshold指定もない。Thread限定時だけthread_scope={thread_id,on_current_path}を返し、採用path上ならtrue、completed出典としてだけ対象ならfalse。globalではこのfieldを省略し、所属を推定しない。

`NodeProvenance` はkind/created_at/source_revisionと任意turn_id/run_id/origin_thread_id。未確定のIDを生成せず省略し、originを共有nodeの全所属と同一視しない。`HistoryBody.provenance` is required and is derived from the saved node.

`HistoryRelationsArgs` はnode_id、必須relation（previous/next/sources/replacements）、limit既定50・最大200、任意cursor。`Page<RelationItem>` の各要素はnode_id/provenanceと、置換関係に限るreplacement_id/source_revision/source_hashを持つ。previous/nextは会話辺、sourcesはcompleted置換出力→出典、replacementsは出典→completed置換出力である。relation item直下のsource_revision/hashは置換記録時の出典witness、provenance.source_revisionは返却nodeの現在版であり区別する。cursorは対象node・relation・revisionとwatermarkに束縛し、追加関係を途中pageへ混入させない。履歴変更・無制限の祖先/子孫探索は含めない。採用経路は既存history.listで取得する。

`history.list` and `history.window` execution links may include `description`:
the shell execution's text description, passed through the same bounded,
control-character-safe label formatting as the TUI. The field is omitted when
missing, non-text, or empty after formatting. It is display metadata only and
does not alter the command or execution. Reading it adds no table scan.

#### history.window（#268）

`HistoryWindowArgs` はthread_id、任意のordinal又はnode_id（両方は不可。どちらも無ければ末尾が基準）、任意のexpect_node_id（ordinalと併用のみ）、before・after（各既定20）、excerpt_chars（history.listと同じ）。`before + after + 1` は最大200（`MAX_LIST_LIMIT`）で、超過は`invalid_argument`。結果`HistoryWindow`は`{thread_id, revision, total, items}`で、itemsはhistory.listと同じ`HistorySnippet`（`selection.ordinal`が位置）を位置の昇順に並べる。cursorは持たず、絞り込み（run/since/state）も受けない。history.listの引数は変えない。

- `total` は採用経路の最後のordinal+1、`revision` はThreadのselection_revisionである。ordinalは全ての本番writer（追記はMAX+1、経路の採用＝rewind・compactionは0..nの書き直し、forkは接頭部の複製、中断tool結果の挿入は後続を後ろへ移して連続を保つ）で0から欠番なく連続するため、totalは件数と一致する。欠番は書き手のバグであり、windowは存在する行だけを返し、totalは最後の位置+1のままとする。
- 追記でrevisionは進むが既存の位置は変わらない。rewind・compactionはrevisionを進め、位置が移動又は消える（pruneは本文だけを回収し経路と位置を変えない）。revisionだけでは追記と巻き戻しを区別できないので、呼び出し側は保持している位置のnodeを`expect_node_id`として渡す。
- 失効：ordinalが末尾より後、nodeが採用経路上に無い、`expect_node_id`が一致しない場合は`cursor_expired`を返し、`details`に現在の`revision`と`total`を付ける。呼び出し側は基準無し（末尾）又はtotal内の位置で読み直す。存在しないThreadは`thread_not_found`。
- 費用：総数は`thread_path`の主キー末尾の1回のseek、基準nodeはUNIQUE(thread_id,node_id)の1回のseek、窓は主キー範囲（`ordinal>=? AND ordinal<=?`）で、経路の長さに依存しない。EXPLAIN QUERY PLANとVM step数（経路100行と100,000行で同等）はtests/history_window.rsで検査する。read laneで実行し、writer laneを使わない（`PureRead`）。

## 操作追跡・context・削除影響の採用済み拡張

現行操作数は37。`thread.delete_preview` は既存ThreadDeleteArgsを用いる読み取り専用操作で、CLIは `thread delete-preview --thread ID [--descendants]`。同snapshotの阻害Run・直接参照・対象件数を返し、実削除の再判定は維持する。`reclaimable_bytes` はnullと理由を返し、GC到達性や物理容量を推定しない。[全体設計](observability-and-deletion.md)、[削除影響](deletion-impact.md)を参照。

Thread archive (#222 stage 1) adds `thread.archive` and `thread.unarchive` (both `ThreadArchiveArgs {thread_ids: 0..=200 IDs, descendants}`; empty means the calling Thread, over 200 is `input_limit`) and the `archived` field of `ThreadListArgs` (`exclude` default, `include`, `only`; omitted on the wire when `exclude`, so existing operation fingerprints are unchanged). `ThreadSummary` and `ThreadDetails` carry an optional `archived_at`. Results: `{archived|unarchived, already, not_found, descendants}`. Details: [thread organization](thread-organization.md).

Re-parenting (#222 stage 2) adds the optional `parent_id` to `ThreadSetArgs` (a Thread ID moves the Thread under that live Thread, the empty string detaches it; omitted on the wire when absent, so existing operation fingerprints are unchanged). A successful move adds `parent_change {previous_parent_id, live_run_id, live_run_owner_run_id}` to the `ThreadDetails` result and reports the moved Thread, the old parent and the new parent in the changed set (`Control::changed`, so `events.wait` clients refresh all three). Setting the current parent again is a no-op that publishes nothing. Errors: a self-parent or a new parent inside the Thread's own subtree is `invalid_state`; an unknown, deleted or missing parent or Thread is `thread_not_found`; a new parent with more than 1,024 ancestors is `input_limit`.

Thread tags (#222 stage 3) add `thread.tag` (`ThreadTagArgs {thread_ids: 0..=200 IDs (empty means the calling Thread), add: 0..=64 tags, remove: 0..=64 tags}`; at least one tag, a tag in both lists is `invalid_argument`) and `thread.tags` (`ThreadTagsArgs {limit 1..=200, cursor}`, a page of distinct tag names in byte order, names only). `ThreadListArgs` gains `tags` (0..=64, AND; omitted on the wire when empty) and `cwd` (the saved cwd compared after the same lexical cleanup as saved cwds; a relative path is `invalid_argument`; omitted when absent), so existing operation fingerprints are unchanged. `ThreadSummary` and `ThreadDetails` gain `tags` (sorted, omitted when empty). A tag is trimmed, one line, 1..=128 Unicode scalar values, no control characters; case matters. A Thread carries at most 64 tags and a call that would exceed that is `input_limit` and changes nothing. `thread.tag` result: `{changed, unchanged, not_found, add, remove}` (`add`/`remove` are the normalized request); `changed_threads` is the changed Threads only, a replay with the same operation ID publishes nothing, `updated_at` is untouched. `thread.cwds` (`ThreadCwdsArgs {limit 1..=200, cursor}`) returns distinct saved cwds of non-deleted Threads (archived included) as strings in byte order, no counts. A list cursor is bound to a digest of the sorted tag set and the cwd (and to the archived mode), so it cannot move between filters (`cursor_expired`). Details: [thread organization](thread-organization.md).

RunDetails/TurnDetails/MessageDetails/HistoryBody require context. HistorySnippet requires provenance, context, and selection; HistoryBody also requires provenance. ThreadDetails may omit context for a fork. These records are not defaulted to empty values to accept older owners. Current ShellOutput.decode_errors, history execution_count/executions_truncated, and StatusSnapshot.receipt_budget are also required; payload_chars remains nullable for reclaimed bodies and an empty executions array may be omitted. 保存された時刻・所有・状態と構造化理由だけを返し、検索NodeProvenanceは拡大しない。lifetime_msの閉区間と未知値省略は[実行情報](execution-context.md)に従う。

operation.getは既存state/outcomeへinstance_id/created_at/current_instance/result_uncertain/replay_policy/targets/downstreamを追加する。compactの受付receiptは不変とし、後続結果はdownstreamへ分離する。attachment.addの保存済み同ID照合は現在のファイル/Threadのpreflightに先行する。認証・構文・要求fingerprintは維持する。全mutationの境界は[operation観測](operation-observability.md)を参照。

## Originの認証と保存境界（P3/W6）

`Origin` はIPCの認証用wire型であり、tokenは本体の発行記録と全IDを照合する。保存・表示には別の借用型 `OriginIdentity` の5識別子だけを使い、receiptのorigin/senderとruntime intentにtokenを保存しない。request fingerprintは従来どおり検証済みOrigin全体のhashであり、異なる資格情報による同operation IDの再送を同一視しない。識別子だけのidentityを認証入力として受理しない。型・worker・表・依存の追加はこの借用projection型1件だけで、認証と保存の混同を型で防ぐためである。

この機構は同ユーザー内の誤識別防止であり、同権限の任意shellを隔離するsandboxではない。tokenを自動ログ・通常モデル入力・保存provenanceへ複写しないが、shellが環境変数を明示出力すればその出力は原文rawとして保存され、tool結果にも含まれ得る。任意出力を書き換える機密フィルタや完全な漏洩防止は保証しない。利用者は秘密を出力しない責任を持つ。資格情報の正規置場は実行環境と発行mapであり、保存originに資格情報を入れてよい理由にはならない。

## 送信前の拒否と送信後の不確実性（P5）

要求全体をJSON化しenvelopeを含む8MiB上限を検査してから、requestのprefixを書き始める。
この検査で拒否した場合は要求未送信であり、`request_may_have_been_applied=false` と
使用予定のoperation IDを示す。本文を減らす等の修正後に再実行でき、存在しないreceiptを照会するよう促さない。
requestの書込み開始後にI/O失敗、途中切断、または不正な応答が起きた場合は、部分送信/適用の可能性を
否定せず、同じoperation IDの照会を案内する。対象を変えない観測はreceiptを持たないため、同じcommand/filterでfreshな再照会へ案内する。semantic検索は再びquery embedding費用が生じ得る。waitは待機状態を保存するためreceiptを維持する。
サーバ側でoperation IDを確定できない不正frame/JSONは接続を閉じ、架空IDの応答を作らない。
frame長が適正なJSON requestをdecode後に検証で拒否した場合は、元IDのerror応答を返して閉じる。
未知のcommand、未知のtop-level/args項目、型違い・必須欠落（例: 新しいclientが古いownerへ送る追加項目 `human`）も、非空 `operation_id` を持つ適正なJSONなら同様に元IDの `invalid_argument` を返して閉じる（#244 A1）。`message` は未知のcommand/項目名（制御文字を `?` に置換し64文字まで）を示し、値は含めない。`details` は `reason`（`unknown_command` / `unknown_field` / `unknown_request_field` / `invalid_shape`）、該当する `command` と `name`、`request_may_have_been_applied=false` を持つ。decodeは副作用・dispatch・admission・receipt作成より前に完結し、拒否時は何も実行しない。応答を受けたclientは不確実な変更として扱わず、receiptの照会も案内しない。`operation_id` を取り出せないframeは従来どおり応答なしで閉じる。requestは厳格（`deny_unknown_fields`）のままで、変わるのは失敗の返し方だけであり、PROTOCOL_VERSIONは変えない。
wire版・8MiB上限・既存error codeは変更しない。


## owner操作ID未発行のローカルエラー（P4）

原CLI契約どおり、handshake前のローカル検証/接続失敗は、operation IDを持つ場合だけ表示する。
不在時に空文字や仮のowner IDを生成しない。ローカルerror JSONから `operation_id` を省略し、
ownerからのResponse型は引き続き非空ID必須とする。明示 `--operation-id` または通信失敗の
`details.operation_id` が既知の場合は従来どおり表示し、再照会可能な実IDを保持する。


## CLI構文エラーの回復情報（P9）

clap解析エラーは固定invalid_argumentを保ち、parserのerror kind、既知引数名、commandのusageを
ローカルerrorへ含める。parserの完全な表示文は不正値/長い本文/資格情報を含み得るためそのまま複製しない。
引数名はCLI定義に存在するlong/short名との一致だけを返し、未知subcommand/引数や入力値を捏造して
正当な引数として表示しない。usageとerror kindで原因区分と有効な構文へ到達できるようにする。
value parserがusageを持たない場合は静的なroot command usageへfallbackし、個別commandのhelpへ案内する。
owner接続前の失敗なのでreceiptは作らず、P4の未発行operation ID省略を維持する。
### IPC受付の有限性と障害（P1）

listenerと接続taskは既存IPC task内で管理し、同時に保持する接続taskを32件までとする。
完了taskの回収をacceptより優先し、満杯時は追加acceptを停止する。OS backlog内の接続は
枠が空くまでhandshakeを待つため、CLI側の接続/handshake期限で失敗する場合がある。
枠を超えるtaskや無制限の待ちqueueを追加しない。既にControlへ渡したmutationの所有は
IPC接続へ移さず、観測切断・受付停止でも受付済み処理は継続する。

Interrupted/接続途中の切断・一時的resource不足（EMFILE/ENFILE等）は50msから1秒までの
有限backoff後に同じlistenerで再試行する。backoff中も停止できる。恒久的なaccept障害では
listenerを閉じ、接続の観測を解除してtaskを回収し、自instanceのendpoint metadataとsocketを
清掃してから一度だけ失敗状態をwatchへ公開する。TUI frontendはその通知を一般statusに
表示し、CLI受付の復旧には本体再起動が必要だと示す。Run/Turnの状態や保存済み操作結果を
変更しない。新しいコアworker/event/schemaは追加しない。

Windowsのpipe接続後に次のpipe instance作成が一時失敗した場合、接続済みpipeを保持して
backoff後に作成だけを再試行する。接続前にclientが消えたpipeはdisconnectして再利用する。
共有受付ループの障害注入は実fd上限を下げずに行う。Windows実機の動作確認とは区別する。

### 要求frameの読取り期限とbuffer（P6）

Helloの既存5秒期限に加え、Welcome後の要求frame全体にも別の5秒期限を設ける。
prefixの途中・payloadの途中・未送信のいずれも同じ期限を消費し、partial到着で延長しない。
期限切れはrequestをdispatchせず接続を閉じ、bufferと接続枠を解放する。待機操作の処理時間や
受付済みmutationの完了待ちはこの読取り期限に含めない。

frameの8MiB上限を先に検証し、payloadはTokioの長さ制限付きreaderで実受信に応じて蓄積する。
広告された長さだけで8MiBを先にzero-fillしない。宣言byte数に満たないEOFはJSON解析前に
拒否し、次frameのbyteを取り込まない。上限変更や公開timeout設定は追加しない。

### IPC接続と受付済みworkの資源所有（P1/P6）

接続上限は当初候補128から32へ下げる。対話用のCLI並列・待機の余地を残しながら、
最大frameを同時に受ける負荷を4分の1にし、他のDB/socket/shell用fdも残す内部既定値である。
32接続の受信payload合計は最大256MiBだが、Vec容量・JSON展開・返信・結果・その他の
cache/queueを含むプロセス総メモリの上限ではない。prefixだけの接続にはこの全量を確保しない。

接続枠と処理枠を分ける（PR1-2採用差分）。通常workは24、停止操作（RunStop/ShellStop）は2、Status観測は2の独立した有限permitとし、32接続中4枠以上をhandshake/拒否応答へ
残す。長時間waitは通常枠を使い、通常処理の満杯だけでは停止/Statusのtransport受付を塞がない。
32接続/受信payload予算は増やさず、常時保持するIPC workは合計28以下とする。slow frameの
接続も従来の5秒期限に従う。同ユーザーによる全接続枠への意図的な占有を防ぐpermission engine
ではなく、各分類の上限到達はあり得る。

要求の構文/instance検証後に対応permitをtry取得し、満杯ならControlへ渡さず `invalid_state` と
`request_may_have_been_applied=false` を返す。待機queueや満杯待ちtaskを追加せず、同じIDでの
後の再試行を案内する。正当なhandshake応答の版不一致と無応答timeoutを区別し、後者は要求未送信の
I/Oエラーとする。受信済みrequestの未知操作や版互換そのものは別の契約である。

取得したpermitは`Invocation`内部fieldへ所有を移し、`Control::serve`の評価・副作用反映の終了まで
保持する。観測切断で早期返却したり受付済みmutationを取消したりしない。従来の「切断して再接続で
未完workを無制限spawn」回帰を維持する。transport上の停止/観測枠はreceipt予算や認証を迂回せず、
操作自体の容量拒否は別に返す。UI直接操作やmodel/shell自体をこのIPC予算に含めない。

内部の3 semaphore所有・分類だけを既存IPC moduleへ集約し、新worker/queue/core event/永続型を
加えない。各分類の許可済work、接続、受信frameを別に数え、満杯の通常work下で停止/Statusが進むことと、
切断済mutationが処理完了まで枠を保持することを障害注入fixtureで検証する。

### Unix socketの専用namespaceと残存物（Issue #2）

ソケットの配置先はownerが決める。使えるXDG_RUNTIME_DIRを最優先とし、macOSでは次にOSの `confstr(_CS_DARWIN_USER_TEMP_DIR)` から得たユーザー専用一時領域、最後にcanonical `/tmp`の順で、ランダム名の0700 directoryを原子的に作る。Linux等ではXDGの次がcanonical `/tmp`となる。macOSのユーザー一時領域も実効uid所有0700・canonical祖先の保護と103byte socket境界を満たす場合だけ使い、環境変数TMPDIRを配置の根拠にしない。候補への作成が失敗すれば次の安全な候補へ進み、再起動時の既知locator清掃も同じ許可base集合を使う。baseの所有・権限・sticky・symlinkを検証し、不適切/長すぎる環境候補は使用しない。data-dirの権限や祖先をソケット配置の成立条件にせず、固定uid/data-dir hash、`/run/user`候補探索、data-dir下の`.ipc`は使わない。ソケットのUTF-8 pathは103bytes以下とする。新しい表・worker・設定は追加しない。

`endpoint.json`の既存addressが配置の正本であり、ランダム名は認証nonceではない。人のCLIはmetadataを読み、モデルのshellには `SHELLBOY_SOCKET` で実際のpathと既存のinstance ID/data-dirを渡す。CLIは環境data-dirと対象が一致し、有効なsocket/instanceがある場合に限りmetadataを読まずに接続する。明示的に別data-dirを選んだ場合は通常のmetadataへ戻す。環境経由の失敗から別endpointへ自動fallbackせず、既存instance handshakeを維持する。ownerからControlへはpublish済みendpointのメモリ上のsnapshotを共有し、tool起動のためのfile読込み・DB状態・workerは増やさない。

Unix接続の両側で、hello/request送信前にTokioのpeer_cred（Linux SO_PEERCRED、macOS getpeereid）からpeer uidを得て実効uidと照合する。未検証接続は拒否し、サーバーはその接続だけを破棄して受付を続ける。配置先の秘匿性やorigin tokenの代用とは扱わず、Windows named pipeの既存経路は変更しない。

owner lock取得後に前回endpoint metadataの既知pathだけを確認し、許可したbase・正しい専用名・実効uid所有0700・symlinkでないdirectory・既知socketだけの内容・socket所有uidを検証する。250msの接続probeが拒否/対象消失の場合だけstaleとし、directoryのdevice/inodeとsocketのdevice/inode/ctimeが変わっていないことを再確認してsocketと空directoryを削除する。生存/判定不能、未知形式や別の場所を指すmetadata、他者のfileは保持し、任意走査・recursive deleteはしない。新規directoryのmkdir前に予定pathを既存endpoint.jsonへ原子的に公開する。locatorだけ、空directory、bind済みsocketの各段階の異常終了後も、次回起動で安全に検証・回収できる。正常終了・公開失敗の清掃も自身のidentityに限定する。旧固定名領域の自動移行清掃は行わない。

会話保存先はソケットと独立に、Unixで新規作成時0700とする。既存保存先も自分のdirectoryであることを確認し0700へ限定するが、symlinkや他uid所有へchmodしない。作成・検証は起動のfile workerで行い、UI threadへI/Oを持ち込まない。OS一時領域自体へ書けない等の残る失敗では、利用者へ次の操作を示し、詳しい理由は詳細情報へ保持する。XDG_RUNTIME_DIRを優先する。

Unixのdata-dir末端はsymlinkを拒否し、実ディレクトリを明示するよう案内する。祖先のsymlinkを一律禁止する契約ではない。起動中のprobe失敗・生存socket・probe timeout・namespace lease・bind失敗は既存`io_failed`のまま、人向け原因と次操作、detailsの`component=local_ipc`・`phase=startup`・`reason`・低層`cause`を返す。生存/判定不能socketは保全する。稼働後listener失敗の「受付済みreceiptを確認する」案内を起動時には流用しない。wire型・状態・永続記録は増やさない。

Linuxでは新規live namespace directory自身を`O_DIRECTORY|O_NOFOLLOW`で開き、作成したdevice/inode・
実効uid所有0700を照合して、既存fs2のnonblocking exclusive BSD flockを取得する。directory FDは
既存のSocketFile/EndpointFilesがlistener終了と自身の清掃まで1本保持し、最後の正規所有者のDropで
明示unlockする。stale候補のcapture/probeはlockを取得せず、生存socketへの二重bind拒否を維持する。
取得失敗時はbindせず、同じdirectory identityの空領域と自身のendpoint metadataだけを清掃して理由を返す。
新しいlockfile・table・event・worker・queue・依存は追加しない。

この保護は[systemd-tmpfilesのage掃除](https://www.freedesktop.org/software/systemd/man/latest/tmpfiles.d.html)
がBSD flock済みdirectoryと配下をskipする仕様に対するもの。macOSの標準清掃全般、Linuxの最終logout/
user-runtime-dir unit停止、reboot/unmount、管理者や利用者の明示削除、非協調cleanerまで防ぐ保証はない。
XDGはセッション寿命を持ち、Shellboyはlinger設定を変更しない。消失後の自動再bind/環境endpoint差替えや
mutation再送は行わず、保存済みoperationを確認してownerを通常終了/再起動する。


PR1-12ではUnix socketの型・field・清掃をUnix cfgへ揃え、Windows named pipeにはmetadata清掃だけを残す。
未生成型をOption field/cleanup経由で使うだけではRustのdead_code警告は必ずしも発生しないため、
元指摘のWindows Clippy失敗は確認済みとは扱わない。偽のSocketFileやallow(dead_code)を足さず、
OSごとの所有対象を型の構成で明確にする。Windowsの実機受入はcross checkとは別に残す。

### 同じwire版の旧ownerと未知操作（PR1-10）

Historical protocol-2 behavior (superseded by Issue #244 A2): protocol-2 request/response envelopes rejected all other versions; current required fields were not made optional for older owners. That version-mismatch diagnostic and receipt guidance remain, but protocol 3 added the capability list and tolerant output records described above; protocol 4 is current for the strict `events.wait` extension. For external clients, the generated contract is `docs/reference/protocol.schema.json`; the version-4 `events.wait` result is included with its concrete type. Other results still represented by `serde_json::Value` remain opaque. The exact drift test keeps it synchronized with the wire types. Additive result fields are accepted by open records; unknown request arguments remain typed `invalid_argument` errors before dispatch. Any incompatible protocol change requires a new `PROTOCOL_VERSION`, exact handshake match, regenerated schema, and an explicit migration note; no silent downgrade is supported. The strict typed `events.wait` result uses protocol 4; capabilities describe feature support but cannot make protocol-3 result decoders tolerate new fields. No capability advertisement alone makes a strict result additive; every incompatible result shape requires a version bump.

## R3-3 / R3-4: Command分類・語彙・既定値の正本

Commandのwire名とdispatch routeはprotocolの網羅matchを正本とする。route、runtime actionをshutdownでdropできるか、取消後も許可するcleanup、C5 capture前に完了すべきcontext mutationは別の意味であり、独立した網羅matchに既存集合を移す。共通metadata registryやschema DSLへ統合しない。人間向け日本語title、入力前のTUIフォーム名、IPC通常24/停止2/Status2枠、shell所有者の認可targetはそれぞれ層の責務を保持する。元レビュー時点のfrontend英語36名は既に日本語表示となっているため、protocol名への置換で表示を退行させない。

read-onlyは対象の状態を変更しない観測であり、Issue #3対応ではreceipt不要とする。API利用や一時pin/identity検証の有無は別概念である。Waitはorigin Turnのwaiting/runningとshell release outcomeを記録するためread-only=falseのままreceiptを維持する。IndexStatusは有限の保存情報readで、取消集合の変更は行わない。

AttachmentKind/IndexScope/StorageCategory/HistoryRelationの語彙をprotocolへ揃え、CLIには薄いclap ValueEnum adapterを残す。protocolへclap deriveを入れない。storageのwire managed_imagesとCLI managed-imagesは層固有の表記として維持する。検索10/最大50、一覧50/最大200、wait30000ms、本文64KiB、UTF-8の同義既定値をprotocol定数へ揃える。TUIの一覧20件は管理フォームで最初に詳細を読める小さなpageを選ぶ意図的差分として名前付き定数で維持し、status/relationsの50件やraw表示用16KiBを同値と誤認して置換しない。

R3-3とIssue #3の挙動採否: IndexStatusの取消集合への追加はしない。Waitのcleanup/Turn状態保存も既存所有経路で完了する。送信後に観測の返信が失われた場合はread_only=trueと同じcommand/filterでのfresh再照会を案内し、観測結果のreceipt照会へ誘導しない。変更とWaitは元ID/要求/callerによるreplay又は期限切れを照合し、未実行と決め付けない。認可、送信前拒否、受付上限、frame上限を維持する。

## R3-5: 検証責務と公開境界

Commandの非自明な検証をargs型へ移し、単純なtarget参照とvariantに依存するindex対象必須判定はCommandに残す。Requestのstrictなtop-level field集合を両parse入口で共有し、args/origin/WaitConditionの未知field拒否も維持する。RFC3339 parserは共有するが、要求のinvalid_argumentと保存時刻不整合のinvalid_state、既存message、呼出元ごとの検証順序を混ぜない。

原レビューの単位統一は採用しない。Statusのnative stateは32件・各64 UTF-8 byte、検索kindは採用済みsearch契約の32件・各64 Unicode scalarである。前者を拡張／後者を制限するのは別の公開契約変更であり、同じ64という理由でまとめない。共有helperへ機械的に押し込まず両境界を明記して保持する。添付path等の本文単位やframe8MiBも代用しない。

HistoryListのsinceは原契約のoffset付きRFC3339をprotocolの事前検証でも確認する（純移動後の独立挙動変更）。同時に不正なnative statusがある場合は、従来history readerと同じstatus→sinceのfirst errorを保つ。既存thread/limit/cursor/run/空文字検証の順序は変えない。readerの直接呼出しでも検証を保ち、保存済みnode時刻の破損とユーザー入力不正を区別する。副作用受付やDB読取前に無効な時刻要求を拒否できるようにし、valid要求のJSON・receipt・結果上限を変えない。

## 索引の候補preview（G3）

`index.preview` / `shellboy index preview` は既存IndexArgsとFeePreviewを公開する追加のread-only操作。An omitted scope/Thread/source previews currently failed authorized work for global retry; explicit selections retain source preview semantics. A space alone restricts retry preview rather than authorizing backfill/rebuild.既存runtime routeへ通し、ファイル読取りを含むためshutdown取消可、changes_context=falseとする。既存一時pinは維持し、receiptを作らず、embedding API/job投入/candidate作成/過去分許可は行わない。previewは同IDでも最新候補を再観測する。確認tokenや長期snapshotを発行せず、backfill/rebuildの公開構文と非対話受付を維持する。

## 観測の再照会と結果失効（Issue #3）

get/list/search/preview/statusはreceiptを作らず、同IDでも再観測する。変更操作とwaitは保存済み要求照合を維持する。現instanceの終端結果は24時間経過後の保守又は圧迫時に結果本文を縮退できるが、pending/未反映参照を保護しID/hash等は保持する。同ID再送はoperation_expiredで、変更を再実行しない。別内容はoperation_conflict。operation.get/statusのresult_expiredとreplay_policyで区別し、読取り自身の照会IDは受付記録の対象ではない。既存error code・wire版・frame上限は維持する。詳細は [operation設計](operation-observability.md) を正本とする。


`context.edit`は`thread_id`（省略可能）、`action`（spill/summary/omit/trim/restore）、`from_node_id` / `to_node_id`（モード別）を持つ。受付結果は既存compact同様のqueuedと`context_edit`引数を含み、receiptを上書きせず後続結果をdownstreamで返す。Thread詳細には`context_usage`を追加する。停止中など推定の非storageエラーは`available:false`/errorとして表現し、Thread照会自体を失敗させない。詳細は[圧縮設計](compaction.md)。

### Thread list activity (Issue #44)

`thread list` preserves `state` (latest Turn/native fallback) and separately
returns snake_case `activity`, using the same saved-fact classifier as the
conversation footer. In particular, `state: "completed"` can coexist with
`activity: "background"` or `"continuing"` while the latest Run remains live.
`background` requires an active owned shell or child Run; `continuing` does
not assert such work. Exit markers and Revisit are independent of activity.

### Cross-Thread human waiting list

`thread.list` accepts optional `awaiting: "human"` (exclusive with `query`,
`tags`, and `cwd`; it composes with `archived`, `limit`, and `cursor`). The CLI
spelling is `shellboy thread list --awaiting human`. This returns a keyset
page of only Threads with pending human-directed reply receipts. Items carry `awaiting_human_count` (an exact count up to 128, saturated at 128), `awaiting_human_count_saturated` (true iff the count exceeds 128), and nullable `awaiting_human_oldest_at` (the oldest `created_at` among the first 129 matching receipts). Both the count and oldest-time values are derived from at most 129 matching receipts; the count is exact through 128, then fixed at 128 with the saturation flag, never an estimate. Thread-directed
reply duties do not qualify. Cursor scope binds the filter, archived mode, and
receipt watermark. The reader uses the schema-36 partial index
`receipt_human_reply_pending(target_thread_id, created_at)` and hydrates summary
rows only for the returned page. Plan and SQLite VM-step counts are fixed by a
regression test; no full Thread-table scan is permitted.

### Observing long-running shells (Issue #45)

`shell get --execution ID` and shell entries in `status` expose saved
`started_at`, nullable `last_output_at` (latest retained output sequence),
and `elapsed_ms`. While starting/running/stopping, elapsed_ms is calculated
at read time from the saved start and wall clock, not a process-liveness
check or monotonic measurement. For terminal shells it is the saved completion
duration, if known. No output or reclaimed output gives last_output_at: null.
Status timing does not affect activity_at, sorting, cursor or fingerprint.


## Index operation additions (#42/#43/#55)

`index.probe` is a cancellable runtime mutation with ordinary operation receipts.
Its optional `dimensions` is an integer in 1..8192. It sends exactly one fixed
input and records success or failure; it does not enqueue indexing or retry
with another dimension. `index.rebuild` accepts the same optional field; all
other IndexArgs commands reject it. There is no dimension configuration default.

Index status exposes saved `paused_at`/`pause_id`, up to 200 `spaces` with
`spaces_total`/`spaces_truncated`, and the newest 32 `probes`. Each space carries
its connection/model, observed `dimension`, optional `requested_dimensions`,
segment count, state/selection, `vector_payload_bytes` and, for an int8 space,
`int8_scale`. Payload bytes mean vector values only (4 bytes per element for float32,
1 for int8), not database/WAL or reclaimable storage. Missing historical
pause time remains unknown. Probe observations are per endpoint/model/request
mode, not a provider capability catalogue. See [index operations](index-operations.md).

## Index shrink and remove (#253)

`index.shrink` is a cancellable runtime command with ordinary operation receipts and optional
`dry_run`, `cancel` and `revert` booleans (`cancel` excludes the others). It never sends an embedding
request and never deletes a space. `index.remove` takes `space_id` and optional `dry_run` and deletes
one inactive space. Index status adds an optional `shrink` (source_space, target_space, phase,
source_segments, target_segments, started_at, error), an optional `retained_vector_payload_bytes`,
and per space an optional `int8_scale` and `retained_remove_with`; `vector_payload_bytes` follows
the element size. At that time the change was additive and left protocol version unchanged; protocol 3 was introduced later by Issue #244 A2; protocol 4 is current after #275. See
[search and relations](search-and-relations.md#int8-conversion-253).

## Instruction references (#51)

thread.create/set/fork accept optional `agents`; thread.get returns required
`agents_path`. At the time of this change, protocol 2 was retained for this pre-1.0 additive change; protocol 3 was introduced later by Issue #244 A2; protocol 4 is current after #275.
See [the reference contract](agents-reference.md) for validation, relative paths,
atomic mutation and context_changed/agents_reference delivery.


## Pending Input Operations (#52)

Adopt [pending messages](pending-messages.md). `message.send` accepts optional
`now: bool` (default false); its typed MessageAccepted result reports `now` as
the explicit foreground-release choice. Normal input queues at the next request
boundary. Neither mode cancels response generation.

`message.cancel` is a receipt-backed database mutation with `message_ids`, a fixed
list of1..128 distinct nonempty IDs. Its `items` retain each ID and typed outcome
(cancelled, already_cancelled, already_admitted, not_found, unavailable),
with optional thread_id/archive_draft_id. Per-ID domain failures permit partial
success; a storage failure rolls back the transaction. Any model or human caller can cancel queued input in the target Thread. Admission/path facts
are checked in that transaction. Replay returns the original saved result.
Internal receipt metadata is projected by its saved assignment fields, without
relaxing public DTO unknown-field rejection or losing the immutable assignment.

Human Turn interruption is deliberately an in-process TUI operation, not a new
model wire command. Exact Turn targeting, durable intent, Run-stop priority and
model notification follow the adopted design; existing lifecycle enums remain.

### Failure-driven context editing (#50)

`compact --maximal` selects ContextEdit action maximal with no from/to nodes;
`/compact-max` is its TUI counterpart. It calls no summary API and does not
resume a Run. Edit results replace token estimates with before_characters,
after_characters and changed_suffix_characters (replay JSON Unicode scalars).
No target_tokens field remains. thread get context_usage reports only the last
successful measured usage, start/end time and next_request_pending after an edit.

## Message sender inspection (#59)

`message.get.sender` and user-node `NodeProvenance.sender` expose saved sender
facts through `receipt.message_node_id`, not the receiving Run's starter.
Sender kind is `model` (verified Origin identity, five credential-free IDs),
`external_cli` (new message.send without Origin), `human` (in-process draft/edit
submission), or `unknown` (no sufficient saved evidence). Legacy bare complete
OriginIdentity JSON remains factual model provenance; legacy null/empty or
incomplete evidence stays unknown. This is a read-only provenance projection,
not an old Core request-format compatibility path or identity reconstruction.
Sender IDs do not require the originating Thread to still exist.

`message.send` also accepts optional `human` (boolean, default false, omitted when false): `shellboy chat`
sets it when stdin is a terminal so the sender is recorded as `human`, as for a TUI submission. It is an
unverified client claim, never authority, and is ignored when a model Origin is present (kind `model`).

`message.send` accepts optional `sender_label` (`--sender-label`): at most 64
Unicode scalar values, no control characters or Unicode line/paragraph separators.
Empty labels are allowed. Labels are self-asserted display metadata, never
identity, authorization, ownership, stop, retry or cancellation authority. Store
kind/label alongside existing top-level identity fields in `receipt.sender_json`;
keep the top-level `thread_id` used by queued-message cancellation unchanged.
No schema/table/migration is added. Input cannot select a sender kind or IDs;
Control derives them from the authenticated Origin or the trusted internal send
entry point. Cross-data-dir CLI still strips Origin, retaining only the label.
When a non-empty label is saved for a non-human sender, admission appends one `sender_label` context notice (message id, label, "self-asserted, not verified") after the message node, through the same append-only admission-notice path as `reply_expected`; message text is not changed and earlier nodes are not rewritten.

TUI sender display is confined to F6 message details (including the original
page), fetched on demand through existing bounded original reads. It labels a
self-asserted label `名乗り：…`, separate from verified sender kind/IDs. Unknown
sender facts carry a human-readable missing-evidence explanation in F6, including
old records, without changing the wire `unknown` kind. Conversation band styling,
indexing and refresh scheduling are not redesigned here. Unknown evidence never
becomes a claimed human or external CLI merely from message role or absent Origin.

The existing conversation source classifier recognizes newly recorded `human`
receipts as its established Human category, without adding a new badge or
reading labels. Its historical null/Human display convention remains unchanged
by scope; it is not evidence of verified sender identity. Sender inspection in
CLI/F6 reports such legacy null receipts as unknown. No renderer/layout or
index-refresh change is part of this preservation adjustment.


Decision D-006 (#68) explicitly preserves the conversation's historical null/Human
band convention to avoid changing all old inputs' appearance. New external_cli
objects fall into the existing generic input category instead, so the claim that
all conversation rendering is unchanged would be inaccurate. CLI provenance and
F6 remain evidence-based unknown for old null receipts; the human band is not a
sender fact. This follow-up changes documentation and F6 explanation only, not
source classification, bands or #41 rendering.


## End-message read projection (#65)

history.get accepts optional end_message_only (false by default; omitted from
ordinary serialized requests). It applies only to assistant_response originals,
selects the last assistant message with phase final_answer or the last unphased
assistant message, and excludes reasoning/commentary. Its history.get.end_message
cursor scope is distinct from the unchanged ordinary history.get scope; node,
revision and UTF-8 cursor binding still apply. Missing final messages are explicit
unavailability, not fabricated answers. No provider/API execution format or
stored payload changes. The exec client uses this projection for local Schema
validation rather than parsing reasoning as an answer.

## History-index policy visibility (#71)

#254: `IndexArgs.kinds` (optional repeatable words: the six node kinds or `all`) adds
kinds to `indexing.auto_kinds` for that command's Thread/all selection;
`FeePreview` gains `excluded_sources` (approximate: sources of other kinds in the selection, deduplicated across selected Threads), `kinds` and `indexed_outside_selection` (kind -> sources the active space holds that a `--scope all` rebuild would drop; that rebuild is refused without `--kind`); `IndexStatus` gains
`auto_kinds`; history `scope_status` gains `auto_kinds` and
`kinds_not_auto_indexed`. These are additive fields without serde defaults on
responses (owner and client are the same build). `index retry` rejects `kinds`.

HistorySearch now returns `{items, auto_history, degraded, scope_status}` instead
of a bare hit array. Disabled automatic ingestion is an explicit partial-coverage
warning, not a claim of measured missing counts. Existing IndexHit provenance and
span semantics remain unchanged. IndexStatus emits the actual auto_history bool. The #71 review explicitly adopts
serde defaults for its additive auto_history (false when absent) and
held_auto_history (0 when absent) projection fields. This narrow decoding rule
does not change the configuration default (true), permit unknown request fields,
or add Core data/schema compatibility paths. held_auto_history counts pending
automatic entries excluded from pending while auto_history is false.

## Owner memory observation (#80)

StatusArgs.memory defaults false. True selects an owner-wide runtime read-only
observation (no immutable receipt), exclusive with activity scope/time/state/page
filters. It uses existing UI event/update delivery for a request-completion reply,
not a new work queue. Native collection shares existing control-file admission and
does not touch the DB worker. Other status queries keep their existing projection
route. Cancellation discards the reply; native file tasks retain their existing
permit until real exit. No persistent schema or local protocol version changes.

## Thread observation extension (#74)

See [Thread observations](thread-observation.md). WaitArgs adds optional after;
WaitCondition adds thread_settled with explicit Thread IDs and settled-state
filters. Existing any-condition results keep their shape, with a Thread cursor
and snapshot.changes/more when these conditions are present. ThreadSummary and
ThreadDetails add last_end_message/last_input optional saved-fact objects. They do not
change lifecycle, authorization, MessageSender identity or model-facing tools.
At the time this historical section was written, Schema16 contained only derived indexes and protocol version remained 2; Issue #244 A2 later advanced the wire handshake to protocol 3; #275 later advanced it to protocol 4.

## Terminal-free startup rows (#5 stage 2)

`serve` is a local process adapter, not a new IPC operation. Its readiness row
has protocol_version, event/state `ready`, absolute UTF-8 data_dir and instance_id;
it is not an operation Response and does not issue an operation receipt or use
an inherited Origin. It is flushed only after IPC binding and dispatcher startup.
Startup failure is one stderr JSON object with protocol_version, ok=false and
error; there is no successful stdout result. Failure to write readiness closes
the prepared session before reporting failure. Existing IPC commands, version,
validation and authorization remain unchanged in this startup-only stage.

## Explicit owner shutdown

owner.shutdown takes instance_id and requires a durable operation receipt. Only
serve owners accept it; local IPC/OS access remains the authority, not a human
label. It does not change conversation context. Result state=accepted with
shutdown_complete=false acknowledges committed acceptance at that instance,
not successful process termination. The session final stdout row event=stopped
reports state=stopped/failed and ok after cleanup. Startup cancellation instead
reports state=cancelled without a ready row. All rows carry protocol_version;
normal ready/stopped rows identify the session; cancellation before readiness
may carry null data_dir and instance_id.
No final row is guaranteed after force-kill or a closed output receiver. CLI
shutdown requires explicit data-dir, reads that endpoint rather than inherited
owner variables, and pins the request to its instance. Old saved acceptance
remains inspectable but does not authorize stopping a restarted owner.

Run get optionally includes `waiting_reason` for a protected in-flight context
edit on its exact live Run/active Turn. Fields identify kind, primary operation,
action, accepted_at and operation_count; no completion time or OS state is
inferred. Empty/terminal/ordinary background runs omit it. Same saved projection
feeds the TUI, without a new Run/Turn state or protocol-version change (#102).

## Grouped saved usage (#101)

`usage.get` optionally accepts `group_by` (`hour`, `day`, `thread`, `model`, `shell-exit`), `timezone` (`local`, `UTC`, fixed signed HH:MM), `limit` (1..168), and
`cursor`.
Timezone/limit/cursor require grouping; existing scope exclusivity still applies.
Grouped unscoped reads span non-deleted Threads, not an implicitly selected Thread.
Model groups use saved `model_attempt.model_id`; they retain the same bounded
rowid window and omit summary-compaction queries because records have no model
attribution. Summary fields are explicit null/unknown with
`coverage.summary=not_attributed_by_model`, never assigned to a guessed model.
For `shell-exit`, terminal records group by owning Thread and persisted exit code; null codes are unknown. Ownership follows `owner_run_id` to `run.thread_id`.
The default last24h/max31day saved-start period, 10,000+1 candidate scan bound,
group/scan cursor, page-local statistics, unknown coverage and max-attempt
references are defined in [usage aggregates](usage-aggregates.md#bounded-grouped-cli-readback-101).
The response exposes `items`, `more`, `next_cursor`, `requested_period`,
`scanned_candidates`, `matched_attempts`, `scan_truncated`, `scope_complete`,
`statistics_scope`, `excluded_time_count`, `outside_period_count`, coverage and
observed `elapsed_ms`. No migration/index is added. No receipt or new lifecycle
state is created. Existing ungrouped response shape is unchanged.

## Optional Deletion Target Marker (#122)

`thread.delete` accepts the optional `args.preview_marker` additively under the
current protocol 4 contract; older protocol-2/3 notes about the original change
are historical, not the current wire version. The marker contains
`thread_count` (1..257) and `ids_hash` (64 lowercase SHA-256 hex characters).
Old requests omitting it, or using null, retain unguarded current behavior;
serialization of None omits the field. Preview responses include the marker or
null plus `preview_marker_unavailable_reason: "input_limit"` for incomplete
bounded target selection. A marked execution recomputes the bounded selection
inside its write transaction and returns typed `preview_stale` before deletion
when identities differ or cannot be enumerated completely. Re-preview and obtain
new human confirmation; never automatically retry the mutation. See
[deletion-impact](deletion-impact.md) for canonical hash encoding and bounds.
The marker is not a permission token, Run-state snapshot or GC byte estimate.

## Single-attempt usage facts (#142 part 1)

Protocol remains v2. `usage.get` adds optional `attempt_id`; this selector is
exclusive with Thread/Run/Turn, children, time window, grouping, timezone,
limit and cursor. It returns one attempt's IDs/state, bounded standard `usage`
scalars and `prompt_cache_diagnostics` (provider object or JSON null). Missing
attempts return `target_unavailable`; aggregate requests/output stay unchanged.
See [cache diagnostics](cache-diagnostics.md) for bounds and ownership.

### Namespace Failure Copy (#152, Next Release)

Application-generated Unix namespace-creation failure text and its recovery hint
are English before any interactive locale is available. Keep `io_failed`, the
`local_ipc` component, the original reason, and all existing directory/lease/
permission/path guards unchanged. Source details are not translated. This copy
change is for the release after 0.6.0, not the 0.6.0 integration batch.

## Content-filter recovery facts (#159)

`turn get` and `run get` keep the existing `recovery_hint` JSON string. For an
incomplete conversation attempt its saved ApiError details additionally contain
`content_filter`: `reason`, `reason_recorded`, bounded `facts` (each has `path`,
`value`, `recorded`), `limited`, and stable `next_actions`. `recorded=false` is
unknown evidence, not a provider-returned false/null; `recorded=true,value=null`
is a returned JSON null. Provider path components are JSON-pointer escaped, not
renamed or translated. `limited=true` explicitly marks omitted/truncated facts;
original saved response/audit data is not changed. The response/protocol version,
Turn/Run state, saved IDs, retry admission and explicit resume semantics do not
change. Human UI projections translate only generated labels and guidance; copied
hint bytes and raw detail JSON remain canonical, not translated prose.

## Unix Namespace Diagnostics And Last-Resort Data Directory (#163)

After the existing XDG, OS user temporary and system temporary bases are
exhausted, the owner may use a fresh random `sb-<uid>-<random>` namespace
directly under its data directory. This avoids creating a persistent shared
`ipc` directory. The data-directory base must pass the same absolute-path,
owned-0700, canonical ancestor and socket UTF-8/103-byte checks as a private
runtime base; OwnerLock validation alone is not sufficient. Existing ordering,
locator-before-mkdir, eight attempts per distinct base, socket ownership,
identity-aware cleanup, Linux lease and live-socket preservation remain.
Restart cleanup recognizes only the exact current data-directory base under the
same validation, not an arbitrary locator path. Windows named pipes are unchanged.

If all bases fail, startup returns bounded aggregates of candidate type, fixed
reason code, optional numeric OS errno and count; paths, generated names, raw
OS error text and environment contents are omitted. Hint text prioritizes
permission/sandbox denial and path length, otherwise advises checking the
specific summarized conditions and using a short private XDG_RUNTIME_DIR.
All startup-generated text is English. No setting, transport or endpoint JSON
schema changes. Actual lower-level syscall failures remain distinct from a
candidate rejected before mkdir; aggregation must not grant deletion authority.

## Explicit Replies (#194 Step 2)

The adopted [reply design](reply-expected-messages.md) adds `expect_reply` to
`message.send`, `message.reply` (`message_id`, text, optional sender label),
`reply_received` to wait conditions, and `reply_cancelled` to cancel outcomes.
`message.get` removes `answer_node_id` in favor of optional `expect_reply`,
`reply`, and `reply_to`. `thread.get` exposes up to 128 `awaiting_reply` entries.
Reply eligibility, authority, body creation, delivery, and terminal-state CAS
commit atomically. Operation replay returns the saved result, not a second
reply. Any model or human may reply after application.

## Change hints `events.wait` (#243 stage B)

Adopted from [events-and-external-protocol](events-and-external-protocol.md). `events.wait`
is a read-only runtime-route long-poll: no receipt, no `app_meta` row, no DB access, no
wait graph or Turn `waiting` state. Args: `after` (optional opaque cursor), `thread_ids`
(0..64 non-empty IDs, 0 = all), `timeout_ms` (default 30000, 0 allowed). Result:
`status` (`baseline | changed | timeout | resync_required`), `cursor`, `changes`
(`{thread_id, seq}`, ascending by seq, at most 64) and `more`.

The cursor is base64url JSON `{v:1, instance_id, seq}`. A cursor from another owner instance
or with `seq` below the clock's eviction floor returns `resync_required` with a current
cursor; a malformed cursor, or one ahead of the clock for this instance, is
`invalid_argument`. Without `after` the call returns `baseline` immediately. When `more` is
true `cursor` is the last returned seq; otherwise it is the clock head read in the same lock.

The clock lives in `Control` (`src/control/change_clock.rs`): one sequence number per
`Control::changed` call, latest sequence kept per Thread, at most 4096 Threads (oldest
evicted, raising the floor). Memory only; it dies with `Control`, has no task or queue, and
the owner keeps nothing per client. The call is admitted in an ordinary request slot,
returns on a matching change, timeout, caller disconnect or owner shutdown, and is
cancellable by the shared runtime-observation wrapper. Models may call it.

The cursor moves past changes to Threads excluded by `thread_ids`; a client that adds a Thread to the
filter reads it once. A pending call at owner shutdown or caller disconnect ends with an error
(`read_only`, not applied); after an owner restart the next call returns `resync_required`.

## Reading and changing the configuration file: `config.get`, `config.set` (#266)

Both are `CommandRoute::Runtime`. `config.get` is a read with no receipt. `config.set` is a mutation:
`reserve_runtime` saves the intent first, the same ID and arguments return the saved outcome without
touching the file, the same ID with other arguments is `operation_conflict`. It is not cancellable: a
started write finishes and is reported. There is no authority check (#256). Neither is in the TUI catalogue
(`OperationId::for_command` returns `None`, like `events.wait`), since a form would not be generated.

```jsonc
// config.get: args {}
{
  "config_path": "/home/me/.config/shellboy/config.toml",
  "loaded": {                       // what this owner parsed at startup
    "present": true,
    "content_hash": "<sha256 of the bytes it parsed>",
    "values": {"default_model": "model-a",
               "connections": {"proxy": {"base_url": "https://proxy.example/v1",
                                         "api_key_env": "PROXY_KEY",
                                         "headers": {"X-Org": "[redacted]"},
                                         "extra_body": {"metadata": "[redacted]"} /* every field */ }},
               "shell": {"env": {"MY_VAR": "[redacted]"} /* ... */} /* every section, defaults filled */},
    "redacted_fields": ["connections.proxy.extra_body.metadata", "connections.proxy.headers.X-Org", "shell.env.MY_VAR"],
    "credential_env_set": {"PROXY_KEY": true}   // in the owner's environment; never the value
  },
  "file": {                         // the file now
    "present": true,
    "content_hash": "<sha256 of the bytes; of no bytes when absent>",   // pass as expected_hash
    "valid": true,
    "error": null,                  // the validation message when valid is false; values are then null
    "values": { /* same shape */ },
    "redacted_fields": [ /* ... */ ]
  },
  "differs": true,                  // file bytes differ from what was loaded, comments included
  "differing_fields": ["shell.env.MY_VAR"],  // effective pending values; null when the file is invalid
  "restart": {"all_fields_require_restart": false}
}

// config.set: args. field is a TOML dotted key; a missing or null value removes the key.
{"expected_hash": "<file.content_hash>",
 "changes": [{"field": "default_model", "value": "model-b"},
             {"field": "connections.\"my.proxy\".request_timeout_ms", "value": 90000},
             {"field": "shell.env.MY_VAR", "value": "x"},
             {"field": "ui.bell"}]}

// config.set: result
{"config_path": "...", "written": true,
 "previous_hash": "...", "content_hash": "...",    // the new hash, usable for the next change
 "restart_required": true, "applied_to_running_owner": true, // mixed edit: live default + restart-only shell
 "changed_by": null,                               // Origin identity of a model shell; null for a terminal
 "changes": [
   {"field": "default_model", "previous": "model-a", "value": "model-b",
    "sensitive": false, "restart_required": false, "applied_to_running_owner": true},
   {"field": "shell.env.MY_VAR", "sensitive": true, "restart_required": true,
    "applied_to_running_owner": false}
 ]}
```

`changes` lists effective values that differ, not the request: removing a key shows the default that now
applies, adding a connection lists its fields, and a sensitive entry has no `previous` or `value`.

Errors: `invalid_argument` for a malformed or refused change (`details.change_index`; `details.reason =
"credential_value_refused"` for a credential in a name field, `"redacted_placeholder_refused"` for a value
containing `[redacted`), `config_invalid` when the whole result fails
`Config::parse`, `operation_conflict` with `details.reason = "config_changed"` and `current_hash` for a stale
hash, `io_failed` or `input_limit` otherwise. An interrupted Windows replace that couldn't be finished is
`io_failed` with `details.reason = "replace_interrupted"` and `details.recovery_file`; when it could be finished,
the result gains a `warning`. All carry `details.written = false` when nothing was written,
and none quotes a value from the file or the request. Bounds: 64 changes, 512-byte field, 128 KiB value
(request validation), 1 MiB file before and after the change, 512 KiB result (checked before the replace, so a saved outcome never hides
a finished write).

The design is in [config.md](config.md#reading-and-changing-through-the-owner-266).

## Live text on `events.wait` (#243 E3)

`include_text` (default false) adds non-reasoning `live_text` answer deltas to the result: one entry per attempt in the filter
that holds text, sorted by Thread ID, `{thread_id, attempt_id, gap, deltas:[{offset, reasoning, text}]}`.
Absent unless asked. `status` gains `text`, returned when text is new and no change is; `baseline` and
`resync_required` carry everything retained. `offset` counts Unicode scalar values already streamed by
that attempt, including reasoning and refusal in the model stream; `live_text` omits reasoning deltas. The offset
restarts with each attempt, and a retry has a new `attempt_id`. `gap:true` means text after the caller's position was lost. A Thread missing from
`live_text` holds no text: its attempt ended, or 16 others hold text and it enters later with `gap:true`. Start with a baseline.
A looping client can get "work capacity is busy" at once when ordinary slots are full; nothing was applied, so poll again with the same cursor. The text is never saved, not a position in saved output and not part of any
receipt; retained text is limited to 64 KiB per attempt, counting 40 bytes per delta, and 16 attempts, and all of it
fits one frame. Those limits are not a memory bound; the measured heap is in the validation record. Client rule
and state record: [events-and-external-protocol](events-and-external-protocol.md) 2.5.

## Live reasoning activity on `events.wait` (#275)

With `include_reasoning_activity` (`shellboy events wait --reasoning-activity`), `reasoning_activity` reports the active model request attempts for the selected Threads, including attempts that have not emitted text yet. Each entry is `{thread_id, attempt_id, elapsed_ms, received_bytes?}`. Presence means the request attempt has started; elapsed time is monotonic time since the local streaming attempt began, not a claim about provider-side scheduling. `received_bytes` is omitted until a reasoning delta is observed, then reports cumulative UTF-8 bytes received in reasoning deltas (not tokens); providers that supply no reasoning text or token count remain unknown. Reasoning content is never included in this field. An attempt retry gets a new ID; a finished attempt disappears. Activity uses the existing 16-attempt in-memory bound and is removed on stream completion/cancellation; it adds no persistence, queue, or worker. Timeout responses include the latest elapsed observation, so clients can use their own monotonic clock between polls. The additive field is covered by the `events.reasoning_activity` protocol capability; clients must check `Welcome.capabilities` before relying on it.

## Stored keys and connection test: `credentials.set`, `credentials.clear`, `connection.test` (#269)

All three are `CommandRoute::Runtime`, outside the TUI catalogue (`OperationId::for_command` returns `None`), and
require a running owner; there is no standalone path. `credentials.set` and `credentials.clear` are mutations like
`config.set`: `reserve_runtime` saves the intent, the same ID and arguments return the saved outcome without touching the
file, other arguments are `operation_conflict`, and a started write is not cancelled. `connection.test` is a read: no
receipt, cancelled when the caller disconnects. No value is returned by any of them. Design:
[config](config.md#stored-api-keys-and-connection-test-269).

```jsonc
// credentials.set: args. value is write-only; 1..4096 bytes, printable ASCII only
// (no spaces, line breaks or invisible characters). connection must be in the configuration file (or loaded by this owner).
{"connection": "proxy", "value": "<key>"}
// result
{"connection": "proxy", "stored": true,
 "replaced": false,                  // an entry existed before
 "key_source": "stored",             // "env" when the variable also has a value: the variable wins
 "credentials_path": "/home/me/.config/shellboy/credentials.toml",
 "permissions_tightened": false,     // Unix: the file was group/world accessible and is now 0600; always false elsewhere
 "changed_by": null,                 // Origin identity of a model shell; null for a terminal
 "warning": "..."}                   // only for an interrupted Windows replace that was finished

// credentials.clear: args
{"connection": "proxy"}
// result. Idempotent: no entry, or no file, is cleared:false and writes nothing. The connection need not exist.
{"connection": "proxy", "cleared": true, "key_source": "none",   // "env" if the variable has a value
 "credentials_path": "...", "changed_by": null}

// connection.test: args. Exactly one of connection (saved) and base_url (candidate).
{"connection": "proxy"}
{"base_url": "https://api.example/v1",
 "api": "responses",                 // optional: responses (default), chat_completions, anthropic_messages
 "api_key": "<key>",                 // optional, write-only; or
 "api_key_env": "EXAMPLE_API_KEY",   // optional, a variable name in the owner's environment (not both)
 "api_key_header": "x-api-key",      // optional
 "ssl_verify": false}                // optional, default true
// result
{"request": "GET model catalog only",
 "connection": "proxy",              // null for a candidate
 "candidate": false,
 "api": "responses",
 "sent": true,                       // a request was attempted; false when none was (credential_missing, invalid_configuration, a URL that fails validation).
                                     // Not delivery: a refused connection is sent:true with http_status:null
 "ok": true,                         // a catalog came back; not a statement that the key is valid
 "http_status": 200,                 // null only when no HTTP response arrived; a 200 that is not a catalog keeps it (failure: response)
 "failure": null,                    // auth | url | rate_limited | server | http_status | timeout | unreachable |
                                     // credential_missing | invalid_configuration | response
 "models": ["model-a", "model-b"],   // sorted; the whole catalogue (the client bounds it at 2 MiB, 10,000 ids)
 "model_count": 2,
 "credential": {"source": "stored",  // env | stored | inline | none
                "api_key_env": "EXAMPLE_API_KEY",   // null if unset; a credential-looking name is refused, so it is never echoed
                "env_set": true,                    // presence in the owner's environment; null without api_key_env
                "credentials_file_error": null}}    // why credentials.toml could not be read, without content

// config.get additions
{"loaded": {"credential_source": {"proxy": "stored"} /* env | stored | none, per connection */, ...},
 "file":   {"credential_source": {"proxy": "stored"} /* null when the file is invalid */, ...},
 "credentials_file": {"path": "...", "present": true, "valid": true, "error": null}}
```

Replay limit of `credentials.set`: its saved request fingerprint uses a digest of the key salted with a value that
lives only as long as the owner process. Within one owner run the same ID and key replay the saved result and another key
is `operation_conflict`. After an owner restart the same ID can no longer be matched (the stored digest cannot be
recomputed), so a retry gets `operation_conflict` even with the same key; the error's hint says to check
`config.get` `credential_source` and repeat with a new operation ID. This is a deliberate cost of never storing a hash
an offline guess could confirm.

Errors: `invalid_argument` for a bad value or connection, an unknown connection (`details.field = "connection"`), a
connection that defines its own `Authorization` header, both or neither of `connection` and `base_url`, both `api_key`
and `api_key_env`, or a credential-looking `api_key_env`/`api_key_header` (`details.reason =
"credential_value_refused"`); `input_limit` for a key over 4096 bytes or a result over the bound; `config_invalid` when
`credentials.toml` is invalid (unchanged, `details.credentials_path`); `io_failed` otherwise. `details.written = false`
when nothing was written. A refused value is never echoed. Rejected unknown argument names as for every command. No
protocol version change: the commands are additive, as `config.get` was.

### Live connection application (#269 stage C)

For an owner-routed `config.set`, each `changes` entry now contains `applied_to_running_owner`. Ordinary
`connections.*`, `default_connection` and `default_model` changes report it as true and `restart_required` as false.
Changes to the running embedding connection and other settings report false/true. The top-level flags are ORs of
entry flags (a mixed edit can set both; no effective changes sets neither). Removing the running default or embedding
connection is rejected with `config_invalid` and `details.written: false` before any write. An already-started request
retains its original connection; next-request lookup of a removed Thread connection uses existing recovery.

`config.get.restart.all_fields_require_restart` changes to false. `loaded.values` and `differing_fields` account for
live connection/default adoption. `loaded.content_hash` and `differs` remain startup-byte facts, not a restart gate.
Consumers such as Studio must use the per-change flags/differing fields rather than infer restart from `differs`.

Stage C review clarification: `applied_to_running_owner` means adopted by the owner registry. New Runs use that
registry, while an already-active Run pins its start-time generation across tool continuations, admission refresh,
automatic summaries and retries. It does not mean an active Run switches provider mid-conversation. Credential
lookup remains next-request. The response shape and restart flag classification are unchanged by this clarification.

## #267: removal decision and deferred reclamation

`index.remove` without `dry_run` commits a durable removal decision and returns `removed: true`,
`reclaim_pending: true`, and `state: "removing"`. This does not assert that bytes have already been freed.
`vector_payload_bytes_freed` and `unfinished_jobs_deleted` are zero at this boundary; their previewed quantities are
returned as `vector_payload_bytes_pending` and `unfinished_jobs_pending`. Dry-run retains its projected counts and
never changes state. Removal remains irreversible and does not delete conversation history.

The owner rechecks active/running/conversion/rebuild protection in the short transaction that marks removal. Once
committed, the space cannot be selected, retried or reused by normal index operations. It is no longer a protected
candidate: later metadata changes cannot make reclamation unsafe or revive it. Repeated removal while pending is
idempotent. Once fully reclaimed, the ordinary unknown-space response applies.

Existing maintenance and `storage.collect` reclaim at most the declared row budget per unit: chunk/vector/preview,
then job/input, group pointers and orphan vector rows. Each successful unit commits independently; a failed unit
rolls back only itself. Empty vec-table DROP and fixed per-space metadata removal finish reclamation. Remaining rows
and the durable state are the restart cursor; no new worker, queue or epoch table is required. Database file size
still requires explicit `storage vacuum` to shrink; logical usage decreases as chunks are reclaimed.

#267 review clarification: exact counts are requested only by `dry_run`. Actual removal returns
`counts_exact: false`, `segments: null`, `vector_payload_bytes_pending: null` and
`unfinished_jobs_pending: null`. Actual freed/deleted counts remain zero. A null quantity is unknown, not zero.
Dry-run returns `counts_exact: true` and exact numeric projections, and can take time proportional to index size.
The actual decision never calls these COUNT queries, even before its short write transaction.

## #270 common instruction configuration and inspection

The root configuration field `extra_instructions` is an ordered array of absolute UTF-8 file paths, default `[]`.
`config.get` exposes it and hash-checked `config.set` accepts its replacement. Path/list validation occurs before
writing; file readability and captured-content bounds are checked during Turn preparation. Runtime edits report
`applied_to_running_owner: true`, `restart_required: false` for this field; independent mixed-field flags retain
existing semantics. Disk-only edits require restart. Configuration success is not evidence that listed files exist
or user consent was obtained. A registered file failing observation returns a visible preparation/recovery error,
not a truncated or partially populated request.

`thread.get` optionally returns `instruction_sources: {extra: [{path,hash,bytes}],snapshot:"latest_prepared_turn"}`
after preparing common instructions. It omits bodies and is historical, not live file/config state. `thread request`
also returns `instruction_sources` with extra metadata, `agents_path` and `local_present`, alongside the usual
per-request item hashes/character counts. Request body output intentionally includes saved text. Saved source order
matches the instruction-prefix order. No schema migration or new daemon task is required.

#270 review correction: raw user AGENTS JSON is always literal text. Bundle representation uses a writer-owned
persisted digest-kind namespace; no JSON field in a user file activates source decoding. `instruction_sources` omits
raw snapshots even when they contain an `extra` JSON field. Repair/remove broken common-file registrations through
the model-independent config CLI, then explicitly retry; fail-closed semantics are unchanged.

## #274 spill-to-tool provenance

`history.list` and `history.window` retain all existing fields and add `spill_tools` only for spill nodes:

```json
{"spill_tools":{"status":"known","items":[{"call_id":"call-id","execution_id":"execution-id","original_node_id":"source-node-id"}],"truncated":false}}
```

This is immutable saved provenance, not the current execution state. Each spill records at most eight tool-output
sources in replay order; more sources set `truncated:true`. Identifiers are limited to 256 UTF-8 bytes each. `call_id`
is required per item; `execution_id` and `original_node_id` are omitted when absent, never inferred from a global
call-ID match. Non-shell tool results may have only a call ID. Existing `executions`, `execution_count` and their
truncation flag keep their original direct-node meaning and are not silently broadened to replacement sources.

Legacy spills without this captured metadata return `{"status":"unknown","items":[],"truncated":false}`;
unknown is not proof of no tool execution. No history backfill, source-payload traversal or reverse lookup is run.
Use `history relations --relation sources`/`history get` for explicit forensic exploration. Source IDs can outlive
source payload retention; tombstones do not invalidate captured IDs. `excerpt_chars:0` still includes provenance.

Creation reuses the already assembled spill projection and writes a separate bounded `node.spill_tools_json` column
in the same transaction. Migration 36 adds the nullable column and `node_spill_tools(id,spill_tools_json)` covering
index without extracting or backfilling old payload JSON. Legacy columns remain NULL/unknown. Reading forces that
covering index: it accesses neither the node table record nor the payload/JSON functions. At most 16384 metadata
bytes are parsed, independent of spill payload size. No new table, worker or queue is added. A plain indexed node
lookup was insufficient: parsing payload JSON before applying substring limits still scaled with its size.
### #273 branch-relative usage and provenance

`UsageGetArgs.since_node_id` (`--since-node NODE`) is an alternative to `since`; the saved node timestamp is the inclusive period lower bound. JSON compatibility is additive: clients that omit the field retain previous behavior. Before aggregation, the DB worker resolves exactly one non-collected node timestamp through the selected Thread's `(thread_id,node_id)` path uniqueness and rejects a node from another Thread/path. The timestamp becomes the existing inclusive `since` boundary; `until` remains exclusive. The same resolved time is used by grouped and ungrouped usage. When grouping has no explicit Thread, node resolution anchors the grouped scope to that Thread rather than all Threads. No candidate scan, aggregate statement, query count, or page bound is added; only a two-index point lookup is added.

`thread.branch_kind` records `fork` or `rewind` when a branch is created. Pre-migration rows and non-branch roots retain NULL and serialize as `unknown`; no kind is inferred from path shape. Migration 37 adds only this nullable checked field.

### Saved assistant response references on status attempts (#279)

Durable `status` attempt WorkItems expose `response_nodes`, an output-item-ordered list of saved `assistant_response` references from the existing `output_item` rows for that exact `attempt_id`. Each entry preserves `ordinal`, nullable provider `item_id`, provider `kind`, nullable `phase`, and the saved `node_id`; repeated items that point at the same node remain repeated entries so item identity/order is not lost. Studio may pass `node_id` to `history.window`; it must not use `end_message_node_id` for intermediate attempts. That field remains exclusively the Turn's final answer pointer. For each attempt, the list contains only eligible assistant-response items found within the inspected candidate prefix. It can be empty because no row was saved yet/at all, because saved rows have no assistant-response node, or because all inspected candidates were ineligible. An empty list with `response_nodes_truncated=false` means the full candidate range was inspected and contained no eligible response; an empty list with `response_nodes_truncated=true` means later candidates were not inspected and eligible refs may exist. Failed/retried attempts retain their own refs; retries use different attempt IDs. Live-only observations use an empty list and `response_nodes_truncated=false` because they have no durable output-item ownership.

The query first reads at most 257 rows from the existing `(attempt_id, ordinal)` primary-key range. It examines at most 256 candidate rows, publishes at most 64 eligible output-item refs, and sets `response_nodes_truncated=true` if either limit leaves a row unexamined. Thus truncated means the result is incomplete, not that omitted eligible responses are known to exist. There is no continuation cursor for this nested list; clients must not claim complete coverage, and an empty+truncated list must not be treated as proof no response was saved. No new index/table/migration is added.

This is additive to protocol 4. Before this change `status` result was an opaque JSON value; existing readers were already required to tolerate its fields and additive output. The new fields are not added to `events.wait`, do not change request args, and do not change the meaning of `end_message_node_id`; no version bump is required under the current additive-result rule. Revisit if a supported status client proves strict against unknown response fields.

Visibility lifetime: intermediate `assistant_response` rows are appended to the selected `thread_path` in the same transaction as their `output_item` links, so every published `node_id` is immediately a valid `history.window` anchor. A rewind/fork may later remove a node from that selected path; as with other positional history anchors, `history.window` then returns its normal stale-anchor error. No lookup is performed by body text or timestamp. Response adoption calls `Control::changed(thread_id)` after the commit; it records a durable thread change on the `events.wait` clock before notifying waiters, so the persisted-ref change is observable without polling or guessing.
