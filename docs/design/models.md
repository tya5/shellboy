# 接続先のモデル一覧

採用: 2026-10-25、利用者の「Shellboy内でモデル一覧を確認したい」に対応。組込みモデルcatalogを持たない既存方針を保ち、選択した接続先のlive一覧を取得する。

`/models [CONNECTION]` は省略時に現在の会話／新規下書きの接続を使い、既存接続のAPI rootへ `GET /models` を送る。接続の認証とheaders、HTTP clientのredirect／retry無効、取消し、有限body読込、秘密値を出さないエラー処理を再利用する。UIは非同期actionを渡し、メインOSスレッドでHTTPを待たない。取得は時間・body・件数・ID長を制限し、全結果を得られない場合は明示エラーにする。モデル生成は要求しない。

専用一覧に現在値のマーカーとモデルIDを表示し、文字入力で絞込み、上下で選択、Enterで現在の会話のモデルを変更する。変更は既存 `/model CONNECTION MODEL` と同じ型付きThreadSetへ接続し、未保存下書きではローカル選択を変更する。新規会話の既定やeffortは変更しない。Escは閉じるだけ。取得中／失敗／空一覧は区別する。閉じた画面や古い再取得の応答はrequest IDで拒否する。

応答中のIDをsort／重複除去して表示し、能力や契約可否をモデル名から推測しない。接続先がembedding等も列挙した場合はそのまま含む。一覧取得の成功はResponses利用可否や生成の動作保証ではない。IDに空白・制御文字がある等の不正応答は拒否し、IDをコマンド文字列へ再解釈せず型付きの変更操作へ渡す。

検証はGET pathと認証、有限読込・取消・不正応答・エラー秘匿、一覧の描画と絞込み、変更の型とscope、閉じた画面へのlate応答、実LiteLLMの一覧取得と専用WezTermでの表示を対象にする。利用者の設定・保存会話を検証用に変更しない。

## CLIと共有Control操作（2026-10-25採用）

利用者の追加指示により、原文CLI契約への追加として `model.list` を公開する。`shellboy model list [--connection NAME] [--thread ID]` はownerの型付きRPCを使い、`ModelListResult { connection, models }` を返す。接続の優先順位は明示connection、明示Thread、認証済みoriginのThread、ownerの既定接続。明示Threadはconnection併記時も存在検査する。通常の外部CLIで省略した場合に画面の選択Threadへ依存しない。資格情報はowner側だけで扱う。

HTTPはDB transaction外で行い、既存の受付・結果保存・operation IDによる再送規則と取消しを適用する。同じoperation IDの再送は取得済み一覧の再現であり、更新は新しい操作として要求する。一覧取得はThreadも既定設定も変更しない。TUIの一覧も同じControl実装を使い、F8へ33番目の型付き操作を追加する。モデル変更は既存 `thread.set` を使う。
