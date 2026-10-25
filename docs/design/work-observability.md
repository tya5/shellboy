# 作業状態と操作への到達性

## 採用仕様

2026-10-25のユーザー採用を受け、CLIの最短例・失敗回復、現在の作業状態、依頼と実行結果の関連付けを改善する。原文のRun所有・Thread選択経路・操作受付境界を変更しない。新しいjob engine、状態表、migration、モデルツール、価格カタログは追加しない。

1. 関連する`--help`からshell get/read、wait、委譲、原文追跡の実行可能な例とガイドIDへ進める。失敗時は対象照会・原文取得・操作受付照会を先に案内する。受付不明のmutationを新しいIDで再送する案内は禁止する。
2. `shellboy status`はowner内の保存済みRun・Turn・shell・未適用message・model attemptの作業一覧を返す。Threadの現在設定とattemptの実要求モデルを区別する。既定は非終端、`--all`で終端も含める。`--thread`は完全一致、`--run`はそのRunへの所属、`--since`は保存された生成/開始/終了/更新時刻の最大値（inclusive）を対象とする。状態変化時刻やプロセス生存は推測しない。状態フィルタは保存されたnative stateの完全一致とする。
3. 履歴一覧からnodeの生成Thread/Run/Turn、親Run、関連shell executionと原文へ進める。Thread filterは現在の採用経路を示し、生成元Threadと混同しない。cwdはshell実行時の保存値とThreadの現在値を区別する。短い要約は既存本文の抜粋であり、関連付けの根拠には使用しない。絞り込みは保存済みID・時刻・状態を使う。

TUIでも同じstatus結果を構造化して表示し、対象IDから既存get/readへ進む。通常の会話欄へ管理JSONを流さず、UIスレッドのDB・I/Oを増やさない。

## 実装設計

- `status`は単一のDB worker読取transactionで作るprojection。既存getがoperation receiptを保存することを理由にstatusもmutation化せず、専用のread経路を使う。新しい永続状態は不要。
- 共通Thread情報と、kind・ID・native state・所属ID・記録時刻・既存照会へのtyped targetを持つflat itemを返す。Run所有辺とThread親子辺を別々に公開する。1ページ既定50・最大200。ページ間は版・owner instance・durable_only scope・正規化フィルタ・初回watermark・最後のsort keyへcursorを束縛する。projection fingerprintは公開の観測識別値として保持するがcursorの失効条件にはしない。観測時刻や動的経過時間はfingerprintから除く。
- 範囲は`durable_only`。独立RAG store、現在の検索リクエスト、OSのPID生存、driver内queueを同一snapshotと称さない。索引・RAG・検索の状態は既存照会へ案内し、取得していないものをidleと表示しない。
- statusには本文、shell command全文、origin/sender token、環境変数、input manifest、error JSON全文を含めない。短い理由にも上限を置く。
- 履歴は既存HistoryListArgs/HistorySnippetを後方互換なoptional metadata/filterで拡張する。shellへのリンクは保存済みtool/Turn関係から作り、command文字列や受付JSONから推測しない。cursorはfilterに束縛する。
- CLI例と回復hintは既存clap/API errorを拡張する。追加のガイド検索機構は作らない。docsは操作手順、historyは会話根拠、RAGは明示資料横断という既存の区別を案内する。
- CLI/TUIは同じowner operationを使用する。TUIは既存管理結果・詳細表示を再利用し、スクロール位置と下書きを保持する。

## 受入条件

### 境界の具体化

`status --state`は複数指定、`history list --status`は関連shellの単一native状態を指定する。履歴node自体にRunやshellと共通のライフサイクルは作らない。各nodeの実行リンクは8件までで総数と省略を明記し、残りは`status --run RUN --all`のページから取得する。TUIの`/status`は人間向けの全Thread一覧、`/status thread`は保存済み現在Thread、`/status all`は全Threadの終端も含める。未保存Threadの限定要求を全体へ読み替えない。

管理結果の参照・再取得は発行世代を持ち、新しい要求の後で古い応答が画面を置き換えない。statusには対応receiptがないので、観測要求のoperation IDを受付参照先として表示しない。shell内部operation IDもexecution UUIDであり、owner receiptへのリンクに流用しない。

匿名fixtureで親子RunとThreadの違い、background shell、未割当message、attemptモデル、終端/失敗、時刻・状態・所属filter、ページ継続と失効、依頼から回答・shellへの実IDリンクを検証する。status照会がDBを変更しないこと、秘密情報を返さないことも確認する。CLI helpの例と次操作を解析・実行検証し、TUI表示と詳細導線をテストする。標準チェック、release実行での操作確認を完了してからローカル実体へ反映する。既定データと稼働中ownerには変更を加えない。

## P10 / R3-7: 作業一覧の弱ページングと投影責務

原CLI Contract §6を適用する。以前の全projection fingerprint照合は、背景shellやattemptの1件の更新だけで後続ページを失効させ、弱ページングより厳密な独自制約になっていたため撤回する。公開StatusSnapshot/WorkItemのfield・ID・target参照は全て維持し、fingerprintはその読取transactionでの対象投影の識別値として残す。

cursor v2はversion/instance/scope/正規化filterのSHA-256/最後のSortKey/初回watermarkを持つ。state最大32件のJSON escapeで有効cursorが8 KiB上限を越えないよう条件本文をtokenへ複製しない。既存v1 cursor、別instance/scope/filter、不正tokenはcursor_expiredで先頭から再取得する。limitはfilterに含めず1〜200の範囲でページ途中に変更可能。sourceの生成時刻（Run/Turn/messageのcreated_at、shell/attemptのstarted_at）の初回最大値をwatermarkとし、継続時にそれより後に生成された項目を除く。初回に既存の未来時刻も見落とさないよう、ホストの現在時刻ではなく保存されたsource時刻の上限を使う。時刻巻戻しやbackdateされた後挿入を厳密に識別するsnapshotではない。これを避けるための永続sequence/ID集合/長期transactionは追加しない。

並び順はactivity_at降順、kindとID昇順。activity_atは既存4時刻の最大値で、source生成時刻とは分ける。更新で状態filterや並び順が変わる場合の重複/見逃しを許容し、last項目の削除・状態変化・順序移動だけでは失効しない。新規項目を見る場合はcursorなしで更新する。状態変更のたびに全ページの再取得を強いない。

内部ではcheck_scope/decode_cursor/work_item/encode_cursorを分離し、WorkKindとSortKeyはprivate型にする。公開kindは同じ文字列を維持する。kind別のterminal/target判定を網羅し、activity_at・execution/message/attempt ID・targetsは同じrow投影で一度だけ導出する。cursor比較のために公開WorkItemを複製しない。Turn所属などの保存不整合はinvalid_stateで返し、推測参照やpanicを作らない。1transaction内の走査と最大201候補の保持、新state/table/workerなしという責務は維持する。
