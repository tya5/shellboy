# Shell raw output取得

採用状態: 実装済み。基準はCLI契約3/5/6節、詳細設計7節、引継ぎA12・大量出力/GC境界。`control::output::ShellOutputReader` がCLI read/get、wait、モデル結果excerpt、索引の共通取得層である。raw BLOBをdecode済み文字列で置き換えない。

## 公開APIとcursor

`ShellOutputReader::new(DbWorker,instance_id)`、`get(execution_id)` はShellDetails、`read(ShellReadArgs)` はShellOutputを返す。ShellOutputはtext/encoding/cursor/raw_bytes/output_complete/decode_errorsを持つ。出力完全性とtargetの成功終了は別であり、終了状態はgetで取得する。`read_stream(args,StreamSelection)` でBoth/Stdout/Stderrを選択でき、通常CLIはBoth。

**2026-10-24採用のローカル契約追補（CLI契約3/5/6節の照会情報を拡張）:** `shell get` は既存fieldに `command` / `cwd` / `thread_id` / `turn_id` を追加する。実executionの保存値を返し、command文字列を解析して別の実行へ読み替えない。新fieldはoptionalとし、旧応答のdeserializeとcommand未保存の旧記録を扱う。DB migration・操作・所有判定・raw/cursor契約は変更しない。commandはget時だけ取得し、rawの各ページ走査へ追加しない。モデル向けshell tool結果にも、execution_idを持つ全終了/待機解除経路で同じ保存値と既存run_idを付ける。結果保存transaction内で取得し、履歴とモデルreplayの対応を保つ。エラー/cancelledでexecutionを作っていない場合は実行情報を捏造しない。

背景: モデルが `shell(command="shellboy message send …")` の返却IDを、依頼先で実行するはずのshell IDと繰り返し誤認した。実rawは正しかったが、旧getではcommand/Thread/Turnを確認できず、利用者にDB調査を頼る診断になっていた。短いsystem/tool説明と実データの対応情報で、この曖昧さを解消する。commandは原文であり秘密を含み得るため、通常画面や診断ログへ追加出力せず、明示照会/既存モデル履歴の範囲で返す。

cursorはbase64url JSONのopaque version1。execution ID、全stream共通sequence、chunk内元byte offset、retention generation、canonical encoding fingerprint、stream選択、EOF flush済みを含む。byte位置にdecoded text長を使わない。chunk末尾は次sequence/offset0に正規化する。callerがcursor内の数字を演算する契約ではない。

unknown executionはtarget_unavailable、不正/別対象/stream不一致cursorはcursor_expired、旧encodingはcursor_encoding_mismatch、原文回収・欠落・generation変更はoutput_expired。回収時はexecution/gen/最古の残存sequenceをdetailsへ返し、黙って末尾へ飛ばさない。存在する最古位置が0以外ならstateful decoderを復元できないため初回読取りも明示失敗する。欠落を空文字の成功としない。

**採用したP1型修正:** ShellReadArgs.encodingはOption<String>。CLIの `--encoding` 未指定はexecutionの現在保存値を使用し、Someで明示した時だけDBのoutput_encodingを更新する。以前のString/UTF-8既定は、CP932で起動したexecutionを引数省略で勝手にUTF-8へ変更するため修正した。HistoryGetやshellツール起動のUTF-8既定とは別。再decodeは旧cursorを外して要求し、過去のrawは変えない。

## decoderとbounded取得

一要求はsaved_sequenceのsnapshot上限まで、最大65,536 raw bytesを読む。DB専用workerへ一chunkの `substr(BLOB,offset,count)` だけを依頼し、DBから一度に渡す本文は最大64KiB。長い読取りtransactionや全rawコピーは持たない。page間でTokioへyieldする。

stdout/stderrは独立したencoding_rs Decoder。UTF-8、CP932/Shift_JIS、ISO-2022-JP等のstateful encoding、明示UTF-16LE/BEを扱う。文字途中はdecoderへ保留し、raw cursorを進めてもlive/page端で代替文字を出さない。真のterminal状態かつsnapshot末尾でのみ両decoderをlast=trueでflushする。cursorのeof_flushedにより再取得で同じ代替文字を二度返さない。BOMを使ったencodingの自動切替はせず、指定encodingで処理する。

stateless CLI cursorはdecoder内部を非公開形式のままserializeしない。保持された先頭からcursorまで短いDB pageで再decodeし、prefix表示だけを捨ててdecoder状態を復元する。この経路はO(prefix) CPU/readで、長いcursor連続取得では累積コストが増える。checkpoint cacheはV7の結果を見て追加可能だが、正しさの代わりにUTF-8決め打ちやstream結合を採らない。

索引と大量走査には `stream(execution_id)` / `stream_selected(execution_id,selection)` を使う。返値OutputStreamReaderは固定snapshotとdecoderを保持し、`next_page(limit_bytes)` がDecodedOutputPage{text,raw_bytes,decode_errors,done}を返す。一方向の一回走査でprefixを読み直さない。encoding()/retention_generation()/saved_sequence()でlate adoption用のsource witnessを取得する。doneは固定snapshot末尾で、live実行の終了を意味しない。terminal snapshotだけEOFをflushする。

`excerpt(execution_id,limit_bytes)` も一回走査し、保存済みencodingで解釈したUnicode安全なhead/tailと省略markerだけを保持する。markerを含む返却UTF-8 byte数をlimit以内に収め、DB worker内で全rawをscanして巨大な文字列を返す方式を避ける。

## GCと取消し

payload取得前に同DB transactionでshell_output runtime pinを作る。resource_idはexecution ID、generationは原文版、instance_idは現在owner。raw GCはこのpinを尊重する。meta参照とpin登録の間にGCを挟まない。各raw pageでもgenerationを照合し、契約を破る削除があれば失敗する。

通常read/excerptは結果を返す前にpinを解除する。streamはsnapshot末尾で解除し、途中dropもDBへ解除をqueueする。pin guardをDB admissionより先に作るため、INSERTが受付された後のcaller取消しでもDELETEが同じDB laneへ後続し、pinを置き去りにしない。runtime終了で解除できなかったpinは起動時instance整理の対象で、永続保持理由へ流用しない。

wait_position(connection,execution_id,cursor)は同期の短いDB helper。対象/版/encoding/stream/position/保持を検査し、cursor後の選択raw byte又はtarget terminalで成立する。waitの状態確認・登録と同じControl順序で呼ぶ。改行完成やUnicode文字完成を条件にしない。

## 検証

`tests/output.rs` は実bundled SQLite/DbWorkerを使用する。UTF-8途中byteのlive/page跨ぎとwait位置、EOF replacement一回、CP932再decode/raw不変、stdout/stderr分離、ISO-2022-JP状態、UTF-16 surrogate、pin中GC抑止と完了/drop解除、snapshot watermark、原文欠落/世代/対象拒否、Unicode head/tailのbyte上限を検査する。

2026-10-24 macOSで8件とClippyを実ファイル参照harnessで成功確認。並行するindex.rsの編集中は製品全体compileが未完了だったため、protocol/storage/outputのみを実ソースのまま参照しstubを置かなかった。製品全体の統合チェックはindex統合後に実施する。

index統合後、同日macOSのrepo本体で `cargo test --test output --test cli --test protocol` を実行し26件成功（output8/CLI8/protocol10）を確認した。raw output harnessのClippyも `-D warnings` で成功。全体のControl/compaction統合中のwarningは別作業で解消する。

raw GCの `app_meta.raw_tombstone.<execution_id>` がdeleting/deletedのどちらでも、残存chunkを読める原文と扱わない。read/stream/waitは保持期限切れを明示し、excerptは理由付き期限切れ表示を返す。2026-10-24にrepo本体の `cargo test --test frontend --test ui --test output --test cli` が37件成功（output9、frontend4、UI16、CLI8）。追加のraw試験は削除marker後もchunkが残る中間状態を検証した。

## ReadPinの登録責務（R1-4）

stream/read/excerptの同一runtime_pin INSERTをReadPinのprivate insert_txへ集める。既存id/instanceの値だけをDB closureへcloneし、RAII owner自体はclone/moveしない。snapshot→raw/cursor/encoding検証→pin登録→同TXcommitの位置と、その後の明示release/Drop時deleteを維持する。新しいpin wrapperやworkerを追加せず、取消し/DB error/EOF/stream dropでの解放境界も変えない。
