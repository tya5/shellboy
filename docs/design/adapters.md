# APIとOSアダプタの契約

Shellboy固有の確定境界は [Control設計](control.md) と [公開protocol](protocol.md) に従う。以下はRustへ落とす内部契約で、製品互換性の実測結果ではない。

## Responses

`store: false`、`stream: true`で採用経路をreplayする。接続先のモデル容量表・自動再送・別APIへのfallbackを追加しない。effort未指定はfield自体を送らない。モデル向けfunctionはshell一つで、引数の既定はCLI契約に合わせる。

SSEはバイトの分断・CRLF・複数data行を扱う。`output_text.done`や引数doneだけでは実行を始めない。completed response全体のoutputと全callを検証してから一群を確定する。`failed`、`incomplete`、error、completed以前のEOFは復旧待ち。phaseは保存・replayし、phaseだけで追加生成や復旧を起こさない。

生のoutput itemをJSONとして保存し、ID・call_id・reasoning・encrypted_content・phase・未知fieldを失わない。未知itemは元形式を保存した上で互換性エラーを返し、推測変換しない。最終itemはdone/completedの値を使い、addedの不完全なopaqueデータを継続用正本にしない。

初期資源制限はSSE event 8 MiB、tool引数1 MiB、応答全体64 MiB。上限で黙ってtruncateせず失敗情報を保存する。これらはプロバイダーのトークン上限や利用者のトークン予算ではなく、受信メモリを有限にする内部値。

fixtureは空回答、commentary正常完了、reasoning/phase replay、複数call、重複call ID、壊れた引数、途中EOF、split UTF-8、CRLF、未知エラー、明示context超過を含める。実HTTP proxy互換とモデル別添付・effortはV4で別途検証する。

公式資料（2026-10-24確認）:

- [Streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events)：responseの終端イベントとitem lifecycle。
- [Reasoning](https://developers.openai.com/api/docs/guides/reasoning)：statelessな推論継続の保存・再送。
- [Function calling](https://developers.openai.com/api/docs/guides/function-calling)：call_idと結果の対応。

## Shellとプロセス

| 対象 | 起動契約 | 停止・試験 |
|---|---|---|
| Bash | `--noprofile --norc -c COMMAND`。stdinはコマンドソースへ転用しない。BASH_ENVは継承 | 実行ごとprocess group。TERM→猶予→KILL→reap、shell終了時も残存群を停止 |
| PowerShell | `-NoLogo -NoProfile -NonInteractive -OutputFormat Text -EncodedCommand BASE64_UTF16LE` | Windows生成時Job所属、KILL_ON_JOB_CLOSE、Job handle非継承。未所属spawnを成功にしない |

shellの実際のexit codeを使う。勝手なset -e、exit LASTEXITCODE、構文補正なし。stdout/stderrは別byte stream、受信sequenceは保存順。文字コードは指定値で再デコードでき、ページ端の不完全文字を終端と誤認しない。

試験条件はstdin閉鎖/追加入力/部分write、UTF-8分断/CP932/改行なし、孫のpipe保持、TERM無視、出力洪水、停止重複、spawnとstop競合、保存失敗。WindowsはPowerShell5.1/pwsh/Git Bash、UnixはmacOS/Linuxを分ける。脱離daemon・本体強制終了後のUnix孤児完全回収は初版保証外。再起動時に古いPIDでkillしない。
