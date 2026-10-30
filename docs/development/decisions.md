# 開発判断

公開版の判断を記録します。日付、理由、影響、代替案、再確認条件を必要な範囲で残します。過去の私的作業記録を利用の前提にしません。

## Measure Saved Totals First (#47, 2026-10-30, Active)

[Latest comment 6515082156](https://github.com/tya5/shellboy/issues/47#issuecomment-6515082156)
withdraws the earlier arbitrary-period/write-time additions (comments6513644118/
6513653608) and the complex approved design through308dfef. Preserve those sources
in archive refs; do not carry their schema/observer/ledger/period gates forward.
The started, untested schema-only WIP is isolated at protected archive commit
ff2d0cd and is not part of the current branch or an integration candidate.

Use existing saved state/standard usage/timing and measure indexed SQL first.
Roughly100ms is a measurement goal, not an absolute CI correctness threshold.
Only measured slowness warrants a separately approved simple Run/Thread total.
This limits scope without pretending missing usage is zero. No period filters,
client-invocation observation, summary accounting or new GC policy in version1.


## Standard Usage Scalar Boundary (#49, 2026-10-30, Active)

The leader adopts five independent optional u64 measurements, restricted to
`0..=i64::MAX` so Rust saves and SQLite reads have identical exact-number
semantics. Check JSON integer type and SQLite extraction type before accepting
legacy data; never cast REAL overflow to integer. Missing/invalid fields are
unknown, including when all fields are unknown; explicit zero is known. Do not
repair totals, cache or reasoning by inference. Keep the normalized Rust value
available to future #47 writers rather than reparsing saved JSON.

Normalize only known provider-envelope usage members, including summary and
failure/stream audit response copies. This closes the second persistence path
for attribution; do not recursively remove user-content keys named usage or
alter output, error diagnostics or HTTP request bodies. Historical raw and usage
rows remain unchanged, with SQL scalar reads supporting large legacy usage.
The alternative full-document cap loses reported numbers and the alternative
unbounded document load retains provider appendices. SQLite must still parse old
large documents internally. No special rewrite migration is justified. Revisit
only if the provider standard fields or exact numeric storage contract changes.


## Measured CI test layout and failure bounds (#46)

Use the [adopted performance design](../design/ci-test-performance.md) and
[investigation record](issue-46-ci-performance.md). Historical Windows logs
separate substantial repeated linking from native fixture execution; the first
candidate reduces links and fixture-only commits without changing production
behavior. Keep subprocess suites standalone and enforce complete registration
and normalized identity/ignored-set equality. The initial native comparison
retained two Windows test threads. After fixing
shared runtime_features 5s watchdog/poll helpers, source `8fa0429` passes both two
and four threads with identical test/ignored sets. Harness sums 350.876s versus
237.908s favor a bounded four-thread CI default; verify it in the normal three-OS
workflow and retain explicit two-thread diagnosis. Its first normal verification
found three response_retries 5s fixture guard failures; repair those with existing
notifications and bounded saved-phase diagnosis, preserving the failed run and
all actual product timeout/retry/absence/responsiveness assertions. The repaired
ordinary four-thread source `03ceac1` then passes all three OSes: Windows Tests/job
285/379s versus 640 / 888s at `0469ca0`. Keep the measured four-thread default, while
preserving source-specific failures and runner/cache variability in the evidence. These independent-runner
samples are not a latency guarantee. Discard the 405-source index rewrite:
isolated measurements did not support it, unlike historical-DDL seed batching.

A short mock-arrival or waiter-completion watchdog is not a product latency
contract. Raise deadlock guards, retain phase diagnostics and require all 24 IPC
wait receipts to remain pending when status succeeds, before stop. Their expiry
is longer than the watchdog, so expiration cannot make a blocked dispatcher pass.
Do not blanket-convert native upload/termination tests to 1ms: those verify real
phase/ownership wiring, whereas existing pure tests cover deadline and retry
policy. Do not add test-only production clocks, new dependencies or filesystem
security exclusions without evidence. Reconsider higher concurrency and further timer
refactoring when repeated native measurements or changed failure profiles justify it.

## Review follow-up: bounded lifecycle fixes (2026-10-30)

Keep lifecycle fixes in the existing Control, capture and supervisor ownership
boundaries; no new persistent table, worker, queue, generic recovery engine or
compatibility route is needed. Findings and commit mapping are tracked in the
[follow-up record](issue-36-39-follow-up.md).

- Revisit remains a strict current-format state. Optional projection may omit
  malformed state and stop/startup may discard it; explicit reads report the
  error. Defaulting absent counts would manufacture facts. SQL failures still
  propagate. Revisit capacity recovery keeps the existing retry behavior,
  including its possible empty-input model Turn.
- Thread-local malformed capture metadata can be isolated, but a recovery JSON
  value violating the schema CHECK is database-integrity corruption. Do not
  weaken startup quick_check to make a synthetic corrupt database look usable.
- Latest Run retains the adopted created_at/id order and existing covering
  index. Activity and list must agree. No migration/sequence is justified by
  this review; wall-clock rollback remains a timestamp-order limitation.
- HUP is a terminal/session-loss notification and cannot reliably express a
  second human request. Make it idempotent; repeated explicit TERM retains force.
  The reviewer's normal-close orphan finding was withdrawn after its process-wait
  script was corrected. Exact baseline/current zsh comparison confirms the
  duplicate-HUP defect, not a new bash cleanup defect.
- Forced exit best-effort signals only groups owned by live supervisor tasks.
  One set entry per active native task is removed on confirmed disappearance or
  task destruction. UI access uses try_lock and a racing spawn sees an atomic
  force flag. No persisted numeric process identity is trusted. A stalled spawn,
  OS process-group reuse, blocked terminal restoration and Windows callback
  deadlines remain explicit limits rather than durable-success promises.

Reconsider these choices if current-format state evolves, native OS evidence
contradicts the bounds, or product requirements require non-timestamp Run order.
See [acceptance](../validation/acceptance.md) for executed checks and remaining
platform limits.

## Shellboyへの改名と公開構成（2026-10-26）

利用者の指示により製品名、Cargo package、実行名、環境変数、既定保存先をShellboyへ統一する。旧名の互換は不要とし、別名launcherや旧保存先の自動探索・移行は追加しない。旧データの移動も行わない。影響は起動名・設定参照・新しい既定保存先であり、旧環境を変更しない。保存先と検証は [setup](setup.md)、[検証状況](../validation/acceptance.md) を参照する。

非公開資料を含む旧リポジトリの履歴は保管し、公開用には別ディレクトリで新しいGit履歴を用意する。履歴の書換えだけで非公開資料を隠す方法は採らず、公開用ファイル集合を明示する。利用ガイド・採用設計・ソース・テスト・fixture・配布migration・検証ツールは保つ。非公開原文、過去レビュー台帳、個人環境記録、私的な画像・ログは公開構成に含めず、公開版のリンクやテストをそれらへ依存させない。

改名前の実行結果は改名後の合格に流用しない。新しいsourceと公開用構成でチェック結果を記録する。公開準備の段階ではpushを行わず、ライセンスは利用者指定のMITを採用した。後続の公開許可は本書の「公開後の保守と初版配布」に記録する。公開方針や互換要件が変わった場合は、この判断と入口文書を更新する。

## コアの責務

UI投影・module分割・既存資源の所有整理を、新しい業務状態や独立workerの追加と混同しない。新しいCore型・永続表・event・queue・依存が必要な場合は、既存責務では不足する理由、上限、所有者、寿命、終了時回収、互換を対象設計に記録する。数値を減らすためだけの削除や汎用frameworkの追加は行わない。

## 非同梱DDLへのテスト依存を外す

公開対象から原仕様snapshotを除くため、依存smoke testが読み込むDDLを原仕様の論理候補から配布中の `migrations/0001_initial.sql` へ変更した。初期migrationの35表（core 33表、採用経路、migration ledger）とintegrity/foreign-key検査を維持する。元DDLをコピーして別のschema正本を作らず、後続migrationの検証は既存のstorage試験へ残す。これにより非公開資料なしで結合試験をコンパイルできる。

## 未統合だった復旧表示修正の扱い

改名前に用意されていた修正は、履歴やerror JSONを変更せずtyped errorのmessage部分を短い表示へ投影する内容だった。名称変更とは別のcommitとして取り込み、完全fieldのコピーと利用者本文中の `provider:` は保持した。変更順序を再構成するための旧履歴のmergeや書換えはせず、公開用履歴には修正済みソースだけを反映する。

## 公開後の保守と初版配布

利用者の追加指示によりpushと初版Releaseを許可された。ローカル標準checkの成功をGitHub上のWindows/Linux成功として扱わず、GitHub CIの結果を確認してからversion tagを作る。tagに対応するnative release binary、公開資料、SHA-256をReleaseへ掲載する。接続済みGitHubツールにはRelease asset upload操作がないため、Git CLIと認証済みGitHub CLIでpush・Release操作を実施する。

レビュー指摘PR2-1〜3はコード経路を確認し、公開文書に条件・回避策・再現未確認の範囲を記載した。ここでgoalを再開したり、指摘の全是正完了とは扱わない。製品作業は公開リポジトリ、非公開台帳はGit管理外の `.local/review/` 内で独立したローカルGitとする。旧Gitを公開履歴へmergeしない。

実proxyのURLとモデル名を持つ2つのignored試験は、合成データ専用・明示実行のみであり、秘密情報ではないため保持する。製品既定値や利用可能モデルの保証ではなく、通常CIでは外部APIを呼ばない。

## 初回CIで判明したOS固有の境界

LinuxとmacOSでopenptyのwinsize引数のconst性が異なるため、共通試験の4箇所はraw pointerで渡す。OSを跨ぐ広いlint抑制は追加しない。WindowsのSECURITY_ATTRIBUTESはCreateFileWだけが借りる同期blockへ閉じ、非SendなC構造体をasync状態へ残さない。unsafe Sendの追加、worker変更、Win32呼出し順の変更は不要と判断した。

Windowsの標準検証で、Unix専用テストのjson macro importにもcfgが必要と判明し、importだけを同じcfgで制限した。CIはClippyのkeep-goingで全targetの診断を集め、失敗時も依存cacheを保存して同じ依存の再ビルドを減らす。通常CIのpushはbranchに限定し、version tagでは専用release workflowを実行する。コンパイルやテストの失敗を成功扱いにはしない。

## モデルAPIのTLS検証設定（2026-10-26）

利用者の依頼により初版0.1.0へ接続ごとの `ssl_verify`（既定true）を追加する。reqwest 0.13.5/rustlsに環境変数だけで検証を無効化する機能はなく、既存ConnectionとHttpModelClientの生成箇所で完結させる。設定省略で無効にならないようRust DefaultとSerdeの既定値を揃える。会話/要約/embedding/モデル一覧で同じ設定を使い、ownerのhot reloadや新規環境変数は追加しない。証明書チェーンだけでなくホスト名なども検証対象外になることをガイドへ明記する。追加CAや接続時の対話確認は今回の範囲に含めない。

確認は合成証明書のローカルHTTPSで行い、HTTPだけのmockをTLS成功の根拠にしない。既存リリース候補のCI結果と変更後の確認を区別し、変更後の候補でCIを確認してからタグを作る。

TLSのローカルfixtureに限り、既存lockにある `tokio-rustls = 0.26.6` を固定のdev dependencyとして直接参照する。実行時のOpenSSLやOS trust storeへの書込みを不要にし、合成の公開テスト鍵で全OSのHTTP要求を検査するためである。製品依存・worker・永続表は増やさない。HTTP(S)プロキシとNO_PROXYは既存ライブラリの環境変数対応を案内し、独自の設定schema/除外規則は実装しない。

Windows CIで判明した管理UI試験のUnix絶対パス前提は、fixtureをOS別絶対パスへ変更して解消する。製品の起動準備完了判定は緩めない。CIのTestsにはno-fail-fastを付け、一つのtargetが失敗しても後続targetの診断を集める。

Windows全target試験で判明した添付fixtureのUnix絶対パス11件と、AGENTS参照pathの区切り期待値1件はfixtureだけを修正する。残るoverflow回復試験の5秒timeoutは段階が不明であり、PowerShell起動遅延や製品障害と断定しない。期限/製品動作は変えず、失敗時の段階と有限の保存状態を記録する。Windows CIでは当該回帰を全target testの前に実行し、不要な全実行ファイルのリンク待ちを挟まず診断する。成功後は従来どおり全targetを実行する。

初回配布workflowのUbuntu 22.04ではPython 3.10にtomllibがなく、build前のversion検査が失敗した。Python/action依存は増やさず、固定Cargoのmetadata JSONからpackage versionを読む形へ変更する。Rust製品ソースは3 OS CI合格時のまま。Releaseも配布assetも未公開のため、初期配布前のv0.1.0タグだけをこのworkflow修正へ更新する。公開済みReleaseのtag/assetはこの方法で置換しない。失敗runと旧候補hashは検証記録に保持する。

配布tagの再CIではWindows overflow試験の5秒待ちが再発し、追加診断で「最初のshell command echoがrunning・raw未到着、overflow開始前」と判明した。単独実行2回と前回fullは成功しており、製品側に5秒完了契約はない。Windowsに限りこの実プロセスを含むfixtureの待機上限を既存native shell fixtureと同じ15秒へ揃える。Unixは5秒、診断・全ての意味検証・再実行回数のassertを維持する。OS内部の遅延原因までは断定しない。

この追補はtest harnessと判断記録だけで、製品ソース・同梱guide・Cargo依存は配布tag96e65bfから変更しない。タグを再度更新して同一製品を再ビルドすることはせず、配布binaryとそのsourceを保持し、追補後CIを別hashの検証として記録する。公開済みのtagやassetの置換は行わない。

## ストリーム差分と失敗後の受信本文（2026-10-26）

会社Windowsの社内proxyで複数モデルが最終JSON不一致による復旧待ちになる報告を受けた。当初はmetadata以外の意味項目だけを照合する案としたが、利用者からAPIがその一致を保証するのか指摘を受け、途中と最終の差分自体を失敗条件から外す方針を採用した。最終responseそのものの既存検証を行い、最終データだけを採用・実行する。途中の差分はAPI違反と断定せず、固定分類の診断と保存済み原文から調査する。

成功時も差分があれば既存node表にpath外のmodel_stream_observation監査nodeを同じ採用transaction内で保存し、通常モデル再送と索引に入れない。新しい業務state・永続表・worker・依存は不要。16件までの差分分類と総数、既存64MiB stream上限内の受信回答/平文推論/確定itemと最終responseを保持する。DBの既存payload容量計数とGCのTurn所属rootを共用する。履歴のために会社の実データやAPI資格情報をGitへ入れない。

不正な最終応答等で復旧待ちになっても受信した有料本文をUIから消さず、保存済み証跡から再表示する。最新窓の表示上限は維持し、過去の受信データはUI一覧と既存原文cursorで発見・閲覧可能にする。Thread削除・GC時の既存回収責務には平文推論も含める。

受信deltaと最終本文の差も観測対象とし、item.doneのない応答でも途中本文を失わないようにする。response全体の回答/平文推論差には架空のoutput_indexを付けない。同一HTTP chunk内で後続JSONの解析が失敗した場合も、先に解析済みのeventを受信本文へ反映する。成功時のmetadataだけの差は原文・診断へ残し、通常会話へ同じ回答を重ねない。

## Windows継承環境の隠し変数（Issue #1、2026-10-26）

`environment_block`が`=C:`だけを例外にしていたため、既存の`inherited_environment`が読み取れる`=::`、`=ExitCode`等を再構築時に拒否していた。ドライブ名の列挙を増やさず、先頭以外の`=`を区切りとする既存の読込み規則へ検査を揃える。[MicrosoftのMSBuild実装](https://source.dot.net/Microsoft.Build.Framework/BackEnd/CommunicationsUtilities.cs.html)にも隠し変数と`=ExitCode`の説明があり、継承環境を通常設定の環境変数名制約と同一視しない。値のNUL、名前のNUL・空名・途中の`=`は拒否し、設定envの英数字/underscore制約は変更しない。

不正entryの診断は変数名だけを先頭128文字に制限して引用・escapeし、値はコピーしない。UTF-16の環境block本体はlossy変換せず、値中の`=`や非Unicodeのcode unitも保持する。新しい状態・依存・worker・設定は不要。親processの環境を変更しない合成fixtureで混在環境と注入拒否を試験し、Windows専用の起動回帰は実機確認と分けて扱う。

## WSL/LinuxのIPC起動条件（Issue #2、2026-10-26）

利用者にXDG_RUNTIME_DIRの作成や祖先権限の調整を求める旧方式を廃止する。XDGが使えなければmacOSではOSのユーザー一時領域を検証し、それも利用不能ならcanonical /tmpのsticky領域へランダム0700 directoryを原子的に作る。以前の固定hash由来socket配置を、既存endpoint.jsonのaddressにランダムpathを保存する方針へ変更する。pathは認証情報ではなく配置のlocatorであり、instance handshakeと接続両側のpeer uid照合を併用する。[Linux unix(7)](https://man7.org/linux/man-pages/man7/unix.7.html)と固定Tokioのpeer_cred実装を確認し、独自OS syscall wrapperや新依存は追加しない。

ディレクトリ作成後・socket bind後の公開前クラッシュでlocatorを失わないよう、予定pathをmkdir前に既存publisherで原子的に保存する。通常のCLI接続はbind完了後に可能になる。owner/Control間にはpublish済みEndpointの小さなメモリsnapshotを共有し、shellへSHELLBOY_SOCKETを渡すための毎回file読込みや業務stateを増やさない。別data-dir指定では環境pathを使わず、指定環境pathが失敗しても他ownerへfallbackしない。

清掃は現在認められる安全なbase・専用名・所有・0700・内容・file identity・生存probeで限定する。別XDGへ変更した場合や改変されたlocatorを根拠に探索範囲を広げず、削除対象と検証できない残骸は保全する。旧固定名の自動移行清掃や/tmp全域走査は行わない。保存先はsocket配置から独立して新規0700、既存の自分のdirectoryも0700へ限定し、symlink/他uidへのchmodや親の権限変更をしない。実行UI thread、Core状態・表・worker・キューは追加しない。

## Enter直後の送信表示と障害切り分け（2026-10-26）

既存のdraft ID/revision・operation ID・message IDを使い、UI内のpendingだけに送信開始時刻と反映確認を持つ。Coreの状態・表・event・workerは増やさない。受付済みと画面反映済みを分け、source IDと履歴node IDが一致した事実を一方向に保持する。高速完了でsource通知が遅着した場合は結び付けまで保持する。Completedとsource IDが揃ったら現在の履歴窓外でも解放し、画面が表示待ちに固定されることを避ける。実際のRun/Turnが動いていれば保存後の表示待ちは補足へ回す。アニメーション時計も主表示に従い、復旧・異常等の静止状態で送信のためだけに10fps描画を続けない。

固まった場合の切り分けには、既存の有界診断writerへfrontend受信・処理開始・保存/失敗を記録する。operation IDをfrontend受信時に作り、その同じIDでControlへ渡す。保存成功はUI通知の前に記録し、保存前と通知後の問題を区別できるようにする。本文や任意エラー文字列の記録、経過時間だけによるtimeout判定・自動再送は追加しない。UI描画完了の永続的な追跡は今回の範囲外であり、ログ欠落だけから未受付と断定しない。

## レビューgoal再開時の検索境界（2026-10-26）

> Historical decision: earlier compatibility choices are superseded by Issue #33
> and the latest current-format decision at the end of this log.

> Historical choice; compatibility portions are superseded by Issue #33.

PR2-2は採用する。旧splitter identityの選択空間を、新規sourceの索引成功だけで非選択へ変えない。旧vectorへ異なる分割を混在させず、既存candidate/group pointerへ新規分を保管し、全job成功後の明示selectまたはrebuildで切り替える。自動の全履歴再索引は行わない。新しい表・worker・Core状態は不要。

PR2-3は採用する。巨大rawは既存decoder/SQLite BLOB page読取りでspan全体をincremental hash検証し、返す原文referenceを最大64K文字rangeに限定する。小rawの全文referenceとhybrid融合は保持する。正常な巨大hitをinput_limitでscope全体失敗にせず、実際のsource不整合は区別する。

PR2-14の可変400文面によるpause回避は採用する。provider codeがない場合はkind/statusの障害クラスで連続計数し、文面中のrequest ID等でresetさせない。同じ根本原因と断定せず、成功時resetと既存の有限counterを維持する。読取identityと分割設定の分離は続けて検証する。

PR2-6は既存CLI契約に沿い、末尾のstatus.next_cursorを省略する。非末尾のcursorと、旧null/新省略の読込みは保持する。

PR2-8の『elapsed_msを直接の子終了時までへ変更』は採用しない。既存shell設計のS6は配下確認・drainを含む監視期間と明示しており、実装も両OSで一致する。子終了後の回収待ちが含まれるという指摘自体は正しいが、保存済み値の意味を変えず、利用ガイドへ期間を明記する。CPU時間や直接子だけのdurationが必要となった場合は、別の測定事実として検討する。

PR2-1は採用する。通常256MiBのreceipt上限は維持し、停止2枠に必要な最大outcome＋request frame/管理metadataの固定余白を追加する。RunStop/ShellStopだけその有界上限を使い、同じreceipt/counter/再送経路を共有する。追加は内部のAdmission分類だけで、Core lifecycle・表・workerは増やさない。停止自身が余白を埋めれば新IDは拒否し、元ID照合とreceipt-free TUI停止は維持する。

PR2-14の読取identityには、分割設定と独立したendpoint/model hashを既存app_metaへ1space1件保存する。旧spaceは現在の完全keyまたはlegacy keyを検証できる場合だけ補完する。既存markerが一致する読取では書き直さない。RAGは読み取り専用を維持する。元splitが分からず検証できない旧spaceを推測再束縛せず、元の設定へ戻して明示selectする導線を残す。新しい永続表・workerは不要で、markerはspaceと寿命を合わせる。

PR2-7は採用する。観測経過を適用仕様へ明記し、背景移行の日本語文面をFrontendへ移す。既存の公開Rust型EngineEvent::Noticeのin-process payloadを対象execution・既存ForegroundRelease・取消件数へ置き換える。既存enumの可視性だけをadapterへ広げ、新しい状態・表・event種別・worker・依存は追加しない。EngineEventはCLI/保存wireではなく、公開CLI・DB契約は維持する。Coreはgroup保存成功後だけ事実を通知し、Frontendで現在と同じ文言へ投影する。将来の表示変更もCoreの翻訳変更を不要にする。

PR2-9は対象を調整して採用する。現行macOSはユーザー別TMPDIRではなくcanonical `/tmp`を使う。Linuxでは新規namespace directoryへの既存fs2 flockをSocketFileの生存期間保持し、協調するage清掃から保護する。[systemdの推奨](https://systemd.io/TEMPORARY_DIRECTORIES/)に沿う限定対策で、既存listenerごとにFDを一つ持つ。新しい表・worker・poller・依存は不要。stale metadata捕捉はlockを取らず、既存のlive socket probeを維持する。XDG最終logout・reboot・明示削除は別の寿命境界であり、防止や自動再bindは加えない。macOSのcleanerが同lockを尊重する根拠は未確認なので、不要なlockを追加して消失防止を保証しない。Linuxの実清掃試験は実行環境がある場合だけ実施し、macOSの合格で代替しない。

PR2-10/11は部分採用する。Turn作成の共通保存と復旧TurnのCASを共有し、認可・wake・context actorは各受付に残す。receiptは復旧したTurnのものだけを解放する。LiveRegistryはmapを非公開にして原子的な登録/通知/slot照合削除を所有する。一つのRunの停止と所有木の停止は対象・戻り値・計算量が異なるため統合しない。DB・spawn・Changed・再kickをRegistryへ移さず、新しいengineや遷移DSLは不要。構造整理と復旧範囲の是正を別commitへ分ける。

PR2-12/13/16〜18は採用する。受信済み本文は保存IDの通知だけで消さず、同じapplyで本文を取り込んでから解放する。eviction後の遅着応答では仮本文を維持する。確定待ちの対応表は既存provisional/cache予算内に限る。表示groupは保存済みの中断参照、または隣接する同executionのcall/resultだけに限定し、全memberの原文・anchor・完全URLを保持する。離れた結果やmetadataだけの結果を根拠に呼出しを隠さない。Coreの履歴や状態を表示都合で変更しない。

PR2-5は競合を作る待ちと、保存状態を観測するpollを分けて対処する。RAGの3競合は要求ごとのarrival/releaseへ変更する。添付の条件pollは残し、失敗時だけ有限の状態診断を追加する。過去のtimeoutやWindows cleanup待ちの原因を、この修正だけで証明したとはしない。新しい汎用fixture engineや製品の遅延hookは追加しない。

PR2-15は既存65,536のgraph work境界を維持して、seedの走査起点を狭める。LIMITだけでは無関係なreplacement走査を防げないため、Turn node/legacy receipt起点のqueryとcovering index2本をschema v9へ追加する。新tableや探索engineを作るより小さく、追加disk・書込保守・初回構築費用を負う。旧SQLとの集合一致とbundled SQLiteのVM命令数で利益を検証し、当該Turn自身の候補数やDB待ちまで一定時間とは約束しない。移行の原子性・既存本文/割当/予約の保持も確認対象とする。

PR2-19は採用する。重複admissionの競合を既存callへ保存する新しいtool errorへ変換せず、競合として返す。failed attemptの再分類も保存済みtyped context-limitだけへ限定し、入力側の原因を信じて失敗原文を置き換えない。新しい状態や再実行機構を足さず、既存の保存事実とCASを強化する。

表示の所属と原文位置は別に扱う。group本文は実際のresult/summaryのnodeとUnicode位置へ結び、生成headerで本文位置を消費しない。複数callの依頼表示と結果表示を分担し、結果はcallが画面外でもcommandと成否を読める形にする。索引の初回利用可能通知は簡潔にするが、既存の選択spaceが別spaceへ変わる通知を通常のreadinessに分類して隠さない。

F4の再測定は製品と同じ32件/4msの更新経路を使用する。測定用の旧256件pumpを廃止し、pump後の表示source・入力からTestBackendまでの時間・DB往復を観測する。中間値の統合も観測間のskipに含むため、queue待ち時間や通知配送件数とは呼ばない。保存確定は仮本文を除いて確認する。既存の測定用コードだけを修正し、製品のhook・workerは追加しない。条件の異なる旧baselineとの性能非退行や、実端末の描画・IME性能をこの測定から主張しない。

PR2-17/18の実端末再確認で、正式な親子終了記録のkind・詳細参照が一般fieldsの後方で切り捨てられ、要約と詳細が重複する条件を確認した。中断envelopeの関係情報を既存fields予算内で優先する。照合条件や上限を緩めず、Coreの保存形式・原文・表・workerは変更しない。小さな表示用fixtureだけでなく、実保存形式とmetadataの多い場合を回帰対象にする。

## v0.1.2タグ前の再レビュー（2026-10-27）

外部レビューは採否の入力とし、コード・保存契約・再現条件で判断する。受信本文の有無と最終本文との差を表示の根拠にし、監査記録の存在だけで会話本文があるとは表示しない。巨大rawの複数spanは同じwitnessの検証走査を共有する。起動エラーの次操作は起動段階に合わせる。いずれも既存の保存形式・worker・Core状態を増やさず対処する。

> Historical choice; compatibility portions are superseded by Issue #33.

索引更新時の旧active維持は継続採用する。新candidateの履歴はselectまで既定semantic検索から外れ、select後に新旧を統合するには明示的な有料再索引が必要という制限を明示する。pause/失敗理由と切替案内は併記し、一方を隠さない。自動selectや暗黙の有料backfillは追加しない。

最初のv0.1.2候補のWindows CIはUnix専用テストの補助accessorがWindowsのtest buildにも含まれ、strict Clippyのdead_codeで停止した。全呼出しがUnix限定であるため同じcfgへ揃える。警告抑制やWindows jobの成功扱いには変更しない。macOS/Linuxは同候補で成功したが、後続修正は改めて3 OS CIで確認する。

受信previewを元のTurnへ戻す際は、表示用の項目をcanonical ordinalへ数えない。履歴の次窓・前後端・cache再利用・trimの位置は採用履歴だけで計算する。遅着した監査読取りは既存の履歴世代と保存revisionへ束縛し、新しいpreviewを古い結果で消さない。新しい永続状態やCore状態機械は不要。

追加レビュー後の最初のnative CIで、WindowsはUnix専用importのcfg漏れ、Linuxは既存lease回帰の古い英語message期待が失敗した。importを対象OSへ限定し、テストは新しい機械reasonへ照合する。lease保持やlive socket拒否のassert自体は維持する。ローカル標準checkは履歴ordinalの追加是正を優先してClippy中に中断し、成功には計上しない。

## 判断記録の再レビューと長時間稼働（2026-10-27）

Issue #3は採用する。観測は同IDでも再計算しreceiptを作らない。waitはTurn待機/解除を保存するため従来のintentと再送保護を維持する。semantic検索の再観測で外部費用が再発生する場合があり、read-onlyを無課金や完全なSQLite query_onlyとは扱わない。変更とwaitの終端結果はcreated_atから24時間経過後の既存GC保守、又は圧迫時に回収するが、ID/hash/origin等を同じrowの期限切れ記録として保持する。古い結果を失った同IDで変更を再実行せずoperation_expiredを返す。pendingと未反映参照は保護し、無制限のID tombstone削除はしない。任意IDを安全に失効させる新しいticket発行機構より、既存表の小さい結果縮退を選ぶ。墓標のmetadataも有限予算内なので、無限稼働は保証しない。

> Historical choice; compatibility portions are superseded by Issue #33.

Issue #3の追記を採用し、受付記録の正規入口はoperation getへ統合する。status --operationはv0.1.2だけ非推奨aliasとして残し、同じ処理へ正規化してstderrで移行先を案内する。古いwireのstatus wrapperも同じ詳細投影を共有する。次のリリースでaliasを削除する。別pool・表・worker・queue・依存や新error codeは追加しない。読取りの保存済み観測再取得を廃止する契約変更は、このIssue対応と利用者の実装指示に基づく。

macOSのユーザー一時領域優先案を採用する。環境変数TMPDIRを盲信せず、既存libcのconfstrでOSの選択を取得し、canonical化・uid/0700・祖先保護・103byteのsocket長を検証する。XDGを優先し、その後OSユーザー領域、最後に/tmpを使う。再起動cleanupも同じ候補集合で認識し、LinuxだけのflockをmacOSのage清掃保証とは扱わない。

Historical interpretation, superseded by Issue #33 on 2026-10-29: startup
schema upgrades and credential-removal transforms stay only inside removable
migration modules. Normal runtime readers accept the current format only; legacy
index reads, marker inference and old-owner response defaults are removed.
General validation and current witness checks remain, without a release-specific
compatibility promise. Launcher/env/data-directory fallbacks remain absent.

index previewは無課金・有限の候補観測、operation getは結果不明の副作用を重複させない観測として採用を維持する。通常の追加設計は自律判断の委任範囲として進める。Quit後の保存完了までは所有Runが短時間動き得ること、Unix PGID再利用の検査/送信間競合を原子的には排除できないことは、対象設計と利用ガイドに限界として明記する。

metadataだけが毎回変わるproxyでも、差分監査nodeには受信本文・完了item・最終responseを保存する。本文保全を優先して現契約を維持するが、頻度とDB増分の実測は未実施である。64MiBは受信stream上限で、再serializeや複数表への保存を含むDB増分の上限ではなく、常に約2倍とも断定しない。論理容量を圧迫する場合は、本文/原文到達性を維持する監査表現を別途検討する。会社proxyの最終JSON差分報告を、reasoning item再送拒否が再現した証拠にはしない。他provider・社内環境は未検証として公開する。

65,536件の履歴探索と128件batchは内部の安全上限で、最適値や処理時間の保証ではない。上限到達やDB占有が問題になる場合に対象を絞って再測定する。u64 ms最大値のtimer overflowは過去のmacOS試験で再現せず、推測だけでclampを加えない。他OSや時間境界で再現した場合に再評価する。過去のcheckpoint標本は読取64件のPASSIVE呼出65→0、decoder依存だけのdebug最適化標本は抜粋4.444秒→75.7ms・release34.4msだった。これは古いfixture/負荷条件の記録で、現sourceの性能保証や非退行測定とはしない。現時点の限定測定と環境は [検証状況](../validation/acceptance.md) に記録する。過去の添付timeout/owner lock競合の原因未確定と、fixture安定化や後続の成功は別の主張として保持する。

今回の増分はschema9のcovering index2本、既存app_metaのspaceごとのquery fingerprint、同じreceipt表の停止余白/期限切れmarker、既存listener当たりLinux directory FD一つ、およびUIの有限対応表/revisionである。新しい永続entity・独立worker・queue・汎用job engine・依存は増やさない。EngineEvent::Noticeの内部Rust payloadは文面から事実へ変わり、既存Rust adapterへの影響はある。公開wire版を変えていないことを全API不変とは言わない。既存body/metadata上限とowner終了時の回収は対象設計で維持する。

判断レビュー追記の利用者条件に従い、Quit開始後は新しいtool実行を開始しない。既存処理の保存と終了待ち、強制終了の別経路を保ち、Quit開始から新規spawnまでの受付境界を回帰で検証する。PGID再利用の残余競合は今回の制限として維持するが、将来shell監視を作り直す際は先頭プロセスをcleanupまで回収しないwaitid/WNOWAIT方式を採ることを再確認条件とする。nohup/setsid等で所有追跡外へ切り離した処理の完全回収・デーモン化維持は今回に含めず、OSごとの所有機構を含む別の設計対象とする。

追補レビューのLowは、容量拒否hintを圧迫回収実施済み・pending/保護/ID metadataの観測・必要時の明示再起動へ更新する。保持時間はRESULT_RETENTION_SECONDSからSQL修飾子も生成し、status表示と別々の24時間定義を持たない。墓標の増加量は仮定を置いた計算例として公開し、固定稼働期間の保証とはしない。

## HTTP期限の共通化（Issue #8）

接続後に無期限で残るembeddingをHTTP層で制限する。全呼出しでApiEndpoint別の同じ小さな期限helperを使い、呼出し側のRAG90秒・モデル一覧20秒wrapperは撤去する。Responsesは全体を切らず受信停滞600秒、embeddingは全体60秒、モデル一覧は同設定と20秒の短い方を採用した。生成の誤中断は料金・時間・受信本文の保全に影響するため長め、前面で待つ検索は有限とし、接続ごとの設定で変更可能にする。接続確立30秒は現契約を維持する。時計範囲外の加算はconfig_invalidとして拒否し、0以外の値を暗黙に無期限へしない。

2026-10-27に指定のローカルproxyへ合成入力・high effortで1要求を送ったが、HTTP400（header待機約0.893秒）で拒否され正常streamの無通信時間は測定できなかった。600秒は安全性を実測確認した値ではなく、変更可能な暫定既定値である。proxyのhigh設定が正常受理された時点で、複数回の受信間隔と十分な余裕を再確認する。本文・鍵・利用者DBは測定記録へ保存しない。この1要求からproviderの遅延上限を推定しない。

期限切れはkind timeout、header/body段階・観測ms・設定keyをローカル生成し、HTTPエラーの既知statusも保持する。401等の本文が止まっても認証/creditのpause判断をtransientだけへ変えない。索引のretry/pauseを再利用し、検索は呼出し元切断とowner停止でHTTPをdropする。Control観測の外側には既に切断取消しがあり、これを失われていたと断定しない。IndexServiceの直接呼出しにも所有tokenを渡す形へ揃え、実IPC切断の回帰で既存の保護も維持する。会話の部分出力は既存AttemptFailure/復旧待ちへ保全する。未受信時の自動retryは別Issue #7として分離し、今回暗黙に料金が増える再送は追加しない。

受信の時刻だけを同じ有限channelの型付き通知で渡し、保存状態やprovider JSONへ追加しない。通知飽和時はmodel eventに添えた元の単調受信時刻で補完し、UIへの追加通知は最大1回/秒に抑える。表示は1秒未満の観測差を許す。UIの既存時計で30秒閾値を投影し、DB周期照会・新worker・新table・新queue・依存追加は行わない。

## Windows CIの起動取消し試験の切分け

公開commit211750eのCI40420705214はWindowsの起動取消し試験1件だけがSaveFailed / SQLite database is lockedで失敗した。同じ製品sourceの先行CI40418072576は成功しており、失敗commitの変更は文書だけである。旧試験は別SQLite接続でwrite lockを保持し、Database通知を受けてから解除していたが、通知はDB worker受付前、watchは観測用で、製品のbusy timeoutは2秒である。試験側に2.3秒の解除遅延を一時的に加えるとmacOSでも同じ失敗を再現した。Windowsで実際にどのscheduler待ちが何秒生じたかまでは断定しないが、この試験は正当な有限競合失敗を誤って不合格にする。

busy timeout延長や無制限retryは採用しない。既存の起動進捗callbackをprivate helperへ通し、同じDatabase通知の呼出しで取消すことで、移行完了・DB close後の返却・endpoint未公開を検証する。実際に受付済みwriterが呼出し元drop後もlockを持つ条件は既存storageのchannel同期試験で維持する。外部writerを取り除いてscheduler依存だけを排除し、成功条件や公開契約を弱めない。globalな試験registryや本番用の待機hookは追加しない。

## 未受信時の自動再試行（Issue #7）

利用者の採用指示により、#8の有限HTTP期限と統合した会話Driver再試行を採用する。HTTP層は事実（受信byte有無、対象HTTP status、Retry-After）だけを渡し、料金を伴う再要求の回数・永続証跡・取消しは既存Driver/attemptで管理する。何も表示していなくてもmetadata/commentを受け取った場合は再送しない。HTTPエラー本文はモデル出力とは区別する。timeoutは受信前でも対象外とし、#8の停滞保全/明示retryを維持する。

予算は要求ごとに復活させずTurn/generationあたり最大2回（設定最大5）とする。context圧縮の2段階予算とは独立し、成功したtool要求の後も同じ予算を共有する。待機は指数1秒から、Retry-Afterは秒/HTTP date対応、既定上限30秒。取消し可能なsleepのみで専用schedulerは追加しない。既存failed attemptのerror_jsonとChanged通知を利用し、表示のための新しい状態・表・eventを追加しない。上流処理済みによる重複課金は許容される残余リスクとして保存する。要約・索引・独立RAGへ再送を拡張しない。

automatic_retryのnumberは保存後の待機を含む予約回数であり、HTTPが送られた回数の証明にはしない。待機中に停止すると次のattemptは作成しない。実要求の追跡は後続のmodel_attemptと診断記録を併せて確認する。

上限到達時のTUI復旧理由では、元ModelErrorと回数の接尾辞を別々に保存する。Frontendは同generationのincomplete attemptの型付き回数と、構成した診断文全体の完全一致が成立する場合だけ「モデル要求」と表示する。単なるprovider接頭辞の推測へ緩めず、欠落/不正/不一致/古い記録は従来の段階不明表示を保ち、保存JSONは変更しない。全体check後に見つかったこの表示投影だけの補正は、対象回帰と最終Clippy/fmtで別に検証し、先行実装の全体成功と区別する。

## 解決済み受信記録の注意表示（Issue #6）

受信記録そのものと現在の対処要否を分け、元Turnのrecovery_waitだけを注意色の根拠にする。後の別Turnが完了したことを古い失敗の解決とみなさず、各sourceの元Turnを参照する。途中本文は確定回答へ変更せず、原文・同Turn対応・再送対象外を維持する。現在のTurn状態は既存DB workerで読み、Frontend/既存UiUpdateで色を投影する。UIがanchor保持のため古い本文windowを残す場合も表示分類は更新する。

一覧は同Turn割当receiptの最初の依頼を有限抜粋として使い、無ければ不明を示す。続く追加指示すべてを1行に列挙せず、Turn短縮IDを補助に残す。通常のRust子moduleへ一覧SQL/投影を分けるだけで、汎用一覧frameworkや新しい永続状態は足さない。

## 2026-10-27: EOFの監査をtransport推測より先に行う

- 判断: BOM/CR単独で受信済みcompletedを取りこぼす自前SSE decoderを修正する。空行未到達・DONEのみ・JSON type欠落を自動成功扱いにはしない。
- 理由: 修正前テストでBOMは実報告と同文言、CR単独はpartial EOFを再現した。HTTP受信順序にEOF追い越しは見つからず、Windows専用decoderも存在しない。BOM/CRは仕様上有効だが、会社Windowsの実wireが未観測なので頻発原因の確定とは分ける。
- 調査性: 受信/解析/残留byte数と固定分類を既存errorに記録し、実データを新たに自動ログしない。Coreの状態・保存schemaや完成条件を緩めず、小さなtransport修正に限定する。
- 再確認条件: 修正版でも頻発する場合、完了を示す受信データがどの形式で到着したかを診断結果から絞り、SSE event名や非標準終端の扱いを別途判断する。

## 2026-10-27: Ctrl+Lは同一viewportの再描画へ集約する（Issue #9）

- 判断: UIのbool予約一つで重複をまとめ、既存FPS制限内でfullscreen Terminal::resizeを同じareaへ呼ぶ。下書きやpopupを変更せず、Coreへ操作を送らない。
- 理由: 初回の実PTY試験で、固定Ratatui版のTerminal::clearがget_cursor_positionのDSR応答を待つと確認した。実際のサイズ変更と同じ内部clear_viewportを使えば、端末への往復照会を増やさず、画面と差分bufferとリンクmetadataを無効化できる。
- 影響: 選択固定中も手動再描画だけを許可する。送信/改行へのCtrl+L設定は優先し、ヘルプで無効理由を示す。周期的repaintや文字幅方針の変更は含めない。
- 再確認条件: Ratatui更新時にclear/resizeのcursor照会・buffer無効化契約を見直す。Windows実端末での表示ずれ原因は別途確認する。

## 2026-10-27: 環境別の起動案内は実装と確認範囲を分ける（Issue #4）

- 判断: 端末・起動元shell・モデル用shellを分け、Windows/PowerShell、Windows/Git Bash、mintty、WSL、macOS/Linuxの構成と設定例をガイドへ追加する。モデル用PATHはcurrent_exe親を自動追加する実装へ記述を合わせる。
- 理由: 提案のmintty一律非対応やWSLからWindows版起動不可能という一般化は採用しない。mintty公式はConPTY対応を説明しており、Windows/WSL相互運用自体と製品が採用している構成は別である。未確認を保証/非対応と取り違えず、まずWindows Terminal等からWindows版、WSL内はLinux版へ案内する。
- 検証: macOSの実binary/PTYとloopback mockで、親PATHをOS標準directoryだけにして絶対パス起動し、モデルのBashが短い名前で同じCLIのversion/docsを実行できた。Windows/Git Bash/pwsh/mintty/WSL実機へ結果を読み替えない。
- 影響: ドキュメントだけを変更し、shell adapter、PATH優先順位、保存schema、依存は変更しない。Windows実機での組合せごとの受入は環境が得られた時点で別途記録する。


## 2026-10-27: provider固有の項目IDは送信用copyから除く（Issue #10）

- 判断: replay adapterを2へ上げ、message（type省略のrole形式を含む）とfunction_callの直下idを共通変換で外す。IDのprefixを判定する方式にはしない。call_id、phase、status、未知フィールド、reasoningは変更しない。
- 理由: 接続先で有効なitem ID形式が異なり、特定prefixだけの許可/拒否ではproxy変換に追従できない。toolの対応はcall_idで維持する。元JSONをDBへ残し、送信copyのみに適用すれば監査と再送を両立できる。OpenAI Docsの[function calling](https://developers.openai.com/api/docs/guides/function-calling)もcall_idで結果を関連付け、reasoning項目の再送を案内している。
- 影響: 通常履歴・item wrapper・spill内outputへ同じ変換を適用する。spillの保存時点ではIDを除去せず、古いspillを含めAPIへ送る際に正規化する。新しいschema、設定、worker、自動HTTP400再試行は追加しない。
- 境界: 既存のowner再起動は復旧待ちを含む旧Runを停止するため、停止済みRunへの/retryを新たに許可しない。再オープンした同じThreadで新しい依頼を送る場合と、生存owner内の復旧待ちへの明示/retryを確認する。
- 再確認条件: reasoningのID/暗号化本文に別provider互換問題が確認された場合は、そのprovider入力契約とepochを別途検討する。今回のmessage/function_callの処置をreasoningへ拡張しない。


## 2026-10-27: 長時間の会話を連続失敗予算で再試行する（Issue #11）

- 判断: Issue #7の「Turn全体の予算」「metadata/timeoutは対象外」を更新する。検証済み最終応答の採用後に連続失敗回数を0へ戻し、既定8回、指数待機上限60秒とする。正常終端前のEOF/transport/停滞timeoutを対象にし、生成出力ありでも既定で再試行する。response_retry_partial=falseで後者を止められる。
- 理由: 数時間のtoolループで独立した一時失敗が累積し、前半の失敗によって後の復旧機会がなくなるため。出力のある途中応答も、未採用toolは実行されず、同じ応答の副作用を増やさずに作り直せる。ただし上流料金と生成時間は重複し得るため、証跡・表示・optoutを備える。OpenAI Docsの[streaming events](https://developers.openai.com/api/docs/guides/streaming-responses)に照合し、生存通知と生成出力を区別する。
- 影響: 失敗attemptと部分受信監査を同じtransactionで保存し、監査本文を次のモデル入力へ再送しない。通常の採用/CAS/停止・GC境界は維持する。Retry-Afterは600秒まで指定どおり待ち、超過は復旧へ進める。指数上限で切り詰めて早期再送しない。要約/embedding/RAGの再試行、DB schema、依存、worker/queueは増やさない。
- 再確認条件: 実proxyで連続的に復旧できない場合、固定予算と待機の実測を見直す。出力を持つ新eventや別providerのterminal契約は明示的に確認し、任意protocol失敗を通信一時失敗へ広げない。

## 2026-10-27: 復旧のベルには既存Changedのcommit後の事実を使う（Issue #12）

- 判断: 既存Changedに実際の復旧遷移を示すboolを付ける。UIのpending boolと単調時刻だけで2秒集約し、描画と同じ端末backendからBELを出す。
- 理由: 選択Threadのsnapshotだけでは背景/子の復旧と速い再開を見逃す。全Threadの永続状態監視・無限の重複排除集合・専用worker/queueは増やさず、既存の復旧CASのcommit後に通知する。
- 影響: 永続状態・wire・event種別は増やさない。既存broadcastの過負荷欠落、端末の音/点滅の無効化は到達保証の対象外とする。起動済み状態・停止・自動再試行待機では鳴らさない。
- 再確認条件: 通知の永続配達が必要になった際は、今回のbest effortと区別して目的と費用を再検討する。


## 2026-10-27: HTTP層は事実を返しDriverへ再試行判定を集める（Issue #13）

- 判断: AttemptFailureの再送許可を受信事実ResponseFactsへ置き換え、対象判定をDriverのclassify_retryへ集約する。期限・有界JSON本文・Responses受信を通常のRust子moduleへ分ける。
- 理由: Issue #11の動作を維持しつつ、EOF・HTTPエラー・transportごとの読取経路に再試行ポリシーを重複させない。完全なJSON responseの検証失敗と未終端EOFを別の事実として扱い、ModelError.kindの永続形式を変更しない。
- 影響: 新しいworker/queue/依存/永続状態を追加しない。分類関数はDriverとResponseStreamで共有し、Deadlineは時間だけを持つ。HTTP methodとJSON本文の共通手順を抽出する。clientの再利用という挙動変更は別commitに分ける。
- 再確認条件: 新event形式や別のHTTP要求を足す場合、受信の事実とポリシーを混同せず、同じ表形式の判定試験と既存の期限/原文保全回帰を通す。


## 2026-10-27: ownerの接続別HTTP clientを共有する（Issue #14）

- 判断: Control配下のStoreが、設定接続数に固定されたHttpClients表を構築し、会話・要約・索引・意味検索・モデル一覧へ共有する。各entryはclient又は初期化エラーで、mutex/動的登録/worker/queueを増やさない。独立テスト/RAGの従来constructorは維持する。
- 理由: reqwestの内部poolを要求ごとに捨てず、TCP/TLS初期化の繰返しを減らせるため。静的な接続設定と、要求ごとに読む資格情報を分ける。起動時に全体を失敗させず、使わない接続のbuilder失敗も要求時のエラーとして保持する。
- 影響: pool_idle_timeoutは45秒（提案30〜60秒の中間）を初期値とし、実会社proxyの最適値とはしない。API key/headerは送信ごと、HTTP(S) proxy/no-proxyはclient構築時なので後者の変更にはowner再起動が必要。SSEのterminal時点終了を維持し、再利用できない接続のためのdrainを追加しない。再試行は#11のDriverのみが判断する。
- 再確認条件: 会社proxyで古い接続の失敗や接続再利用率を実測できたらidle値を見直す。小さな実proxy標本から速度向上や原因解消を断定しない。全profile数/同時要求数は既存設定と所有に従い、pool保持をプロセスRSS上限とは扱わない。


## 2026-10-27: Issue #15と追加レビューの採用判断

- 会話と要約のThread UUIDは要求引数として渡す。共有clientにsessionを固定すると子Thread・forkが混同されるため、設定とpoolは共有し識別情報だけを要求単位で付与する。header既定true、prompt_cache_key既定falseで接続先依存の本文拡張をopt-inにする。同名固定headerは自動付与時だけ上書きし、opt-outで明示headerを消さない。
- 保存済みusageは同Turn最新attemptで推測せず、output_itemのnode関係から正確なattemptを参照する。既存DB workerの有限page読取だけを使い、schema・永続event・workerは追加しない。値0と未報告・不正・対応不明を区別し、UI付加情報を原文へ書き戻さない。
- #4/#9/#10のLowは文書と固定中再描画試験へ反映する。Windows端末の利用者報告は受入成功へ読み替えず、WSLENVと再起動後の新規依頼の条件を明記する。
- #11の再試行対象HTTP statusはエラー本文がtimeout/切断しても失わせない。認証・非対象status・終端のprovider失敗は引き続き自動再送しない。推論項目開始の部分出力扱いと、既定無通信例で約93分になることをガイドへ明記する。生成全体期限としては扱わない。
- #13の共通要求文脈を局所型へまとめる提案を採用する。引数の重複とpanic前提を除き、再試行方針をHTTP層へ戻さない。
- #14のSSE terminal即終了はHTTP/1.1接続をpoolへ返せない場合がある。正常terminalの検証後だけ最大1秒・64KiBの本文読捨てを採用し、終端境界と取消しを維持する。末尾エラーは完成済み応答を無効にせず、上限到達で接続再利用を諦める。過去のdrain不採用判断はこの条件付き判断へ更新する。会社proxyでの最適値は未測定で、再確認条件は遅延・接続再利用率の実測報告。
- キャッシュによる切断低減や全provider共通のroutingは保証しない。実API確認はローカルLiteLLMへの合成入力4要求に限定し、15万tokenの負荷や会社の実会話は使わない。Windows実機なしの端末操作は対象外のまま、3 OS CIを別に確認して公開する。


## 2026-10-27: Windows CIのcheckpoint観測競合

v0.1.4候補の初回CIで、Linux/macOS成功に対しWindowsの既存idle checkpoint試験が1件timeoutしました。DB workerは返信を送ってからidle処理へ進むため、別jobでtracerを登録すると更新直後のcheckpointを観測できない場合がある。またcheckpointはperiodic workerではなくjob後のidle opportunityなので、counterを待つだけでは再試行を駆動できない。tracer登録と更新を一つのfixture jobにし、意図的にeligibleな更新境界を観測する。待機helperはread jobで有限に機会を作り、callback後はactor barrierで同期する。timeoutだけを伸ばして不明な状態を隠さず、expected/actual counterを診断へ残す。製品の保存・throttle・schemaは変更しない。


## 2026-10-27: frontend fixtureの終了待ちとWindows CIの負荷

次のWindows CIでcheckpoint観測は成功し、既存management_scope試験のcleanupが10秒でtimeoutした。mutationの永続化と遅着拒否は成功済みで、終了猶予5秒は完了SLAではない。多数のfixtureの同時実行でWindows lib全体が100.72秒まで伸びた状況と整合するが、遅延原因の全てを断定しない。12か所のcleanupを直接JoinHandle awaitと5msの有限更新排出へ共通化し、busy-pollを避ける。watchdogはWindows30秒/その他15秒で失敗診断に最終ShutdownProgressを残す。forceによる成功偽装や製品shutdownの変更は行わず、猶予・forceの明示試験は保持する。

Windows CIのRust harnessは同時2件へ制限し、独立DB/IPC/file worker fixtureの同時起動負荷を抑える。除外・ignore追加ではなく全件を実行し、個々の試験内で作るTokio並行処理は維持する。再確認条件は同条件での継続的なtimeoutや最終進捗診断で、製品deadlockをfixture対策と混同しない。


## 2026-10-28: 範囲編集と切断後の縮小（Issue #16〜#18）

- #16の構造移動は`5ee28ba`へ分離し、旧手動/overflow規則と新規則を混ぜない。更新コメントの「途中範囲の省略」も取り込み、prefix専用の新状態を作らず共通omit部品で扱う。新規table/worker/queue/event/依存は不要。引数は既存receiptに付随させ、既存pending保護と回収を使う。
- 切断と大きさの関係は相関であり、会社proxyの約100秒上限は未確認。未指定の起動目安を75,000推定tokens、目標をその時点の半分とする。明示compact_hint_tokensは起動目安と目標の両方。75kは報告標本に合わせた暫定値で、context上限や全providerの最適値ではない。attemptの縮小記録と次の成功/失敗で効果を測り、別provider/多言語の精度や失敗率で見直す。
- 不十分な縮小を残さないため、新自動縮小のspill/omitは計画後に原子的に採用する。旧manual compactのspill/summary別採用は維持する。現在の依頼が途中にあればそのgroupを残し、markerに原順序と保持anchorを記録する。明示omitは範囲中の依頼を黙って除外せず拒否する。
- restore後もthread_replacementを監査/GC rootとして保持する。添付は自動復活させない。外部ファイルの現在状態が変わっている可能性があり、文脈の復元とファイル送信の再許可は別の判断であるため。通知・CLI結果・TUIに注意を残し、必要なら新しい添付IDで追加する。
- 隠れた範囲のsummaryは最初は一つのmarkerの元全体に限定する。部分省略の再分割は別markerの作り直しとanchor管理を増やすため、正確な候補を示して拒否する。需要が確認されたら既存出典/CASを維持して検討する。終点巻き戻し、focus引数、モデルの自作要約採用は今回の対象外。
- TUIの`/restore 番号`は失敗下書き復元に既存利用があるため維持し、引数なしだけ入力範囲復元に使う。特定markerはCLIで明示する。補完とhelpで二つの対象を明記する。

- 入力推定はoutbound replay変換と共有し、provenanceや内部event metadata、旧epoch reasoningを数えない。full inputとhistoryの実測文字数差で固定分を分け、同接続・モデルの直近64成功attemptから補正可能な最新値を使う。補正根拠と未測定対象を返し、部分的な推定を実測tokensとして表示しない。大きい添付の除外後は固定分の推定が過大になり得るため、縮小の採用可否は保守的になる。
- TUI概要は同Turn/generationの直近256 attemptの数値投影に限る。常時監視・新しい状態/表を増やさず、長いtool-loopで範囲外になった場合は未取得として扱う。markerと元attemptの保存情報は保持する。

- 最終レビューで2点を修正する。引数なしcompactの連続受付は従来どおり一度の処理へまとめ、明示編集は必ず独立実行する（明示編集を越えてまとめない）。余計な要約API要求を増やさず、旧downstreamを維持するため。引数なしrestoreは経路の後方ではなく、選択中で最後に作成されたmarkerを選ぶ。途中範囲を後方→前方の順に省略したときも直前の編集を戻せるため。nodeの既存挿入順を参照し、専用履歴stackや状態を追加しない。


## 2026-10-28: Issue #16〜#18のレビュー対応

- #16のコミット再分割は行わない。純粋な構造移動は`5ee28ba`で既に分離済みで、共通validator・Snapshot採用fence・後続2機能とその試験は`e8153f6`で一体として検証した。レビュー記録・Issueコメント・検証結果がこのhashを参照しているため、その対応を保持する。共通検証を先行commitへ抽出して履歴を書き換えるより、レビュー修正を別commitにし、今回の境界共通化と後続の差分を追跡できる形を採る。これは#16レビューMediumが認める「分けない理由の記録」で解消する判断。
- #16 Lowの最新group/現在依頼の保護規則は、計画と検証が同じ`trim_group_protected`を使う。#18 Lowの端点変換失敗でも、omit/trimの候補はそのmodeで実行できるものだけを返す。
- #17の通知には、保持した現在依頼より後ろの省略node数を明示する。位置に印を増やす代わりに、同じmarkerへ作業途中も省略した事実と副作用未取消し・再実行前の現状確認を記す。新しいnode/stateを増やさず、復元の順序を維持する。
- #17の旧attemptの文字数不足は、未計測として扱い続ける。過去のprovider入力を現在の経路から推測復元したり、tokenizer/言語判定/移行処理を追加したりしない。日本語中心の旧会話は文字数/4で過小評価し得るため、最新の実測input_tokensも確認し、必要なら明示compactまたは小さいcompact_hint_tokensを設定する。新しい成功要求の計測が揃ってから補正へ移る。旧会話の推定を正確と主張せず、実proxyでの効果は引き続き未実施とする。
- #18 Lowの明示summaryは、反映済みの現在依頼も指定可能な契約を維持する。ユーザーまたはモデルが正確な範囲を選んで意味を要約へ保存する明示操作であり、自動の省略とは区別する。未反映receipt・未完結tool groupの共通保護は維持し、自動compactの現在Turn保護は変更しない。通常は現在依頼を範囲外にするようガイドで案内する。
- 復元の通知は特定markerを指すよう、既存`--from`に省略元の最初のIDも受け付ける。選択marker IDを優先し、元IDの複数候補は拒否する。引数なしは最新作成markerのまま。添付警告は実際にactive参照が除外されたときだけ出す。


## 2026-10-28: 一覧からの会話削除（Issue #19）

- DeleteとCtrl+Dを採用する。macOSの通常deleteはBackspaceであり絞り込み編集に既存利用されているため維持し、fn+deleteかCtrl+Dを使えるようにする。通常の文字dはフィルター用のまま。
- 既存ThreadDeletePreview/Deleteと確認popupを使う。子の会話も削除する指定は既定falseで、切替え後は最新previewが届くまでYを無効にする。子の存在だけをUIで独自blockにしない。Coreは親だけを削除し子を残せるため、その実際の意味を変えない。
- Core削除結果はAcceptedでID一覧を持たない。Core契約を増やす代わりに、削除成功後、UI既知のIDだけをDB workerで照合して削除済みview/下書きを破棄する。新しい全子孫一覧APIや永続削除jobは不要。UI-localの要求/返答には対象・descendants・要求IDを持たせ、既存runtimeの寿命と有限admissionを使う。
- 削除された現在会話からは新しい下書きへ移る。背景会話では一覧を開いたまま隣行へ移す。確認を閉じても受理済み削除は取消したことにせず、成功のcleanupは適用し、閉じたpopupは再表示しない。保存途中の古い下書きや画像所有が会話を復活させないよう、既存所有/世代とbacklog回収を再利用する。

## 2026-10-28: Issue #19〜#23の対応範囲

- #19は削除確定とFrontend後片付けの結果を分ける。後片付けの失敗でCoreの削除事実を否定せず、確定済みrootと照会できた子だけを整理し、未観測の子を推測しない。UI-localの有限inflight IDで重複操作を防ぐ。historical generation fenceを全て削除観測の候補に足すことはやめ、現在UIが所有する有限候補へ限る。Coreの操作/表/worker/eventは増やさない。
- #20は既定除外とunsupportedのskipped分類を不具合修正として採用する。完全scanで新たに対象外となる旧active資料はremovedへ移すため検索から減り得るが、原本を消す処理ではなく現在の対象定義へ同期するための判断。除外忘れによる復活を避けるためpatternはsourceに保存し、解除は明示する。新しい独立RAG列とatomic migrationだけが必要で、会話schemaは変えない。glob構文を自作せず [globset 0.4.20](https://docs.rs/globset/0.4.20/globset/) を固定追加する。これはstandalone RAGでだけ使う有限pattern照合で、Core機構は増やさない。gitignoreのネスト・否定は今回範囲外。
- #21は明示hybridの準備不足だけを成功したkeywordへのdegraded結果として返す。従来の全てのscope_errorで非0終了という契約を、確認済み未準備warningだけに限って変更する。利用者とモデルが結果を使えることを優先し、scopeごとに使用手段・理由・次の索引操作を示す。本当の実行失敗や設定不正は隠さない。別の人間向け出力modeはJSON統一の方針に反するため採用しない。
- #22の新しい操作結果保存表やCore receipt連携は採用しない。既存shell出力と既存RAGの永続状態で取得・追跡できるため、status投影とガイドを補う。最終走査時刻と内容更新時刻は区別し、embedding作成時刻の新列は作らず現在設定の不足件数を示す。
- #23は提案1と3を採用し、2のJSON重複除去は今回は見送る。共有nodeの生成元context.thread_idとfork先selection.thread_idは同じとは限らず、ExecutionContextとPageは他の公開操作/構造表示にも利用されている。一覧だけの派生型と後方互換投影を増やすより、既存形を保ち抜粋制限とSQL長さだけを追加する。0指定は本文の転送/解析を避けるが、provenanceや実行リンクが残るので16KiB/50件を保証しない。実測と意味の異なる長さの説明をガイドへ残す。

## 2026-10-28: 0.1.6へ対象拡大（Issue #24〜#30）

- 利用者が#19〜#30完了、push、0.1.6公開、常用binary更新をgoalとして明示。初期5件を検証済みcommitへ分け、#24は#20の対象選択へ重ねて内部形式変換を削除する。外部変換の主経路とモデル解釈の代替経路を分け、原本hashと生成側申告を混同しない。原本の大きさは保存量ではないため固定bufferの逐次hashで16MiB制限を外し、保存する本文/metadataの上限は保持する。
- #25〜#30のレビューの推定値を性能の証拠とは扱わず、同じ合成fixtureで変更前/後を測定する。#25/#26（経路と索引）、#27/#28（読取り隔離と通知）、#29/#30（描画と配送）を並行準備し、Cargo/Gitはrootで直列管理する。新しい表・汎用pool・worker engineを作らず、追加する有限admissionや通知の所有/終了を各設計へ記録する。
- #26コメントの未起票項目（全DB使用量counter、retention/VACUUM、履歴keyword FTS、下書き/一覧全体の再設計）は今回の範囲へ広げない。必要な索引はEXPLAINと測定に根拠があるものだけ採用する。

## 2026-10-29: #24〜#30の採用境界と最終検証

- #25は既存node採用の末尾追加だけを共通化し、任意経路の置換APIを増やさない。revision/CAS、末尾とcursorの整合、順序、既採用nodeのno-opを維持する。既存edgeのINSERT前triggerを再実行しないSQLにし、新規cycleの拒否は保持する。
- #26は会話DBの3索引と独立RAGの2索引を採用する。実際のEXPLAINと同一fixtureの比較を根拠にし、app_metaの不正JSONに対する認可の意味を変える部分索引は採用しない。schemaは会話10/RAG3で、起動前バックアップと旧版への直接復帰不可を公開時にも明記する。
- #27はIndexService所有の1検索slotと既存runtimeのreadonly blocking処理に限定する。取消し後も実処理終了までpermitを保持し、owner終了でdrainする。ランキング後に新しいsnapshotで出典/可視性を確認するため、検索自体の高速化や長期snapshot保証とは扱わない。GCの集計分離は同じtransactionの回収判定と一緒に別途検討する。
- #28は出力Notifyをownerに1個追加する。終了待ちをraw出力ごとに起こす負荷を除き、ShellOutput待ちのexecution別分配mapや専用workerは作らない。
- #29は既存16MiBのlayout予算内でrender unitを共有する。高さ変更は再描画のcache keyにしない。全可視nodeのresize取得と、本文window再取得のpreflightを保持する。最終試験が検出したwindow再読込の退行は期待値を緩めず修正した。debug測定のp95は約52msであり、常時30fpsの保証とはしない。
- #30はfrontend所有の有限outboxとUI専用の有限broadcastを採用する。通常受付を抑制する閾値と既受理結果の予備枠を設け、重要な結果をcoalesceしない。例外的な上限ではbackpressureを残し、停止/終了のpriorityと強制終了を維持する。詳細な所有・上限・切断・遅着・終了は採用設計へ記録した。
- 性能はdebug build・合成DB・loopback HTTPで測る。会社proxyや外部モデル、Windows端末の体感を推測しない。3 OS native CIと配布物の実取得はローカル検証から分離して記録する。

## 2026-10-29: 配布runnerの更新と既存tagの再ビルド

- 0.1.6の3 OS CIはすべて成功したが、配布用macos-14 jobが10分以上queuedになった。GitHubの公式告知 [runner-images #13518](https://github.com/actions/runner-images/issues/13518) で、2026-11-02の廃止と、それに先立つ待ち時間増加を確認した。待機の唯一の原因とは断定しないが、4日後に使えなくなるrunnerを維持しない。
- 配布runnerをmacos-15へ更新し、macOS deployment target 14.0を明示する。実行OSを対応最低OSと混同せず、配布物のMach-O load commandも確認する。14実機の動作確認とは扱わない。
- 公開済みtagを書き換えず、既存の安定tagをcheckoutするworkflow_dispatchを追加する。v0.1.6の製品sourceは3 OS CI済み025388cのままで、mainのworkflowだけを直す。tag/version/native hostの照合、全3 archive/checksumが揃ってから公開する規則、公開済みreleaseの上書き拒否を維持する。再実行でもdraftのrelease notesを保持する。

## 2026-10-29: English defaults and Japanese user guides

- Interpret the English default across all product guides, not just `user-guide.md`: sending English readers to Japanese-only feature guides would leave the requested navigation incomplete. Preserve all twelve user-facing originals as `.ja.md`, sharing one English-only Agent Guide. Root README/AGENTS and documentation entry points are English. Historical design/development/verification records are not bulk-translated; new records are English.
- Keep the existing thirteen bundled IDs and three categories, with English text and metadata. Do not add a docs locale API or duplicate every guide in semantic indexing. Japanese copies are source/archive files; mark links as repository references and explain literal English search in both languages. Existing version hashes invalidate old cursors/index readiness normally; no automatic paid rebuild is authorized.
- Recommend English-default UI with explicit Japanese startup selection, preserving one renderer and the current Japanese copy. This needs only presentation-layer resources/settings in a future task. Do not implement it, add an unsupported config field, change stored error/domain values, or force a response language in this task. Live switching, OS locale detection, per-Thread locale, and more languages have no current requirement. The [comparison](../design/documentation-language.md) records costs and later acceptance criteria.
- Translation exposed an existing shell-guide typo: ASCII `SHELLBOY_BG_READ_V1` is 19 bytes, not 15. Correct both languages. Also remove the contradictory claim that CLI has no preview-only command immediately before its `index preview` example; CLI lacks the TUI confirmation screen, not the preview command. Remove the fixed operation total in the User Guide while retaining access to the full catalogue. (Correction from Issue #31: the TUI total of 36 was accurate; removing a duplicated count was a maintenance choice, not correction of a stale value. The CLI count of 37 describes a different catalogue.) Update a previously stale Control-to-HTTP heading link and translation-affected heading links.
- No UI implementation/configuration files, Core API, migration, dependency, model prompt, or user data change. Validate guide fidelity, links, TOML examples, actual offline catalogue, and docs/RAG behavior; do not run paid discovery APIs or claim real-terminal/other-OS verification for a documentation change. Local commits only; the preceding release authorization covered the completed 0.1.6 release, not publishing this new task.


## 2026-10-29: Reopened #19/#20/#27/#30 boundaries

Adopt all four Medium findings against the existing contracts. Confirmed deletion
is independent of cleanup success, but a cleanup write failure still stops the
owner. A per-file extraction failure is independent of complete-scan removal of
other keys. A readonly search failure is independent of writer durability.
Transient UI state and worker results need one ordering boundary.

The [reopened-issue plan](reopened-19-20-27-30.md) records every Low disposition.
Use one shared, finite UI outbox rather than adding a dispatcher task or Core
state. Keep the semantic admission slot across embedding to avoid charging for
queries that cannot be admitted; document parallel-child contention and retries.
Keep incomplete RAG walks conservative for all updates. Preserve Windows legacy
file-key identities where possible without changing POSIX backslash semantics.
Serialize UI deletions rather than querying an unbounded parent/descendant graph.

Defer replay of a live prefix filtered before a queued thread-selection action
is handled: offset gaps and canonical terminal refresh remain the recovery path.
A separate Core replay buffer/handshake adds ownership and retention rules for
transient presentation. Revisit if canonical history cannot be recovered. Force
quit may abandon UI delivery, but must not silently enter another grace or wait
for UI queue capacity after it has already been accepted.


Cross-review also identified inherited exceptional backpressure at all 512
critical entries: one frontend handler can wait before polling priority controls.
Preserve finite FIFO rather than dropping a result or bypassing the shared queue.
Document the responsiveness limitation; removing it needs both a finite main
reserve and incremental batch continuation, not a larger queue. The ordinary
pressure test is not evidence for every exhausted-reserve case. This follow-up
is tracked in the reopened-issue plan and known limitations.


## 2026-10-29: Remove #20 Windows legacy-key compatibility for 0.2.0

Issue #20 comment `6480908316` clarifies that the next release is 0.2.0 and does
not carry forward old-version migration/compatibility. This overrides the initial
Windows-key preservation decision above. Remove the native-key lookup and its
two preservation tests rather than carrying compatibility-only database queries
and identity rules. Keep current slash normalization, failure/removal separation,
root-glob protection and case-sensitive matching unchanged. Legacy key identities are not preserved; ordinary complete synchronization registers
current keys under new identities. Database recreation is unnecessary (clarified
by the subsequent #20 re-review); slash normalization shipped in 0.1.6. Do not
open, reset or convert user data automatically. No version
bump or release is performed as part of this targeted follow-up.


## 2026-10-29: Issue #31 documentation review

Accept all four Medium findings. English literal search needs English discovery
terms in recovery hints, Agent Guide and RAG guidance; Japanese quoted labels
remain searchable without query translation. Keep the system prompt unchanged:
it already routes models to Agent Guide, whose explicit English-search instruction
fixes discovery without a new prompt or language state. Restore the full strength
of Windows unverified-status and cache-invalidation instructions. Restore the
AGENTS requirement to reflect discovered design discrepancies, including existing
ones beyond intentional contract changes, before changing code or fixtures.

Quote the actual current Japanese TUI labels in both languages, with English
explanations, rather than translating navigational targets into nonexistent UI.
A narrow regression compares documented operation labels with the real OpKind
labels; an actual-binary regression follows the InvalidArgument recovery search
and requires useful shell examples. No runtime UI language implementation changes.

Resolve all eight Low findings in this task. Correct the earlier count-removal
rationale: F8 has 36 operations and CLI has 37; the former was accurate. Keep the
User Guide free of a duplicated total for maintenance, without calling it stale.
Add all missing user-guide sibling links, state busy InvalidState and rejection
before charge separately from cancellation, remove change-relative product prose,
restore version/catalogue qualifiers, correct English and zero-budget/expiry
wording. Correct the existing deprecated alias help without changing alias behavior.
Define documentation entry points explicitly and translate the small design and
validation indices; existing design/validation records are not bulk-translated.


## 2026-10-29: Follow-up to reopened-issue re-review

Adopt the small boundary fixes: explicit storage-stop wording, local deletion
unwind-to-uncertain-result handling, unavailable symlink root, readonly-specific
repair/charge hints, and one shared UI Notify. Keep existing workers, permits,
result channel, cleanup ownership and Core contracts. Add the missing complete
frontend cleanup-failure test and actual direct-file removal test. Label the lock
fixture as rollback journal, not WAL. Retain current #31 guide fixes when updating
search and RAG wording. AGENTS now records the requested pre-1.0.0 no-shim policy;
general migration/protocol-version mechanisms are allowed. Existing one-off code
inventory is a separate forthcoming review, not a reason to add more shims now.

Correct the earlier #20 “recreate indexes” phrasing: users need not recreate the
DB. Complete sync deactivates old keys and creates new identities, without
preserving legacy IDs/keys; slash normalization is already in 0.1.6. A persistent
file failure still returns partial_failure/nonzero even with other applied work.

Accept best-effort ForceShutdown presentation when explicitly forcing termination:
nonzero forced result/error remains reliable, while waiting to guarantee the UI
marker would break the one-request force contract. Create Issue #32 for exceptional
full-reserve priority responsiveness. This remains separate from the Notify wakeup
fix; neither ordinary pressure tests nor FIFO exhaustion tests prove its solution.


## 2026-10-29: latest agreements and #32 / #33 supersede compatibility choices

Adopt the [complete tracking plan](issue-32-33-plan.md). This supersedes earlier
entries preserving pre-G5 keys, inferred query markers, deprecated status aliases,
and runtime old-owner optional fields. Those entries describe earlier decisions,
not the current contract. Keep the general schema runner, immutable historical
SQL, ledger/user_version/sqlite-vec checks and strict protocol validation. Isolate
all startup transforms in removable migration modules; initialize fresh databases
from the current schema independently. Old readonly schemas are rejected without
conversion. Conversation schema stays 10, RAG stays 3; wire protocol becomes 2.

Only User Guide Japanese is maintained. The earlier twelve-pair language scope
and prescribed English search queries are superseded by the full #31 comment
agreements. Catalogue language and query-language choice are separate concerns.
UI language work remains an evaluated future task. The intended next release is
0.2.0; source package version stays at the last release until release preparation.
No push/release or user-data mutation is part of this work.


## 2026-10-29: review round 4 persistence and current-format upgrade

The [round-4 tracking record](review-round-4.md) supersedes remaining pre-upgrade
assumptions in the earlier #32/#33 plan. The new reviews confirm #19/#20/#27/#30/
#31 fixes; no change is justified solely to rewrite already accepted behavior.

Use schema 11 for the derived failure-counter format boundary. A startup-only
migration removes malformed/non-object/wrongly typed counters, preserves current
objects and existing pauses, and drops the unused receipt_turn_source index.
Fresh schema and historical upgrades must converge. This is preferable to a Core
scalar fallback: public v0.1.x wrote integer reset values while remaining schema
10, so accepting that schema without normalizing derived metadata stranded work.
No content, vectors, authorizations or immutable interruption nodes are rewritten.
Current malformed counters still fail strict decoding; a separate transaction
preserves the provider failure and fixed input, marks work failed and pauses
spending. General worker failures also acquire an existing durable pause; if even
that diagnostic cannot be saved, stop the index workers rather than keep billing.
If both normal and diagnostic DB writes fail, preservation of the original
provider failure is not guaranteed. Avoid adding another diagnostic queue/service
for that exceptional storage-failure boundary; revisit if concrete operational
failures show the existing error/pause reporting is insufficient.

Keep immutable versioned SQL in conventional root migrations/. Only the isolated
storage::migrate imports historical SQL in src; ordinary Core tests use current
schema. Explicit migration integration tests retain old SQL intentionally. Moving
all SQL under a custom module directory adds path churn without improving runtime
isolation. Removing the runner in future requires removing its declaration/call;
obsolete SQL can then be removed alongside the explicit migration fixtures.

The metadata audit finds no further required startup conversion: public Work
writers already supplied the now-required fields and public draft writers supplied
positions. Invalid versioned GC cursors restart safely. A missing query-profile
marker cannot be inferred without configured endpoint identity and remains an
explicit pre-HTTP error. Saved mutation receipts remain scoped to their original
owner instance, so changed draft fingerprints do not authorize old-instance replay.
The old interruption payload key remains an intentional display-format break;
known limitations describe raw/F6 access rather than a fallback reader.

For #32, persist accepted closing sends and drafts before presentation-only
shutdown waits. Omit closing Accepting projection: after Quit the main loop no
longer handles its completion, and this transient label must not gate persistence.
Normal sends retain immediate Accepting. Keep autosave pumping independent of UI
reserve; retain only its failed-thread projection while snapshot ownership stays
in the existing backlog. A repeat bit on existing pending receipts preserves a
fair scan before starting a finite follow-up pass, including lag recovery. Clone
always yields worker admission; shutdown borrows the unique main handle. No new
Core operation, table, channel, task service, dependency or background poller.

Use rag init as the explicit writer-owned upgrade for an existing RAG store;
it does not register a source, index documents or call a model API. Protocol
mismatch reports both versions and same-release recovery; only endpoint/instance
change recommends refreshing/reconnecting. Preserve uncertainty after a request
was sent. Earlier guide old-record notes are replaced with actual unknown-state
conditions. The pre-1.0 policy permits isolated schema upgrades and forbids Core
compatibility shims, rather than forbidding migration itself.

Validation fixture decisions: stalled mock HTTP was also reproduced on baseline
`e977dfe`, before this follow-up. In the index fixture, the seed request stalled
before the migration/workers under test, with a small queued server response.
Use TCP_NODELAY on accepted local mock sockets and write JSON response headers
and body together in the index/shell fixtures. Apply TCP_NODELAY to the two
summary fixtures as well. Keep assertions, deadlines and production HTTP clients
unchanged. The index worker regressions pass with these fixture changes. The later #33 review supplied independent raw TCP tests proving a roughly
16KB loopback transfer stall and recovery after Mac restart. Root repeated
successful transfers up to 10MB after restart and the exact 6c577c8 standard
check/HTTP-PTY now pass. The original packet-timing explanation for 8368874 is
not established; compare the unchanged baseline without those fixture changes. Record unsuccessful
whole-suite/manual attempts rather than treating earlier runs as evidence for
the current source.

The saturated shutdown test sends forty messages to forty independent existing
Threads held in recovery_wait. This isolates UI delivery from valid same-Thread
selection-revision CAS conflicts. Assert exactly forty durable receipt/user-node
pairs and draft revision 99 before forcing the blocked presentation to exit;
do not reduce the expected count to accommodate rejected concurrent requests.

RAG migration assertions use the new actionable rag init hint and preserve the
original source count (including built-in sources). Existing readers must still
reject without modifying the old store. Both v1 and v2 writer-only migration
regressions pass, without registering material sources or indexing.


## 2026-10-29: early maximum reduction and truthful HTTP observations (#34)

A failed large input is reduced at its first eligible failure rather than after
eight more attempts. Reduction removes every eligible group outside existing
protection, not an arbitrary half-size target. Keeping a particular token count
has no established benefit; immutable originals, replacement provenance, scoped
keyword search, selected summaries and explicit restore provide recovery paths.
The latest completed tool group, current request, incomplete work and pending
user instructions remain protected. One adopted reduction/restart belongs to a
Turn/generation; another failure goes to recovery rather than consuming another
paid retry cycle. Only an explicit compact_hint_tokens enables pre-POST reduction;
it is trigger guidance, not capacity inference or a reduction target.

Transport observations must distinguish facts from inference. A Body stream
reports how much source reqwest consumed, not a TCP write acknowledgement or
proof that the peer read/processed it. The public client API does not expose
exact socket reuse. Record known incompleteness, unknown delivery while waiting
for headers, and response-body reception separately. First use of a fresh pool
is a pool fact, not a promise about every later connection. Replacing a failed
pool generation prevents subsequent requests from borrowing its idle sockets;
it cannot promise immediate closure of an HTTP/2 connection still used by other
requests. Keep those limitations explicit rather than add another transport,
dependency, worker or per-connection lifecycle engine.

Issue #11 retry-count reassessment: retain the default of eight consecutive
retries provisionally. The supplied incident demonstrates that retrying the same
large input eight times before trimming is wrong; it does not establish the
optimal retry count for smaller transient failures after this change. No
representative successful production retry dataset is available in this task,
and routine verification must not call paid APIs or inspect real conversation
content. Loopback tests validate retry mechanics and bounded request counts, not
provider recovery rates. Revisit with anonymized attempt number, phase, error
category, header latency and elapsed waiting after these observations have been
deployed. The production-data part of item 7 remains explicitly unmeasured.

The maximum-omission rule is structural: omit all eligible groups, including
when verbose provenance/notification overhead means a tiny removed prefix does
not lower the estimated token count. Report the actual before/after estimates;
do not claim a numeric reduction in that corner case or restore the removed
content automatically. The once-only resumption bound still prevents a loop.
Revisit marker size if measurements show its overhead defeats recovery on
realistic large inputs; do not reintroduce an arbitrary target/all-or-nothing
planner to address a synthetic tiny-prefix case.

A failed shared HTTP pool generation gains only two bounded counters and its
existing client owner; the lock is held for snapshot/replacement, never across
await. Retired clients are retained only by already in-flight requests, bounded
by existing request concurrency, not a new registry or background reaper. Proxy
environment variables are read from the owner process when a pool is built or
rebuilt. Editing a different shell's environment still cannot reconfigure a
running owner; the documented practical procedure remains owner restart.

Additional #32/#33 review decisions: stop pumping a known-closed draft worker
after its first critical notice while retaining newest unsaved snapshots for
shutdown failure reporting. Reset receipt repeat bits only at a fresh scan, not
during an unfinished pass. Expose a bounded in-memory index storage-stop reason
when the diagnostic itself cannot persist, and report actual RAG migration
versions. Keep the already conservative uncertain reply-ID diagnostic and the
existing historical-language sections; the reviewer marks both cosmetic points
as optional. Same-Thread concurrent-send CAS rejection is not covered by the
independent-Thread shutdown persistence claim and is outside this delivery fix.

Header deadline decision: default response_header_timeout_ms is a positive
120,000ms, separate from the 600,000ms body-idle deadline. The opt-in local
HTTP/1.1 measurement sent eight 1,024-character requests and eight
3,480,000-character requests successfully; the latter had p50 2ms / max 3ms
from send initiation to headers. Full samples are retained only in ignored
synthetic evidence. These loopback measurements establish instrumentation and
large-body delivery after restart, not provider queue/prefill behavior. The
issue reports successful generation around five seconds and possible header
latencies of tens of seconds. Choose two minutes as conservative initial
headroom for that reported range while cutting the previous ten-minute header
wait; no statistical optimum or provider capacity is inferred. Reassess this
provisional default with representative deployed successful/failing header
observations. An explicit longer positive value remains available.


## 2026-10-29: native CI and portable bounded-delivery fixtures

Initial run 40795744408 on df6aafb found Linux/Windows Clippy's while-let-loop
lint; 6f19b89 applies the equivalent while-let form. Its local strict Clippy and
14 delivery regressions pass. The first macOS job separately hit two five-second
attachment preparation watchdogs; post-timeout state was recovery_wait with no
model attempt. Local eight-way parallel reproduction (200 cases) did not fail,
and the next native macOS run passed with deadlines/assertions unchanged. No
cause is established for that isolated event; it is distinct from the earlier
local raw-TCP loopback fault.

Run 40796272377 on 6f19b89 passed Linux and macOS but found three Windows FIFO
fixtures timing out. Each drains a synthetic one-slot raw channel over 512 items
with a one-millisecond timer wait per iteration and a five-second watchdog. The
pinned Tokio Sleep documentation explicitly permits resolution larger than one
millisecond on Windows. Those timing assumptions are unnecessary to prove FIFO,
capacity or permit ownership; the runner's exact timer quantum was not measured.
Replace the three per-item sleeps with cooperative yields. For completed
shutdown, require the first resumed critical transfer to occur without fixture
flush, then explicitly flush after capacity is released for the remaining bulk
transfer. Preserve the five-second deadlines, all IDs/counts, ordering and permit
assertions. Dedicated producer wake/Notify regressions remain unchanged. No
production queue, capacity, interval, dependency or global timer setting changes.


## 2026-10-29: explicit standalone RAG writer lease release

The third native run passed Linux and Windows, including the corrected FIFO
fixtures, but found two RAG-selection failures on macOS. Local 14-way parallel
reproduction confirmed source add/re-registration could return busy immediately
after an awaited preceding command. Temporary probes established flock
WouldBlock/errno 35, not Interrupted/EBADF. All prior File destructors had
returned before API completion. A matching-inode FD scan and subsequent lsof
found no other retained user FD/process; that does not establish the underlying
kernel mechanism. This is distinct from the old loopback fault and from the
first isolated attachment watchdog observation.

Use a conventional private WriterLock(File) owner guard, one existing handle
per write command. Its Drop explicitly unlocks before closing the handle; Store
drops its SQLite connection before this last field. Thus logical lease release
does not rely only on the lifetime of every reference to an open file
description. No retries, sleeps, global mutex/registry, table, queue, worker or
dependency are added. A deterministic regression retains a duplicate handle,
checks live-writer exclusion, drops Store and reacquires while that duplicate
remains open, then verifies closing the old duplicate cannot release the new
owner's lease.

Old uninstrumented parallel tests failed three of the first six rounds; after
the explicit unlock, 20 rounds x14 cases passed without retries or relaxed
assertions/deadlines. Initial registration and status fixtures now assert their
prerequisite successes and report useful diagnostics instead of hiding a busy
error behind a later null unwrap. All temporary logging, FD probes, lsof calls
and diagnostic guard types are removed. Revisit with exact OS/unlock/holder
evidence if contention recurs; do not label an unobserved descriptor leak or
precise kernel cause as proven.


## 2026-10-29: IPC failure notification versus owner-cleanup watchdog

Windows CI run 40802346281, job 123213135953, failed the permanent-listener
frontend fixture only at its final two-second bridge join. Its earlier
two-second failure-notice check, local database mutation and Quit submission
completed. LocalServer publishes the failure watch after listener termination
and endpoint cleanup, so that notice establishes the listener task has already
finished; the final join additionally drains Control and closes SQLite, diagnostic
logging and owner filesystem resources. DataOwner's diagnostic shutdown may
legitimately wait the default five-second grace by itself. A two-second whole-owner
cleanup bound is therefore not the IPC-notification contract being tested.

Keep the two-second notification deadline and original liveness/error assertions.
Use the existing frontend fixture's 15-second (30 on Windows) owner-cleanup
watchdog only for the final join, drain bounded updates as a real UI does, retain
a duplicate-notice count across that drain and report last shutdown progress if
the watchdog expires. Also assert the database worker is actually closed before
return. No product timeout, grace, lifecycle ordering or native Windows success
is inferred or changed. This diagnosis does not identify which individual
Windows cleanup operation exceeded two seconds; revisit with the recorded
shutdown progress if the lifecycle-sized watchdog also fails.


## 2026-10-29: attachment fixture preconditions and native watchdog diagnostics

Two attachment cases in Windows run 40802346281 exceeded the shared incoming
request watchdog. The old panic identifies neither the caller nor whether the
Driver is preparing, recovering or awaiting HTTP. No production transport,
retry or attachment cause is established. Keep the five-second watchdog and
all semantic assertions; do not add retries or alter product code on that
evidence alone.

The synthetic ordered-content setup now confirms the exact accepted Turn
entered recovery for the intended invalid-UTF-8 AGENTS error, with no saved
instruction snapshot or model attempt, before editing synthetic source content
and issuing RunRetry. Generic absence of active Turns is insufficient evidence
for that particular setup condition. Each incoming watchdog now captures its
real caller and, only after failure, obtains a separately bounded one-second
saved-state diagnostic including synthetic recovery/attempt errors. No success
path wait or product logging is added. If native failure recurs, use that phase
evidence before deciding whether a production fix or fixture change is needed.


## 2026-10-29: completed-pass identity, per-Thread save failure, index stop evidence

Review #32/6486175965 identifies a completed receipt pass whose retained checked
bits become indistinguishable from an actual partial pass after another send.
Clear checked bits when continue_scan returns idle; retain repeat/fairness during
actual partial scans. A completed scan plus a later accepted receipt/Changed is
one fresh pass, not a partial pass and a duplicate full pass.

The stopped autosave worker still has one lifetime closed bit, but its existing
bounded backlog now stores one notification bit with each retained snapshot.
Warn every affected Thread once, including a Thread edited after closure; newer
revisions retain that bit. Do not add a separate Thread registry or notification
queue. Backlog ownership/limits and final SaveFailed remain unchanged.

Review #33/6486176260 requests more useful local stop evidence. Replace the
boolean with a once-published pair of existing ErrorCode values for processing
and diagnostic-pause failures, before cancelling workers. Two enums are bounded
and contain no provider/SQL/user text. Preserve any existing durable reason
verbatim and append local cause codes for readable status; search error details
carry those codes. The once cell has the existing service lifetime; no durable
state, queue or worker is introduced.

## 2026-10-29: recurring attachment watchdogs are unresolved preparation evidence

The same compacted-source and corrupt-source cases exceeded watchdogs on macOS
and Windows in separate native runs. A shared Driver/preparation issue remains
a credible hypothesis; successful local probes and one green native run do not
resolve it. The original Windows incoming helper did not identify which POST
was missing, so neither the precise common stage nor separate causes are proven.
Earlier statements separating this from the old raw-loopback fault must not be
read as establishing independent causes for the two attachment occurrences.

Retain the five-second attachment watchdog and exact accepted-Turn preparation
precondition from b8f1f84. Run multiple native gates on the current fixes, count
them as observations, and inspect caller/state/reason/attempt evidence before
changing production behavior on recurrence. Never retry until green and call
that a fix. Bounded shutdown stage timestamps separately address the IPC cleanup
measurement gap; a five-second configured diagnostic grace is an upper bound,
not evidence that diagnostics caused the earlier two-second timeout.


## 2026-10-29: retry and scope review boundaries for v0.2.0

Review #34 separates the once-per-Turn/generation trim allowance from its
immediate resumption guard. Spend the allowance only after an adopted edit;
clear the guard after the first durable response adoption, preserving ordinary
later retry and context-overflow recovery. An actual partial body source may
trigger one safe reduction for large input; before-body/connect failures follow
ordinary retries. A second known partial failure still stops without another
trim, even if the first edit was rejected. Keep the pre-header deadline inclusive
of upload time and explain this combination rather than imply a separate upload
delivery guarantee.

Optional proactive edits are not prerequisites for a valid request. Ignore
non-storage cancellation/rejection and let stop and existing generation/profile/
selection admission fences decide; preserve storage failures. Use actual prepared
input counts with bounded calibration before planning, and retain typed planning
outcomes so changed path estimates cannot rewrite the activation reason. Adopt a
bounded oldest whole-group provenance window when the automatic restore metadata
cap permits some reduction; never split a group or exceed protected boundaries.

Scoped keyword membership now walks indexed reverse replacement consumers per
candidate rather than rebuilding the full scope. It still scans candidate IDs
up to the current global watermark and may traverse deep ancestor chains; do not
claim constant-time or a latency SLA. There is no materialized ID cache, temp
write, new index or persistent search state. Overlapping history scopes preserve
all memberships but contribute the best single rank vote per retrieval method;
keyword and semantic remain separate evidence. Revalidate live Thread existence
after embedding before classifying a selection change.

Only appropriate failed Responses requests retire their pool generation. Build
the next client on the existing blocking executor, install under a short lock,
and never cache a failed build as a permanent pool value. Existing request
admission bounds concurrent rebuilds; a racing rebuild installs only into a
still-retired pool, while generation checks reject stale failure retirement.
Exact TCP delivery/reuse remains unknown. Shutdown observations reuse the
existing bounded writer/event with fixed enum facts and per-scope elapsed time;
keep that writer alive through endpoint cleanup, close it last, and preserve
Control/DB/endpoint/owner-lock ordering and error precedence. The writer cannot
log its own completed join; its last begin record is intentionally not an End.


The former public trim_disconnect wrapper had only four unit-fixture callers
and kept obsolete snapshot-before-threshold logic. Remove it and its dedicated
failed-input estimate field rather than retain an unused Core compatibility
path. Existing maximum/no-candidate fixtures use trim_activated; actual prepared
input calibration is tested directly. The duplicate wrapper-only small-context
case is covered by real Driver retry tests, which now assert unchanged original
content/path, no replacement and no irrelevant trim error annotation.


## 2026-10-29: bounded native repetition instead of repeating all-target builds

For the unresolved #33 attachment symptom, run the full native gate and then
exactly two additional attachment-provenance test processes on every native OS.
Each process gets fresh temporary owners/stores; keep all cases, assertions and
five-second watchdogs. A failure stops the step immediately, so this is fixed
sampling, not retry-until-green. The existing three-job matrix/queue/production
code is unchanged. This concentrates repetition on the actual recurring cases
instead of spending another full all-target link/build cycle merely to repeat
unrelated tests. Earlier/new full native jobs provide separate-run observations;
repeat-process evidence within a runner is explicitly identified and does not
prove the original cause or absence of a rare failure. Report standard totals
separately from the repeated executions.


## 2026-10-29: a three-attempt connectivity fixture is not a five-second SLA

Windows run 40810247300 passes all production/other test cases but times out
while waiting for three saved closed-endpoint failures within five seconds.
The fixture explicitly permits 3,000ms per request and 100ms retry delay, so
three legal attempts alone can consume 9,300ms. The original aggregate watchdog
cannot cover its own configured behavior. Keep all per-request/product deadlines,
retry limits, immediate recovery-state rejection, minimum attempt count, retry
numbers, no-trim and final-completion assertions. Derive observation time as
5,000ms fixture preparation allowance plus three times (3,000+100ms), totaling
14,300ms. After listen, allow one pre-existing failed attempt and the next
successful request: 5,000+2*3,000+100 = 11,100ms for accept/completion observation.
These are fixture observation budgets, not latency requirements or new product
settings. No OS-specific timeout relaxation is introduced.

Record at most nine saved states/classifications after watchdog failure, with a
separate one-second diagnostic bound. On success, bind immediately without a
new observation await that could consume retries; print bounded kind/phase/source
consumption/approximate wall-duration facts only after recovery. Do not assume
closed loopback endpoints fail immediately on Windows or label a pre-body timeout
as a proven OS connection-refused exception. A dedicated native CI execution
prints these facts; it is a repeated observation of an existing case, not a new
distinct test. The old exact delay cause remains unproven until observed.

## Complete interrupted current-format tools without replay (#37, 2026-10-29)

Startup must restore the call/result invariant even when a previous owner has
already terminalized its Turn. This is repair of the current format, not a
compatibility reader or migration. Keep the existing startup transaction and
original interruption snapshot ordering. Distinguish pending/no admission
(`not_started`) from an uncommitted result with possible side effects
(`interrupted`). Preserve actual result nodes and report saved execution facts
separately; never repeat a shell automatically or infer success from lost output.

Cancel unapplied authenticated shell-origin attachments before completing the
source group; completing a group must not release uncertain side effects into
another Thread. Retain applied sources, refs and external files. Keep existing
DAG edges immutable, splice only newly created repair nodes, and normalize model
input using source-preserving replay frames. Current spills need their converted
items normalized once before caching because interleaved context and results
can share one source node. Cache entries retain canonical path order for reuse;
validate complete pairs before pin acquisition and before HTTP.

Only explicitly collected, unreachable GC tombstones belonging to a deleted
Thread are skipped, without restoring payloads or fabricating completion.
Retained inconsistent source bindings remain errors. Normal live admission
conflicts keep their existing guard: do not globally repair groups while the
current owner may still be collecting a shell. Missing pairs instead fail input
preparation locally; startup repairs the interrupted durable state.

The DB worker owns repair; keyset pages hold at most 32 groups/calls/Threads,
while SQL splices selected paths. Temporary replay maps are bounded by the
existing input frames/items and released during request preparation. No table,
worker, queue, dependency, schema or protocol bump is needed. Revisit if a new
source format changes grouping, attachment eligibility, or GC ownership rules.
See [design](../design/tool-group-recovery.md) and
[validation](../validation/acceptance.md) for executed tests and remaining limits.

## Revisit at normal completion (#36, 2026-10-29)

Adopt [Revisit](../design/revisit.md) after #37: reserve one bounded instruction
per live Run and consume it at the existing transactional completion decision.
The same event/delivery and ordinary Turn path handles the instruction. It is a
developer event, not a synthetic human message. Re-registration is deliberately
unlimited by default; an optional per-Run cap, visible count/pending facts and
existing stopping/recovery controls bound operation without a goal engine.
Direct-parent authority follows retry's one-Run mutation scope; cancellation
still propagates through descendants under the existing stop contract.

Use one existing app_meta row per registered Run. Schema 12 extends the existing
history-byte covering index to include that prefix, so reservation/count metadata
is not hidden from logical capacity. Repeated firings overwrite that state;
ordinary events/history account for their own growth. No table, scheduler, worker
or dependency is added. Protocol 2 gains additive operations and genuinely
optional summaries (Runs without registration have none). Historical conversion
remains isolated in startup migrations.

Explicit stop clears pending state; shutdown/startup capture original text and
setter/count before clearing it. Keep compaction protection on the current event
source. Do not infer task success or classify questions in Core: the Agent Guide
explains cancelling/rewriting while awaiting the user. Mechanical mock tests
prove lifecycle/delivery behavior, not better real-model task judgement. Revisit
that evaluation separately with original requests, changed constraints and
question false triggers; no paid evaluation is silently added to routine checks.

## Isolate unresolved tool repairs (#37 review, 2026-10-29)

Adopt M1 isolation and L1 consistent saved execution facts. A current-format
structural inconsistency rolls back only its tool group's savepoint, keeps the
unresolved original history and continues other conversations. Preserve fatal
handling of actual SQL/persistence failure and missing global root metadata.
Bounded fixed-reason diagnostics identify affected Threads/groups; they contain
no command, instruction or arbitrary source text. Startup capture must also
avoid appending to a structurally inconsistent selected cursor before that
isolation can run. A skipped snapshot/group is not successful repair.

Original interruption observations precede terminalization; synthetic results
then refer to the post-startup stopped execution state. Keep L2's existing live
admission guard: completing a possibly live collector's call would risk racing
real results or repeating effects. An unreproduced live conflict still requires
inspection/restart; do not add speculative live repair. The candidate scan pages
RAM but examines the tool_group PK range at startup; it is not proportional only
to unfinished groups. Keep indexed per-group lookups and document that cost
rather than adding a partial index which cannot cover all missing-result cases.

## Bounded OS termination and exit markers (#38–#39, 2026-10-29)

Route delivered TERM/HUP and Windows console-close events through the existing
draft-preserving shutdown. One owner-lifetime async observer on the shared
runtime holds bounded atomic facts; the OS callback never owns DB/UI work. A
fixed cleanup deadline and second delivered signal force exit, relying on #37
for durable repair when cleanup cannot finish. Terminal loss disables further
render/input attempts and leaves backend drain alive. The existing windows-sys
Console feature is enabled without a new crate or version change.

Windows logoff/shutdown notification is best effort under OS delivery rules;
user32/gdi32 can suppress those console events. A hidden-window message pump is
not introduced for this change. Native Windows desktop/console behavior is a
separate unverified boundary, not established by a mock. Revisit if actual
Windows operation demonstrates the need for that extra lifecycle owner.

Derive exit markers from the latest Run's saved stop_reason only; no new state
or auto-resume. Preserve explicit_stop during interrupted startup as normal
shutdown already does. A new Run naturally clears the marker. Bulk resumption is
an explicit message operation with batching and existing ownership rules; the
Guide adds no extra restriction on the model's judgement about uncertain effects.

### Quit admission is not ordinary replay cancellation

The Revisit shutdown guard applies only at the idle completion decision. Closing
new tool starts must still allow already accepted tool groups to save cancelled
results and follow existing request/drain behavior until real Control shutdown
cancels them. An early blanket next() return regressed two existing Quit tests;
it was removed rather than weakening those tests. A pending Revisit stays
unconsumed so the shutdown snapshot can record it.

### Select the bounded Crossterm Unix polling backend (#38, 2026-10-30)

A real PTY-master-close test failed with exit 1 after 5.1 seconds: diagnostics
contained the first HUP and force deadline, no frontend shutdown stage, and the
persisted Run, tool group, call and shell remained active. Inspection of pinned
Crossterm 0.29.0 `event/source/unix/mio.rs` found an inner readiness/read loop
that neither exits on zero bytes nor propagates EIO; it cannot honor the caller's
poll timeout while inside that loop. A pre-poll HUP check would retain a race,
and moving the same backend to EventStream would retain the spinning reader.

Enable the library's existing `use-dev-tty` feature instead of adding a parser,
vendored library fork, emergency Quit path, or longer shutdown deadline. Its
`tty.rs` source breaks the zero-byte read loop, propagates EIO and checks its
outer poll deadline. Both sources call the same `tty_fd()` and retain the same
terminal-stdin versus `/dev/tty` selection; Windows input is unchanged. The
filedescriptor 0.8.3 package is already locked through termwiz; only the
Crossterm dependency edge and existing Rustix process feature are activated.
Keep the five/four-second cleanup budgets and actual successful PTY-loss
assertions. Re-evaluate this feature choice on Crossterm upgrades against the
source behavior and the same physical PTY regression. Source inspection is not
a substitute for running that regression or native Windows verification.

## Issues #44/#45: read projections without new lifecycle state (2026-10-29 UTC)

Status: adopted. The list retains native state and adds typed activity through
one shared pure classifier. Running-only hints are gated so completed-answer
background pages avoid historical tool-group scans. Existing indexed ownership
EXISTS predicates distinguish shell/child work from finalization. No new schema
is needed. Revisit and app-exit facts remain separate.

Live shell elapsed_ms is explicitly a read-time wall-clock observation, not
the terminal supervisor's monotonic duration. Highest retained output sequence
provides last_output_at; reclaimed or absent output is null. Dynamic timing is
added only to the bounded status page after fingerprint/cursor derivation.
Revisit this choice if a future contract requires output-driven ordering or
monotonic live timing; that would require a separate persistence/ownership
design, not an implicit change to the present read projection.


## 2026-10-30: bounded index operations (#42/#43/#55)

- Preserve singleton embedding requests by default (`batch_max_inputs=1`).
  No portable proxy batching limit is known; configurable count/Unicode caps
  are explicit bounds rather than a provider support claim. Revisit the default
  only with evidence across supported endpoint configurations.
- Keep requested dimension separate from observed dimension. Native spaces omit
  the wire option; an explicit rebuild freezes its mode with the space and all
  query/ingest consumers reuse it. No provider catalogue or silent truncation or
  fallback is permitted. The first authorized input validates a candidate once;
  rejection stops that group while keeping the old active search available.
- Use bounded app_meta observations (32 probe tuples, one candidate mode per
  profile, one optional mode per retained space). Existing DB/HTTP workers and
  cancellation own them; add no background engine or polling notification.
- Expose float32 payload arithmetic only, not claimed file reclamation or
  retrieval quality. Explicit GC may collect inactive derived spaces, protecting
  current active/candidate pointers and running work. SQLite file/WAL shrink
  remains separate. Paid operations require the guide's user consent, without
  adding an approval state machine to Core.
- Historical pause observations remain immutable. Preserve the first saved
  timestamp for the same pause cause, expose unknown time honestly, and clear
  current UI attention only on fresh state. Existing Turn-boundary observation
  markers deduplicate notices; do not introduce a poller for immediate delivery.

These decisions are provisional only where endpoint support or retrieval
quality is unknown. A failing endpoint or measured quality regression warrants
reconsidering the user's chosen caps/dimension, not inventing a provider default.

## #51: explicit AGENTS references (adopted)

Explicit selection is strict and has no clearing operation. Relative paths use
the effective target cwd, lexical normalization and revision-checked preflight.
Only reference-change events defer to the next new Turn. This preserves existing
settings authority and snapshots without adding a worker, queue or table.
Revisit clearing/composition only under a separate user requirement.

### #51 review: persistence and unsaved TUI selection boundaries (adopted)

Keep History fork helpers DB-only: explicit references require Control, and are
rejected rather than silently ignored by the helpers. Return inherited paths
truthfully. Unsaved TUI selection must adopt the created Thread into the original
draft; a generic management result is not adoption. A draft/generation-bound
UI action/result uses the existing command slots and ordered outbox; one local
pending request identity blocks a racing first send. No new Core event, persistent state,
worker or queue is added. Later edits are retained; replies cannot select an
unrelated active draft. The existing worker/shutdown owns completion.

UI-to-frontend admission rejection unwraps submitted/management actions and
releases only the matching draft/generation/request selection. Submitted slash
text retains its normal restoration behavior. A local UUID identifies each
pending selection (one bounded value per view) so a stale rejection or result
cannot release a later selection in the same draft generation.

## #51 external review: deterministic idle checks and deferred Low items

Status: adopted follow-up to public comments
[6511989087](https://github.com/tya5/shellboy/issues/51#issuecomment-6511989087)
and [6512018897](https://github.com/tya5/shellboy/issues/51#issuecomment-6512018897).

Idle correctness must not be inferred from a quiet receive timeout. Use the
existing durable RunEnded boundary, compare target-scoped saved work identities
and states before/after operation completion, and independently verify the
settings MutationResult cannot wake a driver/retry. Keep reference-change audit
pending without delivery/history adoption until a new Turn. No production
behavior, table, worker, queue or test binary is added for this correction.

The following are record-only Low observations, not closing conditions:

- agents-only and same-path selection still increments profile_revision through
  the shared settings update, so an admitted preparation may be discarded and
  rebuilt before POST. Preserve this existing behavior for #51; revisit separate
  revision domains only in a future scoped change with admission-race coverage.
- create/fork override audit reports the default/inherited reference as before,
  even if this new Thread never used it. This is the adopted audit contract;
  revisit initial-reference wording/delivery only with an explicit UX decision.
- optional argument fields require changes across constructor literals because
  not every argument struct implements Default. Defer that ergonomics refactor;
  it is outside #51's production scope and must not obscure behavioral changes.

Publication dependency: #46 lands before #51 is rebased onto origin/main.
Adapt existing regressions to #46's resulting layout, then repeat standard
checks. Do not preemptively transplant or resolve conflicts while #46 is absent.
This records ordering, not permission to push, post to GitHub, update main or
replace installed binaries. Physical-terminal verification remains unclaimed.

## #48: attach provenance to the actual instruction message (adopted)

Choose proposal (a): prepend one source/snapshot metadata line to the AGENTS
developer message, not another system instruction or authority/permission layer.
This keeps provenance adjacent to the body even when the Agent Guide is unread.
JSON-quote the fixed Turn snapshot path and escape controls/line separators;
append one LF and the unmodified body. Preserve all snapshot storage and roles.
The guide explains configured-reference inheritance/override independently of
cwd and requires explicit permission for edits, without promising automatic
exceptions to other higher-priority instructions. Model behavior improvement
requires later dogfood/real-model observation; mocks prove request contents only.


## 2026-10-30: recover input without rewriting ownership (#53)

Use explicit-resume path adoption of the same immutable node, rather than a
second receipt for that message. Existing message/context readers assume one
receipt per message; duplicating it would expand identity and cursor semantics.
Keep original assignment and add one persisted admission-fact column instead.
Normal acknowledgement still follows response adoption; terminal repair can
settle admitted input without inventing an answer.

Fresh known-unadmitted input follows proposal A. For historical rows lacking
positive path/attempt admission evidence, prefer explicit restoration (proposal
B) to silently resurrecting a rewound instruction. The old format cannot prove
never delivered. Record unknown honestly and expose it in #52's queue. Revisit
only with additional positive historical evidence, not an absence-based guess.
A bounded 128-ID resume rejects excess input atomically instead of overtaking
old messages or adding a recovery scheduler.
## Pending-message implementation boundaries (#52, 2026-10-30)

- Use a frontend read-order stamp assigned inside the serialized DB read, rather
  than a new durable queue version. History and pending rows are one transaction
  and one update; existing generation/view checks still apply. Revisit if reads
  leave this shared DB lane or a second UI producer bypasses paired delivery.
- Freeze a submission in a separate row of the existing draft table. One editable
  row cannot preserve revision R while accepting a newer autosave of R+1. Bind by
  operation/source draft/revision, never text; preserve newer edits and make
  failed precommit snapshots explicitly restorable. Original source references
  serve cancellation archives, retaining attachments and provenance without
  copying conversation bodies into another store.
- A nullable per-Turn interrupt timestamp records durable human intent without
  adding a lifecycle state. Keep the Turn active until adopted call/result pairs
  finish, because attachment admission relies on that boundary. The existing
  Run-wide cancellation token cannot represent a light Turn interrupt. One
  Turn-bound token/native-start gate is owned by each driver and released with
  it; DB callbacks never acquire the gate. Native Windows behavior needs fresh
  CI evidence; passing a loopback test on macOS will not establish it.
- Ordinary sends no longer release foreground waits. Immediate delivery is an
  explicit persisted fact and never interrupts response generation. Existing
  send-key configuration wins over the Ctrl+Enter convenience; the two-key chord
  remains available. Timed automatic backgrounding stays deferred by the latest
  #56 decision, avoiding an unrelated timer/configuration mechanism.


## Published Message Recovery Review (#53, 2026-10-30)

Review of main b686ec1 reports no Medium issue and approves closing the Core
work after exact-SHA three-OS CI. Track unknown historical admission explicitly
in #52 with synthetic list/cancel/restore verification; do not auto-replay it.
Retain the two factual existing reason codes: Thread deletion says
target_unavailable, while general terminal repair may report target or source
unavailable. No alias/compatibility layer is needed merely to unify wording.
The five-millisecond polling in a few isolated recovery fixtures observes saved
state under a sixty-second watchdog; it is not product polling or elapsed-time
correctness. Prefer existing Changed notifications when next editing those
fixtures, rather than broadening this fix into an unrelated test rewrite.


## Compaction owner-scope absence is a saved fence (#52, 2026-10-30)

Adopt exact absence checks for the existing optional Run/Turn snapshot fences.
A snapshot taken before a first Run or between Turns must not adopt after a new
owner scope starts. The prior validator checked captured Some values only,
leaving None-to-Some transitions unchecked. Reuse indexed Run/Turn existence
checks; do not add a counter or bump selection_revision at message acceptance.

A same-active-Turn queued message remains off-path and cannot be erased by the
captured selected-source edit. Existing path/source/profile/generation validation
therefore remains sufficient in that case. A general thread.revision fence would
also reject unrelated metadata and routine queued reports, potentially discarding
paid automatic summaries without improving this source-safety boundary. Revisit
only if a concrete change alters selected/protected source semantics without
advancing an existing fence. Deterministic Store-only regressions distinguish
new Run, new Turn and unchanged active Turn instead of racing driver adoption.


## Shutdown registration readiness (#52, 2026-10-30)

Main CI41016702414 passes Linux/macOS but fails Windows at the three-second
wait.pending registration barrier in the shutdown capture-failure fixture,
before prepare_shutdown runs. The identical product/test tree passed manual
Windows41015832578. This supports a fixture readiness investigation, not a
claim that shutdown failed or that runner scheduling is proven responsible.

Start Control explicitly before installing synthetic owner facts; use one
shared60-second saved-registration watchdog with premature-result diagnostics
and a120-second observed wait. Initial human wait registration has no Changed
notification, so use a bounded5ms fixture poll instead of a tight yield/DB loop.
Keep all failure caching, receipt, Run-stop and drain assertions. No product
timer, retry, worker or native lifecycle changes. Revisit if the expanded
diagnostics expose a real request failure or a missing durable registration.


## Pending-message review dispositions (#52, 2026-10-30)

Review6517077748 identifies a reachable busy loop: an interrupted active or
recovery-wait Turn with an unfinished adopted group cannot be acknowledged,
but next previously returned Recheck unconditionally. Use Idle only while that
same interrupted Turn has an unfinished group. Keep Recheck for an intent
committed after acknowledgement when no pair blocks progress. Preserve saved
results and uncertainty; do not call startup repair in a live owner or reject
previously allowed human interruptions. Existing nudge and Run-stop ownership
remain responsible for further progress. No timer, event, queue or worker is added.

Correct the review's failure location using its exact log: tests/recovery_paths.rs
429 is the registration guard before prepare_shutdown, not the later shutdown
watchdog. Retain both failed native run and separately rerun evidence.

Adopt60-second deadlock watchdogs and notification-before-read in the new native
Turn scenarios where Changed is guaranteed. A reserved native-start row has no
guaranteed notification while its start gate is held; retain bounded5ms fixture
polling there. Product timers and side-effect assertions do not change.

Defer submitted-draft retention cleanup (review Low3). Those rows also preserve
crash/replay correlation and the source-revision watermark; deleting them at
admission would revive stale autosaves or lose replay provenance. They already
count toward the existing logical storage cap and are removed with their Thread.
Revisit with measured draft-byte pressure and a design that retains a bounded
watermark/replay guarantee without an independent archive engine. The registry
lock versus long DB work observation (Low5) remains a documented ordering tradeoff,
not a newly discovered deadlock; revisit through the separate DB-query work.
Fix the Agent Guide's "up to128" typo without changing its cancellation contract.


## Large-upload fixture observation (#52 Windows follow-up, 2026-10-30)

The shutdown-fixture correction run41018286041 passes that earlier failed test
but fails the existing response_upload_recovery arrival watchdog after15seconds.
The observed phase is a real1024-byte socket prefix, not a simulated source;
between requests the fixture also reduces/rebuilds16MiB of history. The old
message did not identify prefix ordinal or saved attempt phase, so causality
remains unknown. Raise only arrival/settlement watchdogs to60seconds and report
bounded saved Turn/attempt diagnostics if they fail. Keep the product3-second
header deadline, actual backpressure, partial consumption and exact attempt-count
assertions. Retain the failure and require fresh native Windows qualification.
The reviewer-run unchanged-main second attempt succeeding is independent evidence,
not proof of the cause of either failed attempt.
