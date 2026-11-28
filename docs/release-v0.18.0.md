# Shellboy Core v0.18.0

## Interface changes

- **Read saved artifacts (#445, #458):** `artifact read` returns one regular file from an exact artifact version as base64. A `file:` artifact reads the saved file itself; a `shellboy-app:` folder takes a relative `--path` within that folder. Remote and network-share paths are not supported. On Windows, the reader normalizes eligible local verbatim drive paths before opening the file. The stored artifact URI stays unchanged.
- **Find artifacts by current URI scheme (#461):** `artifact list --scheme SCHEME` searches live, non-archived Threads; add `--thread` to scope the filter to one Thread. Continue with the same filters whenever `next_cursor` is present, even after an empty page.
- **Register relative artifact paths (#460):** `artifact add` and `artifact update` drop `.` components when joining relative file paths or relative `shellboy-app:` folder paths to the working directory. The join preserves `..` and trailing separators, does not resolve symlinks, and leaves absolute paths and other explicit URIs unchanged.
- **Named service endpoints (#462):** Give a service an optional name unique within its Thread with `service add --name NAME`. Rename with `service set --service ID --name NAME`, or clear with `--name ''`. Authenticated local callers can use `service endpoint --service ID` or `service endpoint --thread THREAD --name NAME` to read the current running execution's port and secret token. Lookup does not start the service; ordinary list and get results do not expose credentials. Do not log or save the endpoint response.
- **Watch message delivery and shell placement (#423, #437):** `message get` can report why a message is held, including the exact execution ID when it is blocked by a foreground shell. Use `wait` with `message_delivered` to observe application. Running shell reads report `foreground`; moving a shell to the background changes that flag without stopping the shell or attributing the action to a particular person.
- **Send at a safe boundary (#423):** New sends and asks no longer accept `--deliver now`. Default delivery uses the next safe boundary; `--deliver complete` is still available. An ordinary send does not release a foreground shell wait. Use `shell background` on the exact execution if you want the Turn to continue. Saved metadata and exact replay of older operations keep their original meaning.
- **Keep reply duties open while asking back (#433, #434, #438):** An owing Thread that asks a follow-up can keep the original duty open as `awaiting_own_ask`. The saved blocking Ask identifies what it is waiting for. Saved history also shows the recorded sender of named Asks from Threads when available.
- **Observe exact `exec` outcomes (#441, #446):** Reply mode waits while the duty is open. Each observation uses one owner wait for the remaining deadline and reads its outcome from that wait's snapshot. Timeout or interruption exits with code 5 and does not stop the target. `recovery_wait` exits with code 4; JSON or schema mismatch after repairs exits with code 3.
- **Inspect work and recover safely (#311, #421, #431, #444, #452):** Start reading near the end of saved shell output with `shell read --tail`, then continue using its cursor. `operation get` has a typed receipt result; check it before resending uncertain input. A Run still stopping can report a saved stop-settlement failure in `run get` and in work `status`; an accepted stop is not proof of settlement. A shell list item without saved command text omits `command_characters`, and `run_id` can be null.
- **Schedule commands and notices (#449):** Supply the scheduled program as one explicit command. Recurring schedules can opt into `--notify skipped`: the owning Thread receives a notice when an occurrence is skipped because the previous execution is still active. Notices are off by default.

## Terminal UI

- **More room for the conversation:** Empty work and held-input sections no longer take up conversation rows. Markdown rendering retries for omitted content stay within the loaded history window (#432).

<!-- #311: Add only the Core changes that land on main before the tag. -->

## Guides and reliability

- **Shell starts account for available descriptors (#450 item 3):** On Unix, admission checks live descriptor use, the cost of starting the shell, and headroom for owner IPC. A refused start reports `descriptor_budget` without launching the command or retrying it automatically. This applies to Run shells, services, and scheduled work.
- **Instruction snapshots (#450 item 1):** Consecutive Turns reuse an unchanged instruction snapshot. History collection can reclaim snapshots no Turn references.
- **Recover receipt capacity without a restart (#450 item 2):** When receipt space remains exhausted after reclamation and eligible current-epoch receipts exist, the owner can rotate its operation epoch. The triggering request is refused without being applied; older completed receipts become eligible for ordinary retention. Saved receipts keep replaying their saved outcomes while retained, and unknown old-epoch IDs cannot execute.
- **Collect old scheduled and service executions (#450 item 4, #451):** Raw output can age out without an explicit collect, even below the size cap. Maintenance removes orphaned raw-deletion markers and eligible superseded schedule and service executions after raw collection, without deleting the latest execution from the definition.
- **Retire stale pins and interrupted saves (#450 item 7):** Maintenance reconciles runtime pins when their work ends, including on failure or cancellation. At startup, the owner removes recognized temporary images left by interrupted saves, without touching unrelated temporary files.
- **More reliable regression tests (#442, #447, #454):** Tests check saved outcomes and public reads instead of private bookkeeping, and use controlled observations for deadline and shutdown cases. These are test changes, not new runtime behavior.
- **Guides and examples (#438, #439, #456):** The README includes a terminal demo. The guides explain when to delegate with an Ask, how to reuse project procedures and existing skills or MCP servers, and how to recover an uncertain result over IPC.

<!-- #465: Add only the Core changes that land on main before the tag. -->
<!-- #469: Add only the Core changes that land on main before the tag. -->

## Upgrade notes

- **Backup and compatibility:** Back up the entire data directory before starting the new owner. Upgrade all components that connect to the owner at the same time. Database migrations 66 through 71 cover Run stop diagnostics, open reply duties, schedule notices, instruction-snapshot retention, artifact scheme indexing, and optional service names.
- **Protocol: 36 -> 50.** Versions 37-40 cover delivery observation, the exact Background result, the operation receipt result, and the shell output tail. Versions 41-44 remove immediate delivery and add Run stop diagnostics, shell foreground placement, and the open ask-back state. Versions 45-47 update shell list fields, schedule notices, and artifact content reads. Version 48 adds artifact scheme filtering; 49 adds named local service endpoints; 50 adds the operation epoch.

- **Operation IDs (protocol 50):** When minting operation IDs over RPC, read `operation_epoch` from `Welcome` or `status`, rather than using `instance_id`. A refusal with `storage_limit`, reason `operation_epoch_rolled`, and `request_may_have_been_applied: false` means the request did not execute. A new attempt needs a fresh ID in the new epoch; reusing the refused ID returns `operation_expired`. This does not authorize retrying an uncertain outcome.

| Surface | Before | After |
| --- | --- | --- |
| New Send and Ask delivery | `--deliver now` / `deliver: "now"` accepted | Not accepted; use default safe-boundary delivery or `complete` |
| Open reply duties | `pending` | `pending` or `awaiting_own_ask` |
| Shell list without saved command text | `command_characters: null` | `command_characters` omitted; do not interpret omission as zero |
| `schedule.set` | Description required | Supply description, `notify`, or both |

---

## 日本語

### インターフェースの変更

- **保存済み成果物の読み取り (#445, #458):** `artifact read` は指定した版の通常ファイルを1つ、Base64で返します。`file:` は保存したファイル自体を、`shellboy-app:` はフォルダー内の相対 `--path` を読み取ります。リモートパスとネットワーク共有は対象外です。Windowsのローカルドライブの対象となる拡張パスは、読み取り時に正規化します。保存済みURIは変更しません。
- **成果物をURIスキームで検索 (#461):** `artifact list --scheme SCHEME` は削除・アーカイブされていないThreadを横断して検索します。`--thread` を加えると対象を1つに絞れます。結果が空でも `next_cursor` があれば同じ条件で続けてください。
- **成果物の相対パス (#460):** `artifact add` と `artifact update` は、相対ファイルパスや相対 `shellboy-app:` フォルダーパスを作業ディレクトリに結合するとき、`.` を取り除きます。`..` と末尾の区切りは残し、シンボリックリンクは解決しません。絶対パスやそれ以外の明示的なURIは変更しません。
- **名前付きサービスの接続先 (#462):** `service add --name NAME` でThread内に固有の名前を付けられます。`service set --service ID --name NAME` で変更し、`--name ''` で解除できます。認証済みのローカル呼び出しでは、`service endpoint --service ID` または `service endpoint --thread THREAD --name NAME` で実行中のポートと秘密トークンを取得できます。参照だけでは起動せず、通常の一覧や詳細には認証情報を含めません。応答をログに出したり保存したりしないでください。
- **メッセージの配達とシェルの配置 (#423, #437):** `message get` は保留の理由を返します。フォアグラウンドのシェルが原因なら、該当する実行IDも返します。適用は `wait` の `message_delivered` で確認できます。実行中のシェルの `foreground` はバックグラウンドへ移すと変わりますが、シェルは停止せず、操作者も記録しません。
- **安全な境界での配達 (#423):** 新しい送信・質問で `--deliver now` は使えません。既定では次の安全な境界まで待ち、`--deliver complete` も引き続き使えます。通常の送信ではフォアグラウンドのシェル待機は解除されません。Turnを進めるには対象の実行を `shell background` で移してください。過去の保存データと同じ操作IDによる再照会の意味は変わりません。
- **聞き返し中の返信義務 (#433, #434, #438):** 返信義務のあるThreadが聞き返した場合、元の義務を `awaiting_own_ask` として開いたままにできます。待機先は保存された質問で確認でき、送信元の記録がある名前付きの質問では履歴にも表示されます。
- **`exec` の結果確認 (#441, #446):** 返信モードでは義務が開いている間、残り時間を使う1回の待機とその結果から状態を確認します。タイムアウトや中断は終了コード5で、対象は停止しません。`recovery_wait` は4、修復後もJSONやスキーマが一致しない場合は3です。
- **作業の確認と復旧 (#311, #421, #431, #444, #452):** `shell read --tail` で保存済みシェル出力の末尾付近から読み、カーソルで続けられます。入力結果が不明なら、再送前に `operation get` の型付き結果を確認してください。停止処理の失敗は `run get` や作業の `status` で確認できます。停止要求の受理は停止完了を意味しません。保存済みコマンドがないシェルでは `command_characters` が省略され、`run_id` が `null` の場合もあります。
- **スケジュールのコマンドと通知 (#449):** 実行するプログラムは1つの明示的なコマンドとして指定します。定期実行に `--notify skipped` を指定すると、前回の実行が続いているためにスキップしたとき、所有Threadに通知します。既定は通知なしです。

### ターミナルUI

- **会話の表示領域 (#432):** 作業や保留入力の欄が空なら行を取りません。省略されたMarkdownを再描画するときも、読み込んだ履歴の範囲を超えません。

### ガイドと信頼性

- **シェル開始時のファイル記述子確認 (#450 item 3):** Unixでは使用中の記述子、起動に必要な数、IPC用の余裕を確認します。不足時は `descriptor_budget` を返し、起動や自動再試行はしません。Runのシェル、サービス、スケジュールに適用します。
- **指示スナップショット (#450 item 1):** 指示が変わらない連続したTurnはスナップショットを再利用します。どのTurnからも参照されなくなったものは履歴の回収対象になります。
- **再起動せずに操作レシートの容量を回復 (#450 item 2):** 回収しても容量が不足し、現エポックに回収可能な完了済みレシートがある場合、オーナーは操作エポックを更新できます。きっかけとなった要求は適用せずに拒否し、以前のエポックの完了済みレシートは通常の保持ルールに従います。保存中の結果は引き続き再照会でき、以前の未登録IDは実行できません。
- **古いスケジュール・サービス実行の回収 (#450 item 4, #451):** サイズ上限未満でも、生の出力は明示的な回収操作なしで期限切れになります。保守処理は孤立した削除マーカーや、出力回収済みで条件を満たす古い実行を削除します。定義が示す最新実行は残します。
- **ピンと中断された保存の後始末 (#450 item 7):** 作業の完了・失敗・キャンセル後に不要な実行時ピンを整理します。起動時には中断された保存が残した所定の一時画像を削除し、無関係な一時ファイルには触れません。
- **回帰テストとガイド (#442, #447, #454, #438, #439, #456):** テストは保存された結果や公開の読み取りで確認し、期限や終了時の観測条件を制御します。READMEにターミナルの実演を追加し、ガイドでは質問による委任、既存の手順やスキル・MCPサーバーの活用、結果が不明な場合の復旧を説明します。

### アップグレード

- **バックアップと互換性:** 起動前にデータディレクトリ全体をバックアップし、接続するすべてのコンポーネントを同時に更新してください。DBマイグレーション66〜71は、Run停止診断、開いた返信義務、スケジュール通知、指示スナップショットの保持、成果物のURIスキーム索引、サービス名に対応します。
- **プロトコル: 36 → 50。** 37〜40は配達の観測、バックグラウンド移動の結果、操作レシート、シェル出力の末尾読み取りに対応します。41〜44では即時配達を廃止し、停止診断、シェルの配置、聞き返し中の返信義務を追加します。45〜47はシェル一覧、スケジュール通知、成果物の読み取りを更新し、48は成果物のスキーム検索、49は名前付きサービスの接続先、50は操作エポックを追加します。
- **操作ID (プロトコル50):** RPCでIDを生成する場合、`instance_id` ではなく `Welcome` または `status` の `operation_epoch` を使います。`storage_limit` の理由が `operation_epoch_rolled` で `request_may_have_been_applied: false` のとき、その要求は未実行です。試し直す場合は新しいエポックの新しいIDを使います。拒否されたIDを再使用すると `operation_expired` になります。結果が不明な操作も同様に試し直せるという意味ではありません。
- **その他の変更:** 返信義務には `pending` に加えて `awaiting_own_ask` があります。`command_characters` の省略を0と解釈しないでください。`schedule.set` には説明と `notify` のいずれか、または両方を指定できます。
