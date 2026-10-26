# Agent Guide — shellboy内のモデル向け

[ガイド入口](README.md) · [委譲](delegation.md) · [shell](shell.md) · [CLI辞書](cli-reference.md)

shellboyで作業するモデル向けの手順です。日本語の「会話」はThread、「一回答区間」はTurn、「所有する実行のまとまり」はRunに対応します。「子エージェント」「サブエージェント」「subagent」は、子Threadで開始する所有下のRunを指します。別のモデル用toolが増えるわけではありません。

## このガイドを読ませる

ガイドは実行ファイルに同梱されます。system指示は `shellboy docs list` と検索の入口を案内し、必要な文書だけを読めるようにします。repo外のcwdでも使え、owner・API・設定は不要です。

```bash
shellboy docs list
shellboy docs search --query 'サブエージェント'
shellboy docs get --id shellboy/delegation
shellboy docs get --id shellboy/shell
```

本文はJSONの `result.text`、続きは `next_cursor` で取得します。全文が毎回のpromptへ展開されるわけではありません。同梱リンクの文書IDや見出しは `result.links` で確認できます。作業先の `AGENTS.md` は別の作業指示であり、製品ガイドとは区別します。

## 自分の文脈を確認する

モデルに公開されるtoolは `shell` 一つです。会話操作、子作成、待機、履歴、添付は、そのshellから `shellboy` CLIを呼びます。次はBash例です。

```bash
pwd
shellboy thread get
shellboy run get --run "$SHELLBOY_RUN_ID"
shellboy turn get --turn "$SHELLBOY_TURN_ID"
shellboy --help
```

モデルshellでは `SHELLBOY_THREAD_ID`、`SHELLBOY_RUN_ID`、`SHELLBOY_TURN_ID`、`SHELLBOY_EXECUTION_ID` と接続先の文脈を継承します。`thread get`、`message send`等の省略可能な `--thread` はこの自Threadを使います。IDが必要な引数は省略せず指定します。対象を混同しないよう、返却されたThread/Run/Turn/message/node/execution IDを別々に保持してください。

自分でコマンドを実行するときはshell toolを直接呼びます。`message send` は相手への依頼の受付で、依頼内容のコマンド実行ではありません。送信CLIを実行したshellの `execution_id` でreadすると受付JSONが返ります。依頼先で実行したshellを読むには、そのshell toolが返した別のIDが必要です。background起動の返却IDを受け取り、次のshell toolで `shell get/read` を呼ぶ二段階の操作は通常の使い方です。`message send` もshell tool内のCLI実行であり、「shell経路」と排他的な別経路ではありません。自分自身のThreadへの送信は自分の会話への追加入力で、別エージェントの起動ではありません。

shellの起動cwdはThreadに保存されたcwdです。別Threadは履歴とcwdを分けますが、ファイルシステムのsandbox分離ではありません。同じファイルを並行編集するなら担当ファイルを分けます。AGENTS.mdはTurn開始時のsnapshotに従い、同Turnのretry途中で勝手に入れ替わりません。

OriginはモデルshellからCLIへ継承される認証済み文脈です。環境全体を出力したり、tokenを表示・コピーしたりしないでください。人間の通常の外部CLIとは権限と所有判定が異なります。[所有の説明](delegation.md#所有と人間の操作)を参照してください。

添付の追加もshell内から `shellboy attachment add --path PATH --kind image` を呼びます。返却配列の `source_node_id` と `delivery_status` を確認し、受付をモデルによる読取り完了と混同しないでください。自分のtool groupの全結果が揃ってから専用nodeへ反映されます。過去の添付は元の出典位置で再送されるため、今回の発言が添付なしなら新規画像とは扱いません。今回読取りの版・更新通知はShellboyメタデータで区別します。詳しくは [添付ガイド](attachments.md)。

## 次の操作へ進む最短経路

| 今知りたいこと | 入口 | 次に読むもの |
|---|---|---|
| コマンドの意味・例 | `shellboy <command> --help` / `shellboy docs search --query '背景'` | 結果のguide IDを `docs get` |
| 今の作業状態 | `shellboy status --thread THREAD_ID` | 各itemが返す対象のget。保存状態でありOS生存の推測ではない |
| 送った依頼の割当 | `shellboy message get --message MESSAGE_ID` | 返されたTurnをgetし、answer_node_idがあればhistory get |
| 起動したコマンドの出力 | `shellboy shell get --execution WORK_EXECUTION_ID` | 同じIDでshell read。read結果のcursorで続きを取得 |
| 会話の判断根拠 | `shellboy history search ...` / `history list ...` | node IDからhistory get、関係はhistory relations |
| ガイド・資料・履歴を横断 | `shellboy rag search --scope ... --query ...` | referenceからrag get。履歴は明示scopeで含める |

`status`の既定は非終端の作業です。完了・失敗・停止も調べる場合は `--all` を付けます。

`docs`は操作手順、`status`は現在の保存状態、`history`は会話の根拠、`rag`は明示した
検索範囲の資料取得です。全文や正確な状態が必要なら抜粋・受付だけで終えず、返却対象へ進みます。

同じ作業でもIDの役割は別です。`operation_id`は操作受付、`message_id`は依頼、
`thread_id`は会話、`run_id`は実行所有、`turn_id`は回答区間、`answer_node_id`は回答原文、
`execution_id`は一つのshellプロセスです。`message send`を動かしたshellのIDは依頼先の
shell IDになりません。起動の返却IDを `WORK_EXECUTION_ID`、依頼先の返却IDを
`CHILD_THREAD_ID` / `CHILD_MESSAGE_ID` / `CHILD_TURN_ID`のように分けて保持します。
別のshell tool呼出しへローカル変数が自動共有されるとは仮定しません。

## 一つの作業を進める

1. 指示・現在状態・必要な原文を取得する。
2. shellで有限な作業を行う。長い処理はbackgroundを選び、返されたexecutionを追跡する。
3. 別担当へ分けるなら子Threadを作成し、依頼を送る。受付後に割当を確認してから待つ。
4. waitのstatusと対象状態を確認し、回答nodeの原文を読む。
5. 結果、検証、失敗と残件を利用者へ報告する。

子へは目的、担当範囲、触ってよいファイル、完了条件、返してほしい結果を本文で渡します。子は親会話全文を自動継承しません。最小の実行例とpendingの扱いは [委譲ガイド](delegation.md) にあります。親のTurnが回答完了しても、所有する子/孫・shellが残れば親Runはliveのまま継続します。正常なRun完了は所有処理等が残っていないことが条件です。親Runの明示停止やアプリ終了では配下を停止・回収します。子の結果が必要なら回答通知を確認し、原文を取得してください。

## 受付と完了を区別する

CLIの `ok:true` は操作が成功したことを示し、送った依頼への回答完了を意味しません。`message send` のresultで `message_id`、`delivery_status`、`assignment`、任意の `run_id` / `turn_id` を確認します。`accepted` と `applied` を同一視せず、`held` / `pending` なら `message get` で追跡します。

通信切断で結果が不明なら、まず保存した `operation_id` を `shellboy operation get --operation OPERATION_ID` で照会します。送信後の接続障害では、自動生成IDも返却JSONの `operation_id` / `error.details.operation_id` に残ります。受付記録が見つからないことは未実行の証明ではありません。新operation IDで同じ副作用を無条件に繰り返さず、対象の状態と原文を確認します。get/list/search/preview/statusはreceiptを保存せず、同じ照会IDでも再観測します。履歴semantic検索はquery embedding費用が再発生し得ます。変更操作とwaitの元IDは結果照合用として保持します。詳細は [復旧](recovery-and-storage.md) と [CLI辞書](cli-reference.md) を参照してください。

## 必要な原文を取得する

```bash
shellboy history list --thread THREAD_ID --limit 20
shellboy history get --node NODE_ID --limit-bytes 65536
```

例の大文字IDは返却された実際の値に置き換えます。listや検索のexcerptは抜粋です。getの `next_cursor` があれば、同じnodeへ `--cursor` を付けて続きを取得します。回答はTurnの `answer_node_id` から取得し、要約・spillなら `source_node_ids` をたどれます。前後関係は `previous_node_ids` / `next_node_ids`、回収済み原文はtombstone理由を確認します。取得できない本文を推測で補いません。

検索範囲を限定するなら明示します。検索の `--thread` は他の自己対象操作と異なり、省略時は全体です。

```bash
shellboy history search --query 'エラー処理の判断' --thread "$SHELLBOY_THREAD_ID" --exclude-kind tool_result --limit 10
shellboy history relations --node NODE_ID --relation sources --limit 50
shellboy history relations --node NODE_ID --relation replacements --limit 50
```

Thread検索は現在の採用経路とcompletedな置換の出典を含み、全体検索は採用経路を保証しません。kind・作成期間filterも検索段階で適用できます。hitのprovenanceは生成元で、fork先の所属一覧ではありません。spansは抽出本文のUnicode scalar位置の意味検索chunk範囲、distanceはL2距離です。語の完全一致とは解釈せず、getで原文を確認します。Thread限定時はthread_scope.on_current_pathで採用path上か置換出典だけかを区別し、全体検索では所属を推定しません。同じ内容の異なるnodeを同一履歴とみなしません。relationsは会話のprevious/nextと置換のsources/replacementsを区別し、next_cursorは同じnode・relationへ渡します。採用経路はhistory listで確認し、祖先・子孫の全件を返すと推測しません。詳細は[履歴・検索](history-and-search.md)。

モデルの内部reasoningや秘密値を出力させる依頼はしません。報告には利用者に必要な結論、作業結果、根拠となる公開可能な原文や検証結果を使います。保存されたprovider itemの取得可能性と、秘密・内部推論の開示依頼は別です。

## PowerShellで読む場合

この文書の実行ブロックはBashです。PowerShellでは現在Runは `$env:SHELLBOY_RUN_ID`、現在Threadは `$env:SHELLBOY_THREAD_ID` と書きます。`$SHELLBOY_RUN_ID` をそのまま移さないでください。JSON引数の引用はshellごとに異なるため [shellガイド](shell.md) の例を使います。Windows native adapterは実装とcross-codecheck済みですが、Windows実機の利用例を実行保証したものではありません。


## 出力の対象が分からなくなったら

`shellboy shell get --execution ID` の `command` / `cwd` / `thread_id` / `run_id` / `turn_id` を、起動tool結果の同じfieldと照合します。commandが `shellboy message send ...` なら、そのIDのreadが受付JSONになるのは対象プロセスの実stdoutです。受付JSONから別shellの起動成功やIDを推定せず、目的のcommandを実行したtool結果を取得します。未実行ならshell toolへcommandを直接渡します。「直接実行した」という自分の説明より、保存されたcommandとtool引数を根拠にします。

新fieldは更新後のownerから返ります。稼働中の旧ownerを継続して使っている場合は、fieldがないことを混入の証拠にせず、起動時のtool引数を履歴で確認します。ownerの正常終了・再起動後には保存済みexecutionも新しいgetで照会できます。

## モデル一覧と自分のThreadのモデル変更

shellから次を実行します。認証されたモデルのshellではThread指定を省略できます。

```sh
shellboy thread get
shellboy model list
shellboy model list --connection proxy
shellboy thread set --connection proxy --model YOUR_RESPONSES_MODEL_ID
shellboy thread get
```

`model list` の成功JSONは `result.connection` と `result.models` に接続名とモデルID配列を返します。明示接続がなければ自分のThreadの接続を使います。外部端末ではowner既定を使うため、特定Threadなら `--thread THREAD` を付けます。別のThreadを変更する `thread set` にも対象IDを明示してください。

一覧取得は設定を変更しません。モデル名から能力を推測せず、一覧にembeddingも含まれる点に注意します。`thread set` は指定Threadだけを更新し、新規Threadの既定や省略したeffortを変えません。既に進行中のHTTPへは遡及せず次の安全境界から適用されます。利用者から任された作業範囲でモデルを選択してください。


会話の目的が明確になったら `shellboy thread set --title '調査と修正'` で自身のタイトルを編集できます。別Threadには `--thread THREAD` を明示します。`--title ''` で手動名を解除し会話由来の自動表示へ戻します。省略すればタイトルを維持します。512文字以内の一行にし、本文・モデル設定は変更しません。

## 操作追跡・保存済み状態・削除前の確認

返答を見失った操作は `shellboy operation get --operation OPERATION_ID` で照会します。受付結果のstate/outcomeと後続完了を区別し、targetsから対象getへ進みます。messageとcompactはdownstreamも確認します。compactの同ID再送は後続完了後も元のqueued受付を返します。pending/result_uncertainは副作用未実行の証明ではなく、新IDで安全に再送できる意味でもありません。照会自身にはreceiptを作らず、そのoperation IDで観測結果を再取得する保証はありません。attachment.addの同ID再送はmutableなpreflightより先に元結果を返しますが、認証と要求一致の検査は維持します。

Thread/Run/Turn/message/history getのcontextは、保存済み時刻・状態・理由・所有関係です。parent_thread_idはThreadの系譜、owner_run_idは実行所有です。未知のstarted_at、未終了のlifetime_msは省略され、lifetime_msは準備・待ち込みの作成→終了です。recovery_reasonはJSON値として読み、復旧待ち・再起動時の結果不明をfailureと決めつけません。historyの生成元と採用先は区別します。

削除を任された範囲で `shellboy thread delete-preview --thread THREAD_ID`（子孫も対象なら `--descendants` を追加）を使い、阻害Run、対象子孫、直接共有・出典・添付参照を確認できます。これは削除許可の取得やGC容量保証ではなく、reclaimable_bytesはnullです。実deleteが最新状態を再判定します。詳細は[復旧・保存](recovery-and-storage.md)と[CLI辞書](cli-reference.md)を参照してください。

## 過去分の埋め込みと費用の確認

過去の会話を埋め込むbackfill/rebuildは、会話中の通常の検索とは別にAPI費用が生じます。利用者から依頼されていない範囲、特に全履歴の処理を自発的に開始しないでください。任された対象を `shellboy index preview --scope threads --thread THREAD_ID`（全体を依頼された場合だけ `--scope all`）で確認してから、依頼された範囲のbackfill/rebuildを実行します。previewは無課金の候補読取りであり、正確なtoken数・請求額や対象予約ではありません。CLI実行自体には対話確認がありません。詳細は[履歴と検索](history-and-search.md)を参照してください。

### 操作結果の保持と期限切れ

変更操作とwaitの結果は、created_atから24時間経過後の内部保守、又はreceipt容量の圧迫時に回収される場合があります。pending/未反映参照は保護されます。同ID再送が `operation_expired` になっても、変更が未実行とは限りません。IDの期限切れ記録を残して再実行を拒否しているため、`operation get` の `result_expired` と対象の状態・履歴を調べ、新IDで同じ副作用を自動反復しないでください。

読取りを繰り返してもoperation receipt予算は消費しません。変更/待機のpending予約や期限切れIDのmetadataは有限予算を使うため、無制限の受付保証はありません。`status` / `storage status` で状況を確認できます。詳しくは [復旧・容量](recovery-and-storage.md#operation-receiptの容量で拒否された場合)。
