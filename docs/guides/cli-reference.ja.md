# CLIリファレンス

[English](cli-reference.md)

[ガイド入口](README.ja.md) · [Agent Guide](agent-guide.md) · [設定](configuration.ja.md)

会話・実行制御のサブコマンドは、既に稼働するownerへローカルRPCを送ります。ownerの自動起動はしません。引数なしの `shellboy` は対話端末でTUI ownerを起動します。以下のowner操作表は現行 `src/cli.rs` / `src/protocol.rs` の37操作と `attach` aliasです。`docs` と `rag` はowner不要の独立コマンドで、[ガイドの取得](docs.ja.md) と [文書・履歴の検索](rag.ja.md) に構文・返却値をまとめています。モデルに公開するtoolはshell一つであり、これらはshell内で実行するCLIです。

## 独立コマンド

```bash
shellboy docs list --parent shellboy
shellboy docs search --query 'subagent'
shellboy docs get --id shellboy/delegation
shellboy rag --help
shellboy rag search --scope docs --scope history --query '復旧' --mode keyword
```

`docs` は実行ファイルに同梱した同版の英語ガイドをAPI不要で読みます。カタログ・検索対象は英語で、検索語の自動翻訳はしません。日本語版ガイドはリポジトリまたはリリースアーカイブ内の `docs/guides/*.ja.md` を直接読んでください。
`rag` は登録資料・ガイド・明示した履歴scopeを検索します。生成モデルの起動は不要です。
独立コマンドのJSONにowner operation_idはなく、`--operation-id`併用は拒否します。
RAGの一部失敗は `ok:false` と部分resultを併記し、scope別のerrorを確認します。

## 共通構文と接続

`shellboy status`は保存済みの作業一覧です。モデルからは既定で呼出元Thread、通常の外部端末からは全Threadが対象です。明示`--run RUN`はそのRunへの所属で絞り、所有子孫を暗黙に含めません。`--thread THREAD`との併用は両条件です。`--all`は終端も含める指定で、Thread範囲を広げません。`--state failed`等はnative状態の完全一致で終端も取得できます。

```sh
shellboy status --limit 50
shellboy status --run RUN --all
shellboy status --thread THREAD --state failed --since 2026-10-25T00:00:00Z
```

`items[].targets`は対象のget/readへ進む型付き操作です。`filters`は正規化した条件、`next_cursor`は同じ条件で次pageを取得するcursorです。状態更新や対象削除だけではcursorは失効しません。初回watermarkより後に生成された項目はcursorを省略した新しい一覧で取得します。更新による並び替えでは重複・見逃しがあり得ます。owner instance・対象範囲・条件・cursor版が異なる場合はcursor_expiredとなり先頭から再取得します。ページ途中のlimit変更は1〜200で可能です。fingerprintはその照会の投影識別値であり、全ページ固定の保証ではありません。`observed_at`は照会時刻で、実行中OSプロセスや独立RAG storeの同時観測を保証しません。`activity_at`は保存された生成・開始・終了・依頼更新時刻の最大値で、記録のない状態変更時刻ではありません。Threadの`current_model`とモデルattemptの`attempt_model`はそれぞれ現在設定と実際の要求時設定です。

通常は非終端だけなので、作業が0件でもThreadや履歴が消えた意味ではありません。索引の詳細は`shellboy index status`、独立文書storeは`shellboy rag status`、操作方法は`shellboy docs get --id shellboy/agent-guide`で確認します。

```text
shellboy [--data-dir PATH] [--config PATH] [--operation-id ID] <group> <operation> [flags]
shellboy --help
shellboy <group> <operation> --help
shellboy --version
```

global flagはサブコマンド側にも置けます。`--data-dir` はownerの保存先（作業cwdではない）、`--config` はTUI起動設定と、独立RAGのembedding接続設定に使います。CLI接続先は `--data-dir` > `SHELLBOY_DATA_DIR` > OS既定で、TOMLを読みません。[OS別パス](configuration.ja.md#配置と優先順位) を参照してください。

モデルのshellへ注入された `SHELLBOY_*` 文脈はCLIが自動利用します。origin tokenを表示・コピー・手動生成する必要はありません。別の保存先を選んだ場合はその文脈を引き継ぎません。任意指定の `--thread` を省略できる操作でも、通常の外部端末には「現在Thread」がないため明示してください。省略時の解決は認証された呼出元Threadであり、画面の選択Threadを外部CLIが勝手に使う意味ではありません。

例の `THREAD` / `NODE` / `RUN` / `TURN` / `EXECUTION` / `MESSAGE` / `ATTACHMENT` は返却された実IDへ差し替えます。Runは実行寿命、Turnは一回の指示/応答単位、executionはshell、nodeは不変履歴、messageは受付/割当の追跡IDです。

## JSON、終了コード、再送

通常のCLIは成功・失敗ともstdoutへ一行JSONを返します。help/versionは通常のテキストです。成功終了は0、構文/RPC/接続失敗は1。`wait` がtimeoutやrecovery_waitを正常に観測した場合もRPC成功なので0になり、対象作業の成否は `result` で判断します。

返信を失った場合、`details.read_only=true`は対象を変更する操作ではないことを示します。get/list/search/preview/statusはreceiptを作らず、同じoperation IDでも最新値を再計算します。通信断後は同じcommand/filterで再照会できますが、semantic検索のquery embedding費用は再発生し得ます。`wait`は呼出元の待機状態を保存する場合があるためread-onlyとして扱いません。

```json
{"protocol_version":1,"operation_id":"INSTANCE:NONCE","ok":true,"result":{"accepted":true}}
```

```json
{"protocol_version":1,"operation_id":"INSTANCE:NONCE","ok":false,"error":{"code":"invalid_state","message":"...","hint":"...","details":{}}}
```

`hint` / `details` は任意で、成功にはerror、失敗にはresultを付けません。通信開始前のエラーはoperation_idが空の場合があります。protocol frame全体は8MiB上限です。

CLIは省略時にinstance固有operation IDを生成します。変更操作とwaitは、`--operation-id` で同一ID・同一内容・同一呼出元を再送すると保全中の結果を取得でき、別内容なら `operation_conflict` です。読取りは同じIDでも再観測し、その照会自身のreceiptを作りません。message受付の保存とreceiptは同一transactionです。副作用操作のintentが受付済みで結果未確定ならpendingを返すことがあり、**同じ操作を別IDで再発行してよい証明にはなりません**。`operation get --operation ID` の `state` / `outcome` と対象状態を確認します。古いinstanceのIDは保存済みreceiptの照会用で、新規操作として実行しません。未保存の旧IDと結果失効済みIDの変更再送は `operation_expired`、存在しない対象receiptの照会は `operation_not_found` です。現instanceの終端結果は24時間経過後の保守/圧迫時に回収できますが、IDの期限切れ記録を残し再実行しません。pendingや未反映参照は保護します。`result_expired` と `replay_policy` を確認し、結果が消えた変更を新IDで無条件にやり直さないでください。

通信断で受付の有無が不明になった際に復元できるよう、重要な変更は既に得た同一instanceのID形式 `INSTANCE:任意の一意nonce` を明示して保存してください。IDのinstance部分はそのownerから返されたoperation_idに対応します。Run/Turn/message/nodeのIDをoperation_idへ流用しません。

`message send` の `delivery_status=accepted|held|applied|cancelled` は受付/適用状態です。`assignment=new_run|new_turn|existing_turn` ではrun_idとturn_idが揃い、一度保存された割当は変わりません。`assignment=pending` では両方なく、後で `message get` から初回割当を確認します。送信成功だけで回答完了にはなりません。`answer_node_id` は得られた後に原文取得へ使います。

### 保存済み実行コンテキスト

`thread get` / `run get` / `turn get` / `message get` / `history get` は任意の `context` を返します。state、保存済みcreated_at/updated_at/ended_at、thread_id/run_id/turn_id/node_id、Threadのparent_thread_idとRunのowner_run_id、構造化recovery_reason等を、意味のある対象だけに含めます。記録されていないstarted_atやThreadのcreated_atは省略します。Run/Turnの `lifetime_ms` はcreated_atからended_atまでの壁時計時間で、準備・待機を含み、未終了時は省略します。nodeのThreadは生成元であり、fork先の採用経路ではありません。

`operation get` の `state/outcome` は受付操作の保存結果です。`replay_policy=return_saved_outcome` は元結果の返却、`do_not_reexecute` はpendingで副作用を再開始しない意味です。`result_uncertain=true` は未実行の証明ではありません。`targets` は保存結果にある対象IDだけを返し、`downstream` はcompactの後続処理かmessageの現在配送状態を示します。その他はtargetsから各getへ進み、索引はspace単位の `index status` で確認します。照会自身はreceiptを作らず、同じ照会IDでも現在値を読み直します。`result_expired=true` / `replay_policy=operation_expired` は元の結果本文が回収済みで、同IDの変更を再実行しないことを示します。

## 操作辞書（37操作）

角括弧は任意、`|` は選択です。ここに挙げるflagに共通global flagと `--help` を加えられます。

### 作業状態（1操作）

| 操作・全固有flag | 主なresultと条件 |
|---|---|
| `status [--thread THREAD] [--run RUN] [--since RFC3339] [--state STATE]... [--all] [--limit N] [--cursor C]` | `items`、`filters`、`observed_at`、`scope`、`consistency`、`fingerprint`、`next_cursor`。既定50・最大200。単一DB読取transactionの保存状態。新しい実行・API要求・操作receiptを作らない |

### モデル（1操作）

| 操作・全固有flag | 主なresultと条件 |
|---|---|
| `model list [--connection NAME] [--thread THREAD]` | `connection` と `models`（モデルID配列）。接続先のlive一覧を取得し、設定は変更しない |

接続は明示 `--connection` > 明示Thread > 認証済み呼出元Thread > owner既定の順です。明示Threadは接続併記でも存在検査します。外部端末で引数なしならowner既定を使います。同じoperation IDでも一覧を再取得し、照会自身のreceiptは保存しません。embedding等も一覧に含まれ、生成能力を保証しません。接続先の一覧API非対応・認証エラー等は明示エラーを返します。

### Thread（7操作）

| 操作・全固有flag | 主なresultと条件 |
|---|---|
| `thread list [--limit N] [--cursor C] [--query TEXT]` | `items`（thread_id/title/cwd/parent_id/state/updated_at/excerpt）と任意 `next_cursor`。既定50、1〜200件。queryはID/title/cwdのフィルタ |
| `thread get [--thread THREAD]` | thread_id/title/cwd/connection/model/effort/parent_id、現在run_id/turn_id、children_count/shells_count、recovery_reason |
| `thread create [--cwd PATH] [--connection NAME] [--model ID] [--effort VALUE] [--parent THREAD]` | 新Thread詳細。まだ指示もRunも開始しない。parent指定は子の関係を作り、実行所有は親Runからの送信時に決まる。通常は親設定を継承 |
| `thread fork --thread THREAD --node NODE [--cwd PATH]` | 選択経路のNODEまで（NODE含む）を共有する新Thread詳細。元を変更せず、未完tool群の途中等の不正境界は拒否 |
| `thread delete-preview --thread THREAD [--descendants]` | 読み取り専用の削除影響照会。deletable_now、阻害Run件数と詳細（最大50件）、対象/子孫数、直接共有・置換出典・添付参照数、更新日時。reclaimable_bytesはnullと理由。削除時は再判定 |
| `thread delete --thread THREAD [--descendants]` | 削除受付結果。live/stopping Runがない対象のみ。子孫も削除する場合は明示。履歴の到達可能性を確認して後続GC、共有元まで一括消去しない |
| `thread set [--thread THREAD] [--title TEXT] [--cwd PATH] [--connection NAME] [--model ID] [--effort VALUE]` | 更新したThread詳細。少なくとも一つ変更が必要。進行中HTTPの設定は変えず次境界へ適用。`--effort ''` は解除 |

`thread list` / `thread get` の `title` は、保存タイトルが空なら最初のユーザー本文を基に補完します。選択経路を優先し、本文がなければ同じThreadの受付済み・保留中メッセージを確認します。表示名での `thread list --query` も可能です。DBの保存titleや過去本文は変更せず、モデルAPIによる命名はしません。

`thread set --thread THREAD --title '調査と修正'` で表示タイトルを設定します。`--title ''`（空白だけも同様）で解除すると、会話から求めた自動表示名へ戻ります。省略時は保存タイトルを変更しません。タイトルは前後空白を除き512文字以内、改行・制御文字不可です。タイトルだけの更新はモデル・effort・reasoning epochを変えません。外部CLIは `--thread` が必要で、エージェントからは省略すると自身を対象にします。

ルートcwdの既定はowner起動cwd、子は親cwd。相対cwdはその作成/変更対象の基準に対して解決されます。既存ThreadのAGENTS参照はcwd変更と別です。詳細は [設定](configuration.ja.md#起動設定とthread設定)。

### Message / Operation（3操作）

| 操作・全固有flag | 主なresultと条件 |
|---|---|
| `message send [--thread THREAD] (--text TEXT | --text-stdin)` | message_id/thread_id/delivery_status/assignment/run_id/turn_id。受付と完了は別。稼働中は安全境界へ追加、stopping中はheld/pendingになる場合がある。通常の機械送信はrecovery_waitを勝手に再開しない |
| `message get --message MESSAGE` | 受付時と同じ割当と現在delivery_status、得られたanswer_node_id |
| `operation get --operation ID` | operation_id/state/outcome、instance_id/created_at/current_instance、result_uncertain/result_expired/replay_policy/targets、対象によるdownstream。共通 `--operation-id` はこの照会自体のID、`--operation` は調べる対象ID |

`--text` と `--text-stdin` は必ず片方。stdinはUTF-8を上限8MiBで読み、超過時に切捨てません。message/下書きの実際の許容サイズ等には設定上限も適用されます。

Bash例（ここでは接続先ownerが起動済み）：

```bash
printf '%s\n' 'この変更を調べて報告してください。' | shellboy message send --thread THREAD --text-stdin
shellboy message get --message MESSAGE
```

PowerShellでも `--text '...'` またはpipelineで `--text-stdin` を使えます。native stdinの文字encodingや空文字引数の扱いはshellの設定に依存します。複雑なJSON例はUTF-8ファイルを経由するとquoteを保ちやすくなります。

### Run / Turn（4操作）

| 操作・全固有flag | 主なresultと条件 |
|---|---|
| `run get --run RUN` | run_id/thread_id/state/owner_run_id/turn_id/answer_node_id、owned_run_ids/owned_execution_ids、recovery_hint。所有IDは現在のlive資源参照で、過去全子の台帳ではない |
| `run retry --run RUN` | 復旧するRun/Turn参照。live Run内のrecovery_wait Turnを明示再準備。同じTurn/元messageを再利用し、実行済みshellを無条件再実行しない。人間または直属親が判断 |
| `run stop --run RUN` | stoppingの確定/Run状態。自Runと所有子孫を停止、独立Runへ波及させない。モデルは自分の所有範囲のみ、人間は明示対象を操作可能。資源回収完了はrun get/waitで確認 |
| `turn get --turn TURN` | turn_id/run_id/thread_id/state/answer_node_id/recovery_hint。回答後も背景所有処理があればRunはliveのまま |

### Wait（1操作）

```text
wait --conditions-json JSON [--timeout-ms MS]
```

条件はtype付き配列、**any**（一つ以上成立）です。既定timeout30000ms、0は待たず現在を観測します。空配列は不可。成立しても他条件の対象を停止しません。timeout、観測切断、取消も対象作業を止めません。

Bash例：

```bash
shellboy wait --conditions-json '[{"type":"turn_completed","turn_id":"TURN"},{"type":"run_ended","run_id":"RUN"}]' --timeout-ms 30000
shellboy wait --conditions-json '[{"type":"shell_ended","execution_id":"EXECUTION"}]'
shellboy wait --conditions-json '[{"type":"shell_output","execution_id":"EXECUTION","cursor":"CURSOR_FROM_SHELL_READ"}]'
```

PowerShellではnative引数へのJSONの渡し方がBashと異なります。UTF-8ファイルから文字列を読むだけでquoteの問題が解消するとは限りません。[shellガイドのPowerShell節](shell.ja.md#powershellの引数)を参照し、使用している版の引数渡しに合わせてください。

`turn_completed` は回答単位、`run_ended` は所有資源を含む寿命の終了、`shell_ended` はshell終了、`shell_output` はshell readのcursor以降の出力観測です。自分・祖先等への循環待機は `wait_cycle`。`result` は `status`、成立条件 `matched`、対象状態の `snapshot` と任意cursor/reason/hintです。statusは `satisfied|timeout|interrupted|recovery_wait|unavailable`。satisfiedでも対象がstopped/failedや非0exitなら正常回答/正常実行を意味しません。snapshotを確認し、必要ならrun/turn/message/getとhistory getへ進みます。

### History（4操作）

| 操作・全固有flag | 主なresultと条件 |
|---|---|
| `history list --thread THREAD [--excerpt-chars N] [--run RUN] [--since RFC3339] [--status STATE] [--limit N] [--cursor C]` | `items`（node_id/kind/excerpt/payload_chars/provenance/context/selection/executions）とnext_cursor。statusは関連shellの保存状態。選択経路を既定50・最大200件ずつ列挙。excerpt-charsは既定512・0〜4096、0では本文を取得せずexcerptは空文字。payload_charsは保存JSONの文字数で回収済みはnull。実行リンクは最大8件・総数/省略を明示 |
| `history get --node NODE [--limit-bytes N] [--cursor C] [--encoding UTF-8]` | node_id/kind/text/provenance、source_node_ids/previous_node_ids/next_node_ids、next_cursor、tombstone_reason。本文は表示用テキスト、encrypted reasoningは含めない |
| `history search --query TEXT [--limit N] [--thread THREAD] [--kind KIND]... [--exclude-kind KIND]... [--from RFC3339] [--to RFC3339]` | 配列（node_id/excerpt/distance/spans/provenanceと任意thread_scope）。既定10・最大50。filter適用後にactive embedding空間で順位を決める。active空間がなければ空配列 |
| `history relations --node NODE --relation previous\|next\|sources\|replacements [--limit N] [--cursor C]` | `items`（node_id/provenanceと置換時のreplacement_id/source_revision/source_hash）、next_cursor。直接の関係を既定50・最大200件ずつ取得 |

検索の `--thread` 未指定は全体検索で、モデル自身のThreadを暗黙に補いません。指定時はそのThreadの現在の採用経路と、そこからcompletedな要約・spillの置換出典を再帰的にたどったnodeが対象です。fork元の全枝を含む意味ではありません。全体検索のhitが現在の採用経路にあることは保証しません。`--kind` 内はOR、Thread・期間・除外とはANDです。kindはnodeの種類（例: `user` / `assistant` / `tool_result`）で、tool結果を除くなら `--exclude-kind tool_result`。含有・除外は各最大32件、各kind最大64文字です。`--from` は含む、`--to` は含まないnode作成日時の期間で、offset付きRFC3339を指定します。両端指定ではfrom < toが必要です。

`provenance` はkind/created_at/source_revisionと、保存上の対応がある場合だけturn_id/run_id/origin_thread_idを持ちます。originは生成元であり、forkで共有する全Threadの所属一覧ではありません。`spans` はsource_revision/content_hash/body_kind/start/endを持ち、start/endは抽出本文のUnicode scalar位置の半開区間です。意味検索が選んだ索引chunk範囲で、キーワード一致箇所ではありません。distanceはL2距離、threshold指定は未導入です。chunkはnode単位にまとめ、同じ内容の異なるnodeは統合しません。`--thread` 指定時だけthread_scope（thread_id/on_current_path）を返します。on_current_pathは現在の採用path上ならtrue、completed置換の出典としてだけ対象ならfalseです。全体検索ではこのfieldを省略し、所属を推定しません。

relationsのprevious/nextは会話辺、sourcesは置換出力から出典、replacementsは出典から置換出力です。後二者はcompletedな置換のみを返します。relation item直下のsource_revision/source_hashは置換記録時の出典で、provenance.source_revisionは返却nodeの現在版です。祖先・子孫の一括探索や履歴編集は行いません。同じnode・relationへnext_cursorを渡し、別対象へ流用しません。採用経路は `history list --thread` で確認します。

getの既定/最大は65536バイト、1以上。UTF-8文字の途中で分割せずページ化します。前後の枝や要約のsource参照を順にgetでき、listだけで全DAGを列挙する意味ではありません。

### Shell（5操作）

shell開始はモデルshell toolで行い、CLIに `shell start` はありません。

| 操作・全固有flag | 主なresultと条件 |
|---|---|
| `shell get --execution EXECUTION` | execution_id/run_id/state/output_complete/exit_code/reason、保存されたcommand/cwd/thread_id/turn_id。新fieldはoptionalで旧記録/旧ownerでは欠落し得る |
| `shell read --execution EXECUTION [--cursor C] [--encoding NAME] [--limit-bytes N]` | execution_id/text/encoding/cursor/raw_bytes/output_complete/decode_errors。既定/最大65536バイト、1以上。encoding省略は実行に保存したencoding |
| `shell write --execution EXECUTION (--text TEXT | --text-stdin) [--encoding NAME]` | bytes_written/status/stdin_open/reason。statusは `complete|timeout|closed|stopped|failed`、bytes_writtenは実際に書けたbyte数で、部分書込みも確認。encoding既定UTF-8。stdin_openで開始した生存shellのみ、設定stdin上限・write期限を適用 |
| `shell eof --execution EXECUTION` | accepted等。stdinを閉じる。stopとは別で、EOF後も実行は続き得る |
| `shell stop --execution EXECUTION` | accepted等。停止を開始し、終了/pipe回収を別途観測。自停止ではCLI返信に有限猶予を設ける |

write/EOF/stopはモデルの所有Run範囲を検査し、独立Runへの副作用を拒否します。read/getは照会可能です。shell終了とraw回収完了は別で、完全な末尾が必要ならoutput_completeも確認します。[shellガイド](shell.ja.md) を参照してください。

### Attachment（3操作＋alias）

| 操作・全固有flag | 主なresultと条件 |
|---|---|
| `attachment add [--thread THREAD] --path PATH --kind image\|file` | 添付詳細の**配列**（各要素はattachment_id/source_node_id/delivery_status/thread_id/path/kind/selected/managed/unavailable_reason）。1件追加でも `result[0].attachment_id` を読む。外部ファイルをパス参照として選択し、モデル要求時に読む |
| `attachment list [--thread THREAD]` | 添付詳細の配列。保存済み選択IDを確認 |
| `attachment remove [--thread THREAD] --attachment ATTACHMENT` | attachment_id/removed。選択解除であり外部ファイルを消さない |

`shellboy attach PATH --kind image|file [--thread THREAD]` はaddのaliasで、pathだけ位置引数です。kindはaliasでも必須。CLIが渡す相対添付pathはCLI起動cwdに対して絶対化されます。画像/PDF/ファイル種別とモデル対応、変化・欠落・圧縮時の除外は [添付](attachments.ja.md) を参照してください。add成功はAPIへの送信完了ではありません。

添付add/listの各要素は `source_node_id` と `delivery_status`（accepted/held/applied/cancelled）を返します。独立した添付追加nodeを保存し、shell由来なら完全tool group後の安全境界で経路へ反映します。addの受付JSONを人間のmessage receiptやモデルによる読取り完了と混同しないでください。再送でも元位置へ展開し最新発言へ付け替えません。同ID・同要求の再送は現在のThread・ファイルのpreflightより先に保存結果を照合するため、受付後にThreadが削除されても元結果を返します。初回要求の検査や呼出元の認証は省略しません。詳しくは [添付ガイド](attachments.ja.md)。

### Compact（1操作）

`compact [--thread THREAD]` は次の安全境界での圧縮要求を保存し、`thread_id/status=queued/operation_id` を返します。通常はモデル要約を伴いAPI利用が生じます。queuedは圧縮採用完了ではありません。同じoperation IDの再送は後続処理後も元のqueued受付を返します。`operation get --operation ID` の `downstream.state`（queued/running/completed/failed）と終端のoutcome/ended_atを確認します。元nodeを上書きせず、source参照を持つ置換経路へ切り替えます。詳細は [履歴と検索](history-and-search.ja.md)。

### Index（5操作）

全操作に共通の固有flagは `[--scope threads|all] [--space SPACE] [--thread THREAD ...] [--source NODE ...]`。thread/sourceはflagを繰り返します。

| 操作 | 主なresultと条件 |
|---|---|
| `index status` | space_id/pending/running/done/failed/paused/reason。状態照会。自動的な全履歴backfillではない |
| `index preview` | sources/segments/characters/already_indexed/unavailable。候補の読取りだけで、embedding要求・job投入・candidate作成はしない |
| `index backfill` | sources/segments/characters/already_indexed/unavailable。明示選択を索引jobへ受付。会話の履歴とは別にAPI費用が生じる |
| `index retry` | accepted。失敗job・pauseの明示再試行。過去全件を新規backfillする操作ではない |
| `index rebuild` | backfillと同形式の受付規模。選択範囲を再構築し、新しい空間が検索可能になるまで既存activeを保持 |

preview/backfill/rebuildはscope、thread、sourceのいずれかによる**明示対象**が必須です。`--scope threads` だけでは対象IDが増えないため、thread IDも指定してください。`--scope all` は全対象を許可する強い指定です。`--space` だけでは明示範囲の代わりになりません。無指定spaceは現在の設定に対応する空間。非active空間への許可範囲や不整合な組合せはエラーになるため、status成功を任意空間変更の許可と解釈しません。返却characters等は金額見積そのものではありません。先に同じ対象の `index preview` で量を確認し、対象範囲と契約単価を確認して実行します。previewは対象を予約せず、後の追加・削除・設定変更で処理量は変わり得ます。CLIは非対話で、backfill/rebuild自体には確認待ちがありません。

### Storage（2操作）

共通flagは `[--category raw|managed-images|history|index|all]`。省略時collectはall、statusは全体情報です。

| 操作 | 主なresultと条件 |
|---|---|
| `storage status` | logical_live_bytes/reclaimable_bytes/db_bytes/wal_bytes/managed_bytes/log_bytes、任意filesystem_free_bytesに加え、database_reusable_bytes・論理カテゴリ内訳・warning/limit_reached等 |
| `storage collect` | raw/managed/index/summaryの回収件数・bytes、nodes_tombstoned/history_bytes_collected・deferred等。pin/保護中の資源は後回し。categoryは対象GCを絞る |

logical使用量・回収可能量・DB/WAL物理サイズ・SQLite再利用ページは別です。GCが通ってもファイルサイズが同量縮む保証はなく、自動VACUUMはしません。historyは明示削除後の到達不能部分が対象で、存続会話を年齢だけで消しません。[復旧と保存](recovery-and-storage.ja.md) を参照してください。

## Cursorの取り扱い

list/get/readのcursorは不透明な継続tokenです。返された文字列をそのまま、同じ操作・対象・filter・encodingへ渡してください。自作・書換え・別操作流用はしません。listのnext_cursorがなければそのページ列挙は終端です。shell readは継続観測用cursorを返し、output_completeまで後続を読みます。

履歴経路/revisionや対象が変わると `cursor_expired`、encodingとの不一致は `cursor_encoding_mismatch`。先頭から再取得してください。shell cursorは実行・raw保持世代・sequence/byte位置・encoding状態を拘束します。rawが回収済みなら `output_expired` で、単なる空出力と扱いません。readのencodingを明示変更すると以前のcursorは使えません。`decode_errors=true` は変換で不正byteがあった印で、元rawを正しいencodingで読み直す判断材料です。

## Errorコード辞書

| code | 意味・次の行動 |
|---|---|
| `invalid_argument` | flag/JSON/値/明示範囲を修正してhelpと照合 |
| `config_invalid` | 接続・モデル・資格情報環境・設定を確認。設定変更は必要に応じowner再起動 |
| `app_not_running` | 同じdata-dirのownerが稼働しているか確認。CLIは自動起動しない |
| `protocol_mismatch` | ownerとCLIの版を揃える |
| `thread_not_found` | ID/削除状態を確認 |
| `target_unavailable` | Run/Turn/shell等が不在・利用不能。元状態を再照会 |
| `operation_conflict` | 同一IDを別内容に再利用した。元receiptを確認 |
| `operation_expired` | 未保存の旧instance ID、または現instanceの回収済み結果を含む失効済み結果での変更再送。保全中の操作メタデータと対象状態を確認し、重複実行の有無を判断 |
| `operation_not_found` | receipt未発見。通信断時は対象状態も確認 |
| `invalid_state` | 現状態・所有境界では実行不可。details/hintと対象getを確認 |
| `wait_cycle` | 自分/所有辺/他waitを含む循環。待機の分解や報告へ切替 |
| `cursor_expired` | 同対象の先頭からページ取得し直す |
| `cursor_encoding_mismatch` | encodingを元に戻すかcursorなしで読直す |
| `output_expired` | raw回収済み。保存抜粋や履歴の参照へ進み、空出力と混同しない |
| `input_limit` | 本文/添付/stdin/frame等の上限。分割/選択見直し |
| `storage_limit` | 論理容量上限。storage status/collect・不要Thread削除等を判断し明示retry |
| `save_failed` | 正本保存に失敗。成功を仮定せず受付/operationと保存先を確認 |
| `io_failed` | 通信・file・shell等のI/O失敗。hint/detailsと対象状態で復旧判断 |

エラーやtimeoutを受けて新しいIDで無条件に再実行せず、保存済み受付、対象の終端/復旧待ち、同じoperationの結果を確認してください。

## 照合先

構文は [cli.rs](../../src/cli.rs)、公開型/定数は [protocol.rs](../../src/protocol.rs)、JSON出力とexitは [main.rs](../../src/main.rs)、操作結果は [Control](../../src/control.rs) / [DB操作](../../src/control/db.rs) と照合しています。公開CLIにはTUI専用の編集送信・添付reload・索引費用previewの独立サブコマンドはありません。画面操作は [TUIガイド](tui.ja.md) を参照してください。

### 非推奨alias

`status --operation ID` は `operation get --operation ID` と同じ照会へ正規化し、stderrで移行先を案内します。現在も受け付けますが非推奨です。新しいスクリプトでは正規の `operation get` を使ってください。JSON stdoutのresultは正規照会の結果です。通常の作業一覧filterとの併用は拒否します。


## compactの範囲指定

引数なしの `compact` は従来のspill→要約です。`--spill` / `--summary` / `--omit` と `--from NODE --to NODE` で範囲を両端inclusiveに指定できます。`--omit`はprefixにも途中にも使え、API要求なしで通知へ置換します。`--trim --to NODE` は先頭からの省略、`--restore [--from MARKER]` は省略の復元です。モードは併用できません。全モードは `--thread THREAD` と通常のoperation ID指定に対応し、queued/downstreamの契約を共用します。使用できない境界では `valid_boundaries` を確認します。

`thread get` の `context_usage` は現在推定、接続の目安、直近成功の実測usageを返します。編集結果は `before_tokens` / `after_tokens` / `target_tokens` / `cache_invalidated_tokens`、`omitted_nodes`、`marker_node_id` と `warnings` を含みます。正確なtokenizer値やキャッシュhitの保証ではありません。手順と注意は[履歴と検索](history-and-search.ja.md#入力の大きさと範囲編集)。

`compact --restore --from ID`のIDは選択中markerか、その省略元の最初のnodeです。通知に書かれた特定範囲を戻す場合はこの指定を使います。引数なしの復元は最新作成markerを選ぶため、複数の省略がある場合に対象が異なることがあります。


RAG sourceの除外は `rag source add PATH --name NAME --exclude '*.db' --exclude build` と複数指定します。既存sourceへの無指定再登録は維持、指定時に置換、`--clear-excludes`で解除。`rag index --dry-run`の`skipped`とsource別`selection`で対象/対象外を確認します。`rag status`はsource別の走査/内容更新時刻、現profileの`semantic_missing`、上限付き失敗一覧を返します。hybridの確認済みsemantic未準備は`ok:true`・`degraded:true`・`scope_status`/`warnings`で返し、実エラーや明示semanticは非0終了を保ちます。具体的な回復と背景indexの結果取得は[RAGガイド](rag.ja.md)を参照してください。
