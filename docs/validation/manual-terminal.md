# 実端末・IME・clipboardの確認手順

この文書は再現手順であり、改名後の実施済み結果ではありません。結果は [検証状況](acceptance.md) に対象source・環境・操作とともに記録します。自動PTY試験を、端末emulatorの描画・実IME・OS clipboardの合格へ拡張しません。

手元に実機・操作環境がある項目を実施し、ない項目は未実施とします。実機の用意や手動回答を待つことを、利用可能な環境の作業を進める条件にしません。

## 課金なしのローカルfixture

`examples/terminal_fixture.rs` は開発用のloopback HTTP例。通常モードは外部API、shell起動、ファイル実体の読取り、意味検索を行わない。専用の確認モードを明示した場合は固定のtool呼出しを返すため、この基本手順では有効にしない。Responsesは受信テキストの文字数と先頭4000文字を返し、embeddingsは固定3次元vectorを返す。意味解析・provider互換・検索品質の検証には使わない。HTTP requestは8MiB、並行接続16、embedding inputは128件まで。これらはfixtureの制約であり、製品の設定上限ではない。request本文や資格情報をログへ出さない。

端末A:

```sh
cargo run --locked --example terminal_fixture
```

表示された `http://127.0.0.1:<port>/v1` を使う。停止はCtrl+C。portは省略時にOSが空きを選び、指定する場合は末尾へ数字を渡せる。

端末Bで、任意の使い捨てディレクトリに次のTOMLを保存する。鍵は不要。

```toml
default_connection = "fixture"
default_model = "fixture"
[connections.fixture]
base_url = "http://127.0.0.1:REPLACE_WITH_PRINTED_PORT/v1"
# 必要な場合だけ。固定vectorなので検索の意味品質は判定しない。
# [embedding]
# connection = "fixture"
# model = "fixture"
```

repoで `cargo build --locked --bin shellboy` を実行し、生成したbinaryの絶対pathを控える。使い捨てディレクトリをcwdにして起動する:

```sh
/absolute/path/to/shellboy --config /absolute/path/to/fixture.toml --data-dir /absolute/path/to/disposable-data
```

Windowsでは `target\debug\shellboy.exe` を同様に使う。source・実会話・実data-dirをfixtureへ混ぜない。画像確認用には小さな使い捨てPNGを自分で用意する。fixtureは画像を理解せず、モデル側の意味解析結果は判定対象外。

## 環境記録

実施日、OS/build/architecture、commit、debug/release、端末名/版、local/SSH/tmux、表示倍率とfont、IME名/版、言語、send/newline key設定、clipboard機能/OSC52設定を記録する。試験前後のキー割当や端末設定を変更した場合は差を残す。

代表候補はWindows Terminal＋PowerShell 5.1/pwsh/Git Bash、macOS Terminal/iTerm2、LinuxのVTE系/kitty/Alacritty等。インストール済みのものを使い、全製品の互換を暗黙に保証しない。

## 操作と合否

| 操作 | 合格に必要な観測 |
|---|---|
| 日本語・中国語・韓国語で変換前/変換中/確定/候補選択 | 候補位置と入力cursorが対応し、欠落/二重挿入がない。変換確定Enterで送信されるかを別に記録する |
| Enter送信と改行キー | 確定前・確定後を分けて試す。意図した改行と送信が区別できる。端末がShift+Enterを区別しない場合はその制約を記録し、設定可能な別キーでも確認する |
| 日本語・emoji・結合文字・tab・複数行の通常paste | 内容/改行が一致し、slash先頭や末尾改行でコマンド実行/自動送信されない |
| 長文paste | 行数/文字数のfoldが現れ、F3確認/F4編集/F5削除で元本文を扱える。送信後はfixtureの文字数とDB受付済み本文で一致を確認する。fixtureの4000文字表示制限を製品の切捨てと混同しない |
| 末尾追従・PgUp/PgDn・Thread切替 | 新着と追従停止が区別できる。元Threadの下書き/閲覧位置が戻り、他Threadの本文や添付が混入しない |
| F2選択/コピーモード | モードが表示され、アプリのmouse取り込みが解除される。新着で選択が動かず、端末に取られないキーで解除できる |
| 端末標準の範囲copy | その端末の実際の修飾キー/操作を記録する。Shift/Altを全端末共通と扱わない。copy結果に意図しない罫線や新着が混じらない |
| `/copy NODE-ID` / code block copy | 装飾/時刻/折返しを混ぜない元本文がclipboardへ入る。画面外nodeも非同期で取得できる。OSC52を使う場合は端末/SSH側の許可と結果を別記する |
| 画像をOS clipboardへ明示copy→`/image` | 元画像でalphaを含む小PNGを使う。準備中/利用可能/失敗を識別でき、成功後にmanaged pathが現れる。外部ファイル自体は変更・削除されない |
| clipboard画像準備中にThread切替/取消 | 別Threadへ誤添付せず、取消後の遅い完了で復活しない |
| Ctrl+Q | 端末表示・入力modeを正常復元する。別の保存失敗/強制終了試験の成功をここで代用しない |

IMEのEnter誤送信は、候補表示位置の成功だけで合格にしない。端末がpreeditをアプリへ通知しない場合は、実操作結果と設定による回避の可否を記録する。事前に時間heuristicを導入して完全検知を称さない。

## 結果票

各環境について「成功/失敗/未実施」、操作、期待、実観測、再現手順を埋める。画像・clipboard本文・会話原文・APIキー等をGitへ保存せず、合成markerと文字数・匿名の不一致位置で証拠を残す。手順を書いたこと自体を合格とはしない。
