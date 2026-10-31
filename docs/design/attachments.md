# 添付・clipboard・managed画像の境界

マルチモーダル添付、ハッシュ/RAM cache、送信選択、clipboard回収、非同期境界、およびsnapshot/CAS/pin規則を実装する。対象は `src/attachments.rs`、`src/attachments/clipboard.rs`、`tests/attachments.rs`。初期添付仕様は非同梱。

`AttachmentService` は共用DbWorker、固定Config、data-dir、instance ID、有限attachment workerと制御file workerを持つ。UIから直接呼ばず、Control/driverのasync側から使う。通常ファイルの実体をディスクへコピーしない。保存するのは絶対パス、入力種別、選択、元node、送信時hash/属性/状態。

`plan_add` はThread cwdに対して相対パスを解決し、現在の保護対象と追加分の送信可能rawサイズをworkerで確認する。同一パスを複数追加した場合も別要素として合計する。単一上限超過や読取り不能は後の準備で通知して会話継続するため、その実体を合計要求へ含めない。追加保護分だけで要求上限を超える場合は不採用。Controlは `add_tx` をoperation receiptのtransaction内で使い、planのselection revisionを再検査する。`list_tx` と `remove_tx` も同じ共通操作へ接続する。removeは選択をexcludedへ変えるだけで外部ファイルを削除しない。`AttachmentService::add` は最終source payloadに宣言済みのIDを登録する補助APIで、本文へ後付けしない。単体transactionを行い、CLI重複受付保証はControlのreceipt層が担う。最終本文のINSERTと登録の順序は[添付provenanceのM12境界](attachment-provenance.md)に従う。

`prepare(thread, cancel, force_reload)` は選択とrevisionをDBでsnapshot化する。managed画像には同じtransactionでruntime pinを張り、ready以外へpinを追加しない。読取りは有限workerの枠を得てから開始し、timeoutは枠待ちを含む。開始済みOS I/Oがtimeout/取消し後も残る場合、実closureがpermitとpinを持ち続ける。timeoutで見かけ上worker数を増やしたり、late結果を送信へ採用したりしない。

各ファイルは毎回statし、不在/権限/単一上限失敗を古いcacheで代用しない。属性が一致する場合だけRAMのBase64を再利用できる。cache missやforce reloadは同じ読取りbyte列からSHA-256とBase64を作る。hashのための二度目の全文readはしない。属性を保つ書換えは通常cacheが見逃してよいが、force reloadで再読込みできる。MIMEはmagicを優先し拡張子を補助に使い、不明バイナリはapplication/octet-stream。画像/ファイルの指定は維持し、形式変換や別API fallbackをしない。任意形式のモデル理解を保証しない。

同じ要求内の同パスは1回の実体readとArc<Encoded>を共有できるが、API入力要素とraw合計は重複したまま。全Thread共通のbyte LRUはBase64/hash/MIMEの保持bytesを管理する。上限0や単体が予算超過でも有効添付を拒否せず、今回だけ使って解放する。弱参照の観測表により、cacheから外れても送信側がpinしているEncodedの生存量を計上する。JSONの今回必要な組立bufferと保持cacheは別で、RAM全体がcache予算に等しいとは表示しない。

準備結果の `placements` はsource node／要素位置ごとにResponsesのinput_image/input_fileとinput_textメタデータを持つ。driverは履歴内の元の要素位置へ展開し、末尾へまとめて追加しない。`notices` は同じ通知の表示用一覧なので、driverは二重でモデル入力に追加しない。不在/単一上限等はunavailable manifest＋対象パス/理由のテキストとし、送信可能分を継続する。以前の成功送信hashと異なると変更通知を入れる。失敗したattemptのhashを比較基準へ採用しない。

合計が要求上限を超えると、過去の非保護添付をselected_order順に除外候補へする。新規保護分が現在の内容で上限を超えれば全要求を保留する。自動除外はまだDBへ確定せず、`adopt_prepared_tx` をAPI attempt受付transaction内で呼ぶ。ここでThread selection CAS、attempt/Turn generation、live Runを検査し、除外とattachment_send manifestを原子的に記録する。除外があればThread/model_attemptのselection revisionを更新し、新revisionを返す。driverは後続採用CASにこのrevisionを使う。容量が減ってもexcludedを自動復活させない。

正常completedのモデル応答をgeneration照合して採用する同じDB transactionで `mark_success_tx` を呼ぶ。completed/current generationを再検査し、同じThread・添付IDごとの送信済み比較hashと保護解除を原子的に確定する。途中切断・取消し・プロバイダー失敗で呼んでも更新しない。`PreparedAttachments` はHTTP attempt終了まで保持する。Drop時にpin解放をDbWorkerへ送り、runtime終了後なら次回startup cleanupが旧instance pinを除く。

clipboard画像は明示要求だけで取得する。`ClipboardAdapter` は有限8件queueと専用1 workerでOS APIを扱い、clipboardオブジェクトを保持してX11 ownershipの早期解放を避ける。timeout後も開始済みOS APIが残り得るが増員しない。取得RGBAは寸法/乗算overflow/128 MiB decoded上限を検査する。OS側で画像をdecodeする前のメモリ使用量まではarboard APIから制限できないことを未保証として区別する。

`save_clipboard_pixels` はcreating行＋pinを先に保存し、workerで透過RGBAをPNGへencode、private temp fileへ書きsync、同一data-dir内でrename後にreadyを保存する。失敗/取消しはfailedへ残す。`ManagedImage` 自身がdraft用pinを持つため、frontendは返却値を添付transaction完了か下書き破棄まで保持する。UIへpathだけ渡して返却値を即dropしない。通常ファイルの既存実体をこの機構でコピーしない。PNG用の直接依存はpng 0.18.1を採用し、不要な他codecを製品機能として追加しない。

`collect_managed` はactive selection（別Thread/forkを含む）とruntime pinを同じDB順序で検査してからdeletingを確定する。deleting以降は新しい選択/pinで復活できない。unlinkはworkerへ分け、成功後にdeleted、失敗ならdeletingを保持し再試行する。対象はmanaged_fileだけで、さらにpathがdata-dir/managed/clipboard/<id>.pngと一致することを確認する。通常外部ファイルはunlinkしない。managed pathを再添付した参照も同じmanaged lifecycleとして保護する。retentionは承認済みの7日/256 MiBをConfigから使い、protected分が超過しても保護を破らない。論理残量と物理空きの回復を混同しない。

`restart_cleanup` はowner lock取得後・新規受付前だけ呼ぶ。旧instance pinを除き、中断creatingをfailedにしてdeleting/failedの掃除を再開する。temp名は同じmanaged IDだけを削除する。プロセス異常終了を副作用再開へ置換しない。

テキストcopyはOS API成功ならOsCopied、OSC52なら1個のterminal request bytesを返す。UI外で原文を取得し、罫線や描画折返しを混ぜない。OSC52は上限を超えたとき分割上書きせずエラー。autoはSSH環境変数を薄いヒントに使うだけで、設定でOS/OSC52を指定できる。OSC52到達を確認済みコピーと表示しない。clipboardの定期読取りやテキストpasteの横取りは行わない。

ローカル試験は変更/削除/同属性force reload、重複要素、成功hashのみ採用、保護上限/古い順除外/復活禁止、CAS rollback、cache0/LRU/pin観測、実worker timeout継続、PNG alpha、active/fork/read/send pinとGC、deletingからの再選択拒否、restart cleanupと外部非削除、OSC52上限を確認する。実OS clipboardと実proxyのファイル形式受入はこのfixtureで検証済みとはしない。

参照: [arboard 3.7.0 Clipboard API](https://docs.rs/arboard/3.7.0/arboard/struct.Clipboard.html)、[png 0.18.1 Encoder](https://docs.rs/png/0.18.1/png/struct.Encoder.html)。

## 圧縮採用とCLI添付の原文位置

「コンパクションで置換された範囲の添付実体は送信対象から外れる」という採用契約を具体化する。summary/spillの採用transactionで、置換する元nodeに属するactive添付を、そのThreadだけ `excluded / context_replacement` へ遷移する。添付ref・元node・外部ファイルを消さず、保護flagも外す。新しい選択経路を採用する `select_path` がThread selection revisionを一度増やすので、添付除外でCASを自己失敗させない。経路採用に失敗すれば除外と置換nodeも一緒にrollbackする。別forkの選択はその時点の状態を保つ。

CLI追加はshell／外部CLIとも専用の補助nodeを作る。保存済みAssistant call nodeへ参照を直接所属させず、operation IDと追加元を保持し、全tool結果が揃った安全境界で採用する。人間の受付messageとは別のdelivery台帳で受付／保留／反映／取消を追跡する。foreground shellを中断しないquiet wakeを使う。未採用nodeもGC保護し、forkは採用済みの元経路だけを引き継ぐ。

この節は2026-10-25の所属契約是正により旧call node所属案を置き換えた。API配置、更新通知、migration 4の既存参照回復、不明な所属の扱い、採用契約は [添付の所属・再送位置](attachment-provenance.md) を正とする。

## 指定消費と実体送信の別の事実（M9）

新しい添付指定の保護は、その指定のready/unavailable/excludedメタデータを実際にadmitした最初の正常完了要求で解除する。不在・読取り不能・単体上限によるunavailableでも、指定の状態文を消費した正常要求後は過去扱いへ進める。失敗/取消/世代不一致では解除しない。解除対象は同attemptのattachment_sendへ保存された指定に限定する。後で復活/縮小したファイルも、過去添付として通常の古い順の容量除外へ従う。

この消費は実体を送った事実とは別である。attachment_sendをsentへ進めhash比較基準に使うのはreadyだった実体だけで、unavailable/excludedは状態を維持し架空のhash/送信成功を保存しない。既存state/table/workerは増やさない。

## 置換除外の通知と原経路再選択（M10）

context_replacementで元sourceが現在のpath外にある添付は、実体を読まず、path・ID・source・除外理由のtrusted metadataだけを次の通常要求の安全境界へdeveloper通知として置く。既存PreparedPlacementに一時的なpath外通知flagを持たせ、manifestへ実際のAPI座標を記録する。元source本文や架空のsource message、実体hash/sent事実は生成しない。path外へ配置できるのはこのexcluded通知だけで、通常添付のprovenance不一致は従来どおり拒否する。

同Threadのcompleted attemptに既存attachment_send/excludedが保存されていれば通知を消費済みとし、失敗/取消attemptでは再通知する。activeからcontext除外へ遷移する既存規則に従い、先行する別理由のexcludedをactiveへ復活させる操作は追加しない。新たな通知台帳/永続state/workerは不要とする。別forkのselection・送信履歴は混ぜない。

元pathを明示的に再選択した場合は、context除外済みでも元sourceのattachment_ref位置へexcluded状態文を展開し、bindを失敗させない。実体を自動復活させず、必要なら従来どおりpathを新IDで再添付する。元source/ref/外部ファイルと共有forkは保持する。compactionがThread selectionを原子的に置換する境界は変更しない。

path外通知は既存一覧既定値と同じ最大50件の候補を一回に準備し、追加developer item全体のserialized UTF-8 JSONを既存本文取得上限64KiB以内にする。これは追加通知の上限であり要求全体の上限ではない。pathはUTF-8境界で最大4KiBへ省略し、省略flag・元pathのUTF-8 byte数/SHA256（ファイルhashではない）・既存attachment list導線を添える。単一の巨大pathでも通知でき、JSON escapingも予算に含める。未通知分はexcludedのまま既存ledgerにadmitせず、成功後の次の自然要求で続きを通知する。失敗時は同じ分を再通知し、自動要求/新queue/公開設定は追加しない。

旧版移行やGCで元nodeが既知のtombstoneになったpath外context除外指定は、通知のために元位置を捏造せず候補から外す。対象はexcluded/context_replacementかつ現在path外で、元nodeが実在しpayloadなしまたはdeleted_reasonありの場合に限定する。ref/selectionと既存取消deliveryは保持する。active/path内の欠落・重複ref、source IDなし、壊れたlive payloadを黙って除外する規則には広げない。

## Selection snapshotとcacheの所有（R2-6/R2-7）

12列の添付selectionはprivate SelectionRowでnamed decodeし、selection revisionのCAS、source/path/順序の検証、managed pin作成をsnapshot_selection_txへ移す。元SQLの列順/走査順/上限/validation順は維持し、呼出し側の同一transactionでsnapshotをcommitしてからfile workerへ進む。snapshotはrevision・Selected集合・pin IDsをまとめた所有結果であり、DBを持つRepositoryは追加しない。Cacheのbase64予算はservice生成時に固定しCacheが所有する。I/O中にcache mutexを保持する時間は増やさず、pin/force reload/LRU/生存object集計の契約を維持する。

## 準備期限とmanaged画像の保存確定（M13）

file_prepare_timeout_msは、DBから選択内容を取得した後、一回のprepare内のfile worker枠待ち＋実体読取りに共有する期限とする。DB取得時間まで含めた要求全体のdeadlineではない。同じpathの重複を一度だけ読む現在の順序を保ち、途中で期限に達すれば残りはunavailableとして同じ要求を継続する。各ファイルで期限を更新して総待機をファイル数倍へ延ばす案は採らない。開始済みI/Oは実終了まで既存permit/pinを持ち、timeoutで追加workerを起こさない。個別の再試行は次の明示要求で行い、準備に新しいtimer/設定/queueを足さない。

managed PNGはtemp本文のsync→rename→Unixの両親directoryのsyncを有限file worker内で行い、その成功後にDBのreadyを保存する。親dir同期失敗も既存failed経路へ返す。Windowsのstd rename経路は維持し、Unixのdirectory fsyncと同じ電源断保証は表明しない。実機の電源断耐久性はこの機能試験で検証済みと扱わない。

## Managed paths as ordinary attachment paths (#79)

Managed clipboard images retain the ordinary absolute `path` field in add/list
results. Per-request attachment metadata exposes that same path and any observed
unavailability. Managed and external files share preparation, current-byte reads,
per-Thread/ID successful-send hash baselines and change notices. Do not change
permissions, introduce exports or add a retention pin merely to display a path.
List joins managed lifecycle state through its existing unique path index on
finite presentation admission. After that reader closes, filesystem availability
is observed through the existing control-file admission, outside the DB writer. This is an observation, not a lease or future-read guarantee.
Existing selection/draft/fork/inflight reclamation rules remain unchanged.
F6 path presentation is delegated to the overlapping #41 display work; #79 does
not modify its rendering files in this first version.

Detached context exclusions retain their existing bounded path notices instead
of duplicating an unbounded path in per-request metadata. This preserves notice
budget/progress and original provenance; ordinary selected attachments carry the
full path, and CLI list retains the full stored absolute path.

### #79 reader/file admission review correction

Attachment list returns owned rows from its read-only projection closure and drops
the SQLite connection before starting path observations. Metadata/open probes then
reuse the existing control-file admission owned by InstructionLoader (the same
finite permit/deadline/actual-closure lifetime as AGENTS/cwd reads). No additional
worker, queue, DB connection or cache is created for the file phase.
