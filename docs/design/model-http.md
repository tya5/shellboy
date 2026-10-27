# Responses / embeddings HTTP transport

API境界・接続契約・[adapter契約](adapters.md) を具体化する。実装は `src/model/http.rs`、ローカルHTTP fixtureは `tests/model_http.rs`。初期のAPI・接続仕様資料は非同梱。

`HttpModelClient::new(Connection)` でURL・headerの静的契約を検査し、接続profileを固定する。資格情報の環境変数は要求を始める直前に取得する。認証不足ならHTTPを開始しない。Responses要求は同API rootの `/responses` へ一回POSTし、`stream:true` / `store:false` を強制する。model/effort/input/tool指定は要求側が組み立てた値を維持する。redirect、HTTPクライアントのretry、別APIへのfallbackは無効。接続確立timeoutは固定30秒。生成全体の期限は付けず、接続別の受信停滞期限と利用者の取消しで中断可能とする。

HTTP(S)プロキシはreqwestの既存環境変数 `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` と `NO_PROXY` を利用する。接続別TOMLのproxyキーや独自の除外判定は追加しない。依存のdefault-features=falseを維持し、OS GUIのsystem-proxy取得やSOCKS featureは有効にしない。

TLS設定は接続profileごとの `ssl_verify`（既定true）をクライアント生成時に適用する。falseはreqwestの `tls_danger_accept_invalid_certs(true)` に対応する。使用中のrustlsでは証明書チェーン・有効期限・ホスト名などの検証が省略され、自己署名の許可だけには限定されない。暗号化とSNIは維持する。HTTPS中継プロキシを利用する場合はAPI宛先がHTTPでもプロキシ証明書へ同じ検証設定を適用する。会話・要約・embedding・モデル一覧で同じクライアントを使い、他接続へ設定を伝播させない。redirect禁止とretry禁止は維持する。検証無効化を制御するライブラリの環境変数はなく、Shellboy独自のグローバル環境変数も追加しない。

`response(request, CancellationToken, Option<mpsc::Sender<Value>>)` は `ModelResponse` または `AttemptFailure` を返す。失敗は元のModelError、部分本文、完了済みraw item、取得済みのraw responseを保持する。HTTPエラー時はモデル出力を捏造しない。SSEは `ResponseStream` を用いてバイト単位の分断・UTF-8分断・CRLFを復元し、completed全体を検査する。terminalイベントを受け取った時点でHTTP本文を閉じるため、proxyが接続を保持しても完了を待ち続けない。terminal前のEOFは復旧対象。未知出力形式は元responseを保持した互換性エラー。

任意のevent senderは有限チャネルで、満杯時はasync backpressureを掛ける。senderの送信待ちも取消し可能。受信側が閉じた場合は `consumer_closed` として失敗し、最終イベントを捨てた成功を返さない。送信の成功はチャネルへの引渡しであり、DB commitの完了ではない。採用・永続化・generation照合はControl/DB側の責務。イベントの到着自体でshellを開始しない。

ResponsesのContent-Typeは `text/event-stream` を基本とする。利用者のproxyが要求の `stream:true` に対して `application/json` の完成responseを返した場合だけ、同じResponsesオブジェクトを完成検証へ渡す。これは同じ要求への本文表現の正規化であり、別HTTP要求・別APIへ切り替えない。JSON時は同じ終端イベントをチャネルへ渡す。他のContent-Typeはプロトコルエラーにする。この互換判断は採用済み。再確認条件は実proxyが不正なContent-Typeを返す場合で、無条件の推測解析へ拡張しない。

HTTP失敗本文は64 KiBまで読み、messageを16,384文字・provider codeを256文字へ制限する。JSONエラーの元code/messageとHTTP statusを保持し、非JSON本文は有界の本文情報を表示する。切詰めを明示する。context超過は既定の明示codeだけで判定し、HTTP 400/413/502やmessage中のcontextという語から自動圧縮を推測しない。reqwestの表示エラーはURLを含み得るため使わず、接続/本文段階を示す固定のtransportメッセージを返す。providerの診断文に設定済みheader/鍵/API rootが現れた場合は伏せる。実会話・資格情報を自動ログしない。

`embeddings(model, input, expected_dimension, cancel)` は `/embeddings` へfloat形式を一回要求する。返却件数・indexの一意性と範囲・非空ベクトル・全要素のfinite f32・次元統一を検証する。返却順はinput順へ整列する。dimension未確定時は最初の正常responseの次元を返すだけで、DBへ勝手にspaceを作らない。失敗/取消し時は採用せず、source版の照合・space pause・retry方針はindex workerの責務。埋め込み本文の受信上限は内部baseline 16 MiB。

検証はローカルTCP fixtureで分断SSE、UTF-8、途中EOF、明示context超過と未知エラー、HTTP本文上限、資格情報不足による未送信、取消しとevent backpressure、event consumer切断、JSON本文、未知item原文、embedding index/次元/finiteを確認する。要求回数も検査し、自動再送の不存在を試験する。TLS・実proxy互換、実モデルeffort/添付、課金APIの検証を代替しない。

公式参照: [Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events)、[Embeddings](https://developers.openai.com/api/reference/resources/embeddings/methods/create)。

2026-10-24の実endpoint診断を受け、最初の完全なterminal SSE frameをdecode境界とする。`error`、`response.failed`、`response.incomplete`、`response.completed` のframeが空行まで完了したら、同じHTTP chunk内の後続event/未完了frameを解析しない。HTTP chunkの分かれ方によって元provider errorが `Event after terminal response` や `EOF in partial SSE event` へ置き換わる問題を防ぐ。terminal frame自体のJSON不正・未完了や、最終responseそのものの形式検証は引き続き失敗になる。最初の終端が失敗なら、その後のcompletedで成功へ変えない。一般用途の `SseDecoder::push` は全eventを解析し、Responsesのstreamだけが終端までのdecodeを選ぶ。これは[公式event定義](https://developers.openai.com/api/reference/resources/responses/streaming-events)を基にしたtransportの境界判断で、後続eventをモデル出力として解釈する拡張ではない。全byte分割点とローカルHTTPで回帰を検証し、課金残高回復後の実endpoint正常完了は別途必要とする。

## HTTP error本文の上限境界（M13）

保持本文は既存64KiB以内とし、超過byteを実際に観測した場合にHTTP error body truncatedを付ける。ちょうど上限でEOFになった本文を省略扱いにしない。上限到達後は次の非空chunk又はEOFを既存cancel観測の下で確認し、追加chunkを本文へ保持しない。診断文字数上限、資格情報のredact、provider分類と自動再試行条件は維持する。

## ストリーム完了時の差分と受信証跡

output_item.doneとresponse.completed.outputの一致をAPI保証として扱わない。最終responseそのものを既存の完了状態・対応形式・ツール引数で検証し、その正本だけを採用する。途中とのJSON差分はメタデータに限らず復旧待ちの理由にしない。ツールは最終responseの採用transaction成功後に実行する。未知形式や不正な最終データ、通信失敗、未完了、保存失敗は既存の失敗経路を維持する。

item doneとの差分があれば先頭16件までの固定分類とoutput_indexをStreamObservationへ保持する。受信した回答・平文推論deltaと最終本文が異なる場合も、response全体のdelta_differencesへ固定分類を保存する（架空のoutput_indexは付けない）。total_differencesは両種類の合計とする。分類はmessage本文/refusal/content/role、function_callのcall_id/name/引数、reasoningのsummary/content/encrypted_content、index、その他metadataとし、分類は警告・調査用であり受理可否を決めない。引数は既存ShellToolArgsとして同値なら整形・既定値の差と扱う。最終raw JSONは書き換えない。

成功した差分観測は同じ採用transaction内でpath外のmodel_stream_observation audit nodeへ保存する。payloadはattempt_id、differences、delta_differences、total_differences、partial_text、partial_reasoning、completed_items、raw_response。モデル再送・semantic索引・採用pathへは追加しない。通常ログは既存有限writerで対象ID・固定分類・index/fieldだけを記録する。本文・任意provider文字列は通常ログへ入れない。

同一HTTP chunk内に正常eventと不正JSONがあっても、先に解析できたeventは保持する。失敗時は最終response解析で失敗しても取得済みrawを失わず、既存AttemptFailure/incomplete_responseへ回答と読み出し可能な推論delta、completed_items、raw_responseを保存する。保存された回答と推論はUIの受信表示・一覧・原文cursorから到達可能にする。会話表示用の有限窓と保存原文を区別し、復旧待ちになっただけでは受信文章を消さない。受信したencrypted_contentを平文推論の代わりとして表示しない。

公式の[item doneとcompleted](https://developers.openai.com/api/reference/resources/responses/streaming-events) を参照した。途中と最終の同一性を追加の必須条件にしていたのはShellboy独自実装であり、その差分をAPI違反と断定しない。会社環境の実SSEによる確認は未実施で、合成streamで両方の正本・保存・表示境界を試験する。

## 接続別のHTTP期限（Issue #8）

HTTP層がApiEndpointごとに同じ期限を適用する。Responses（会話・要約）は最後に非空のbody byte又はheaderを受信してからの受信停滞期限 `stream_idle_timeout_ms`（既定600000ms、0で無効）。header待ちも開始時点から対象とし、長時間の生成全体へ固定期限は付けない。SSEコメントや未完frameも受信として扱う。embeddingは要求全体の `request_timeout_ms`（既定60000ms、最小1000）、モデル一覧は同設定と20000msの小さい方。HTTP失敗本文にも同じ期限を適用し、呼出し側のRAG90秒timeoutは撤去する。

取消しを優先して観測する。期限切れはModelError kind `timeout` とし、header/bodyの段階、観測経過、接続名を含む設定keyをローカルで生成する。provider本文やURLを期限切れ診断へ混ぜない。接続確立期限のreqwest errorもtimeoutへ分類する。SSEの部分本文・完了済みitem・raw responseは既存AttemptFailureで保全する。停滞timeoutは以下の会話自動再試行の対象にしない。

transportのheader/非空body受信は、driver向けの既存32枠event channelでのみ単調時刻とともに渡す。通常response()のmodel event契約は維持し、driver向けresponse_observed()だけ型付きの受信通知を使う。driverからUIへの追加通知は先頭即時、その後最大1回/秒に制限し、最終受信の表示には1秒未満の観測差を許容する。HTTP期限自体は間引かない。単独通知はtry_sendで飽和時にまとめ、model eventへも元の受信時刻を添えることでbackpressure後の補完を行う。provider JSONや保存eventへ混入させず、UIは既存の活動時計で30秒以上の無受信を投影する。新しい表・worker・queue・永続lifecycleは追加しない。

## 未受信の一時失敗の再試行（Issue #7）

HTTP clientは従来どおり一要求一POSTとし、会話Driverだけが再試行を管理する。対象はHTTP429/500/502/503/504の通常エラー、または成功応答本文を1 byteも受け取る前のtransport失敗/空SSEのEOFに限る。HTTPエラー本文と成功応答本文は分ける。空でないSSE comment、metadata、JSON、tool引数の断片も受信済みであり、本文・推論の空文字だけから未受信を推定しない。timeout・取消し・protocol失敗、他status、HTTP200内のproviderエラーは対象外。#8の同じDeadline/body readerで受信の事実を観測する。

接続別response_max_retriesは既定2（0〜5）、response_retry_max_delay_msは既定30000ms（1〜300000）。最初1秒、次2秒の指数待機を上限で切り、Retry-Afterの秒またはHTTP dateが有効ならその値を優先し同じ上限で切る。不正値は通常待機。予算はTurn/generation内で共有し、成功やtool実行、context圧縮では補充しない。明示retryでgenerationが変わると新しい予算になる。再起動後に自動継続しない。

失敗attemptをfailedへ保存し、error_json.automatic_retryにnumber/limit/delay_ms/possible_duplicate_chargeを残してからChanged通知と取消し可能なsleepへ進む。この記録は次の試行を予約した事実で、HTTP再送済みの証明ではない。待機中の停止なら次のattemptは作らない。上流の二重処理・重複課金は防げない。次の要求は通常の受付/CAS/容量/添付/選択境界を再検証する。待機中はThread境界lockを保持せず、Run停止・owner停止を優先する。上限到達時は既存の復旧待ちにし、automatic_retry_exhaustedのretries/limitと最後のエラーを保存する。各実HTTP要求は新しいattempt、次要求のDeadlineも新しい要求で開始する。embedding/catalog/要約の再試行方針は変更しない。

TUIは最新の同generationのfailed attemptのautomatic_retryを既存投影で読み、通常色のアニメーションと再試行回数を表示する。新しい表・永続状態・worker・queue・EngineEvent・依存を足さない。

## EOF判定の実装監査とSSEの行境界

2026-10-27、Windowsのv0.1.2利用でのEOF頻発報告を受け、自前decoderを先に監査した。HTTP読み取りは一つのloopでchunk受信→同期decode→次の受信の順であり、別経路のEOF通知が未解析chunkを追い越す構造はない。ただしLF/CRLF以外の行境界と先頭BOMに欠陥があった。先頭BOM付きdata行だけのcompleted streamは実装が最初の行を無視して正確に `EOF before response.completed` を返し、CR単独区切りは終端を解析できずpartial EOFとなる。通常のcreated先行streamでBOMだけが今回の頻発原因だとは断定しない。

[SSE標準](https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation) に従い、stream先頭のBOMを一つだけ除去し、CR/LF/CRLFを同じ行境界とする。CR直後のLFだけをchunkを跨いで抑止する。終端frameは空行で確定し、未完了frameをEOFでflushしない。`[DONE]`単独やSSEのevent名だけから完成responseを捏造しない。既存の完成JSON検証・tool採用境界・上限・取消し・retry条件は変えない。

EOFのModelErrorには受信byte数、解析JSON event数、item done数、DONE marker数、未解析行/イベントのbyte数、最後のJSON eventの固定分類を付ける。本文・任意event名・URL・資格情報は含めず、既存attempt error経路で保存/照会する。Windows専用の改行変換や別のparserはなく、実会社環境での原因確定はこの診断と受信側の追加観測を必要とする。新しい表・event・worker・依存は追加しない。
