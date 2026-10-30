# Responses / embeddings HTTP transport

API境界・接続契約・[adapter契約](adapters.md) を具体化する。HTTP要求は `src/model/http.rs`、Responses受信は `src/model/http/response.rs`、本文処理は `src/model/http/body.rs`、期限は `src/model/http/deadline.rs` に分ける。ローカルHTTP fixtureは `tests/model_http.rs`。初期のAPI・接続仕様資料は非同梱。

`HttpModelClient::new(Connection)` でURL・headerの静的契約を検査し、接続profileを固定する。資格情報の環境変数は要求を始める直前に取得する。owner内では接続profileごとに構築した共有clientを使い、正常な要求ごとにtransportやpoolを作り直さない。失敗したpool generationの退役はIssue #34の規則に従う。認証不足ならHTTPを開始しない。Responses要求は同API rootの `/responses` へ一回POSTし、`stream:true` / `store:false` を強制する。model/effort/input/tool指定は要求側が組み立てた値を維持する。redirect、HTTPクライアントのretry、別APIへのfallbackは無効。接続確立timeoutは固定30秒。生成全体の期限は付けず、接続別の受信停滞期限と利用者の取消しで中断可能とする。

HTTP(S)プロキシはreqwestの既存環境変数 `HTTP_PROXY` / `HTTPS_PROXY` / `ALL_PROXY` と `NO_PROXY` を利用する。接続別TOMLのproxyキーや独自の除外判定は追加しない。依存のdefault-features=falseを維持し、OS GUIのsystem-proxy取得やSOCKS featureは有効にしない。

TLS設定は接続profileごとの `ssl_verify`（既定true）をクライアント生成時に適用する。falseはreqwestの `tls_danger_accept_invalid_certs(true)` に対応する。使用中のrustlsでは証明書チェーン・有効期限・ホスト名などの検証が省略され、自己署名の許可だけには限定されない。暗号化とSNIは維持する。HTTPS中継プロキシを利用する場合はAPI宛先がHTTPでもプロキシ証明書へ同じ検証設定を適用する。会話・要約・embedding・モデル一覧で同じクライアントを使い、他接続へ設定を伝播させない。redirect禁止とretry禁止は維持する。検証無効化を制御するライブラリの環境変数はなく、Shellboy独自のグローバル環境変数も追加しない。

`response(request, CancellationToken, Option<mpsc::Sender<Value>>)` は `ModelResponse` または `AttemptFailure` を返す。失敗は元のModelError、部分本文、完了済みraw item、取得済みのraw responseを保持する。HTTPエラー時はモデル出力を捏造しない。SSEは `ResponseStream` を用いてバイト単位の分断・UTF-8分断・CRLFを復元し、completed全体を検査する。terminalイベントで解析を終え、正常responseの検証後だけHTTP本文を最大1秒・64KiBまで読み捨て、HTTP/1.1 poolへの返却を試みる。末尾をモデル出力として解析せず、受信エラー・上限超過で完成済みresponseを失敗へ変更しない。取消しは優先し、proxyが接続を保持しても無期限に待たない。terminal前のEOFは以下の会話Driver再試行または復旧の対象。未知出力形式は元responseを保持した互換性エラー。

任意のevent senderは有限チャネルで、満杯時はasync backpressureを掛ける。senderの送信待ちも取消し可能。受信側が閉じた場合は `consumer_closed` として失敗し、最終イベントを捨てた成功を返さない。送信の成功はチャネルへの引渡しであり、DB commitの完了ではない。採用・永続化・generation照合はControl/DB側の責務。イベントの到着自体でshellを開始しない。

ResponsesのContent-Typeは `text/event-stream` を基本とする。利用者のproxyが要求の `stream:true` に対して `application/json` の完成responseを返した場合だけ、同じResponsesオブジェクトを完成検証へ渡す。これは同じ要求への本文表現の正規化であり、別HTTP要求・別APIへ切り替えない。JSON時は同じ終端イベントをチャネルへ渡す。他のContent-Typeはプロトコルエラーにする。この互換判断は採用済み。再確認条件は実proxyが不正なContent-Typeを返す場合で、無条件の推測解析へ拡張しない。

HTTP失敗本文は64 KiBまで読み、messageを16,384文字・provider codeを256文字へ制限する。JSONエラーの元code/messageとHTTP statusを保持し、非JSON本文は有界の本文情報を表示する。切詰めを明示する。context超過は既定の明示codeだけで判定し、HTTP 400/413/502やmessage中のcontextという語から自動圧縮を推測しない。reqwestの表示エラーはURLを含み得るため使わず、接続/本文段階を示す固定のtransportメッセージを返す。providerの診断文に設定済みheader/鍵/API rootが現れた場合は伏せる。実会話・資格情報を自動ログしない。

`embeddings(model, input, expected_dimension, requested_dimensions, cancel)` posts one float-encoded `/embeddings` request. `expected_dimension` validates the response only; optional `requested_dimensions` sends the unchanged integer `dimensions` field and also validates that length. Native mode omits the field exactly as before. Zero dimensions or conflicting expected/requested dimensions fail before HTTP. Validate exact count, an integer index permutation, nonempty vectors, finite f32 values and uniform expected length before returning any result; return vectors in input order. An initially unknown length is returned as an observation and does not itself create a space. Never truncate vectors or fall back to another dimension. Failure/cancellation adopts nothing; source witnesses, pause and retry belong to the index worker. The existing embedding response cap remains 16 MiB. See [index operations](index-operations.md).

検証はローカルTCP fixtureで分断SSE、UTF-8、途中EOF、明示context超過と未知エラー、HTTP本文上限、資格情報不足による未送信、取消しとevent backpressure、event consumer切断、JSON本文、未知item原文、embedding index/次元/finiteを確認する。要求回数も検査し、自動再送の不存在を試験する。TLS・実proxy互換、実モデルeffort/添付、課金APIの検証を代替しない。

公式参照: [Responses streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events)、[Embeddings](https://developers.openai.com/api/reference/resources/embeddings/methods/create)。

2026-10-24の実endpoint診断を受け、最初の完全なterminal SSE frameをdecode境界とする。`error`、`response.failed`、`response.incomplete`、`response.completed` のframeが空行まで完了したら、同じHTTP chunk内の後続event/未完了frameを解析しない。HTTP chunkの分かれ方によって元provider errorが `Event after terminal response` や `EOF in partial SSE event` へ置き換わる問題を防ぐ。terminal frame自体のJSON不正・未完了や、最終responseそのものの形式検証は引き続き失敗になる。最初の終端が失敗なら、その後のcompletedで成功へ変えない。一般用途の `SseDecoder::push` は全eventを解析し、Responsesのstreamだけが終端までのdecodeを選ぶ。これは[公式event定義](https://developers.openai.com/api/reference/resources/responses/streaming-events)を基にしたtransportの境界判断で、後続eventをモデル出力として解釈する拡張ではない。全byte分割点とローカルHTTPで回帰を検証し、課金残高回復後の実endpoint正常完了は別途必要とする。

## Threadごとの要求識別（Issue #15）

会話DriverとCompactorは要求ごとに実Thread UUIDを渡す。接続ごとの `session_header=true`（既定）でResponses要求に `x-shellboy-session-id` を付け、`prompt_cache_key=false`（既定）を明示trueにした場合だけ同じ値を本文へ追加する。同ThreadのTurn・試行・要約・再起動後の続行で同じIDを使う。子ThreadとforkはそれぞれのThread UUIDを使う。共有client・pool・接続設定へ特定Threadの値を保存しない。

自動headerは同名の固定headerを大文字小文字によらず要求単位で上書きする。無効化は自動付与だけを止め、明示headersは従来どおり維持する。Thread文脈を持たない低水準 `response`、embedding、モデル一覧には自動header/body fieldを足さない。伝送するThread IDはASCII英数字・hyphen・underscoreの1〜64byteへ制限し、不正値は送信前に拒否する。通常のThread UUIDは36byte。キャッシュhitや切断頻度の改善は接続先次第で保証しない。

F6は採用assistant nodeからindexed output_item.node_idを経由して正確なmodel_attemptへ到達し、保存済みusageのcached_tokensを表示する。同Turnの最新attemptを推測しない。履歴pageごとに1回の既存DB worker読取を使い、最大2候補・usage 64KiBで表示を制限する。0・未報告・不正・関連不明を区別し、保存原文とschemaは変えない。

## 接続別clientの寿命（Issue #14）

ControlのStoreがimmutableなHttpClients表を所有し、会話Driver・Compactor・索引worker・意味検索・モデル一覧へ同じArcを共有する。表は設定の接続名ごとに一つのHttpModelClient（又は初期化エラー）で、entry数は起動設定の接続数に固定する。通常のRust constructorを使い、runtimeの追加cache、mutex、worker、queue、永続表は持たない。独立テスト向けconstructorは独立の表を構築でき、独立RAGは従来どおりコマンド単位のProfileのclientを使う。

URL/静的headerとclientの構築・検証はowner初期化時に行い、要求時は軽いcloneを取得する。使わない接続のclient初期化エラーは表に保持し、その接続の操作開始時に返す。動いている間のTOML変更を読み直さない。資格情報/環境変数headerは各HTTP要求の直前に解決し、clientへ秘密値を固定しない。HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXYはreqwestがclient構築時（初期化又は失敗poolの再構築）にowner processの環境から読む。通常の設定変更はownerの再起動で反映する。別processのshellの環境変更が既に動いているownerへ伝わるという意味ではない。

reqwestのpool_idle_timeoutを45秒にする。提案範囲30〜60秒の中間を、proxyに閉じられた古いidle接続を長く残さない初期値として選ぶ。会社proxyの実測最適値や切断回避保証とはしない。新しい設定keyやpool_max_idle_per_hostの独自上限は加えない。接続別base_url/ssl_verify/期限は隔離し、redirect禁止・HTTP client自身のretry禁止を維持する。接続の失敗は会話Driverの通常の再試行判断へ渡す。

[固定reqwest版のClient](https://docs.rs/reqwest/0.13.5/reqwest/struct.Client.html)は内部poolを持ち、cloneで共有する。poolへ戻せる完全読取応答は再利用できるが、SSEのterminal後にHTTP本文をまだ読み切っていない場合は接続が破棄され得る。正常terminalの検証後だけ、最大1秒・64KiBの本文読捨てで再利用を試みる。後続byteをSSEとして解析せず、失敗や上限超過では接続再利用を諦める。取消しは受信済み本文・推論・最終rawを保全して優先する。常に全Responses要求が同じTCP接続になるとは保証しない。

表の寿命はControl/Store/Index/Compactorのowner文脈と要求中cloneの所有に従う。停止時は既存の要求取消し・worker終了・drainを行い、最後の所有cloneがなくなればpoolも解放する。停止後も保持される外部Control参照を無視して即時破棄を約束しない。

## HTTP error本文の上限境界（M13）

保持本文は既存64KiB以内とし、超過byteを実際に観測した場合にHTTP error body truncatedを付ける。ちょうど上限でEOFになった本文を省略扱いにしない。上限到達後は次の非空chunk又はEOFを既存cancel観測の下で確認し、追加chunkを本文へ保持しない。診断文字数上限、資格情報のredact、provider分類と自動再試行条件は維持する。

## ストリーム完了時の差分と受信証跡

output_item.doneとresponse.completed.outputの一致をAPI保証として扱わない。最終responseそのものを既存の完了状態・対応形式・ツール引数で検証し、その正本だけを採用する。途中とのJSON差分はメタデータに限らず復旧待ちの理由にしない。ツールは最終responseの採用transaction成功後に実行する。未知形式や不正な最終データ、通信失敗、未完了、保存失敗は既存の失敗経路を維持する。

item doneとの差分があれば先頭16件までの固定分類とoutput_indexをStreamObservationへ保持する。受信した回答・平文推論deltaと最終本文が異なる場合も、response全体のdelta_differencesへ固定分類を保存する（架空のoutput_indexは付けない）。total_differencesは両種類の合計とする。分類はmessage本文/refusal/content/role、function_callのcall_id/name/引数、reasoningのsummary/content/encrypted_content、index、その他metadataとし、分類は警告・調査用であり受理可否を決めない。引数は既存ShellToolArgsとして同値なら整形・既定値の差と扱う。最終raw JSONは書き換えない。

成功した差分観測は同じ採用transaction内でpath外のmodel_stream_observation audit nodeへ保存する。payloadはattempt_id、differences、delta_differences、total_differences、partial_text、partial_reasoning、completed_items、raw_response。モデル再送・semantic索引・採用pathへは追加しない。通常ログは既存有限writerで対象ID・固定分類・index/fieldだけを記録する。本文・任意provider文字列は通常ログへ入れない。

同一HTTP chunk内に正常eventと不正JSONがあっても、先に解析できたeventは保持する。失敗時は最終response解析で失敗しても取得済みrawを失わず、既存AttemptFailure/incomplete_responseへ回答と読み出し可能な推論delta、completed_items、raw_responseを保存する。保存された回答と推論はUIの受信表示・一覧・原文cursorから到達可能にする。会話表示用の有限窓と保存原文を区別し、復旧待ちになっただけでは受信文章を消さない。受信したencrypted_contentを平文推論の代わりとして表示しない。

公式の[item doneとcompleted](https://developers.openai.com/api/reference/resources/responses/streaming-events) を参照した。途中と最終の同一性を追加の必須条件にしていたのはShellboy独自実装であり、その差分をAPI違反と断定しない。会社環境の実SSEによる確認は未実施で、合成streamで両方の正本・保存・表示境界を試験する。

## 接続別のHTTP期限（Issue #8）

HTTP層がApiEndpointごとに同じ期限を適用する。Responses（会話・要約）は最後に非空のbody byte又はheaderを受信してからの受信停滞期限 `stream_idle_timeout_ms`（既定600000ms、0で無効）。header待ちは別のresponse_header_timeout_ms（既定120000ms・正整数）で開始時点から制限し、長時間の生成全体へ固定期限は付けない。SSEコメントや未完frameも受信として扱う。embeddingは要求全体の `request_timeout_ms`（既定60000ms、最小1000）、モデル一覧は同設定と20000msの小さい方。HTTP失敗本文にも同じ期限を適用し、呼出し側のRAG90秒timeoutは撤去する。

取消しを優先して観測する。期限切れはModelError kind `timeout` とし、header/bodyの段階、観測経過、接続名を含む設定keyをローカルで生成する。provider本文やURLを期限切れ診断へ混ぜない。接続確立期限のreqwest errorもtimeoutへ分類する。SSEの部分本文・完了済みitem・raw responseは既存AttemptFailureで保全する。会話の停滞timeoutは以下の再試行条件で扱い、要約・索引・RAGの方針は変えない。

transportのheader/非空body受信は、driver向けの既存32枠event channelでのみ単調時刻とともに渡す。通常response()のmodel event契約は維持し、driver向けresponse_observed()だけ型付きの受信通知を使う。driverからUIへの追加通知は先頭即時、その後最大1回/秒に制限し、最終受信の表示には1秒未満の観測差を許容する。HTTP期限自体は間引かない。単独通知はtry_sendで飽和時にまとめ、model eventへも元の受信時刻を添えることでbackpressure後の補完を行う。provider JSONや保存eventへ混入させず、UIは既存の活動時計で30秒以上の無受信を投影する。新しい表・worker・queue・永続lifecycleは追加しない。

## 会話の連続一時失敗の再試行（Issue #7、#11）

HTTP clientは一要求一POSTを維持し、会話Driverが追加試行を管理する。HTTP429/500/502/503/504の通常エラーと、正常な終端に至る前のtransport切断・EOF・受信停滞timeoutを対象にする。取消し、protocol/JSON不正、失敗terminal、未知/不正な完成response、その他status、HTTP200内のproviderエラーは自動再試行しない。HTTP失敗本文のtimeout・切断でも429/500/502/503/504は対象とし、その他statusは復旧へ進む。

SSEのresponse.created/in_progress、コメント・heartbeatだけは生成出力として数えない。output_item/content_partの追加・完了、本文/refusal/推論/tool引数のdelta等を観測した時点で出力ありとする。HTTPの生存観測と再試行用の出力観測を区別する。JSONの完成前に通信が切れた場合は、非空の本文を保守的に部分出力とする。接続別response_retry_partialは既定trueで、部分出力がある場合も同じ条件で再試行する。falseなら部分出力後は復旧待ちとし、metadataのみ/出力なしの一時失敗は引き続き対象とする。

response_max_retriesは既定8、0〜20。連続した失敗に対する追加試行回数で、最終responseを検証・DBへ採用した後だけ0へ戻す。受信開始、tool実行だけ、context超過圧縮だけでは補充しない。Turn/generation変更でもリセットする。指数待機は1秒開始で倍にし、response_retry_max_delay_ms（既定60000、範囲1〜300000）で上限を設ける。既定8回では1/2/4/8/16/32/60/60秒、合計183秒となる。

Retry-Afterの秒/HTTP dateが有効なら指数待機上限と別に、その指示を短縮せず待つ。固定600秒を超える場合は自動再送せず、指定待機時間と上限、automatic_retry_deferredを保全して復旧へ進む。不正な値は指数待機へ戻す。再起動後に自動継続しない。

失敗attemptをfailedへ保存し、error_json.automatic_retryへnumber/limit/delay_ms/partial_response_discarded/possible_duplicate_chargeを残してからChanged通知と取消し可能なsleepへ進む。部分出力の場合は同じtransactionでincomplete_responseの監査nodeも保存し、GC root_revisionを更新する。通常の復旧保存と同じ処理を使う。保存した本文・完了item・raw responseは/receivedから参照でき、採用pathや次のモデル入力へ混入しない。失敗した途中のtool callは実行せず、正常な最終responseの採用成功後だけ実行する。

記録は再試行を予定した事実で、HTTP送信済みの証明ではない。上流での生成料金の重複は防げない。各追加要求は新attemptとして通常の受付/CAS/容量/添付/選択境界を再確認する。Thread境界lockを待機中に保持せず、停止・owner終了が優先する。回数上限ではautomatic_retry_exhaustedに連続回数/上限を保存して復旧する。設定無効、永久失敗、Retry-After超過を回数上限と誤表示しない。

TUIは既存projectionで同generationの最新failed attemptを読み、通常色の再試行アニメーション、回数、予定待機秒数、再送による重複課金の可能性を示す。途中応答を保存して作り直す場合も明示する。自動再試行待機は復旧待ちではないためベルを鳴らさない。新しい表、永続状態、worker、queue、依存は追加しない。

## 受信事実と再試行判定の分離（Issue #13）

AttemptFailureはModelErrorと受信原文に加え、非永続のResponseFactsを返す。受信段階（未受信/metadata/出力）、HTTP status、Retry-After、応答境界到達を記録するだけで、HTTP層は再送の許可を返さない。応答境界はSSEのtyped terminal又は完全にparseされたJSON responseであり、その後の完成検証失敗を通信中断と混同しない。HTTPエラー本文のtimeoutなどもstatusを保持して会話Driverへ返す。

Driver/retry.rsのclassify_retryが、error kind・status・受信段階・応答境界・partial optout・Retry-After上限を一度判定する。予算/待機計算と永続保存・取消しは従来のDriver責務に残す。ModelError.kindの保存形式は変更しない。ResponseFactsをDBの新しい正本状態にしない。

model.rsのclassify_stream_eventをResponseStreamと表示用Driverが共用し、本文/推論delta、その他の出力、metadataを分類する。期限のDeadlineは時間だけを所有し、本文受信フラグを持たない。全体/停滞/無効はenumで表し、モデル一覧の20秒上限とエラー文字数上限は各定数を共用する。

sendはHTTP methodを受け取ってGET/POSTの手順を共有する。モデル一覧・embedding・JSON Responsesは共通の有界JSON readerでstatus、HTTPエラー、本文上限、JSON、provider error、資格情報redactを処理し、各呼出し側がdata/responseを検証する。Responsesの原文保全・event配送・terminalでの終了時点・各要求の期限/取消し条件は維持する。接続clientの寿命とpoolはこの整理では変更せず、Issue #14へ分離する。

## EOF判定の実装監査とSSEの行境界

2026-10-27、Windowsのv0.1.2利用でのEOF頻発報告を受け、自前decoderを先に監査した。HTTP読み取りは一つのloopでchunk受信→同期decode→次の受信の順であり、別経路のEOF通知が未解析chunkを追い越す構造はない。ただしLF/CRLF以外の行境界と先頭BOMに欠陥があった。先頭BOM付きdata行だけのcompleted streamは実装が最初の行を無視して正確に `EOF before response.completed` を返し、CR単独区切りは終端を解析できずpartial EOFとなる。通常のcreated先行streamでBOMだけが今回の頻発原因だとは断定しない。

[SSE標準](https://html.spec.whatwg.org/multipage/server-sent-events.html#event-stream-interpretation) に従い、stream先頭のBOMを一つだけ除去し、CR/LF/CRLFを同じ行境界とする。CR直後のLFだけをchunkを跨いで抑止する。終端frameは空行で確定し、未完了frameをEOFでflushしない。`[DONE]`単独やSSEのevent名だけから完成responseを捏造しない。既存の完成JSON検証・tool採用境界・上限・取消し・retry条件は変えない。

EOFのModelErrorには受信byte数、解析JSON event数、item done数、DONE marker数、未解析行/イベントのbyte数、最後のJSON eventの固定分類を付ける。本文・任意event名・URL・資格情報は含めず、既存attempt error経路で保存/照会する。Windows専用の改行変換や別のparserはなく、実会社環境での原因確定はこの診断と受信側の追加観測を必要とする。新しい表・event・worker・依存は追加しない。

## HTTP要求内の共通文脈（Issue #13レビュー）

cancel・Deadline・有限event sender・redaction対象を局所RequestContextへまとめ、JSON/SSE/HTTP error本文へ借用する。response_json/response_sseはclient状態に依存しない関数とする。非2xxはhttp_errorを直接呼び、成功しないことをexpectで仮定しない。stream deltaの受信分類と固定診断分類は同じclassify_stream_eventから導出する。要求をまたぐ状態や再試行の判断は持ち込まない。

## Issue #34: independent headers and bounded transport facts

Responses header waiting has a separate positive per-connection
`response_header_timeout_ms`, provisionally 120000ms. Its clock includes request
upload and ends at received response headers; it cannot prove a peer read the
request. `stream_idle_timeout_ms` remains the post-header receive inactivity
budget (600000ms by default, zero disables only this body inactivity budget).
Embedding/model-catalog total deadlines and the fixed 30-second connection
establishment limit remain unchanged. Header latency measures outgoing.send to received headers; the deadline starts
earlier during request setup and also includes local credential/encoding work.
Latency is observed on successful Responses and saved under the existing attempt input manifest's http_observation;
failures retain the observation in the existing ModelError. The default is a
conservative initial choice, not a measured optimum for a particular provider;
local measurement and its limitations belong in the decision record.

Observations contain bounded typed phase, serialized body length, source bytes
consumed by the HTTP client, successful header elapsed milliseconds, pool
generation and a connection-reuse fact. `before_body_source` means connection setup failed or no body-source bytes
were consumed; it does not consume the partial-upload failure budget.
`upload_source_incomplete` requires some but not all source bytes consumed; `awaiting_headers_delivery_unknown` means the source
was consumed but peer delivery is unknown; `body_receive` means headers arrived.
The reqwest public API does not expose socket-write completion, TCP acknowledgement
or actual socket reuse. Never label source consumption as successful transmission
or blame/exclude provider processing from these observations. The first request
issued by a newly built client records fresh_pool; all other socket reuse is
unknown. No private URL, body, header or credential is copied into telemetry.

The existing owner-scoped client owns one bounded shared pool generation. A
short std RwLock protects only cloning/replacing Client and two scalar counters;
it is never held across await. Only eligible Responses transport/timeout/nonterminal-EOF and server-5xx
failures retire their generation so later requests do not select that pool.
Caller cancellation, completed 4xx/provider/protocol errors, embedding and
model-list failures do not retire the Responses pool; a late failure from an old generation cannot reset the
new pool. Existing in-flight clones retain their old pool until dropped, so no
immediate closure of a multiplexed HTTP/2 socket with other active requests is
promised. A fresh pool is not a claim that a concurrent request necessarily uses
a fresh socket. The existing 45-second idle timeout is retained and is not an
absolute connection-age guarantee. No table, worker, queue, dependency or Core
operation is added.

The fixed dependency's public contracts are [streaming request bodies](https://docs.rs/reqwest/0.13.5/reqwest/struct.Body.html#method.wrap_stream),
[response remote address](https://docs.rs/reqwest/0.13.5/reqwest/struct.Response.html#method.remote_addr)
and [pool idle timeout](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.pool_idle_timeout).
These APIs do not provide a per-request peer-delivery acknowledgement or exact
socket-reuse fact; that absence is a documented diagnostic limitation, not a
reason to infer those facts from body polling.

### Review correction: connection setup and pool reconstruction

No consumed request-source bytes, or reqwest is_connect(), is a pre-upload
observation rather than proof that a body transfer stalled. Connection refused,
DNS, TLS and connect timeout therefore retain the ordinary consecutive retry
budget. Only a strictly partial source can consume the bounded upload-failure
budget. Long/slow uploads may hit the header deadline because it includes upload;
they are eligible for the same one-time large-input reduction rather than being
excluded solely for partial consumption. It remains impossible to distinguish
ongoing socket transmission from a network stall using source consumption alone.

Retirement only removes the current generation's client reference; it does not
synchronously construct a replacement on the async runtime. The next request
constructs a fresh client via the existing Tokio blocking executor, then installs
it under the short existing pool lock if a concurrent request has not already
installed one. No new persistent worker, queue or dependency is introduced;
construction tasks are scoped to existing admitted requests. A failed build
returns an error for that call and leaves the pool empty/retryable, never a
cached Err that poisons all later calls. Existing in-flight clones retain their
previous generation. A late failure cannot retire a newer installed generation.

## Conversation dialect boundary (#64)

Adopt connection `api = "responses"` (default) or `"chat_completions"`. A single
HTTP-edge dialect module translates the shared stateless request/replay and
stream to the existing normalized response/event contract. Core persistence,
tool validation/execution, retry/reduction/recovery and schema stay unchanged;
no table, worker or queue is added. Logical conversation deadlines and session
headers remain shared; embedding/catalog endpoints are unchanged.

Chat maps system/developer/user/assistant messages, images, grouped shell calls
and tool results. Developer instructions use system messages for compatibility.
It requests usage, accumulates tool argument deltas by index, and requires a
finish reason plus the SSE [DONE] boundary before adopting successful output.
Length/filter/unknown finish reasons are incomplete, not guessed context errors.
Only explicit context-limit codes use the existing context-limit classification.
Map only reported integer prompt/completion/cached/reasoning/total counts to the
existing usage envelope; missing counters stay unknown, never synthesized.

Responses-only reasoning items/encrypted replay and prompt_cache_key are not
sent for Chat. Vendor reasoning_content is intentionally unused, not displayed
or replayed. Reject Chat + prompt_cache_key rather than pretend it works.
Expose dialect/unavailable features in catalog/check details. Anthropic's OpenAI
facade is outside qualification because it lacks prompt-cache support. No live
provider tests are authorized (#68 D-003); Gemini and all real providers remain
unverified. Provider extra_body/cached_content controls are deferred.


### D-010 fact-preserving Chat tolerances (#64 review)

Accept stop with calls only when every call has a nonempty name and identity
(provider or derived below) and fully parsed JSON-object arguments. Keep the
original finish_reason and a stop_with_complete_tool_calls marker. Core's usual
shell schema/group validation still applies. Repeated full declared `shell`
name deltas are ignored and counted; partial name fragments still concatenate.
For missing/empty tool ids, require a nonempty response id: hash its UTF-8 bytes
once with SHA-256, then hash that hex seed plus the 8-byte big-endian index. Use
`call_sb_` plus 48 hex digits; mark the original id, source/index and synthesis.
No response identity is guessed if both identities are absent.

An omitted choices field is empty only on a chunk carrying non-null usage, with
an omission counter. Other malformed choices still fail. Store normalization
facts in normalized output items (also in the direct-client raw envelope), so
existing assistant_response persistence and raw/F6 inspection retain them;
Core does not persist every top-level response property. An otherwise empty
usage-only answer carries facts on an empty assistant message, not invented text.
These rules are wire normalization, not legacy Core/data-format fallbacks.
