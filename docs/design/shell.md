# Shell runtime

採用状態: Unix群管理とWindows生成時Job adapterを実装済み。OS別のnative CI・配布確認と、Windowsの各shell・端末での実機受入を区別し、実施範囲は [検証状況](../validation/acceptance.md) に記録する。shellはモデル唯一のツールであり、CLIから再帰起動する操作を増やさない。初期のCLI・詳細設計・プロセス制御資料は非同梱。現行の生成・入力・出力・回収契約を以下に示す。

`ShellSupervisor::new(&Config)` は設定snapshotを保持する。`spawn(execution_id,cwd,args,env)` は**呼出し元がDBへ起動意図をcommit済みで、所有Runがstoppingでないと再確認した後**に呼び出す。runtimeはDBを操作しない。起動途中の停止競合と世代照合もControlの責務で、spawn成功を永続受付成功と混同しない。

返値 `ShellExecution` はclone可能な `ShellHandle`、限定容量の `events`、`completion` watchを持つ。foreground/backgroundの違いは呼出し元が完了を待つかだけで、起動方式を変えない。`handle.stop()` は同期の取消token設定のみで、stdin lock/出力保存/queue空きを待たない。最後のhandleのdropも停止要求となる。`write/eof` は非同期直列化し、初期stdinが後続CLI write/eofより必ず先行する。`stdin_open=false` の既定では初期stdinの後でpipeを閉じ、指定がなければ直ちにEOFとなる。

入力は全体を指定encodingに変換できることと、**変換後byte数**がshell_stdin_bytes以下であることを確認してからpipeへ書く。UTF-8、明示UTF-16LE/BE、encoding_rsの既知legacy encodingを使用し、CP932/windows-31j/ms932はShift_JISへ対応する。UTF-16無指定byte orderは拒否し、encoding_rsのUTF-16 encodeがUTF-8を返す仕様に依存せず明示的にu16をbyte化する。表現不能文字を置換して送らない。timeout後は `StdinWriteResult` の実受渡しbyte数とstatusを返し自動再送しない。初期stdinの不完全受渡しはexecutionを失敗として停止する。出力は再decode可能なraw bytesを保ち、output_encodingは起動前に既知labelだけを検査する。stream別のdecoder/checkpointとraw cursorは取得層の責務。

Bashは `--noprofile --norc -c -- <command>`、PowerShellは `-NoLogo -NoProfile -NonInteractive -OutputFormat Text -EncodedCommand`。PowerShell引数はUTF-16LE/Base64で、stdinをcommand sourceに使わない。`set -e`、`exit $LASTEXITCODE`、引用符修正は加えない。Bashのoption終端をcommandの前に固定し、先頭がdashの本文もoptionにせずそのまま一つのscript引数で渡す（S8）。UnixとWindows Git Bashは同じprivate argv helperを使い、本文への空白追加や書換えで回避しない。cwdは起動snapshot。親環境を継承しBASH_ENVを除去しない。設定envの後に呼出し元envを重ね、origin識別値を優先する。shellboy実行ファイルの親PATH追加、origin token発行/検証はControlが行う。

stdout/stderrは別pump。各pipeに最大8KiBの固定read bufferを一つ持ち、それ以外の未保存出力はoutput_buffer_bytesの共有semaphoreで制限する。予算をread前に全予約すると片streamの無出力で他streamが停止するため、read後に実byte数のpermitを取得する。最小予算でも両streamが読める。bufferの固定overheadは最大16KiBで、設定予算に比例して増えない。送信は単一publisher順序でsequenceを付け、同時書込みの因果順を主張しない。

`ShellEvent::Output(OutputChunk)` のprivate permitは、**DBがchunkをcommitしてからchunkをdropするまで**保持する。consumerがbytesを無制限queueへcloneしてpermitを先に返してはいけない。64件のchannelとbyte予算の両方で背圧がかかる。停止control経路は別である。execution当たりshell_raw_bytes超過は保存可能prefixまでを送り、明示 `raw output limit exceeded` を記録して対象を停止する。上限後も実行を継続させて黙って出力を捨てる方式は採らない。

Unixは起動時 `process_group(0)` でexecution固有群へ入れる。停止は群TERM→有限猶予→KILL。直接shellの自然終了でも同じ群の残存処理へTERM/KILLし、pipe保持による無期限drainを防ぐ。direct childのexit code/signal、forced、output_complete、group_cleanup_confirmedは別々に保持する。 S3では完了時に実supervisorが返したforced/exit_signal/group_cleanup_confirmedを既存shell_jsonのcompletion objectへ保存し、同じtransactionでshell_endedへ含める。shell getの追加optional fieldとモデルtool結果も同じ事実を公開する。既存schema/columnは増やさず、旧記録・起動中・回復時の未知な値をfalseやsignalなしへ補完しない。旧履歴/raw/保存済みtool結果は書き換えない。 ShellDetailsの追加fieldはserde default/None省略とし、新clientは旧ownerの返値も受け取れる。欠損や旧extensionの型不明値は未知のまま省略し、JSON booleanのfalseは既知のfalseとして保持する。信号API成功を完全回収確認と同じにせず、`kill(-pgid,0)` が消滅を確認できなければ未確認を残す。群外へ脱離した処理と本体SIGKILL後の孤児完全回収は原文通り保証外。前回PIDを再起動後killする経路は存在しない。

出力/初期stdin taskのdrainはdrain_timeout_msで有限化し、残るtaskはabortする。queueの背圧でdrain未完了となった場合も `output_complete=false` と理由を残す。 S5ではDrain不完全だけで自然終了のExited/exit codeをFailedへ置換しない。初期stdin・wait・raw上限・実I/O等の操作エラーは従来通りFailedとする。direct child待ちで停止branchを選んだ事実をその監視task内のboolへ固定し、終了後の配下確認/drain中に届いたstopを最終stateへ遡及しない。同時にreadyなら確認済みchild終了を先に採る。出力不完全による未実行callの取消しは維持し、理由をprocess失敗と区別する。新しい永続状態/列/workerは追加せず、Unix/Windowsの共通state判定だけをprivate helperへ寄せる。consumerは**eventsをEOFまで取得・永続化してから**completionの終端を保存し、そのcommit後に所有者へ通知する。watch到着だけでは出力保存完了を意味しない。

## 実装時点の検証と残件

macOSで `cargo test --test shell` を実行。実Bashによるstdout/stderr、非UTF-8/改行なし原文、実exit code、初期stdin/既定EOF/明示EOF、strict CP932/UTF-16変換、起動前入力拒否、stdin deadlineと部分byte数、stdin詰まり中の独立停止、TERM無視からKILL、自然shell終了後のpipe保持descendant回収、raw上限の対象停止、16byte予算でstderrのみ出力/背圧中停止、env注入を検証する。DB永続化境界はControl統合テストで別途検証する。

Windowsは下記native adapterで生成時Job割当を行う。OS/Job条件による失敗は明示エラーで返し、単体process killへfallbackしない。Git Bash/PowerShell5.1/pwshのOS実機受入は未実施。Unix版PowerShellも実行ファイルが必要で、現時点のテストはBashのみ。Windows版の初版完了を宣言するにはJob割当時の生成raceを閉じたadapter、Job継承/残存処理/通常異常終了、3 shellの実機結果が必要。

2026-10-24 macOSでshell runtime 11件すべて成功、`cargo clippy --test shell -- -D warnings` 成功。追加fixtureで最後のhandle dropによる停止と、自然終了したshellから残ったTERM無視descendantへのKILLを確認した。post-KILL direct child reapにもdrain timeoutを設け、OS側でreapを確認できない場合を失敗として記録する。

## Windows Job生成調査（2026-10-24）

[Rust stableのCommandExt source](https://doc.rust-lang.org/stable/src/std/os/windows/process.rs.html) では `spawn_with_attributes` が `windows_process_extensions_raw_attribute` のunstable APIであり、固定stable toolchainから直接使えない。Tokio固定版も任意のSTARTUPINFOEX属性を渡す公開spawnを持たない。

[Microsoft UpdateProcThreadAttribute](https://learn.microsoft.com/en-us/windows/desktop/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute) のJOB_LISTはWindows10/Server2016以降で生成時にJob handle列を割り当てる。HANDLE_LISTは継承する標準I/O handleを限定できる。[CreateProcessW](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-createprocessw) はSTARTUPINFOEXにEXTENDED_STARTUPINFO_PRESENTを指定し、Unicode環境blockと書換え可能なUTF-16 command lineを要求する。applicationNameにはPATH探索がないため実行ファイルの明示解決も必要。

採用実装はstable `windows-sys` のnative CreateProcessW adapter。非継承KILL_ON_JOB_CLOSE Jobを先に作り、JOB_LIST/HANDLE_LISTの両属性を生成時に付ける。parent stdout/stderrはTokioのoverlapped named pipe、stdinは完了byte数を取得するnative overlapped pipe、childは同期標準I/O handleとしPTYを使わない。共通stdinはOS adapterの閉じたenumで保持する。直接process handleでexitを観測し、JobのActiveProcessesを確認して残存処理を終了する。生成後AssignやCREATE_SUSPENDEDの隙間へ保証を弱めない。native adapterを実装し、cross-target型検査を通した。Windows実機検証を完了した根拠ではない。


### Windows adapterの確定境界

継承したUnicode環境blockでは、`=C:`だけでなく`=::`、`=ExitCode`等の先頭`=`を持つ隠し変数も保持する。読込みと組立の規則を揃え、名前は非空・NULなし・先頭以外の`=`なし、値はNULなしを要求する。値の`=`とUTF-16 code unitは保持し、設定の`[shell.env]`の名前制約は緩めない。不正entryの診断には先頭128文字までの変数名を引用・制御文字escapeして含め、値は含めない。[Issue #1](https://github.com/tya5/shellboy/issues/1)の対策であり、CreateProcessW/Job/handleの所有や生成順序は変更しない。

`src/shell/windows.rs` は非継承Jobを生成しKILL_ON_JOB_CLOSEを設定、JOB_LISTと標準I/O三本だけのHANDLE_LISTを同じSTARTUPINFOEXへ登録する。Windows10/Server2016以降を必要とし、利用できない場合は生成を拒否する。生成後Assignやsuspend/resume経路はない。明示applicationName、CRT規則で引用するUTF-16 argv、変更可能commandline、Unicode環境blockを作る。PATHは設定env→origin env順に大文字小文字を区別しないOrdinal比較で更新し、hidden drive変数も保持する。入力commandlineがOS上限を超えればinput_limit。Windows inboxのWSL bash launcherはGit Bash adapterとして使わない。

直接processの終了は保持handleへ0msのWaitForSingleObjectを行う非同期pollで観測する。終了code259も終了観測後の実codeとして扱う。明示停止と自然終了後の残存処理はJob単位で終了し、ActiveProcesses=0を有限期限で確認する。Windows Job終了をUnix TERM相当と説明せず、forced=trueで保持する。Jobはcompletion公開前に閉じる。

`src/shell/windows_stdin.rs` はTokio/Mioのqueued write量を実転送量と混同しないため、stdin専用のoverlapped WriteFileを使う。[GetOverlappedResult](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult) の完了byte数を加算し、64KiB単位で進める。初期入力の全操作が完了するまで既定EOFを出さない。deadline/停止は[CancelIoEx](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelioex)を要求し、正常/取消し/失敗の完了を改めて確認する。

取消し要求だけでOVERLAPPED/bufferを解放しない。Rust futureがdropされても、未完了operationは安定したheapとhandle/eventを保持したcleanup waiterへ移す。通常の待機はTokioの短いtimer pollで、最後の安全な回収だけblocking poolを利用する。runtime終了でcleanup jobが開始されない場合は領域をプロセス終了まで保持し、kernel向けpointerをdanglingにしない。取消し完了を1秒で確認できない場合はfailedと未確認範囲を返し、現在<=64KiBは未確認として自動再送を禁止する。成功や完全受渡しを装わない。

`windows_command.rs` のUTF-16 quoting/env組立はmacOSからもfixture試験する。Windows実機向け5ケースはliteral quote/stdout/stderr/exit、明示stdin/EOF、日本語、pipe容量を超える初期入力、自然終了時の子Job回収、停止/詰まったstdin deadlineを含む。公開回帰は [UTF-16 command/env試験](../../tests/shell_windows_command.rs) と [Windows shell試験](../../tests/shell_windows.rs) を参照する。fixtureの存在を実機合格と扱わず、実行OS・native CI・未実施の範囲は [検証状況](../validation/acceptance.md) で確認する。

## 出力の完全性と複数失敗

初期stdin、process wait、pipe read、receiver切断、raw上限、drain未完了などの理由は内部enumで区別し、種類／streamごとの先頭理由を有限件保持する。公開のreason文字列には発生順で連結して返す。raw切詰めは独立flagに保存し、先行するstdin失敗や文字列の表記によって `output_complete=true` にならない。output_completeはdrain成立と切詰め／出力失敗の有無から決める。内部2型は既存failure mutexの内容を置換し、新しい永続形式・event・worker・依存は追加しない。

## raw保存batch

Control collectorは最初のchunkから `io.output_batch_bytes` または `io.output_batch_ms` の先着でflushし、EOFでは残りを直ちに保存する。受信chunkは再結合／再採番せず、stream offsetとsaved_sequenceを同transactionで更新する。次chunkでbyte上限を超える場合は次batchへ持ち越し、pumpのread chunk上限もbatch byte以下へ抑える。各chunkのbyte permitはbatchのcommitまで保持し、成功後だけwait observerへ通知する。

保存失敗はbatch全体をrollbackし、既存の保存障害停止へ進む。consumerは全eventsの保存後にだけcompletionを永続化する。出力のdrain絶対期限は維持し、batch化を理由に無期限待機や単なる猶予延長は行わない。stream原文・cursor・retention generationの契約は維持する。新しい表・event・worker・依存は追加しない。


## 既に閉じたstdinへのEOF（S11）

EOFはbyteの送信ではなく「stdinを閉じた状態にする」冪等操作として扱う。
既知executionのpipeが既に閉じていても成功し、二度目にbyte送信や別processの起動は行わない。
live handleに対する `accepted:true` はこの閉鎖要求を処理した意味であり、新たに閉じた、processが
終了した、またはbyteを届けたという意味ではない。handle回収後の既知executionでは既存shell detailsを返す。
`write` の `status:closed` はbyteを渡せなかったという別の契約なので、EOFへその結果型を追加しない。
不明IDはtarget_unavailable、他Runへの権限検査は維持し、冪等性を理由に未知対象の成功を作らない。
同operation IDは保存済み結果を返す。pipe閉鎖後のfresh IDも同じ閉鎖状態を保ち、原出力を変えない。
公開結果型/状態/表/workerを増やさず、閉鎖済みと終了済みの回帰でこの解釈を固定する。

## 起動登録とstop/EOFの競合（C8/S7）

`starting`行のcommitからOS spawn・live handle登録まではCLIから見える。stop/EOFは
handleの有無によらず既存行へ意図を先に保存し、stopは既存`stopping`、EOFは既存
`stdin_state=closed`を使う。その後live handleがあれば同じ操作を反映する。
起動側はspawn直前に保存済みstop/Run停止を確認し、停止ならOS副作用を作らず終端化する。
spawnを跨いだ競合はcollectorがprocess所有を引き受けた後の登録確認で再適用する。
EOFは初期stdinとの既存直列順序を保って反映し、stopはstdin待ちを経由しない。
新しい永続state、table、受付queueは追加しない。既知の終端へのstop/EOFは既存の冪等照会、
未知IDはTargetUnavailableのままとする。共通責務をmacOSで確認し、Windows実機成功は主張しない。
EOF保存は既存schemaの意図であり、pipeの実閉鎖完了を新たな永続事実として保証しない。
EOF反映失敗は元の要求または起動側に返し、processの所有/回収責務を手放さない。
EOFより後のstop保存は常に優先し、登録側の再確認は古いintentで停止を上書きしない。
EOF/stopの保存失敗はhandleだけを先に操作せず既存SaveFailed経路へ返す。

## 猶予停止の所有と終了（C11）

返信猶予は既存request guardまたはruntime operationの所有期間内に置く。IPCのRun停止は
返却済みの保存結果をtransportへ渡した後、同じrequest workerが猶予とstop_runを完了する。
自己shell停止は保存結果を既存watchへ先に公開し、同じruntime workerが猶予とhandle.stopを
終えてからoperation登録を解放する。shutdownの取消しは猶予を即解除し、既存の全要求/
runtime operation drainが停止処理の完了まで待つ。切り離したsleep task、別registry、
常駐workerは作らない。猶予待機数は既存の所有要求/operation数の内数で、同一operation再送は
同じ結果watchと停止責務を共有する。Forceは既存のbest-effort上書きである。

自己stopと登録が競合する場合は、既存RuntimeOperationに自己停止対象IDを一つだけ保持し、
登録側がその既存watchで保存結果を確認する。成功なら返信猶予の所有workerへ委譲し、
失敗・取消し・watch消失なら保存済みstopを登録側が反映する。watch待機中はmap lockを保持しない。
猶予所有者の除去とhandle確認はruntime_operations→shellsの順で同じ短い保護区間に置き、
「未登録handle」と「既に解放された所有者」を互いに見逃さない。DB待機をこの区間に置かない。
このOptionは既存operation件数の内数で、wire/保存schemaへ追加しない。
shutdownの各所有map確認はguardを一つずつ解放してから次へ進み、逆順の多重lockを作らない。
C5の受付freezeも既存Notifyで猶予を解除するため、既受付Run stopのguard待機と循環しない。

同一executionに複数の自己stopがある場合、登録時点の既存watchだけを有限snapshotとして確認し、
成功ownerが一つあればその猶予を尊重する。全owner失敗/消失なら保存済みstopを登録側が反映する。
後着ownerを再走査して起動登録を無限に引き延ばさず、shutdown取消しはどのwatch待ちにも優先する。

猶予ownerはRuntimeReservation::New確定後、stop意図の保存前にだけ登録する。
Savedの同ID再送は保存結果だけを返し、新しい猶予や停止副作用を作らない。Pending/予約失敗も同様。

保存済みstop/EOF、spawn登録のrunning反映、spawn前停止はcommit後に既存Changed/Notifyを出す。
handle未登録やstdin直列待ちの間も、UI/Waitは保存済みの状態と意図を観測できる。
新しいevent型やpollingは追加しない。

猶予timerは既存Tokio sleepを一度だけ構築してpinし、Notify再確認でも期限を延長しない。
Instantへの直接加算を避け、設定の大きい正値にも固定版Tokioのsafe-delay処理を維持する。

## shellの単調経過時間（S6）

新しく起動するshellはOS spawn直前のTokio `Instant` を既存supervisor task内だけで保持し、
direct child・配下確認・出力drainを終えてcompletionを作るまでの `elapsed_ms` を確定する。
completion生成後のDB保存待ちや後続のexcerpt読取り時間は含めない。drain中の出力背圧は
監視期間に含む。整数msはSQLite JSONと相互運用できる
非負i64の範囲へ飽和させる。Unix/Windows adapterは同じ期間定義を使う。

既存 `shell_json.completion` と `shell_ended` payloadへこの値を保存し、モデルのshell結果と
`ShellDetails` の終端値はその同じ測定値を返す。Issue #45 adds read-time wall-clock
elapsed_ms for live executions only; terminal measurements are never replaced by it.
旧terminal・起動失敗・再起動で回収された未知の値を
wall clock差や0で補完しない。未知は省略し、既存のstarted_at/ended_atは別の時刻事実として残す。
永続Instant・追加列・timer worker・実行状態は追加しない。Run/TurnのUI計時は別のF11範囲とする。

## Unix群の消滅と遅延KILL（S9）

PID再利用について本設計が保証するのは、再起動後に保存された古いPIDだけを根拠にkillしないことで、
稼働中のPGIDに世代付き・原子的な所有権があるという保証ではない。現在のUnix adapterは
直接childをreapしてから残存群を停止するため、群probeとsignalの間にはOS上の競合が残る。

猶予中の最後のprobeからsleepを挟んで無条件KILLする経路は修正する。KILLの直前にもprobeし、
その時点でESRCHならKILLせず回収確認済みで終了する。EPERM等の不明なprobeを消滅と見なさず、
信号の試行と消滅確認は別に保持する。これは不要な遅延KILLを避けるもので、probe→signal間の
原子性やPGID再利用の完全防止は主張しない。新しいregistry・常駐poller・永続OS識別子は追加しない。

leaderのwaitable状態を維持する `waitid(WNOWAIT)` とSIGCHLDでreapを後へ移す案は、Unix child監視と
zombieを含む群消滅確認の意味を変えるため今回は採らない。既存の有限supervisor責務を保ち、
OS残余TOCTOUを明示する。[POSIX waitid](https://pubs.opengroup.org/onlinepubs/9699919799/functions/waitid.html)
のWNOWAITはwaitable状態の保持であり、既存Tokio Child::waitの単なる直前チェックとは異なる。
private helperへprobe/signalを注入して消滅・存在・probe不明を決定的に確認し、実際のPGID再利用や
無関係なprocessへのsignalをテストしない。実Bashの配下回収回帰は別に維持する。

## shell I/Oの共通所有（R3-1）

FailureKind/ShellFailuresは既存S4/S5の型付き失敗・独立truncation判定を再利用する。まず両OSの同じdrain/abort/join/finished/失敗投影を共通関数へ純移動し、その後stdin初期順序と二つのpumpをprivate IoSessionへ寄せる。OutputSinkは既存publisher/byte semaphore/chunk size/stop/failureの所有をまとめ、pumpをreader/stream/sinkの3引数にする。新しい永続state、worker、queue、依存は追加せず、各executionのtask数とbyte/64event枠を維持する。

I/O終了が返すのはstate/reason/output_completeの3値で、exit code/signal/forced/group確認/単調時間はOS supervisorの実観測から従来どおり組み立てる。Windows Jobをterminal通知前に閉じる順、Unixの群停止/回収と残余TOCTOU、自然終了後のstopで状態を変えない境界を保つ。OSのwait/Job/PGIDの違いを二methodのtraitへ押し込む追加抽象は採らない。既存NativeChild/unix_groupで境界を保持でき、共通I/Oとは寿命が異なるためである。

共通型は既存task/資源の一時所有であり、dropで未確認の再実行や新たな監視を開始しない。開始時はpublic handleを返す前にstdin guardを確保し、終了時はdrain期限→未完taskのabortと実join→finished→completionの順序を維持する。macOSの実shell回帰と利用可能なWindows targetの型検査を区別し、未保有Windows実機の成功は主張しない。

## Read-time timing observations (Issue #45)

`shell get` adds the saved `started_at` and nullable `last_output_at`. For
starting/running/stopping shells, `elapsed_ms` is computed at read time from
started_at and the current UTC clock, clamped at zero if the clock is before
the start; malformed timestamps remain unknown. This is not the supervisor's
monotonic completion duration and can reflect clock adjustments. Terminal
shells return only the saved validated completion duration (or unknown).
The last retained output sequence supplies received_at via the output_chunk
primary key, with no output/collected output represented by null. `status`
uses the same reader for returned shell items. Reads do not claim OS liveness.
No new persistent facts, worker or polling cadence are introduced.

## Unexecuted calls after a prerequisite (#56)

The serial tool-group driver saves structured cancellation facts described in
[tool cancellation reasons](tool-cancel-reasons.md): stable reason, human message,
`executed: false`, and known prerequisite call/execution/state/exit facts. It
preserves the existing halt decisions and does not add a foreground timeout or
stop a shell merely because its foreground wait was released.
