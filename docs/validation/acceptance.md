# 検証状況

更新: 2026-10-26。macOS / aarch64、固定Rust 1.99.1で、改名後の公開用ディレクトリから実施しました。この表のローカル標準検証対象は公開Gitの `8de8699` です。後続のプラットフォーム修正とGitHub CIは下記の別記録で区別します。非公開snapshot・過去検証asset・作業台帳を置かない構成を使っています。

| 確認 | 結果 | 手順・対象 |
|---|---|---|
| 固定依存でのビルドとCLI名 | 成功 | 全targetをビルド。実binaryの `shellboy --help` と `shellboy --version`（0.1.0）を確認 |
| fmt・全target Clippy・テスト | 成功 | `sh scripts/check.sh`。67テスト群、982 passed / 0 failed / 15 ignored。修正後の通し実行は約7分 |
| オフライン同梱ガイド | 成功 | `scripts/check-docs.py`、33 CLI確認、13 guide・3 category、同梱リンク115件。空cwd・環境変数なし・存在しない設定・owner/API不要、状態ファイルの作成なし |
| 新しい既定保存先・環境変数 | 成功 | config / CLI / runtime試験。新しい名前・OS既定path・保存先解決を確認。旧名・旧データ形式への互換は対象外 |
| 一時owner・CLI・正常終了 | 成功 | cli_process / runtime / tui_process試験。IPC接続、同ID再送、実shellからのCLI呼出し、PTYでの入力・固定HTTP応答・終了を確認 |
| TUI描画・スクロール | 自動試験成功 | TestBackendと自動PTY。実IME・OS clipboard・各端末GUIの再確認は今回未実施 |
| 実provider互換・意味検索の品質 | 今回未実施 | 通常試験はloopback固定応答・固定vector等。実API/性能/実環境のignored試験を合格数へ含めない |
| 公開用checkoutの独立性 | 成功 | 公開対象320ファイルのみで上記build/test。literal include 63件の欠落なし、旧名称残存なし、公開文書の相対リンク欠落なし |
| 公開Gitの全履歴 | 検査実施 | gitleaks v8.30.1、全ref・全履歴、allowコメント無効、redact有効。検出0件。非公開ファイル・旧履歴commitを含まない |

`CARGO_BUILD_JOBS=2`、`CARGO_INCREMENTAL=0` を指定し、ビルド生成物だけローカルcacheへ出しました。ソース・guide・migration・fixtureは公開用ディレクトリのものです。この表の実施時点ではGitHub Actionsはpush前でした。現在のGitHub実行結果は [Actions](https://github.com/tya5/shellboy/actions) を参照してください。実会話・資格情報・個人path・私的画像・生ログは検証文書へ含めません。

初回の通し検証では、名称変更で長くなった合成トークンの出力を19 bytesと正しく取得していましたが、テストに旧名の15 bytesという固定期待値が残っていました。期待値をtokenのbyte長へ変更し、該当回帰単独と上記の通し検証で成功しました。製品の出力処理は変更していません。追加のガイド検証scriptにも旧cursor期待値があったため、最終ページでcursorを省略する既存契約へ合わせました。

採用範囲は [適用仕様](../design/effective-spec.md)、現在地は [status](../development/status.md)、標準手順は [setup](../development/setup.md)、公開対象と履歴の扱いは [公開構成](../development/publication.md) を参照してください。実端末の追加確認は [手順](manual-terminal.md) と実施結果を区別します。

## 初回のネイティブCIとリリース検証

[初回CI](https://github.com/tya5/shellboy/actions/runs/40241942578) で、LinuxのClippyがUnix PTY試験の引数を拒否し、Windowsではstdin pipe作成中のC構造体がawaitを跨ぐためfutureのSend要件を満たしていないことが判明しました。前者はUnix共通4箇所をraw pointerで渡す形へ、後者はC構造体を同期block内へ閉じる形へ局所修正しました。プロセス操作順序・handle所有・待機期限・保存形式は変えていません。

修正後のローカルmacOS確認はPTY関連5件と対象strict Clippyが成功しました。Linux/Windowsの成功は再実行したネイティブCIで確認し、Releaseに対応する実行結果と分けて扱います。リリースworkflowはWindows x64・macOS ARM64・Linux x64のnative release buildと実行ファイルのhelp/version確認、配布物とSHA-256を作成します。

## TLS設定追加とWindows fixture修正

先行候補 `a67c162` の [native CI](https://github.com/tya5/shellboy/actions/runs/40243159749) はmacOS/Linuxで成功しました。WindowsはClippy成功後、管理UIの5試験でfixtureのcwdがOSの絶対パス条件を満たさず失敗したため、合格とは扱いません。fixtureだけをOS別絶対パスへ変更し、macOS上の管理UI6件は成功しました。Windowsでの確認は変更後のCIを使います。

接続ごとの `ssl_verify` 追加後、ローカル設定13件と既存HTTP12件が成功しました。合成自己署名HTTPSの2試験も成功しました。Responses・モデル一覧GET・Embeddingsについて、既定/明示trueの拒否とfalseの成功、他Connectionへ影響しないことを実要求で確認しました。課金API、利用者設定・保存データ、OSの信頼ストアは変更しません。変更後の3 OS CIの結果は下記「公開済みv0.1.0の結果」に記録します。

## 公開済みv0.1.0の結果

初版の製品コードは `1e581fc` の [native CI](https://github.com/tya5/shellboy/actions/runs/40245585424) で次を確認しました。全OSでfmt・strict all-target Clippyも成功しました。

| OS | suite | passed | failed | ignored |
|---|---:|---:|---:|---:|
| Windows x64 | 68 | 891 | 0 | 10 |
| macOS ARM64 | 68 | 986 | 0 | 15 |
| Ubuntu x64 | 68 | 986 | 0 | 14 |

Windowsの先行単独overflow試験1件は上表へ二重計上していません。添付/AGENTSのpath fixture失敗は解消し、TLS2件、native PowerShell5件、overflow単独と全体の成功を実ログで確認しました。

配布tag `v0.1.0` は `96e65bf` です。上記からの差分は配布workflowのversion検査と判断記録だけで、製品Rustソース・依存・同梱guideは同一です。最初の配布runはUbuntu 22.04のPython 3.10にtomllibがなくbuild前に失敗したため、固定Cargo metadata JSONによるversion取得へ変更しました。Release/assetの公開前に限り初期tagを更新し、旧commitと失敗runは履歴に残しています。

[配布workflow](https://github.com/tya5/shellboy/actions/runs/40246376176) は3 OSすべてでrelease build・実binaryのhelp/version・archive作成を成功させ、[Release](https://github.com/tya5/shellboy/releases/tag/v0.1.0) を公開しました。公開後に全配布物をダウンロードし、SHA256SUMSとの一致、実行ファイル・MIT License・設定例・guide・既知の制限の同梱、非公開資料を含まないことを確認しました。Windows ZIPは認証なしでHTTP 200/9,395,798 bytes、PEヘッダはAMD64でした。

| 配布物 | bytes | SHA-256 |
|---|---:|---|
| Windows x64 ZIP | 9395798 | `9756f870e25d3fc090681f25d90ff78fd5bfa92fd7a8f3d0c82eec10ba5946b2` |
| macOS ARM64 tar.gz | 8986811 | `164f313bc64d284076f48b75de21bae2f1ce54fe1c34a4cd4383c30a7ab0e897` |
| Linux x64 tar.gz | 9707525 | `c1ee749ae916fa036c8e95bc8f77f5db2f5df69bd314acdeb7f63438145b81f8` |

ダウンロードしたmacOS release binaryでもオフラインdocsの33 CLI確認/13 guides/3 categories/115 linksが成功しました。空cwd・環境変数なし・設定なし・owner/APIなしで状態ファイルは作りません。binary SHA-256は `8179b21d6c2ab60dc927cf16a239b9e39a4126710be247b57b3824ff12d02111` です。

### Windowsのテスト待機上限の追補

`96e65bf` の [再CI](https://github.com/tya5/shellboy/actions/runs/40246369812) はmacOS/Linux成功、Windowsは890 passed/1 failed/10 ignoredでした。前回成功したoverflow試験が全体実行時だけ再発し、追加診断では最初のshellがrunning・raw未到着のまま5秒を超過していました。overflow処理やsummary/retryへ進む前の待機であり、製品の5秒完了契約違反とは扱いません。

`6e45ac1` はWindowsに限りそのfixtureの待機上限を既存native shell試験と同じ15秒へ揃え、Unixの5秒・保存状態診断・全ての意味検証は保持しました。製品コード・guide・依存は配布tagと同一で、公開済みtag/assetは変更しません。[追補CI](https://github.com/tya5/shellboy/actions/runs/40247172429) も全3 OSで成功しました。fullは各68 suite、Windows 891 passed/0 failed/10 ignored、macOS 986/0/15、Ubuntu 986/0/14です。Windowsの先行単独1件は別枠で成功し、全体でも同じ回帰が成功しました。OS内部の起動遅延原因までは断定していません。
