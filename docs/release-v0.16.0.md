- Core: `v0.15.0` → `v0.16.0` (main source `137fd2e60a7ca0e70b758c75b5b7de131f2ce263`)

## 日本語

### Core

- **シェルの待機と出力の読み取り（#379）:** `shell_ended` は Run 所有の実行だけでなく、同じ Thread に属するスケジュールやサービスの実行も確認できます。出力のローテーションでカーソルが無効になった場合は、`shell read` をカーソルなしで再開し、`oldest_sequence` と保持範囲を確認してください。失われたバイトは復元されません。`shell_output` で待機した後は読み取って得た新しいカーソルを次の待機条件に渡します。`--after` に渡す待機カーソルは `thread_settled` 用です。CLI ヘルプやガイドでこの違い、依存関係によって待機がタイムアウトし得ること、ドットを含む設定名の引用方法を明確にしました。
- **スケジュール、サービス、保留入力（#380）:** owner が終了を受け付けた後は、開始前の予約済みスケジュールやサービスも起動しません。開始前のキャンセルは `owner_shutdown` として記録され、サービスの無効化や失敗通知を引き起こしません。期限を過ぎた無効な一回限りのスケジュールは `schedule enable` が `invalid_state` を返し、無効のままです。新しい時刻には `schedule add` を使います。直近の実行は時計が巻き戻っても登録順で選ばれます。`remaining_held` は再試行で解放できる保留入力だけを数えます。TUI の使われていない状態フッターを削除し、コンパクション表示は維持しました。
- **状態と Thread の一覧（#381）:** 既定の `status` は終了済みの履歴を走査せず、未完了の作業を索引から読みます。`--all` や終了状態での絞り込みは、引き続き一致するキーを走査します。ページの指紋は正規化した条件と返された永続行を表し、読み取り時のシェル観測や他ページは含みません。`thread list --state` は最大200候補と続きの確認用1件を読み、結果が空でも `next_cursor` があれば同じ条件で続けられます。一覧の保留返信総数と Thread 宛て件数は16まで正確で、17と対応する exact フラグの `false` は「17以上」です。
- **履歴の読み取り（#381）:** `history window` は1ノードあたり保存元JSONを最大1 MiBまで読み、元データを読まないときの `payload_chars` は `null` になり得ます。直接の実行リンクは最大8件で、`execution_count_exact: false` の件数は下限です。`history list` の件数の意味は変更していません。`history get` は `incomplete_response` と `model_stream_observation` の保存済みJSONをUTF-8境界で範囲読み取りします。通常のデコード済み本文と要約の parts はこの最適化の対象外です。

- **Thread 検索・抜粋（#381）:** 更新順の検索・タグ・cwd・状態の絞り込みも最大200候補と先読み1件です。空ページでも `next_cursor` があれば同じ条件で続け、タグの指定順は問いません。抜粋は選択履歴の最新32ノードまでを調べ、未調査の古い履歴が残るときは `excerpt_complete: false` で区別します。
- **購入済み埋め込みと RAG（#383）:** 通常の同期・期限切れ・回収で検索対象から外れても、保存済みベクトルは明示的に削除するまで保持します。同一テキストと同一プロファイルなら再索引時に再利用でき、検索時も同じ接続条件ならクエリ埋め込みを共有します。重複した正規パスの新規登録を拒否し、同期停止後に未検証の旧版を返す検索結果は `stale` と表示します。保持ベクトルの論理バイト総量は一覧の打ち切りに左右されません。
- **IPC と添付準備（#384）:** IPC 応答の書き込みと応答後の切断待ちに既存の期限を適用します。受理済みの操作は取り消しません。添付は本文を読む前に参照単位で raw バイトを予約し、保護対象を守りながら古い未保護添付から除外します。Base64 や JSON を含む総メモリの上限ではありません。
- **接続診断（#386）:** DNS・到達不能・接続・TLS・プロキシ CONNECT など観測できた原因を値を伏せて表示し、先にネットワークや VPN の確認を案内します。`connection test` は既定でモデル一覧の GET のみです。明示した `--post-model MODEL` は再試行なしの有料の合成会話 POST を追加し、結果を `conversation` に分けて返します。
- **秘密と診断（#388）:** 採用された追加 HTTP ヘッダーの値も失敗診断で秘匿し、失敗・中断時の診断を保存前に処理します。会話として採用できる出力、既存履歴、損失のないエクスポートは書き換えません。Windows では資格情報と JSONL 一時ファイルを非公開に作成し、資格情報の ACL が広い、または確認できないときは警告します。Unix では開いた資格情報ファイルの所有者や権限に問題があれば `config get` で通知し、自動修正や読み取り拒否はしません。シェルの生出力やクラッシュログは共有前に確認してください。
- **添付の再送（#382）:** 再送では元の添付位置を維持します。ファイルが変わったり利用できなくなったりすると、その時点の内容や状態を反映します。添付の採用済み通知は現在の配信を示すものではありません。
- **保留中の Revisit と入力（#391）:** Run の完了を待つ Revisit が、現在の Turn に届く後続の入力を妨げなくなりました。

### アップグレード

- **DB:** 更新前にデータディレクトリ全体をバックアップしてください。owner DB は **62 → 64**。`0063_thread_reply_summary` と `0064_thread_cwd_partitions`（#381）は索引を追加し、保存本文は変更しません。独立 RAG DB は **4 → 5**（#383）。古い DB は `rag init` で更新できます。埋め込み API は呼び出しません。読み取り専用操作は自動更新しません。
- **プロトコル:** **18 → 22**。19 は返信件数・状態一覧、20 は履歴ウィンドウ、21 は Thread 検索・抜粋と接続診断、22 は資格情報ファイルの警告です。owner、CLI、RPC クライアントは同じプロトコルのものに揃えてください。旧 `status` と旧状態フィルター用 `thread list` のカーソルは使えないため、カーソルなしで読み直してください。
- **CLI・設定:** `connection test --post-model MODEL` を追加。コマンド・既存フラグ・設定キーの削除や改名、新規設定キーはありません。Unix の `config get` に値を含まない `credentials_file.warning` が追加されます。`wait --after` と `config set --set` の構文は変更していません。
- **ロールバック:** 逆マイグレーションはありません。旧版に戻す場合は、更新前の owner DB と独立 RAG DB のバックアップを使ってください。回収済みのシェル出力は読み直しても戻りません。

## English

### Core

- **Shell waits and output (#379):** `shell_ended` can observe a saved schedule or service execution attached to a Thread, not just Run-owned work. If output rotation invalidates a cursor, restart `shell read` without it and check `oldest_sequence` and the retained offsets; missing bytes cannot be recovered. After a `shell_output` wait, put the new read cursor in the next condition, not in `--after`; the wait cursor in `--after` is for `thread_settled`. Help and guides clarify wait dependencies and show how to quote dotted config names.
- **Schedules, services, and held input (#380):** After the owner accepts shutdown, even reserved schedule and service launches cannot pass the start gate. A pre-launch cancellation is recorded as `owner_shutdown` without disabling the service or sending a service failure notice. `schedule enable` returns `invalid_state` for an expired disabled one-shot and leaves it disabled; use `schedule add` for a new time. The latest execution follows insertion order even if the clock rolls back. `remaining_held` counts only input eligible for release on retry. The TUI drops an obsolete activity footer and retains real compaction activity.
- **Status and Thread lists (#381):** Default `status` reads pending work through indexes instead of scanning terminal history; `--all` and terminal-state filters still walk matching keys. The page fingerprint covers normalized filters and returned durable rows, not other pages or read-time shell observations. `thread list --state` scans up to 200 candidates plus a lookahead. An empty page with `next_cursor` can be continued with unchanged filters. Total and Thread-directed pending replies are exact through 16; 17 with the corresponding exactness flag false means at least 17.
- **History reads (#381):** `history window` reads at most 1 MiB of saved source JSON per node; `payload_chars` may be null when source is not loaded. It returns up to eight direct execution links per node; a count with `execution_count_exact: false` is a lower bound. `history list` count semantics are unchanged. `history get` now reads saved JSON byte ranges for `incomplete_response` and `model_stream_observation`, ending pages on UTF-8 boundaries. Ordinary decoded text and summary parts are not on that byte-range path.

- **Thread search and excerpts (#381):** Updated-order query, tag, cwd and state filters scan up to 200 candidates plus one lookahead. Continue even an empty page if it has `next_cursor`, using unchanged filters. Tag order no longer matters. Excerpts look back at most 32 selected nodes; `excerpt_complete: false` means older nodes remain unexamined.
- **Paid embeddings and RAG (#383):** Ordinary sync, expiry and collection may remove data from search but retain saved vectors until explicit removal. Reindexing identical text under the same profile can reuse vectors; queries can share an embedding across scopes with the same provider request identity. New duplicate canonical roots are refused, and search marks unverified older revisions `stale` after a paused sync. Retained logical vector bytes are counted even if the space list is truncated.
- **IPC and attachment preparation (#384):** IPC reply writes and the wait for disconnect after a reply use the existing deadline without cancelling accepted operations. Attachment preparation reserves raw bytes per reference before reading content and excludes the oldest unprotected references first. This is not a cap on total Base64 or JSON memory.
- **Connection diagnostics (#386):** Reports available DNS, reachability, connection, TLS and proxy CONNECT causes without secret values, and suggests checking the network or VPN first. By default `connection test` only sends the model-catalog GET. Explicit `--post-model MODEL` adds one paid synthetic conversation POST with no retries and reports it separately in `conversation`.
- **Secrets and diagnostics (#388):** Adopted extra HTTP header values are also protected in failure diagnostics, and failed/interrupted diagnostics are scrubbed before storage. Adoptable conversation output, existing history, and lossless exports are not rewritten. Windows creates private credential and JSONL temporary files and warns if a credential ACL is broad or cannot be verified. On Unix, `config get` warns about unexpected credential-file ownership or broad modes without changing permissions or refusing reads. Inspect raw shell output and crash logs before sharing.
- **Attachment replay (#382):** Resent attachments keep their original positions. Changed or unavailable files are reflected at the next preparation. A replayed notice is not proof of current delivery.
- **Held Revisit input (#391):** A Revisit waiting for Run completion no longer blocks later input to the active Turn.

### Upgrade notes

- **Database:** Back up the entire data directory before upgrading. Owner SQLite schema **62 → 64**. `0063_thread_reply_summary` and `0064_thread_cwd_partitions` (#381) add indexes without rewriting saved bodies. Standalone RAG schema **4 → 5** (#383); upgrade an old store with `rag init`, which makes no embedding request. Read-only operations do not migrate it.
- **Protocol:** **18 → 22**. Version 19 covers reply counts and state paging, 20 history windows, 21 Thread search/excerpts and connection diagnostics, and 22 the credential-file warning. Upgrade owner, CLI and RPC clients together. Old `status` cursors and old state-filtered `thread list` cursors expire; restart those reads without a cursor.
- **CLI and configuration:** Adds `connection test --post-model MODEL`. No command, existing flag or config key was removed or renamed, and no config key was added. `config get` adds the value-free `credentials_file.warning` field for the Unix advisory. The `wait --after` and `config set --set` syntax is unchanged.
- **Rollback:** No reverse migrations. To use an older release, restore pre-upgrade backups of the owner and standalone RAG databases. Rereading cannot recover reclaimed shell output bytes.
