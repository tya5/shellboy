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

接続ごとの `ssl_verify` 追加後、ローカル設定13件と既存HTTP12件が成功しました。合成自己署名HTTPSの2試験も成功しました。Responses・モデル一覧GET・Embeddingsについて、既定/明示trueの拒否とfalseの成功、他Connectionへ影響しないことを実要求で確認しました。課金API、利用者設定・保存データ、OSの信頼ストアは変更しません。変更後の3 OS CIの結果は、完了後に記録します。
