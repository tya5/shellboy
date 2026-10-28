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

## 操作追跡・context・削除影響の採用済み拡張

現行操作数は37。`thread.delete_preview` は既存ThreadDeleteArgsを用いる読み取り専用操作で、CLIは `thread delete-preview --thread ID [--descendants]`。同snapshotの阻害Run・直接参照・対象件数を返し、実削除の再判定は維持する。`reclaimable_bytes` はnullと理由を返し、GC到達性や物理容量を推定しない。[全体設計](observability-and-deletion.md)、[削除影響](deletion-impact.md)を参照。

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

Historical older-owner guidance is superseded by Issue #33. Current request/response envelopes use `protocol_version=2`. Other versions are rejected, and current required fields are not made optional for older owners. Use the same release for CLI and owner. A version mismatch reports `details.client_protocol_version` and `details.owner_protocol_version` and requires aligning releases; retrying the same binaries cannot fix it. Endpoint metadata and Welcome version checks reject before command dispatch. Endpoint/instance changes remain a distinct reconnect diagnostic. A lost response after sending a mutation remains an uncertain result and retains receipt lookup guidance. Unknown operations remain errors; a connection loss is still not proof that a mutation did not execute. No capability negotiation or old status wrapper is added.

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

`index.preview` / `shellboy index preview` は既存IndexArgsとFeePreviewを公開する追加のread-only操作。scope/Thread/sourceの明示対象を必須とし、spaceだけでは全体選択しない。既存runtime routeへ通し、ファイル読取りを含むためshutdown取消可、changes_context=falseとする。既存一時pinは維持し、receiptを作らず、embedding API/job投入/candidate作成/過去分許可は行わない。previewは同IDでも最新候補を再観測する。確認tokenや長期snapshotを発行せず、backfill/rebuildの公開構文と非対話受付を維持する。

## 観測の再照会と結果失効（Issue #3）

get/list/search/preview/statusはreceiptを作らず、同IDでも再観測する。変更操作とwaitは保存済み要求照合を維持する。現instanceの終端結果は24時間経過後の保守又は圧迫時に結果本文を縮退できるが、pending/未反映参照を保護しID/hash等は保持する。同ID再送はoperation_expiredで、変更を再実行しない。別内容はoperation_conflict。operation.get/statusのresult_expiredとreplay_policyで区別し、読取り自身の照会IDは受付記録の対象ではない。既存error code・wire版・frame上限は維持する。詳細は [operation設計](operation-observability.md) を正本とする。


`context.edit`は`thread_id`（省略可能）、`action`（spill/summary/omit/trim/restore）、`from_node_id` / `to_node_id`（モード別）を持つ。受付結果は既存compact同様のqueuedと`context_edit`引数を含み、receiptを上書きせず後続結果をdownstreamで返す。Thread詳細には`context_usage`を追加する。停止中など推定の非storageエラーは`available:false`/errorとして表現し、Thread照会自体を失敗させない。詳細は[圧縮設計](compaction.md)。
