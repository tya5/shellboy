# ガイドを見つける

[ガイド入口](README.ja.md) · [Agent Guide](agent-guide.md) · [CLI辞書](cli-reference.ja.md) · [English](docs.md)

`shellboy docs` は実行ファイルに同梱された操作ガイドを読みます。リポジトリ、起動中のShellboy、設定、API接続は不要です。`skill` は将来の別機能のために予約されており、このガイド閲覧機能の名前ではありません。

同梱catalogueは英語です。検索語も原則として英語を使い、日本語クエリの自動翻訳はしません。日本語版はrepositoryまたはrelease archiveの `.ja.md` を参照してください。

## 階層から読む

```sh
shellboy docs list
shellboy docs list --parent shellboy
shellboy docs list --parent shellboy/work
shellboy docs get --id shellboy/agent-guide
shellboy docs get --id shellboy/delegation
```

一覧の `id` と `parent_id` は文書ファイルの配置とは独立した論理IDです。`shellboy/start` は導入、`shellboy/work` は作業別、`shellboy/reference` は辞書の分類です。一覧は直属の子を返し、検索の `--parent` は子孫まで含めます。

## キーワードから探す

```sh
shellboy docs search --query 'child agent'
shellboy docs search --query 'background' --parent shellboy/work
```

既定検索は大文字小文字を区別しないliteral部分一致です。日本語も空白で区切らず検索できます。検索結果の抜粋は全文ではありません。`id` を使って `docs get` で確認します。意味検索や資料との横断検索は `rag search` で明示選択します。

## 版と続きの取得

list/searchは `items` と `next_cursor`、getはMarkdownの `text`、`sections`、`links`、`next_cursor` を返します。続きがあれば、同じID・sectionまたは検索条件へ `--cursor` を渡します。ページサイズは次の取得で変更できます。

`version` は実行ファイルの版と同梱catalogueのhash、`content_hash` は文書全体のSHA-256です。getの `start` / `end` は文書全体を基準としたUTF-8 byte位置で、文字の途中では分割しません。`sections` のIDは `--section` に指定できます。更新後や別条件でcursorを使い回すとエラーになるため、改めて取得してください。

`links` は同梱ガイドへの `bundled`、Webへの `external`、開発repo内だけの資料への `repository` を区別します。後二者は同梱本文ではありません。開発資料へのリンクがあっても、そのファイルが作業環境に存在すると仮定しません。

これらはownerへの操作を発生させず、operation receiptへ保存されません。`--operation-id` とは併用できません。会話の操作コマンドには従来どおり同じdata-dirのownerが必要です。
