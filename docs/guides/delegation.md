# 子への委譲と連携

[ガイド入口](README.md) · [Agent Guide](agent-guide.md) · [CLI辞書](cli-reference.md)

shellboyの子エージェントは、子Thread上で動くRunです。Threadは保存する会話と設定、Runはshellと子Runの所有者、Turnは一回答区間です。子を作っただけでは実行が始まらず、依頼の送信でRun/Turnが割り当てられます。親子は同じruntimeとファイルシステムを使い、別の隔離環境ではありません。

## 所有と人間の操作

Threadの `parent_id` は会話の親子関係です。実行上の所有はRunの `owner_run_id` で確認します。モデルshellから、自Threadを親にした子Threadへ送信して新Runを始めると、その実行は現在の親Runの所有下になります。階層を付けない独立Threadへの依頼だけでは、その独立Runを所有しません。

人間の通常の外部CLIにはモデルのOriginがありません。`--parent` でThreadの関係を作っても、人間のsendだけで任意の稼働親Runの所有に偽装できません。外部CLIでは省略先の自Threadもないため、必要な `--thread` と、別保存先なら `--data-dir` を指定します。稼働ownerが必要で、CLIは未起動ownerを自動起動しません。

モデルshellは発行済みOriginを継承します。tokenを表示・コピー・手作業で設定する必要はありません。外部CLIをモデル権限に見せかける操作は行いません。モデルからの停止は自分の所有下に限定され、復旧retryはliveな直属親が判断します。祖父や兄弟は直属親の代わりに子をretryできません。人間は明示操作で介入できます。

## 子を作成して依頼する

次は親モデルのBash shellで行う例です。`CHILD_THREAD_ID` 等は、それぞれの返却JSONから取得した実IDへ置き換えます。コマンドごとに `ok` とresult/errorを確認し、失敗後に残りを機械的に実行しません。

```bash
shellboy thread create --parent "$SHELLBOY_THREAD_ID"
# result.thread_idを控える
shellboy message send --thread CHILD_THREAD_ID --text '担当: parserの読み取り監査。変更せず、確実な不具合と根拠、確認済み範囲を返してください。'
# result.message_id / assignment / run_id / turn_idを控える
shellboy message get --message MESSAGE_ID
```

作成、送信、回答取得は別段階です。createの `result.thread_id` を
`CHILD_THREAD_ID`、sendの `result.message_id` を `CHILD_MESSAGE_ID`、割り当てられた
`result.turn_id` を `CHILD_TURN_ID` として区別します。sendを実行した観測shellの
execution IDで、依頼先の回答やshell出力を取得しようとしないでください。

現在の保存状態を一覧する場合は `shellboy status --thread CHILD_THREAD_ID` を使い、
返却されたRun/Turn/shell/messageのIDから各getへ進めます。子Threadの親子関係と
Runの所有関係は別なので、停止や復旧の権限をThreadの見た目から推定しません。

親を指定した作成では、cwd・AGENTS参照先・connection・model・effortを継承します。cwd・connection・model・effortは対応するcreate引数で明示指定できます。AGENTS参照先を上書きするcreate引数はなく、子作成時にcwdを変えても親のAGENTS参照先を保持します。親の履歴全文は複製しません。必要な本文や `history get` 対象のnode IDを依頼に含めてください。単独Threadの作成や履歴forkは [履歴ガイド](history-and-search.md) を参照してください。

`assignment` は `new_run`、`new_turn`、`existing_turn`、`pending` のいずれかです。`pending` ではまだRun/Turnが決まっていないため、存在しないTurnを待機しません。`message get` で同じmessageを追跡し、`held` の理由や対象の `thread get` / `run get` を確認します。確定済みRun/Turnへの割当は後から別IDへ付け替えません。新規sendで同じ依頼を重複作成してpendingを回避しないでください。

## any待機と回答の取得

Turn IDが得られたら、その回答区間を待ちます。次はBashで、JSON内のIDも実際の返却値へ差し替えます。

```bash
shellboy wait --conditions-json '[{"type":"turn_completed","turn_id":"CHILD_TURN_ID"}]' --timeout-ms 30000
shellboy turn get --turn CHILD_TURN_ID
# result.answer_node_idがあれば取得する
shellboy history get --node ANSWER_NODE_ID --limit-bytes 65536
```

複数conditionは **any** です。一つが満たされれば返り、全ての子の完了を意味しません。

```bash
shellboy wait --conditions-json '[{"type":"turn_completed","turn_id":"TURN_A"},{"type":"turn_completed","turn_id":"TURN_B"}]' --timeout-ms 30000
```

`matched` と `snapshot.conditions` を確認し、未完了の対象だけを改めて待ちます。`turn_completed` は停止/失敗も含む終端観測なので、`satisfied` でも対象の `state` と `answer_node_id` を必ず確認します。回答nodeがない停止を架空の回答で補いません。本文に `next_cursor` があれば同じnodeをページ取得します。

| wait status | 次の行動 |
|---|---|
| `satisfied` | matchedの対象状態を確認し、必要な回答node/出力を取得する |
| `timeout` | 対象は継続。進捗を見て作業するか、別operationとして再待機する |
| `interrupted` | 観測の解除。対象の停止成功とは扱わず、現在状態と新しい指示を確認する |
| `recovery_wait` | 明示復旧判断が必要。原因を取得し、直属親ならretry/stop、必要なら上へ相談する |
| `unavailable` | 削除等で対象を利用できない。snapshot/reasonを確認し、同じwaitを無条件反復しない |

自分のRun/Turn/現在shell、祖先や他のwaitとの循環は拒否されます。子が親Runの終了を待つと、親による子の回収待ちと循環し得ます。`wait_cycle` をsleepループで迂回せず、依頼と通知の向きを修正します。Ctrl+Cや接続切断はCLIの観測を解除し、対象Run/shellの停止にはなりません。

## 途中報告と生存期間

子は `thread get` の `parent_id` を確認し、必要な単発報告を親へ送れます。

```bash
shellboy thread get
shellboy message send --thread PARENT_THREAD_ID --text '途中報告: 読み取り監査は3ファイル完了。残り1ファイル、変更はありません。返信は不要です。'
```

これは通常のmessage通知です。報告ごとに親からの返答を待つ義務はなく、親は安全な境界で採用します。親への報告で最終回答の取得を代用しません。子の回答完了とRun終了も別です。背景shell等が残れば、Turn回答後もRunはliveのことがあります。全所有処理の終わりが必要なら `run_ended` を待ちます。

```bash
shellboy wait --conditions-json '[{"type":"run_ended","run_id":"CHILD_RUN_ID"}]' --timeout-ms 30000
```

親のTurnが回答完了しても、所有する子/孫Runやshellが残っていれば親Runはliveのままです。正常なRun完了は所有処理や未処理指示等が残っていないことを確認して確定します。親Runを明示停止した場合やアプリ終了では、所有する配下を停止・回収します。子をアプリ終了後も残す永続daemonとしては使いません。独立Threadの独立Runは親Runの所有回収に含まれません。

## 復旧を判断する

```bash
shellboy run get --run CHILD_RUN_ID
shellboy turn get --turn CHILD_TURN_ID
# 原因を確認・修復してから、直属親または人間が明示する
shellboy run retry --run CHILD_RUN_ID
# 続行しない場合
shellboy run stop --run CHILD_RUN_ID
```

retryとstopは選択肢です。上の両方を続けて実行する例ではありません。子の `recovery_wait` へモデルが普通のmessageを送っても、その本文は保留され、自動復旧の代わりになりません。再起動後は以前のRunを自動再実行せず、中断情報・保存済み回答/未処理messageを見て新しい実行を判断します。停止時は未実行指示のmessage IDを保持してください。

PowerShellでは `$env:SHELLBOY_THREAD_ID` を使います。単一引用符のJSON例は利用shellの引数渡しに合わせ、詳しくは [shell](shell.md) を参照してください。Windows実機での動作確認は今回対象外です。
