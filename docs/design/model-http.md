# Responses / embeddings HTTP transport

API境界・接続契約・[adapter契約](adapters.md) を具体化する。実装は `src/model/http.rs`、ローカルHTTP fixtureは `tests/model_http.rs`。初期のAPI・接続仕様資料は非同梱。

`HttpModelClient::new(Connection)` でURL・headerの静的契約を検査し、接続profileを固定する。資格情報の環境変数は要求を始める直前に取得する。認証不足ならHTTPを開始しない。Responses要求は同API rootの `/responses` へ一回POSTし、`stream:true` / `store:false` を強制する。model/effort/input/tool指定は要求側が組み立てた値を維持する。redirect、HTTPクライアントのretry、別APIへのfallbackは無効。接続確立timeoutは30秒、生成全体は利用者の取消しで中断可能とする。

HTTP(S)プロキシはreqwestの既存環境変数 `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` と `NO_PROXY` を利用する。接続別TOMLのproxyキーや独自の除外判定は追加しない。依存のdefault-features=falseを維持し、OS GUIのsystem-proxy取得やSOCKS featureは有効にしない。

TLS設定は接続profileごとの `ssl_verify`（既定true）をクライアント生成時に適用する。falseはreqwestの `tls_danger_accept_invalid_certs(true)` に対応する。使用中のrustlsでは証明書チェーン・有効期限・ホスト名などの検証が省略され、自己署名の許可だけには限定されない。暗号化とSNIは維持する。HTTPS中継プロキシを利用する場合はAPI宛先がHTTPでもプロキシ証明書へ同じ検証設定を適用する。会話・要約・embedding・モデル一覧で同じクライアントを使い、他接続へ設定を伝播させない。redirect禁止とretry禁止は維持する。検証無効化を制御するライブラリの環境変数はなく、Shellboy独自のグローバル環境変数も追加しない。

`response(request, CancellationToken, Option<mpsc::Sender<Value>>)` は `ModelResponse` または `AttemptFailure` を返す。失敗は元のModelError、部分本文、完了済みraw item、取得済みのraw responseを保持する。HTTPエラー時はモデル出力を捏造しない。SSEは `ResponseStream` を用いてバイト単位の分断・UTF-8分断・CRLFを復元し、completed全体を検査する。terminalイベントを受け取った時点でHTTP本文を閉じるため、proxyが接続を保持しても完了を待ち続けない。terminal前のEOFは復旧対象。未知出力形式は元responseを保持した互換性エラー。

任意のevent senderは有限チャネルで、満杯時はasync backpressureを掛ける。senderの送信待ちも取消し可能。受信側が閉じた場合は `consumer_closed` として失敗し、最終イベントを捨てた成功を返さない。送信の成功はチャネルへの引渡しであり、DB commitの完了ではない。採用・永続化・generation照合はControl/DB側の責務。イベントの到着自体でshellを開始しない。

ResponsesのContent-Typeは `text/event-stream` を基本とする。利用者のproxyが要求の `stream:true` に対して `application/json` の完成responseを返した場合だけ、同じResponsesオブジェクトを完成検証へ渡す。これは同じ要求への本文表現の正規化であり、別HTTP要求・別APIへ切り替えない。JSON時は同じ終端イベントをチャネルへ渡す。他のContent-Typeはプロトコルエラーにする。この互換判断は採用済み。再確認条件は実proxyが不正なContent-Typeを返す場合で、無条件の推測解析へ拡張しない。

HTTP失敗本文は64 KiBまで読み、messageを16,384文字・provider codeを256文字へ制限する。JSONエラーの元code/messageとHTTP statusを保持し、非JSON本文は有界の本文情報を表示する。切詰めを明示する。context超過は既定の明示codeだけで判定し、HTTP 400/413/502やmessage中のcontextという語から自動圧縮を推測しない。reqwestの表示エラーはURLを含み得るため使わず、接続/本文段階を示す固定のtransportメッセージを返す。providerの診断文に設定済みheader/鍵/API rootが現れた場合は伏せる。実会話・資格情報を自動ログしない。

`embeddings(model, input, expected_dimension, cancel)` は `/embeddings` へfloat形式を一回要求する。返却件数・indexの一意性と範囲・非空ベクトル・全要素のfinite f32・次元統一を検証する。返却順はinput順へ整列する。dimension未確定時は最初の正常responseの次元を返すだけで、DBへ勝手にspaceを作らない。失敗/取消し時は採用せず、source版の照合・space pause・retry方針はindex workerの責務。埋め込み本文の受信上限は内部baseline 16 MiB。

検証はローカルTCP fixtureで分断SSE、UTF-8、途中EOF、明示context超過と未知エラー、HTTP本文上限、資格情報不足による未送信、取消しとevent backpressure、event consumer切断、JSON本文、未知item原文、embedding index/次元/finiteを確認する。要求回数も検査し、自動再送の不存在を試験する。TLS・実proxy互換、実モデルeffort/添付、課金APIの検証を代替しない。

公式参照: [Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events)、[Embeddings](https://developers.openai.com/api/reference/resources/embeddings/methods/create)。

2026-10-24の実endpoint診断を受け、最初の完全なterminal SSE frameをdecode境界とする。`error`、`response.failed`、`response.incomplete`、`response.completed` のframeが空行まで完了したら、同じHTTP chunk内の後続event/未完了frameを解析しない。HTTP chunkの分かれ方によって元provider errorが `Event after terminal response` や `EOF in partial SSE event` へ置き換わる問題を防ぐ。terminal frame自体のJSON不正・未完了やcompleted outputの整合性検査は引き続き失敗になる。最初の終端が失敗なら、その後のcompletedで成功へ変えない。一般用途の `SseDecoder::push` は全eventを解析し、Responsesのstreamだけが終端までのdecodeを選ぶ。これは[公式event定義](https://developers.openai.com/api/reference/resources/responses/streaming-events)を基にしたtransportの境界判断で、後続eventをモデル出力として解釈する拡張ではない。全byte分割点とローカルHTTPで回帰を検証し、課金残高回復後の実endpoint正常完了は別途必要とする。

## HTTP error本文の上限境界（M13）

保持本文は既存64KiB以内とし、超過byteを実際に観測した場合にHTTP error body truncatedを付ける。ちょうど上限でEOFになった本文を省略扱いにしない。上限到達後は次の非空chunk又はEOFを既存cancel観測の下で確認し、追加chunkを本文へ保持しない。診断文字数上限、資格情報のredact、provider分類と自動再試行条件は維持する。
