# ファイル・画像を添付する

[User Guide](user-guide.md) · [画面操作](tui.md) · [ガイド一覧](README.md)

通常の添付はファイルのコピーではなく、解決済み絶対パスへの参照です。送信準備でそのパスの現在内容を読みます。会話を保存しても、外部ファイルの過去版まで保存・復元するわけではありません。

## 下書きへfileを追加する

TUIで次を手入力し、Enterを押します。

```text
/attach /path/to/report.pdf
/attach "/path/with spaces/notes.md"
/attachments
```

相対パスは現在Threadのcwdから解決します。`/attachments`で下書きのID・kind・pathを確認してから、通常の依頼本文を入力して送信します。初回送信ではThread・依頼・添付参照を一緒に保存します。

現在の`/attach`は`file` kindです。画像として送る外部パスは次のCLIの`--kind image`を使うか、clipboard画像には`/image`を使います。拡張子だけを見てTUIがimage入力へ自動変更するとは扱わないでください。

## CLIで保存済みThreadへ追加する

以下はBash/zsh例です。`THREAD_ID`は実際のThread IDへ置換します。人間の外部ターミナルでは`--thread`を明示し、TUIと異なる保存先なら各コマンドに同じ`--data-dir /path/to/shellboy-data`を付けます。モデルのshell内では現在Threadを省略できます。

```sh
shellboy attachment add --thread THREAD_ID --path /path/to/chart.png --kind image
shellboy attach --thread THREAD_ID /path/to/report.pdf --kind file
shellboy attachment list --thread THREAD_ID
```

成功時の`result`はaddもlistも添付情報の配列です。1パス追加でも単一objectにはならず、追加IDは例えば`result[0].attachment_id`から取り出します。add/listの各要素には`attachment_id`、`source_node_id`、`delivery_status`、`path`、`kind`、`selected`、`managed`、必要なら`unavailable_reason`があります。`delivery_status`はaccepted（受付済み）／held（保留）／applied（経路へ反映）／cancelled（取消）です。appliedもモデルがファイルを読んだという意味ではありません。

`attach PATH --kind KIND`は`attachment add --path PATH --kind KIND`のaliasです。kindの指定は必須で、`image`又は`file`を使います。CLIの相対パスはCLI呼出し時のcwdから解決し、別の人間ターミナルではThreadのcwdと同じとは限りません。

CLI追加は追加元を記録した専用nodeへ所属します。モデルshell内の追加は同じtool groupの全結果後に反映し、実行中要求や最新の人間発言へ割り込みません。停止／復旧待ちは保留されます。同じRunの明示retry後に反映し、停止した旧Runの保留を新しいRunへ勝手に移しません。必要なら新たに再添付してください。次のモデル要求で準備します。受付JSONだけでファイル内容をモデルが読めたと判断せず、送信・回答結果も確認してください。同じパスを二度追加しても別要素になり、要求の合計容量も二つ分として数えます。

添付を含まない次の発言へ過去の画像を付け直すことはありません。過去添付は元のメッセージ内の位置で再送し、履歴再送と今回の新規追加を区別します。同じoperation IDの再送でも添付や出典nodeを増やしません。

## 内容の更新・欠落と明示再読込

ファイルを更新すれば、以後の要求では現在内容を使用します。通常はサイズ・更新日時等の属性でRAMの変換cache再利用を判断します。属性を保った書換えの見逃しや、読取り中の変更まで完全なsnapshot保証はありません。

属性を保った書換えなどが疑わしい場合は、保存済みThreadで次を使います。

```text
/reload-attachments
```

又は`/attachments`の管理画面で小文字`r`を押します。明示再読込はcacheを迂回して現在内容・hashを確認し、結果を表示する操作です。それだけで新しい会話要求を送信したり、recovery待ちを再開したりしません。

変更前後のhash比較はThread・添付IDごとに行い、正常な送信の採用と同時に基準を更新します。元位置には今回読取りの版を示し、変更時には出典と比較hashを持つShellboy更新通知を別に置きます。正常完了後の再送では新規更新として通知し直しません。失敗した要求を「最後に正常送信した内容」として扱いません。ファイルが消えた・読めない・単一上限を超えた場合は、古いcacheを代用せず、パスと理由をモデルへテキスト通知し、送れる残りの会話で継続します。APIが形式や要求全体を拒否した場合は、復旧表示を確認して対処します。

## 送信対象から外す

```text
/attachments
/detach ATTACHMENT_ID
```

CLIでは次の形です。

```sh
shellboy attachment remove --thread THREAD_ID --attachment ATTACHMENT_ID
shellboy attachment list --thread THREAD_ID
```

下書きからの削除と、保存済みThreadの送信選択からの除外をIDで区別します。どちらも外部ファイルを削除しません。元の会話nodeや参照も残ります。一覧の`selected`、`managed`、`unavailable_reason`で、実際の送信選択と利用不能理由を確認します。

容量に空きが戻っても除外済み添付は自動復活しません。必要ならパスで再添付します。forkはその時点の送信選択を引き継ぎ、元Threadの後の変更を無条件に共有する操作ではありません。コンパクションで置換された区間の添付も送信対象から外れ、必要なら再添付します。

## Clipboard画像

画像をOS clipboardへコピーしてから、TUIで`/image`又はaliasの`/paste-image`を実行します。通常のテキスト貼付けとは別操作です。画像の取得・PNG生成は背景workerで行い、準備できた画像を元の下書きへ追加します。操作中に下書きを切り替えたり編集した場合、遅い結果が別の下書きへ勝手に入ることはありません。

clipboard画像だけはShellboyがdata-dir配下にmanaged PNGを保存します。元のclipboardを後から再現するための管理対象であり、通常の外部添付をコピーする機能ではありません。下書き・選択中Thread・fork・送信中の参照は回収から保護されます。送信中はpinを持ち、cacheから外れても実際の使用が終わるまで解放しません。

選択や下書きから外れたmanaged画像は、保持期限・容量の回収候補になります。回収後も履歴には参照と利用不能理由が残り、実体の復活はできません。Shellboyは外部パスのファイルをこの回収でunlinkしません。clipboardの利用可否はOS・desktop/remote環境に依存します。

## 容量・形式の制限

| 設定 | 既定値 | 数えるもの |
| --- | --- | --- |
| `limits.attachment_file_bytes` | 20MiB | 1要素の原文bytes |
| `limits.attachment_request_bytes` | 40MiB | 1要求の添付原文bytes合計（重複参照も別要素） |
| `cache.attachment_base64_bytes` | 64MiB（[設定](configuration.md)） | RAMに保持するbytes・変換結果のcache。API入力上限とは別 |

1MiBは1,048,576 bytesです。Base64・JSONは原文より大きくなります。Shellboyの上限を増やしても、proxy/modelの要求サイズや画像枚数制限が増えるわけではありません。

合計を超える場合、既に過去として送った添付を古い順に除外します。今回追加した添付は正常な要求完了まで保護し、必要性を推測して勝手に捨てません。保護分だけで超える追加はエラーになります。受付後にファイルが成長して保護分が超過した場合も、要求を保留しrecovery待ちにします。対象を減らす・小さくする・分割してから明示retryしてください。

任意拡張子のパスを受け付けることと、モデルが内容を理解できることは別です。画像・PDF・その他file対応はproxy/model依存で、ShellboyはOffice変換、OCR、音声・動画変換を自動追加しません。形式非対応で拒否された時は、対応する入力へ自分で変換する、添付を除外する、適切な接続を選ぶ等で対処します。モデルや形式の自動fallbackはありません。

保持・回収・quotaの詳細は[復旧・保存](recovery-and-storage.md)、原文参照は[履歴と検索](history-and-search.md)を参照してください。
