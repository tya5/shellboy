# 文書と履歴を検索する

[ガイド入口](README.md) · [ガイド検索](docs.md) · [会話履歴](history-and-search.md)

`shellboy rag` は会話本体を起動せず使う検索CLIです。質問への回答を生成する機能ではありません。
まず `shellboy docs search --query '検索'` で操作ガイドを探せます。

## 文書を登録する

```bash
shellboy rag init
shellboy rag source add /absolute/path/to/notes --name notes
shellboy rag source list
shellboy rag index --source notes --dry-run
shellboy rag index --source notes
shellboy rag search --scope source:notes --query '復旧'
```

既定はkeyword検索・索引で、API接続は不要です。登録やdry-runはAPIを呼びません。
管理DBは既定データディレクトリの `rag.db`、`shellboy rag --db /absolute/path/knowledge.db`
で明示できます。会話DBとは別です。DBを選ぶ引数は以降の操作でも同じものを指定します。
保存先の解決は他のCLIと同じく `--data-dir` → `SHELLBOY_DATA_DIR` → OS既定です。
`--config` はRAGのembedding接続に使い、TOMLのdata_dirでは検索対象を切り替えません。
別の会話保存先を検索する場合は `--data-dir PATH` も明示してください。
source addの返却IDまたは名前を指定します。原本はコピーせず、索引と抽出本文を保存します。
同期後に原本が変わっても、getは検索時の保存版を返します。

```bash
shellboy rag status
shellboy rag inspect --id DOCUMENT_ID
shellboy rag source remove --source notes
```

登録解除は管理索引のみを対象とし、元ファイルを削除しません。
同期中のエラーで旧版が残る場合は `stale` と理由を返します。原本フォルダへ一時的に
アクセスできない状態を、文書の削除とは扱いません。

## 意味検索と複数の範囲

```bash
shellboy rag index --source notes --mode semantic
shellboy rag index --source docs --dry-run --mode semantic
shellboy rag index --source docs --mode semantic
shellboy rag search --query '再起動からの復旧方法' --scope docs --scope source:notes --mode hybrid
shellboy rag search --query '以前の判断' --scope history --mode keyword
```

semantic/hybridは既存の `[embedding]` と接続設定を使います。`--config PATH` で設定を
選べます。必要なAPI接続がない場合は明示エラーになり、勝手にkeywordへ切り替えません。
scopeは `docs`、`history`、`source:登録IDまたは名前` を繰り返して指定できます。
省略時は同梱ガイドと登録資料が対象です。会話履歴は明示したときだけ対象に含まれます。
履歴は読み取り専用で、会話本体の起動・復旧や再索引を行いません。

同梱ガイドの意味検索も、先に `rag init` と明示的な `rag index --source docs --mode semantic`
が必要です。dry-runで文書数・本文byte数・分割数と費用が発生し得る旨を確認できます。
検索はガイドを自動でembeddingせず、未準備やbinary更新で版が変わった場合は次のindex操作を示します。
`--source`を省略したindexは登録資料を処理し、同梱ガイドを勝手に追加しません。
semantic/hybrid検索自体のquery embeddingにもAPI費用が発生し得ます。
準備できていないscopeがあるhybrid検索はkeyword結果とscope別エラーを併せて返します。
`shellboy docs`やdocsのkeyword検索はDB・APIなしで引き続き使えます。

結果のexcerptは抜粋です。返却されたreferenceを使って原文を読みます。

```bash
shellboy rag get --ref REFERENCE --limit-bytes 16384
shellboy rag get --ref REFERENCE --limit-bytes 16384 --cursor CURSOR
```

本文は `result.text`、次ページは `next_cursor`。同じ参照を使い続けます。
保存版が回収されていれば `revision_unavailable` で、現在版の本文へすり替えません。
資料の取得単位は `provenance.retrieval_unit=block`（ページやシート等の抽出block）、
ガイドは文書全体、履歴は参照された本文種別です。getのoffsetはこの取得単位のUTF-8 byteです。
位置の `source` はbackendが示した原本位置、`chunk_start_bytes/end_bytes` は保存した
抽出本文内の範囲で、原本ファイルのbyte offsetではありません。
一部scopeが失敗した場合は部分結果とscope別エラーを返し、終了コードは非0です。
異なるembedding空間のdistanceは比較できないため、統合検索は順位で融合します。
複数scopeを同時刻のsnapshotとは扱いません。履歴keywordは走査開始時のwatermarkまでを
短い読取りで調べ、各参照を版/hashで検証します。検索中の新規追記は再検索で取得します。

検索結果は最大100件・512KiBで、出力上限に達した場合はtruncatedを確認します。
同期の失敗一覧は最大50件で、総数と省略有無を別に返します。
認証・残高・レート制限の共通障害では、その実行中の追加embeddingを止め、未処理を
deferredとして返します。通常の通信失敗も同じ実行で3件になると止めます。障害解消後に同じindexを
明示的に再実行すると、保存済み成功分を再利用します。無限retryは行いません。

## PDF と Office

UTF-8 text/Markdownはそのまま扱えます。文字層PDF、DOCX、PPTX、XLSXは
[Xberg v1.3.10](https://github.com/xberg-io/xberg/releases/tag/v1.3.10) の実行ファイルを
明示します。macOSの配布物は同梱dylibを含めて展開先ごと保持してください。

```bash
shellboy rag index --source notes --extractor /absolute/path/to/xberg
```

Shellboyがbackendやモデルを自動取得することはありません。OCR、旧Office、マクロ形式、
完全な版面再現は対象外です。ページ・スライド・シート位置は抽出backendが示した範囲で返し、
抽出本文のbyte offsetと原本内の位置を区別します。追加のJSON extractorは
`--adapter MANIFEST_PATH` で明示します。たとえば次のmanifestを保存します。

```json
{
  "schema_version": 1,
  "id": "my-extractor",
  "version": "1.0",
  "executable": "/absolute/path/to/extractor",
  "argv": ["--json", "{input}"],
  "extensions": ["pdf"]
}
```

`{input}` は一つのargv要素として必要で、入力の絶対パスへ置換されます。
shellを介さず実行し、相対executableはmanifestの親から解決します。
stdoutは単一JSON `{"blocks":[{"text":"抽出本文","location":{"unit":"page","number":1}}]}`
を返します。locationは実際に分かる位置だけを記録します。Shellboyはblock順に改行で連結し、
本文とadapter ID/version/manifest hashを保存します。
入力16MiB、出力16MiB、stderr64KiB、実行60秒が上限です。
API資格情報は子プロセスへ継承しません。backend自身が信頼できることは利用者が確認します。

## 外部で作成した解説を取り込む

```bash
shellboy rag ingest --id explanation-1 --source-file /absolute/path/to/original.pdf \
  --kind llm-explanation --text-file /absolute/path/to/explanation.md
```

`--text-stdin` も使えます。元ファイルのhashと解説本文のhashを区別して保存します。
`--metadata-file PATH` では生成者・モデル等の任意JSON情報を記録できますが、
記録があること自体がLLMによる原本読取りの証明ではありません。
派生本文は通常の原本フォルダ同期で上書きしません。
