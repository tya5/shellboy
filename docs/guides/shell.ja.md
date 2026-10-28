# shell実行と出力の取得

[English](shell.md)

[ガイド入口](README.ja.md) · [Agent Guide](agent-guide.md) · [CLI辞書](cli-reference.ja.md)

モデルの外部作業用toolは `shell` です。CLIの `shellboy shell ...` は、既に起動したexecutionの観測・stdin・停止を扱います。`shellboy shell start` というコマンドはありません。人間は通常の自分の端末で任意のコマンドを実行できますが、それだけではshellboy所有のexecutionとして記録されません。

## モデルtoolの引数

次はtool callのJSON引数です。CLIへそのまま渡すJSONではありません。未知のfieldは拒否され、必須は `command` だけです。

```json
{
  "command": "printf '%s\\n' 'hello'",
  "background": false,
  "stdin_open": false,
  "initial_stdin": "",
  "input_encoding": "UTF-8",
  "output_encoding": "UTF-8"
}
```

| field | 省略時 | 意味 |
|---|---|---|
| `command` | 必須 | 設定されたBash/PowerShellで実行する文字列 |
| `background` | `false` | `true`なら起動後にexecution IDを返してモデルが先へ進む |
| `stdin_open` | `false` | 初期入力後もstdinを開くか。falseなら初期入力後にEOF |
| `initial_stdin` | なし | 起動時に渡す文字列。後続CLI writeより先に書き込む |
| `input_encoding` | `UTF-8` | 初期stdinをbyteへ変換するencoding |
| `output_encoding` | `UTF-8` | 出力の初期表示encoding。rawはbyteのまま保存 |

`command`の例はBashです。shell種別/実行ファイルは起動設定に従い、tool引数に独自の `shell` / `cwd` fieldはありません。cwdはThread設定を使います。作業先を変更したいときは、設定とその変更範囲を確認してください。Bash構文をPowerShellのcommandへそのまま流用しません。

## foregroundとbackground

foregroundはこのtoolの完了をモデルが待ちます。UI/Control全体を停止する意味ではなく、人間は他Threadや別操作へ進めます。backgroundは同じ所有方式で実行し、返却された `execution_id` を後から確認します。モデルのTurnが最終回答を返しても、所有するshellや子Runが残ればRunはliveのままです。正常なRun完了には所有処理等が残っていないことが必要です。Runの明示停止やアプリ終了では所有処理を停止・回収します。

```json
{"command":"sleep 2; printf '%s\\n' 'ready'","background":true}
```

実executionの観測は次のBash例で行います。大文字IDは返却値に差し替えてください。

```bash
shellboy shell get --execution EXECUTION_ID
shellboy shell read --execution EXECUTION_ID --limit-bytes 65536
shellboy wait --conditions-json '[{"type":"shell_ended","execution_id":"EXECUTION_ID"}]' --timeout-ms 30000
```

観測結果から選ぶ次操作は次の通りです。

| 保存状態・結果 | 次の観測 |
|---|---|
| starting / running / stopping | 同じexecutionをget/readし、必要ならshell_endedまたはshell_outputをwait |
| wait timeout | 対象は継続。同じexecutionを確認して観測を続ける。commandを再起動しない |
| exited / failed / stopped | exit_code・end_reason・output_completeと末尾出力を読む。終端を成功と同一視しない |
| cursor_expired / cursor_encoding_mismatch | 対象とencodingを確認しcursorなしで読取りを再開。既読byteが再び返ることがある |
| output_expired | 保持状態を確認する。消えたrawはreadの反復では復元できない |

人間がforegroundを途中で背景化した場合、対象shellは生存し、未実行の後続tool callはcancelledになります。追加指示でforeground待機が解除された場合も、既存executionの状態を確認します。toolの待機解除だけをプロセス終了と見なして同じcommandを再実行しないでください。

この場合のtool resultは `status: "running"`、継続対象の `execution_id`、介入理由 `reason: "human_backgrounded"` 又は `"additional_input"` を返します。後続callがあれば各callに `cancelled` 結果が付きます。UIも同じ保存済み結果から対象と後続の取消しを通知し、後続がなければ取消しを捏造しません。通知後にshellが終了することもあるため、最新状態は同じIDでget/read/waitします。

Ctrl+CによるRun中断は背景化と異なり、対象と所有配下へ停止を要求します。停止受付を全PID回収済みと扱わず、Run/shellの終端を照会してください。中断されたモデルを通知だけのために自動再開することはありません。


### background出力だけを確認する最小手順（Bash）

この検証では、対象shellのcommandを次の文字列に固定します。トークン以外の表示、CLI呼出し、パイプ、リダイレクトをcommandへ足しません。通常のBash設定なら、2秒後にstdoutへASCIIの `SHELLBOY_BG_READ_V1` だけを改行なしで出し、stderrは空、終了コードは0になります。

```json
{"command":"sleep 2; printf SHELLBOY_BG_READ_V1","background":true,"stdin_open":false,"output_encoding":"UTF-8"}
```

これはモデルのshell tool引数です。モデル自身がこのtoolを直接呼び、返却された**この起動の `execution_id`** を控えます。`shellboy message send` で起動を依頼する手順へ置き換えません。

**起動と観測は二段階で構いません。** まず上記のtool結果からIDを取得し、次のshell toolで以下の観測CLIを実行します。shell間で変数は共有されないため、`BG_EXECUTION_ID` には実際の返却IDを文字列として設定します。人間が同じownerへ接続する別端末で観測しても構いません。既定以外の保存先なら各CLIへ `--data-dir PATH` を指定してください。

```bash
BG_EXECUTION_ID='起動時に返ったexecution_id'
BG_READ_CHECK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/shellboy-bg-read.XXXXXX")

shellboy wait --conditions-json "[{\"type\":\"shell_ended\",\"execution_id\":\"$BG_EXECUTION_ID\"}]" --timeout-ms 30000 > "$BG_READ_CHECK_DIR/wait.json"
shellboy shell get --execution "$BG_EXECUTION_ID" > "$BG_READ_CHECK_DIR/get.json"
shellboy shell read --execution "$BG_EXECUTION_ID" --encoding UTF-8 --limit-bytes 65536 > "$BG_READ_CHECK_DIR/read.json"

cat "$BG_READ_CHECK_DIR/read.json"
```

各ファイルのトップレベル `ok:true` を確認してから、以下で判定します。waitがtimeout/失敗なら検証完了にせず状態を調べます。最初のreadにはcursorを付けません。

| 観測元 | 期待値 |
|---|---|
| `wait.json` の `result.status` | `satisfied`。これだけでは正常終了の証明にはしない |
| `get.json` の `result.command` | `sleep 2; printf SHELLBOY_BG_READ_V1` と完全一致。起動tool結果のcommand/Thread/Run/Turnも照合 |
| `get.json` の `result.state` / `exit_code` / `output_complete` | `exited` / `0` / `true` |
| `read.json` の `result.execution_id` | 起動時に控えたIDと一致 |
| `read.json` の `result.text` | `SHELLBOY_BG_READ_V1` と完全一致。改行やJSONは含まない |
| `read.json` の `result.raw_bytes` | `19`。このページで取得したrawの**バイト数**であり、rawの内容や累積総量ではない |
| `read.json` の `result.encoding` / `decode_errors` / `output_complete` | `UTF-8` / `false` / `true` |

判定対象は画面全体やモデルの説明ではなく、保存した `read.json` の `result.text` と `raw_bytes` です。観測CLIを実行したshellにも別の `execution_id` が付きますが、`BG_EXECUTION_ID` をそのIDへ置き換えません。追加指示や他Threadへのメッセージ送信があっても、起動・観測を一度のtool呼出しにまとめる必要はありません。

`shellboy message send` をshellから呼んだ場合、そのshellの `execution_id` が指すのは**送信CLIを実行したプロセス**です。そのstdoutに `accepted` / `message_id` 等の受付JSONが保存されるのは正常です。受付は依頼先でのshell起動完了を意味せず、受付結果のmessage/run/turn IDもshellのexecution IDではありません。委譲して検証する場合は、依頼先が実際に呼んだshell toolの返却から対象IDを取得する必要があります。この最小検証では直接toolを呼ぶことで、この区別を不要にします。

更新後の起動tool結果と `shell get` は実commandと所有文脈を返します。command等のfieldがない旧ownerは正常終了・再起動すると新しい照会情報を利用できます。

`shell read` は指定executionのstdout/stderrの保存rawをdecodeして返します。別executionのCLI返却JSONが自動で混入するものではありません。一方、対象command自体から `shellboy message send` 等を呼べば、そのCLIのstdoutは実際に対象shellのrawへ入ります。余分な文字があれば、まず `result.execution_id` と `shell get` の `command` / `cwd` / `thread_id` / `run_id` / `turn_id`、shell設定を確認し、表示上の混同と実rawへの出力を区別してください。

## stdin、初期入力、EOF

stdinは既定で閉じます。後からwriteする必要がある処理には、起動時に `stdin_open:true` を指定します。

```json
{"command":"cat","background":true,"stdin_open":true,"initial_stdin":"first line\n","input_encoding":"UTF-8"}
```

```bash
shellboy shell write --execution EXECUTION_ID --text 'second line'
# 改行を含む入力ならUTF-8 stdinから渡す
printf '%s\n' 'third line' | shellboy shell write --execution EXECUTION_ID --text-stdin
shellboy shell eof --execution EXECUTION_ID
```

`--text` は自動で改行を足しません。`shell eof` は空文字writeと異なり、pipeを閉じます。EOF後は再openできません。stdinはプロセスのpipeで、PTY/対話端末ではないため、tty前提のアプリを同じように操作できるとは限りません。

writeのresultは `bytes_written`、`status`、`stdin_open`、任意の `reason` です。`complete` 以外に `timeout` / `closed` / `stopped` / `failed` があり、部分writeの可能性があります。timeoutを全量未送信と見なして全文再送すると重複するため、自動再送しません。入力は指定encodingで全体を表現できるかを事前確認し、表現不能文字を置換して送信しません。

## raw、encodingとcursor

stdout/stderrは別streamとしてraw byteを保存し、表示時に独立decodeします。取得ページは途中byte/改行なしにも対応し、encoding変更でrawを上書きしません。保存量と表示抜粋は別で、tool resultの抜粋だけを全出力として扱わないでください。

```bash
shellboy shell read --execution EXECUTION_ID --encoding CP932 --limit-bytes 65536
# result.cursorを使って同じencodingの続きへ
shellboy shell read --execution EXECUTION_ID --cursor CURSOR --limit-bytes 65536
```

取得resultの `text`、`raw_bytes`、`encoding`、`cursor`、`decode_errors`、`output_complete` を確認します。cursorは不透明な値で、実行ID・位置・retention世代・encoding等に結び付きます。手で編集しません。encodingを変えて最初から読み直すならcursorを付けずに指定します。旧cursorに別encodingを混ぜると `cursor_encoding_mismatch` です。明示encoding変更は関連する索引を無効化するため、必要なら改めて明示再索引します。

CP932/windows-31j/ms932はShift_JIS系として扱います。UTF-16はbyte orderが分かる `UTF-16LE` / `UTF-16BE` を指定します。曖昧なUTF-16入力変換に依存しません。出力に不正byteがあれば `decode_errors` を確認し、適切なencodingでrawを再取得できます。

出力がまだ増える可能性があるときは、返却cursorで更新を待てます。

```bash
shellboy wait --conditions-json '[{"type":"shell_output","execution_id":"EXECUTION_ID","cursor":"CURSOR"}]' --timeout-ms 30000
shellboy shell read --execution EXECUTION_ID --cursor CURSOR --limit-bytes 65536
```

waitは対象終端でも解除されるため、空の末尾ページでは `output_complete` とshell状態も確認します。保持期限/容量によるraw回収は `output_expired`、旧世代cursor等は明示エラーになります。空出力成功へ読み替えません。詳細は [復旧と保存](recovery-and-storage.ja.md) を参照してください。

## 停止と保存完了

```bash
shellboy shell stop --execution EXECUTION_ID
shellboy shell get --execution EXECUTION_ID
```

stopの `accepted:true` は停止受付です。終了状態を確認してから停止完了と報告します。shellの `state` は `starting` / `running` / `stopping` / `exited` / `failed` / `stopped`、`exit_code` は存在するときだけ値を持ちます。`output_complete` と終了理由は別です。shell終端でもdrain等が不完全なら、全出力が保存されたと保証できません。

raw上限に達した場合は対象を停止し、理由と不完全性を記録します。保存済みbyteを隠して無限に実行し続ける仕組みではありません。モデルOriginからは自Run/所有配下のshellを操作し、独立Runへwrite/EOF/stopはできません。実行再起動後に以前のPIDを使って停止することもありません。

## PowerShellの引数

Windowsでは設定されたPowerShellを使用します。たとえばtoolのcommandは `Write-Output 'ready'` のようにPowerShell構文で書きます。CLIの環境変数参照は `$env:SHELLBOY_RUN_ID` 等です。Bashの `$SHELLBOY_RUN_ID`、`printf`、here-documentは同じ意味になりません。

Windows PowerShell 5.1と新しいpwshではnative commandへJSONを渡す引用動作が異なります。小さなJSON例を渡す場合も、実shellで `shellboy --help` と利用中の引数渡しを確認してください。たとえばWindows PowerShell 5.1のstop-parsing構文なら、変数展開を使わず実IDを直接置きます。

```powershell
shellboy --% wait --conditions-json [{\"type\":\"run_ended\",\"run_id\":\"ACTUAL_RUN_ID\"}] --timeout-ms 30000
```

`--%` 以降で `$env:...` が通常どおり展開されることを期待しません。このPowerShell例はWindows実機で未検証です。各PowerShell版での実行確認も未実施です。通常の文字列sendには `shellboy message send --thread 'ACTUAL_THREAD_ID' --text '確認してください'` のように単純な引数を使えます。

## shellの経過時間

`elapsed_ms` はOS起動直前から、直接の子processの終了・配下processの回収確認・出力drainを経てcompletionを確定するまでの監視期間です。配下がpipeを保持している間の待ちや出力の背圧も含み、確定後のDB保存待ち・結果読取り時間は含みません。`started_at` / `ended_at` は別の保存時刻です。過去の未計測実行には推定値を足さず、このfieldを省略します。
