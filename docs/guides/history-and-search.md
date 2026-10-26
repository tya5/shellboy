# 履歴・分岐・検索

[User Guide](user-guide.md) · [画面操作](tui.md) · [ガイド一覧](README.md)

会話原文はnodeとして保存し、Threadは採用する経路を持ちます。fork、要約、スピルは元nodeを上書きせず、元の前後や出典へ戻れる形で保存します。検索は別のembedding索引で、原文が存在するだけでは検索対象になったとは限りません。

以下のCLI例はBash/zsh用です。`THREAD_ID`、`NODE_ID`、`CURSOR`、`SPACE_ID`は実際の返却値へ置換します。人間の外部ターミナルからはTUIと同じdata-dirへ接続してください。異なる保存先を使う場合は各コマンドに`--data-dir /path/to/shellboy-data`を付けます。

## 原文を探して読む

TUIではF6で履歴項目を選び、Oで原文page、Nで次page、Pで先頭へ進めます。前後・出典IDを確認し、別nodeを指定する場合はF8の「履歴 · 原文」、関係のページ取得には「履歴 · 関係」を使います。検索結果などの管理画面はTabでfieldを選びEnterで詳細へ進みます。[画面操作](tui.md#管理結果のfieldと詳細page) に全結果・原文・コピーの使い分けがあります。

```sh
shellboy thread list --limit 20
shellboy thread get --thread THREAD_ID
shellboy history list --thread THREAD_ID --limit 20
shellboy history get --node NODE_ID --limit-bytes 65536
```

CLIの出力はJSON envelopeです。`ok`と`error`を確認し、内容は`result`から読みます。`history list`は選択経路のnode ID・kind・抜粋をpageで返します。`result.next_cursor`があれば、同じThread・条件で次へ進みます。

```sh
shellboy history list --thread THREAD_ID --limit 20 --cursor 'CURSOR'
```

`history get`の`text`はUTF-8文字境界を守った本文pageです。`source_node_ids`は要約等の出典、`previous_node_ids`と`next_node_ids`は元の前後への参照です。辿りたいIDに対して再び`history get`を呼びます。原文を読むだけでThreadの採用経路は変わりません。`provenance` はnodeの種類・作成日時・現在の原文revisionと、保存上対応する場合だけ生成元Thread/Run/Turnを返します。fork先の所属を生成元と取り違えないでください。

`history get` の `context` は保存済み作成時刻・作成元Turn/Run/Threadとpresent/tombstone状態を返します。本文回収後もtombstone_reasonと作成元を確認でき、fork先の採用や実行開始時刻を推定する値ではありません。

直接の関係は種類を分けてページ取得できます。

```bash
shellboy history relations --node NODE_ID --relation previous --limit 50
shellboy history relations --node NODE_ID --relation next --limit 50
shellboy history relations --node SUMMARY_NODE_ID --relation sources --limit 50
shellboy history relations --node SOURCE_NODE_ID --relation replacements --limit 50
```

previous/nextは会話の前後、sourcesは要約・spill出力の出典、replacementsは出典を使った置換出力です。置換はcompletedのものだけを返し、途中・失敗した置換を履歴として扱いません。`items` のnode_idをgetして本文を読み、`next_cursor` があれば同じnode・relationへ `--cursor` で続けます。既定50・最大200件です。祖先・子孫を無制限に展開する操作ではありません。relation itemのsource_revision/source_hashは置換が記録した出典の版・hash、provenance.source_revisionは返却nodeの現在版なので区別します。

```sh
shellboy history get --node NODE_ID --encoding UTF-8 --limit-bytes 65536 --cursor 'CURSOR'
```

続きにはその本文が返した`next_cursor`を使います。cursorは不透明な値で、手で加工したり別node・別encodingへ流用しません。現在の本文page上限は64KiBです。削除・回収済みなら`tombstone_reason`等で理由を確認し、空文字を「もともと出力がなかった」と読み替えないでください。

`history get`のtool_resultは会話へ保存した結果・抜粋です。shellの保存raw出力は別に`shellboy shell read --execution EXECUTION_ID`でpage取得します。encoding変更や出力保持期限は[shellガイド](shell.md)を参照してください。外部添付はパス参照であり、historyから過去の外部ファイル実体を復元する機能はありません。

TUIではホイールで3表示行、PgUp/PgDnで履歴表示領域の高さ−1行ずつ移動し、保存する閲覧位置は元nodeと原文位置で保持します。Ctrl+Endで末尾へ戻ります。表示cacheが回収されても原文pageを再取得します。`/copy NODE_ID`は画面の省略・推論placeholderではなく原文を取得します。

## 実行と履歴を関連付ける

```bash
shellboy history list --thread THREAD_ID --run RUN_ID --limit 20
shellboy history list --thread THREAD_ID --since '2026-10-25T00:00:00Z'
shellboy history list --thread THREAD_ID --status running
```

`--run` はnodeを生成したTurnのRunで絞ります。`--thread` は現在そのnodeを採用する経路なので、fork先のThreadを指定しても生成元Runは変わりません。`--since` はnodeの作成時刻をRFC3339の絶対時刻で比較し、指定時刻を含みます。shellの終了時刻や状態更新時刻を意味しません。

`--status` は**関連shell executionの保存済みstate**の完全一致です。`starting`、`running`、`stopping`、`exited`、`failed`、`stopped` を指定できます。RunやTurnのstateとは別で、実shellが関連付いていないnodeは対象になりません。background shellはtool callの返却後もrunningであり得ます。

一覧の `provenance` と `context` はnodeの生成元・保存状態です。`selection.thread_id` / `ordinal` は今回一覧にした採用経路、`selection.current_cwd` はそのThreadの現在設定です。実際にshellを開始した場所は `executions[].cwd` を見ます。現在設定から過去の実行cwdを推測しません。

`executions` は保存済みtool group/callとshell実行の関係から取得し、本文中のIDらしい文字や受付JSONから推測しません。`execution_id` から `shell get/read`、`request_node_id` と `result_node_id` から `history get` へ進めます。要求nodeが一意でない記録ではrequest IDを省略します。`call_state` とshellの `state` を区別してください。

1つのnodeでは最大8実行を掲載し、全件数は `execution_count`、省略は `executions_truncated` で明示します。実行を作らず取消・拒否されたcallはこの件数に含みません。短い `command_excerpt` は最大80文字の保存済みcommand抜粋で、全文は `shell get` や要求原文で確認します。

```bash
shellboy shell get --execution EXECUTION_ID
shellboy shell read --execution EXECUTION_ID --encoding UTF-8
shellboy history get --node REQUEST_NODE_ID
shellboy status --run ORIGIN_RUN_ID --all --limit 200
```

8件を超える実行は生成元Runのstatus一覧でたどれます。`next_cursor` があれば同じ条件で続きを取得します。履歴のcursorはfilterに束縛され、statusで絞った場合は関連shellのstate変更でも失効します。最新状態から再取得し、照会の失敗を理由に依頼を再送しません。

## Forkと依頼編集

```sh
shellboy thread fork --thread THREAD_ID --node NODE_ID
```

指定nodeを含むprefixを共有する新Threadを作ります。返された新しい`thread_id`を使ってTUIで`/switch THREAD_ID`し、続きの依頼を送ります。CLIでは`shellboy message send --thread THREAD_ID --text '続きの依頼'`です。元Threadの原文は変更しません。必要ならfork時に`--cwd /path/to/project`を指定できます。

TUIの`/fork NODE_ID`も同じ分岐操作です。未完結のtool callと対応結果の途中では分岐できないことがあります。エラーに従ってgroupの安全な境界を選びます。

依頼を差し替える場合はTUIで`/edit USER_NODE_ID`を使います。元依頼の全文・添付参照を別の編集用下書きへ読み、送信時に元依頼の直前をprefixとするThreadへ分岐して新しい依頼を保存します。元user nodeへの直接UPDATEではありません。元ThreadのRunが動作中なら、そのRunを止める明示確認があります。編集内容と停止対象を確認してY、取り消すならN/Escです。独立した`shellboy edit` CLIはありません。

## Compactとスピル

```text
/compact
```

CLIは`shellboy compact --thread THREAD_ID`です。これは自動の容量予測ではなく、明示した圧縮要求です。処理中のモデル通信や未完結tool groupがあれば安全境界まで保留します。要約作成・再利用と経路採用の結果を確認してください。受付だけで圧縮完了とは限りません。返却operation IDを `shellboy operation get --operation OPERATION_ID` へ渡し、`downstream.state` と終端outcomeを確認します。同IDのcompact再送は完了後も元のqueued受付を返し、圧縮を二重開始しません。再起動で取消された後続処理はfailed/result_uncertainとして確認し、外部副作用の自動再実行はしません。

スピルは長いtool結果の送信本文を参照付きplaceholderへ置き換える操作です。要約は目的・制約・状態・次作業・参照等を別nodeへ保存します。どちらも保持規則内の元原文を読む道を残し、他Threadの採用経路を勝手に変更しません。該当区間の添付実体は送信対象から外れるので、必要なら[再添付](attachments.md)します。

自動圧縮はproviderが明示したcontext超過だけが入口です。古い結果のスピル→通常要求retry1→なお明示超過なら最新groupを含むスピル・要約→通常要求retry2→なお失敗ならrecovery待ち、という有限の復旧です。対象なしなら中間retryを省略します。未知エラーや単なるHTTP/byte制限をcontext超過と決めつけず、実行済みshellを圧縮のために再実行しません。

recovery待ちで手動compactしても、通常要求は自動再開しません。結果を確認して人間又は所有する直属親が`shellboy run retry --run RUN_ID`で明示再開します。詳細は[復旧・保存](recovery-and-storage.md)にあります。

## 推論表示とモデル変更

返却された推論テキスト・要約はTUIで既定折りたたみです。`/reasoning NODE_ID`で開閉できます。返されなかった推論を補ったり、暗号化reasoningを解読・表示したりはしません。本文の元文字位置とnode IDは開閉で変わりません。

接続又はモデルの変更で送信用reasoning epochを進め、旧epochのreasoning/opaque継続データを以後の要求から除外します。A→B→Aと戻しても旧Aのopaqueを自動復活しません。元nodeはDBに保持し、historyで表示可能な原文テキストを取得できます。`history get`はopaque本文を表示する操作ではありません。effortだけの変更はこのモデル切替と区別します。

## 検索は索引を別に準備する

embedding接続・モデルを設定すると、その後に新規確定した会話を自動索引へ登録します。起動や設定変更だけで全過去履歴を自動backfillしません。モデル入力へ毎回検索結果を自動注入する機能でもありません。

```text
/search 作業中のエラー処理
```

CLIは次の形です。

```sh
shellboy history search --query '作業中のエラー処理' --limit 10
```

検索はactiveなIndexSpaceでquery embeddingを作り、元nodeへまとめた抜粋・distance・node ID等を返します。結果は元nodeの`history get`で確認します。embedding API利用には費用が発生し得ます。該当がない場合は原文不在とは限らず、未索引、別space、個別失敗等を確認します。ローカルフォルダーの自動取り込みはありません。

検索対象を先に絞る場合はCLI又はF8「履歴 · 検索」で指定します。

```bash
shellboy history search --query '作業中のエラー処理' --thread THREAD_ID --kind user --kind assistant --exclude-kind tool_result --from '2026-10-01T00:00:00+09:00' --to '2026-11-01T00:00:00+09:00'
```

`--thread` は現在の採用経路と、そこからcompletedな置換の出典を再帰的にたどった範囲です。指定なしは全体で、現在のThreadへ暗黙に限定せず、hitが採用経路にある保証もありません。kindはnode種類で、同じ `--kind` 内はOR、それ以外の条件はANDです。fromは含み、toは含まないnode作成日時をoffset付きRFC3339で指定します。filter適用後に順位を決めるため、全体の上位結果をshellで絞る場合の取り逃しを避けられます。

結果のprovenanceで生成元と現在の原文revisionを確認できます。spansのstart/endはbody_kindごとに抽出した本文のUnicode scalar位置 `[start,end)` です。これは意味検索で選ばれたchunk範囲で、検索語の一致位置ではありません。source_revision/content_hashを併せて原文を特定します。distanceはL2距離で、距離thresholdの指定はありません。chunkをnode単位にまとめ、同じ内容の別nodeは統合しません。Thread指定時だけthread_scopeを返し、on_current_path=trueなら現在の採用path上、falseならcompleted置換の出典として対象になった結果です。全体検索ではthread_scopeを省略し、所属を推定しません。詳しいfield・上限は[CLI辞書](cli-reference.md)を参照してください。

会話本文、表示可能な推論、tool結果は種類を区別して索引化し、保持されているshell rawの中間部分も対象にできます。添付のバイナリや暗号化opaqueをembeddingへ送りません。rawが回収されると派生vectorも無効化し、回収中の遅い結果を復活採用しません。

## 索引管理・費用preview

TUIで対象を明示します。

```text
/index status
/index thread
/index threads THREAD_ID_1 THREAD_ID_2
/index all
/index space SPACE_ID thread
```

`/index status`は状態表示です。`thread`は現在Thread、`threads`は指定した複数Thread、`all`は保持されたsource全体を選びます。`space`だけの指定では過去分処理の対象を選んだことになりません。管理画面ではspace一覧・active/selected・model/次元・coverage定義を確認できます。jobのdone件数と「全sourceを完全に索引済み」は同じではありません。部分索引やrawの未確認coverageは表示された定義で読みます。

| 小文字キー | 用途 |
| --- | --- |
| `s` | 状態を再取得 |
| `p` | 選択scopeのsource/segment/文字数と既索引分をpreview |
| `b` | 過去分backfill |
| `t` | 失敗分retry・pauseからの明示再開 |
| `r` | rebuild |

`b/t/r`には明示対象と同じ条件のpreviewが必要で、費用が生じる旨を確認してYで実行します。previewはAPIを呼ばず、取得時点の件数を示します。単価不明時に料金額を断定しません。sourceの後続更新を予約・固定する操作ではなく、実行後は次のpreviewを取り直します。

CLIでも対象を選んで操作できます。ただしCLIにはpreview専用コマンドやTUIの確認画面がなく、次の操作は直接受付になります。先に `index preview` で件数・segment数・文字数を読み、費用と対象を確認してから実行してください。preview自体はembedding APIを呼ばず、jobを投入しません。

```sh
shellboy index status
shellboy index preview --scope threads --thread THREAD_ID
shellboy index backfill --scope threads --thread THREAD_ID
shellboy index retry --scope threads --thread THREAD_ID
shellboy index rebuild --scope all
```

対象は`--scope all`、`--scope threads --thread ID`のほか、`--source NODE_ID`、`--space SPACE_ID`で指定できます。spaceだけではbackfill/rebuildの過去分scope選択を代用しません。CLIの`index retry`は対象省略も受け付け、その接続のactive/candidate空間にある失敗job等を再開対象にします。対象を限定したい場合は上の例のようにThread/sourceを明示してください。TUIの実行前preview要件とは異なります。詳しい条件は[CLI参照](cli-reference.md)にあります。

`pending/running/done/failed`はjob状態、`paused`とreasonはspaceの停止理由です。個別の一時失敗には有限retryを行いますが、認証等の共通障害はpauseし、新会話の処理を巻き添え停止しません。原因を直した後は明示retryします。既に成功した部分は再処理しません。

接続・embedding model・実測次元などでIndexSpaceを分けます。設定が変わっても旧空間を勝手に削除せず、rebuildの新候補が失敗した場合は旧active空間を保ちます。過去分を新空間へ入れるには明示backfill/rebuildが必要です。モデルaliasの実体変更まで自動検知する保証はありません。

検索は削除済みnodeや回収済み本文、古い索引を除きます。Thread削除と共有nodeの削除は別で、他Thread等に保持される原文は全体検索で見つかり得ます。provenanceのorigin_thread_idは生成元の記録であり、そのThreadが現在操作可能とは限りません。検索は原文をpinしないので、後からgetしたときのtombstone_reasonも確認します。

relationsのcursorは初回より後の追加関係を含めませんが、全DBのsnapshotや本文の保持期限を予約するものではありません。対象nodeのrevisionが変わればCursorExpiredです。関係先のmetadataはページ取得時点の値です。再検索や現在値の照会はreceiptを作らず、同じoperation IDでも再観測します。cursorのsnapshot条件や有効期限は別に維持されます。semantic再検索ではquery embeddingの費用が再発生し得ます。
