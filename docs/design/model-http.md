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

HTTP 429 bodyの明示的な `usage_limit_reached` / reset事実（解除時刻または解除までの秒数）が、現在の連続retry予算に残る指数待機予定の合計より後なら、追加POSTを行わず既存の復旧待ちへ進む。既定8回の残り予定は1/2/4/8/16/32/60/60秒（合計183秒）で、接続別retry上限と使用済みbudgetから同じ式で計算する。現在の応答のRetry-Afterは次の待機だけに適用し、未来の応答ヘッダーは推測しない。解除がその範囲内なら通常のretry判定を使い、そのretryだけ接続共有のローカルreset guardをbypassし、成功時は同じ事実が更新されていない場合に限り解除する。reset事実が無い/不正なら通常のretry動作を維持する。復旧理由には提供元の種類と時刻を既存の文言・記号で表示する。推測reset値、新設定、provider switchは加えない。

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


### Opaque provider items (#120 stage 2)

The saved conversation is a list of Responses-shaped items. A provider can
return content Shellboy must keep and return but must not interpret: Anthropic
`thinking` (with `signature`), `redacted_thinking`, and block types that do not
exist yet. Such content is stored as one **opaque item**, an envelope that says
which dialect it belongs to:

```json
{"type":"provider_opaque","dialect":"anthropic_messages","kind":"thinking","block":{...as received...}}
```

- `dialect` (non-empty ASCII, at most 64 bytes) names the wire dialect that
  produced the block; `kind` (at most 64 bytes) is the provider's own block type,
  kept for display and diagnostics only; `block` is a JSON object. Shellboy never
  reads inside `block`. The envelope is the only shape the core validates.
- Sending: an encoder for dialect D unwraps only items whose `dialect` is D and
  puts `block` back on the wire unchanged. Every other encoder skips them. This
  generalizes "Chat skips Responses reasoning items": Responses and Chat now drop
  all `provider_opaque` items, and a dialect-D encoder drops items of other
  dialects. Native Responses `reasoning` items keep their current handling.
- Lifetime: the existing reasoning-epoch rule applies to every opaque item. It is
  replayed only while the Thread's connection and model are unchanged, because
  providers bind signatures to the model. Changing either leaves the saved item
  untouched but no longer sent.
- Storage and round trip: the item lives in the assistant output array of the
  adopted node, so adoption, replay, deletion, GC and RAG scoping need no new
  table, queue or worker. Strings inside `block` (thinking text, signature) are
  byte-exact through save and replay. JSON object key order is not preserved
  (`serde_json::Value` orders keys) and numbers use `f64` beyond `u64`; providers
  ignore key order, and no current block carries such numbers.
- Not shown or indexed as body text. History search and compaction summary
  material skip opaque items (like reasoning, never resurrected into summaries).
  The screen and history search read only known text fields of a saved node, so
  an opaque item contributes no text at all; it is still visible in the node's
  raw payload (details view). A `kind`-only line in the transcript is deferred
  until a dialect that produces opaque items ships.
- Code: `model::opaque` owns the envelope (`wrap`, `validate`, `block_for`); the
  completed-response check accepts the type and validates only the envelope;
  `control::replay::convert` and the summary renderer drop opaque items outside
  their epoch; `dialect::{responses,chat}` encoders skip them.
- Unknown item types are still `unsupported_item` failures for every dialect.
  A dialect decoder is what turns an unknown provider block into this envelope,
  so the rule "keep the original, never guess" lives with the dialect, not here.

### Anthropic Messages dialect (#120 stage 3)

`api = "anthropic_messages"` sends `POST {base_url}/messages`. Sources are the
public Anthropic documentation (Messages, streaming, errors, versions, caching,
models) read on 2026-11-01; no real provider call has been made.

- Headers: `anthropic-version` is a constant in the dialect (`2023-06-01`, the
  version whose streaming format is documented in detail), never inferred.
  Raising it is a deliberate code change. `content-type` and the credential path
  stay shared with the other dialects: `api_key_env` defaults to
  `Authorization: Bearer`; `api_key_header` selects a valid non-transport-owned
  header and sends the environment value unchanged. The recommended Anthropic
  configuration is `api_key_header = "x-api-key"`; Azure's `api-key` and arbitrary
  gateway credential headers use the same generic path. A custom credential
  header cannot collide case-insensitively with `headers` or `extra_headers`.
  All resolved values use the existing sensitive-header/redaction path. Real
  Anthropic Bearer authentication remains unverified, not assumed compatible.
- Request: `model`, `max_tokens` (required by the API), `stream: true`,
  `messages`, a top-level `system`, and `tools`, plus the cache markers of the
  common cache hint (#127, below) unless `prompt_caching = false`. `system` is a
  string without the hint and one marked text block with it. The only other
  optional field is the Thread effort, sent as `output_config.effort` when set
  and absent otherwise (#126). The mapping lives in one function of the
  dialect: `minimal` becomes `low`, every other string is sent unchanged (no
  model catalog). No `thinking`, temperature, `tool_choice` or beta header; the
  user adds them with `extra_body` and `extra_headers` (#119). Reserved
  `extra_body` keys: `model`, `max_tokens`, `stream`, `messages`, `system`,
  `tools`, `output_config`, `cache_control`.
- Provider settings have three tiers (#126). (1) Shellboy concepts, converted by
  each dialect's encoder; (2) opaque passthrough, `extra_body`/`extra_headers`;
  (3) opaque response blocks stored and returned unchanged. A setting moves from
  tier 2 to tier 1 only if (a) placing it correctly needs structure only
  Shellboy knows, or (b) it varies per Thread or Turn and Shellboy already holds
  that state. Otherwise it stays in tier 2. Thread effort satisfies (b);
  thinking on/off does not, so it stays in `extra_body`.
- `max_tokens`: connection setting `max_tokens` (positive integer), default 32768,
  chosen to be below the documented 128K output cap of current models and large
  enough that routine coding turns do not stop at `max_tokens`. It is only valid
  with `api = "anthropic_messages"`.
- Conversation mapping from the Responses-shaped items: system and developer
  items before the first user message become `system`; developer items after it
  (event notices, context lines) are sent as user messages (`context_role` does
  not apply here because the dialect cannot carry them anywhere else). Consecutive
  items of one role are merged into one message with several content blocks;
  `function_call` items become `tool_use` blocks of the assistant message and
  `function_call_output` items become `tool_result` blocks at the start of the
  next user message, ordered as Anthropic requires (all results of one assistant
  turn together, before other user content). Images map to `image` blocks with a
  `base64` or `url` source. The final message must be a user message; if replay
  would end with an assistant message the request fails locally with
  `input_invalid` instead of sending a prefill that current models reject.
- Opaque blocks: `thinking`, `redacted_thinking` and any unknown block type are
  stored as `provider_opaque` items with `dialect = "anthropic_messages"` and
  returned inside the assistant message in their original position and unchanged
  (stage 2). `text` and `tool_use` are the only blocks the core reads.
- Streaming: `message_start`, `content_block_start|delta|stop`, `message_delta`,
  `message_stop`, `ping` (ignored) and `error` are read. `text_delta` feeds the
  existing text events; `input_json_delta` accumulates the tool arguments by block
  index (checked against `MAX_TOOL_ARGUMENT_BYTES`), parsed once at the block's
  stop; `thinking_delta`, `signature_delta` and unknown deltas accumulate into the
  opaque block unread. Unknown event types are skipped and counted. A stream is
  complete only after `message_stop`; EOF before it is `incomplete`.
- `stop_reason`: `end_turn`, `stop_sequence` and `tool_use` complete the response;
  `max_tokens` is an `output_limit` only when the response status is `incomplete`;
  `model_context_window_exceeded` remains generic `incomplete` (context limit);
  `pause_turn`, `refusal` and unknown values retain their existing failure kinds.
- Usage: `input_tokens` plus `cache_creation_input_tokens` plus
  `cache_read_input_tokens` is the total input; `cached_tokens` is
  `cache_read_input_tokens`; `output_tokens` is taken from the last (cumulative)
  `message_delta`. Missing counters stay unknown. Totals are not invented:
  input totals require all three components and checked addition within the
  shared signed-64-bit counter range. Independently known cache-read/output
  counters are retained even when the total is unknown. An absent delta counter
  keeps its previous cumulative fact; a present invalid counter replaces it
  with unknown rather than retaining an obsolete value.
  `cache_creation_input_tokens` is also saved on its own as
  `input_tokens_details.cache_creation_tokens` (#127).
- Errors: an in-stream `error` event and an HTTP error body
  `{"type":"error","error":{"type","message"}}` map `error.type` to the provider
  code. `overloaded_error` (529) and `rate_limit_error` (429) are retried by the
  existing transient rules; `request_too_large` (413) and `invalid_request_error`
  (400) are not.
- Summary requests and `input_characters` use the same encoder.

### Cross-API output-limit classification (#293)

The connection's optional `max_tokens` setting maps to the selected API's native
request key, but an output-limit failure is classified only from an explicit
provider terminal fact:

- Responses: `status = incomplete` and
  `incomplete_details.reason = max_output_tokens`.
- Chat Completions: `finish_reason = length` on the final normalized incomplete
  response.
- Anthropic Messages: `status = incomplete` and `stop_reason = max_tokens`.

Only these exact cases use error kind `output_limit` and its connection-setting
remedy. The response remains incomplete: retain the provider raw response and
available partial text/items; never convert truncation into success or silently
retry it. These terminal limit responses are not automatically retried. Other
incomplete/finish/stop reasons are not guessed as output exhaustion: this
includes Anthropic `model_context_window_exceeded`, refusals, filter outcomes,
unknown reasons and transport/EOF failures without the matching terminal fact.
They retain their existing classification and partial-output handling.

### Common cache hint (#127)

A tier-1 common concept (promotion condition (a): placing it correctly needs
structure only Shellboy knows). `dialect::CacheHint` is derived from the
canonical request before `context_role` mapping:

- `prefix_items`: the leading system/developer items. With the tools they are
  the stable prefix.
- `conversation_items`: the end of the growing conversation. Trailing
  system/developer items after it are left after the breakpoint: attachment
  notices and a summary prompt, which change per request, and saved notices
  that happen to be last (event notices, and the `Shellboy context v1` line,
  which is saved history since #183, appended at a Turn start or after a
  change; see [replay cache](replay-cache.md)). The next request's lookback
  still finds the entry.

Connection `prompt_caching` (bool, default true, opt-out) decides whether the
hint is passed to the encoder at all. `false` sends nothing cache-related.

- Messages: explicit `cache_control: {"type":"ephemeral"}` on the last tool, on
  the single system text block, and on the last cacheable block (text, image,
  `tool_use`, `tool_result`; never thinking or other opaque blocks) of the
  conversation, in wire order. At most three of the four allowed breakpoints;
  all use the default 5-minute TTL, so the non-increasing TTL rule holds. Too
  short a prefix is simply not cached (no error). Each request adds only a few
  blocks, well within the 20-block lookback. Top-level automatic caching is not
  used: it would mark the last block, which was the per-request context line
  when #127 was decided (see the #127 decision). Since #183 that line is saved
  history; the explicit breakpoints are unchanged. `cache_control` is a reserved
  `extra_body` key.
- Responses: nothing new. Providers cache prefixes automatically; the existing
  opt-in `prompt_cache_key` (Thread UUID) and session header stay the routing
  hints. `prompt_cache_key = true` with `prompt_caching = false` is rejected.
- Chat Completions: nothing.
- `input_characters` counts content without markers.

Prefix stability: the system prefix is the fixed system text plus the Turn's
working-instruction snapshot; the tools are fixed per configuration. Events
Shellboy inserts mid-conversation are appended as saved history items and never
rewrite earlier items. What does invalidate the cache, by design: editing
AGENTS.md (system, next Turn), an attachment update (its source message content
changes in place), compaction or trim (history replaced), and a connection or
model change.

### D-010 fact-preserving Chat tolerances (#64 review)

Accept stop with calls only when every call has a nonempty name and identity
(provider or derived below) and fully parsed JSON-object arguments. Keep the
original finish_reason and a stop_with_complete_tool_calls marker. Core's usual
shell schema/group validation still applies. Repeated full declared `shell`
name deltas are ignored and counted; partial name fragments still concatenate.
For missing/empty tool ids, require a nonempty response id: hash its UTF-8 bytes
once with SHA-256, then hash that hex seed plus the 8-byte big-endian index. Use
`call_sb_` plus 48 hex digits; mark the original id, source/index and synthesis.
No response identity is guessed if both identities are absent. Response ids have
the same 1024-byte bound as call ids, preventing repeated audit metadata from
amplifying a large id across up to 1024 tool calls. Finish-reason strings are
bounded to 64 bytes for the same repeated-metadata budget.

An omitted choices field is empty only on a chunk carrying non-null usage, with
an omission counter. Other malformed choices still fail. Store normalization
facts in normalized output items (also in the direct-client raw envelope), so
existing assistant_response persistence and raw/F6 inspection retain them;
Core does not persist every top-level response property. An otherwise empty
usage-only answer carries facts on an empty assistant message, not invented text.
These rules are wire normalization, not legacy Core/data-format fallbacks.

## Explicit reset facts and connection cooldown (#76)

Recognize only structured `usage_limit_reached` in error.type/code with a future
integer Unix-second resets_at, or positive integer resets_in_seconds. Prefer
resets_at when both exist. Accept the same object in a JSON error.message, or
the complete JSON suffix after a literal `litellm.RateLimitError` prefix (one
wrapper only, bounded by the existing error-body cap). Plain prose, malformed
values and generic 429 without a reset do not establish quota exhaustion.
HTTP 429 with valid Retry-After greater than the existing 600-second automatic
wait ceiling establishes a rate-limit cooldown, not a guessed plan quota.

One fixed-size optional fact/deadline lives in each configured shared HTTP
client; clones share it, capacity equals configured connections, owner restart
discards it. Expiry uses Tokio monotonic time anchored to the observed UTC reset.
No timer, table, worker or queue is added. Already-in-flight calls are not
cancelled; concurrent facts retain the later deadline. The gate covers Responses
and Chat Completions generation (including summaries), not embeddings/catalogs.
Conversation failures enter existing recovery_wait without retry or reduction.
Cached rejection sends no HTTP; it still saves an ordinary failed local attempt
with locally_blocked=true and no HTTP observation, not a fabricated provider call.

Recovery details and the existing reason string show type, UTC reset, optional
window and choices to wait or select another connection/model using /model.
No auto-switch, auto-resume, usage prediction or extra provider heuristics.
Bottom-line TUI rendering is deferred to #85 to avoid #41 F/G overlap.

This #76 bounded fact slot is the sole exception to the earlier client-reuse
section's no-additional-runtime-cache/mutex statement. Relative durations are
rounded up to a UTC second (never retry early); HTTP-date resets retain their
exact second. Invalid/truncated error bodies do not establish body-based facts.
The connection gate does not skip existing input preparation, and switching
only the model on the same blocked connection does not bypass it.

A long 429 Retry-After is sufficient at headers: return its rate-limit fact
without waiting for the error body. Consequently that case does not extract
body-specific plan/window details. Body-based quota recognition still requires
a complete valid supported object. This avoids waiting on a stalled error body.

### Remaining approved header-wait correction after F (#95)

For a response-header wait admitted before its deadline, classify cancellation
first, then available headers, then local timeout. Never start polling a new
outgoing request when the entry is already cancelled/expired. Preserve known
permanent HTTP status even if its body stalls; do not invent unknown transport
and retry that status. Existing pending deadlines and retry policy are unchanged.
This is the previously approved #95 transport patch, independent of F browser
state. Its paused-clock and request-JSONL tests remain part of qualification.

## Bounded shell descriptions

Shell calls may include an optional model-authored description. Received function
call arguments are saved and replayed verbatim, including long descriptions and
JSON whitespace. Only execution deserialization and display labels are capped at
80 Unicode scalar values. No description normalization or truncation metadata is
added to the response or adopted node. The existing MAX_TOOL_ARGUMENT_BYTES bound
limits the entire arguments string, just as for command text. Display strips
plain-text controls and falls back to
the command for absent/empty/whitespace-only descriptions. The description is not
an authorization or a verified explanation of the command. shell.describe_calls
is true by default; generation schemas require both command and description
while leaving the description text and empty-string allowance unchanged. False
omits the property and its required entry, and uses command labels. Missing/empty
description in received model output remains executable and falls back to command
labels; raw arguments are still saved/replayed verbatim. Adding the required
entry costs 14 serialized UTF-8 bytes, not a measured token estimate.
Existing history/DB formats, lifecycle and workers stay
unchanged. Incomplete opaque audit input remains subject to existing transport
limits; it is not accepted as an executable description.

## Optional cache diagnostics (#142 part 1)

The [adopted cache diagnostic contract](cache-diagnostics.md) adds an opt-in
Responses comparison identity and a separate bounded provider-fact whitelist.
It preserves terminal validation, usage normalization, cancellation and adoption
CAS. No sticky response/request headers are captured or forwarded.

## Bounded content-filter recovery facts (#159)

Incomplete Responses retain the original provider response in the existing attempt
failure/audit record. Recovery additionally projects `incomplete_details.reason`
and provider filter verdicts into `ApiError.details.content_filter` so the existing
Turn/Run recovery hint exposes them without reading unbounded original output.
This is a factual projection, not a new provider error classification or lifecycle.
Unknown or absent reason/verdict fields remain unknown, not inferred from status,
category spelling, output, or free-form messages. Provider keys and JSON values
are opaque; names such as `protected_material_code` do not establish why a match
occurred. Terminal incomplete JSON and typed SSE responses retain the existing
no-automatic-retry boundary. Interrupted transports retain their existing policy.
No retry policy, setting, schema, dependency, worker or queue is added.

The projection visits only bounded filter containers in the response envelope and
output items, never arbitrary text fields. It preserves provider field paths and
JSON scalar values with explicit truncation/omission facts. Limits apply to visited
items, nesting, number of facts, key/value characters and total display length;
missing `filtered`, `detected` and `severity` values are explicit unknowns. Original
saved JSON remains unchanged and is still available through existing raw reads.
Human-facing generated labels and actions follow the configured UI language;
provider keys and values, including unknown enum strings, are not translated.

For the exact reason `content_filter`, show the provider-filter stop fact and
available actions without promising success: explicit retry (verdicts may differ),
rephrase and resume, remove relevant tool output or attachments (`/detach` for
attachments). When copying existing files is the task, shell `cp` or `curl` can
copy them without having the model emit their body. This is optional task guidance,
not a claim about the filter category or automatic tool execution. Machine facts
use stable action identifiers; localized prose is a UI/readback projection.

Source: issue #159 and its protected-material example comment (2026-11-02).
Verification is recorded separately in acceptance; mock evidence does not establish
behavior of a real Azure/OpenAI deployment.

Projection bounds: 32 scalar/unknown facts, 512 visited nodes, depth 8, path 192
characters and scalar string 128 characters. Output/content lists and filter maps
are inspected only up to the fact limit. Saved JSON for synthetic hostile inputs
is checked below 64 KiB. Within this bounded set, a literal returned
`filtered=true` field is shown first in the short band; this is display ordering,
not category interpretation. Full bounded verdicts remain in details. Facts for
the generated recovery guide are projected before generic safe-detail string
expansion so provider strings that look like JSON remain strings. Existing raw
safe-detail/copy behavior is otherwise unchanged.


The optional connection `max_tokens` value is also mapped to Responses
`max_output_tokens` and Chat Completions `max_completion_tokens`; those fields
are omitted when unset. Messages keeps its existing 32768 default. The wire
adapter receives the optional value so it does not invent a default for either
of the other APIs. Legacy Messages-only adapter entry points retain their
existing behavior for non-Messages callers.

When `max_tokens` is unset for Responses or Chat Completions, an existing
`extra_body` value for that API's native output-limit key still passes through.
When `max_tokens` is set, also supplying the same native key in `extra_body` is
an explicit configuration/send error, even for an equal value or null. Other
API keys are not newly reserved. Existing Messages reserved-field rules remain
unchanged; this does not add aliases or silently choose between duplicate limits.
