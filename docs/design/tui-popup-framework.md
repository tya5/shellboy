# TUIポップアップとリストの共通基盤

2026-10-25。依頼者の画面レビュー依頼と「個別調整より先にフレームワーク化し、レイアウト・デザインを統一する」指示に対応する。スレッド/モデル一覧の既存の見やすさを基準に、一覧・フォーム・詳細・検索/管理結果・原文・help・確認画面を揃える。

## レビューと方針

実WezTermの操作一覧・入力フォーム・履歴詳細を匿名fixtureで取得した。現状の弱点は、一覧の境界や選択行が弱いこと、補足・操作説明・値が同じ折返しParagraphへ入り区切りがないこと、余白・枠・footer・件数表示が画面ごとに異なること。管理結果が表示文字列から選択位置を探す実装も型付き行の対応へ置き換える。

`src/ui/popup.rs`を共通基盤とする。既存RatatuiのBlock/List/ListState/Paragraph/Scrollbarを使い、独自の描画engineや汎用操作controller、依存、コア状態、通知、DBを追加しない。各画面は引き続き固有のデータ・入力状態・非同期request ID・copy/原文操作を所有する。

## 共通の見た目と責務

- 共通frameが中央配置・余白・枠・見出し・補足・本文・固定footerのRectを決定する。通常画面は横幅の上限を設け、確認画面は内容に応じたコンパクト表示。小画面では本文を優先し、領域を飽和計算して重複や範囲外描画を防ぐ。
- タイトルは画面の目的、補足は対象/件数/絞込み/状態を短く示す。下部は主要操作とEsc戻るを常に確認できる固定領域とし、本文から区切る。見出し・補足・操作案内を本文と一緒にスクロールさせない。
- リストはRatatuiのList又はTableで項目を明示し、同じマーカー・選択色・横幅全体の背景を使う。未選択項目を読める明るさに保つ。型付きのselected indexを用い、描画済み文字列のmarker検索で選択位置を算出しない。選択に表示範囲を追従させ、件数・位置又はスクロールバーで続きがあることを示す。
- 背面は会話の先頭見出しを除いて行全体を消去し、枠境界を跨ぐ全角文字の断片を残さない。1項目が本文高を超える場合は共通部品で先頭と末尾を収め、省略を明示する。完全な入力・URL・copy対象は表示DTOとは別に保持する。
- 値やURLの長さで一覧全体の構造を崩さない。必要に応じて一覧と選択項目の詳細/入力領域を分ける。通常行の省略には明示的な印を使い、既存の原文/詳細取得・copy対象は完全値のまま維持する。入力値と原文は単なる省略表示を保存・実行へ渡さない。
- 原文/help/詳細本文は共通readerで折返しと表示範囲を計算する。固定footer、明示したscrollとリンクhit領域の基点を同じbody Rectへ揃える。リストとreaderは見た目を共有しても入力対象の意味を混同しない。
- 読込み中・空・失敗・表示外field・raw取得失敗は本文/補足の状態として保持し、選択行と区別する。確認は既定非実行、Y実行・N/Esc取消を維持する。フォームのvalidation errorとwait条件操作を画面外へ隠さない。
- スレッドtreeの枝線/列/詳細、モデルのlive取得/絞込み/現在モデルを維持し、frameとselection style等の共通部品を使う。各リストのPgDn取得等、対象固有の意味を共通描画部品へ押し込まない。

## リアクティブ性と検証

### 会話詳細の対象識別（2026-10-25追補）

「履歴項目」は「会話の詳細」へ改称する。単なる種別名だけを繰り返さず、各項目に位置番号・内容に対応する種別・本文の短い抜粋を表示する。会話はあなた/モデルの回答/推論、shellは呼出し/結果、イベントは保存された状態変更名として区別する。モデル出力にshell呼出しが含まれる場合はその事実も表示し、「実行」だけで閲覧対象を表さない。本文抜粋は既存のMarkdown平文化を再利用し、1行最大80表示桁、取得済みの有限windowだけから作る。未取得は未取得と示し、IDやJSONを本文の代わりに並べない。詳細画面にも選択元の内容を残す。

一覧の選択nodeと原文・copy・リンクの操作対象を一致させる。上下移動後のO/Cが以前選んだnodeを参照しないこと、F6で詳細から一覧へ戻る際は現在の対象に戻ることを検証する。照会を含む操作フォームのEnterは「送信」と表記し、閲覧操作が何かを実行する印象を避ける。確認が必要なmutationの既存確認は維持する。

UIは既存イベントと入力で描画する。共通基盤はI/O・DB・worker待ち・周期通知・履歴revision変更を行わない。overlay中に背景会話をホイールで動かさない。下書き/anchor、原文・copy・リンク参照、受付/確認/待機/キャンセル、古いrequest拒否を維持する。

TestBackendで統一した枠/固定footer/全幅選択、長い値・複数行・最終項目・Unicode・80x24/狭小画面を確認する。原文のcopy・request guard・リンク座標と下書き/履歴位置も回帰に含める。実WezTermで代表画面の変更前後を自己レビューする。匿名fixtureのみ使用し、実provider呼出しは不要。製品変更前にこの設計をコミットし、実装後は実施条件・結果・未検証を区別して記録する。

## ヘルプ内容の再構成（2026-10-25追補）

操作の羅列を、よく使う操作→会話の閲覧/切替→内容/処理状況の確認→モデル/作業場所→添付/貼付け→履歴整理/検索管理→参考情報の順へ整理する。見出しとキー/コマンドを型付きの表示行で区別し、共通reader/frameを使う。キー・構文・説明をslashで連結しない。略語・内部型名・全操作数を目立たせず、各行で利用者が何をできるか説明する。送信・改行は現在のconfigの値を表示する。既存の高度な操作とメモリ保持量の情報は後半へ残し、TUIガイドへ辿れるCLIを案内する。

同じreaderを使うモデル/作業場所/Thread照会には固有の題名と補足を付け、操作ヘルプとの混同を防ぐ。画面を閉じるキーとスクロール操作は固定footerへ置く。内容の確認中にコマンド文字列が入力/送信されることはない。設定値・用途別案内・狭小画面・スクロール・下書き/anchor保持を確認し、実端末で初期表示をレビューする。新規core状態・I/O・依存は不要。

### 大画面でのスクロール末尾（2026-10-25追補）

Ratatui 0.30.2 / widgets 0.3.2のScrollbarは、content_length−1を最大positionとし、viewport長を加えて全体比率を算出する。本文の総表示行数をそのままcontent_lengthへ渡すと、readerの上限（総行数−本文高）でもthumbが下端へ届かない。本文高が大きいほど差が目立つ。共通scrollbarへ渡す長さを「最大先頭行offset＋1」とし、positionをその範囲へ収める。本文の折返し・スクロール上限・原文は変更しない。小画面と大画面で本文最終行とthumb下端、先頭へ戻したthumb上端を同時に検証する。

### 復旧対象と参照対象（review B10）

復旧操作は現在選択中Threadの最新の保存済みRun/Turn投影を使い、過去の管理結果や閲覧中nodeの識別子を現在対象へ流用しない。原文/実行詳細を参照する操作は明示的な参照対象を使う。F8のRun再試行/停止の既定値は現在対象とし、詳細から呼び出したRun照会の対象とは区別する。

操作番号はUI内部のOpKindへ置換し、36操作の一覧・フォーム・typed Commandを網羅matchで対応させる。各操作のfield定義とtyped Command builderは同じmatch armに隣接して定義する。Formは非captureの関数pointerを保持し、共通のCommand validationを経由する。未知番号の成功fallbackは置かない。履歴一覧のrun/since/stateをフォームでも指定でき、削除のdescendantsと状態一覧のallはbooleanとして解釈する。タイトル操作とbooleanをCtrl+Uで空にした場合も、未指定の成功扱いにせずフォーム内で再選択を促し、他の入力値を保持する。表示位置の選択indexは一覧の位置に限定する。

復旧待ちは一時statusと独立した既存保存情報の投影を会話領域に置く。原因・保存済み対象・未反映指示・確認が必要な副作用・次の入口を示す。観測できない失敗段階や時刻、圧縮回数を推測しない。retry/新指示/停止は既存操作へ写し、再起動や設定変更だけでは再開しない。復旧中の時計は処理中の経過として増やさず、保存された時刻がなければ所要時間を断定しない。

復旧待ちの短い入口として `/retry` と `/stop` を採用する。`/retry` は現在Threadの保存投影が復旧待ちのときだけ既存RunRetryへ変換し、`/stop` は既存の人向け優先Cancel（同じcanonical stop reducer）へ変換する。新指示は通常送信を使う。管理結果・モデル設定の変更・時計は再開操作にしない。
保存されたmodel attemptのエラーと復旧理由・generationが一致するときだけ「モデル要求」の段階/終了時刻を使う。それ以外は段階/処理時間が未記録であることを示し、context超過の圧縮回数を捏造しない。背景処理・未回収出力・確定済み結果は既存台帳の別件数として表示する。

状態footerの1行目は持続する処理状態・追従・必要な計数、2行目は一時通知（なければ操作案内）へ分ける。通知を先頭、F1/F8の補助案内を残りの幅へ置き、現在の復旧理由を上書きしない。選択モードでは解除案内を優先する。0件のtool/取消や復旧待ち中の増加時計を出さず、幅不足では主要状態を先頭に維持する。


## 複数ツール呼出しの投影境界（B11）

本文抽出はhistory配下の順序付き借用leaf走査を正本とし、canonical本文、frontendのreasoning範囲、表示blockで同じ既知type/field順を使う。leafは文字offsetと最寄りのfunction_call/output contextを持ち、opaqueな未知typeを本文へ混ぜない。入力JSONの深い構造をRust再帰で辿らず、既存metadataの深さ/byte上限とrelease時のcanonical比較fallbackは維持する。

表示は保存DAGとcall順を並べ替えず、各call/resultに同一group内の呼出し番号を対応印として添え、短いcommandと状態を個別に表示する。未実行取消など結果payloadにcommandが無い場合も、node→tool_result→同一group/callのtool_callという保存済み対応から番号とargumentsを読み、他callから推測しない。対応元が回収済み/不在なら不明と明示し、call IDから原文を追えるようにする。execution ID、理由、状態はその個別blockの事実から取り、別callの情報を転用しない。背景で返された結果は返却時点の状態であることを区別し、履歴を現在状態と誤認させない。取消の保存理由が「先行callの失敗または前景待機の中断」の場合に限り、同じgroup内で先行する最後の非取消resultに記録された背景化/追加入力の理由を補足する。利用者停止など他の取消理由は上書きしない。未記録の原因や時刻は推定しない。raw/source/full-copyと元のoffset・hash・provenanceはそのまま維持し、Core状態や通知は追加しない。


番号・短いcommand・状態は表示用metadataであり、新しいCore lifecycle状態ではない。現在の呼出し状態は既存tool_call/shell_executionを既存の履歴更新で読み、過去のrunning resultには「返却時」と明示する。常時点滅する履歴nodeや新しいpoll/ticker/eventは追加せず、アクティビティの既存アニメーションを維持する。各callの見出しは元node内に残すため、結果を過去callの直下へ移動してカード全体を再配置しない。呼出しと後続結果でcommandの短い再掲が生じる点は元順序・source anchorを保つための意図的な選択とする。

参照取得はfrontendの既存履歴準備経路でDB workerへ渡す。投影済みblock（1 node最大128件、既存page最大50 node）のcallだけを対象とし、group/callの既存主キーを使う。command取得は512文字、見出しは空白を畳んだ120文字まで。UI metadataの64KiB予算にtool情報も課金し、超過時は表示情報を解放して原文導線へ戻す。完全command/IDや保存payloadを短縮値で置換しない。source不在・旧serde・partial window・別groupで同じcall ID・取消理由・metadata上限を回帰対象とする。

## 管理結果の所有と古い要求（R4-3、F5、F12）

管理popupはUI内の一所有者が本文frame・structured選択/詳細request ID・表示mode・索引対象/費用preview/確認待ち・display epochを保持する。frame内に本文とstructured状態をまとめ、閉じた本文とは別にfieldだけが残る状態を作らない。keyはActions/Close/Operations/Confirmの局所Outcomeを返し、描画は既存popup reader/listへ委譲する。アプリ全体の確認、操作formやThreadViewを管理ownerへ吸収しない。Statusの操作意味は型付きmodeで保持し、表示titleの日本語一致から推測しない。

現在display要求のcontextと元画面は同じ管理ownerへ保持する。Escのcloseはepoch・frame・出典context・索引対象/preview/確認待ちをまとめて失効させる（詳細から一覧へのEscだけはその要求IDを維持し、遅れた詳細pageをpending pointerで拒否する）。新要求またはThread切替後の旧report/費用previewは表示しない。fieldの参照先IDと通知ownerは別の意味であり、closed frameのfieldを次の操作へ流用しない。

mutationの成否通知はdisplay generationとは別で、既存のUI Status/DraftStatusへ送る。既存is_read_only分類を使い、report作成前にreceiptのok/error・照会用operation IDを伝える。先頭に成否/停止要求受付、次に操作名、最後に照会IDを置く。RunStop成功をRun終了と表現しない。描画準備だけの失敗を副作用失敗としない。元画面はUI-only ManagementContextのowner_thread（保存済み）またはowner_draft（未保存）に操作時点で束縛し、選択中の画面や操作の参照先へ読み替えない。owner_thread一つの追加はplaceholderへ保存draftをinstallするID置換に対応するためで、Core/wire/event/schema/worker/履歴を追加しない。

通常の送信受付はdraft IDを保持し、画面cacheのevictは本文だけでThreadView自体を消さないので、同UI session内の既存ownerへ通知できる。状態通知は最新観測だけを保持する揮発表示で、複数操作の全結果の正本は既存の有限receiptとoperation getである。UI終了後の通知、receipt契約を超える永続化や新しいjob台帳は保証しない。receiptを生成しない明示添付再読込は照会ID未取得と表示し、IDを捏造しない。旧report本体は新frameを置換/再openせず、別Threadの状態も上書きしない。

費用previewは既存human_labelを使った構造名で表示し、対象資料数などの値を保持する。詳細原文のJSON Pointer・scalar全文・paging/copyは従来通りで、表示名を参照キーへ書き戻さない。

後続の読取reportは表示内容だけを更新し、「結果を取得しました」という装飾statusで操作成否を上書きしない。後から到達した別操作の成否は従来どおり最新statusとなる。

## コピー対象の表示契約（F6）

コピー操作はUI内の有限な種別（原文の現在ページ、管理詳細の現在ページ、項目の完全値、本文全体、コードブロック全体）をworkerの結果まで渡す。ページにnext_cursorがあれば「続きあり」と明示する。next_cursorがないページも最後のページであるだけかもしれないので、本文全体や完全な項目へ昇格させない。追加取得・大きなcache・Core/wire/schemaへの情報追加は行わない。原文/fieldのraw文字列、既存64KiBコピー予算、取得失敗とコピー失敗の扱いは維持する。

OS backendが完了を返した場合だけ、対象を付けて「コピーしました」と通知する。OSC52は対象を付けた「端末へコピー要求を送信（完了未確認）」とし、端末側clipboardの完了を保証しない。画面で省略した表示ラベルではなく、既存の完全field値・原文pageの文字列を渡す。

## 入力の所有と終了表示（R4-2）

popupを単一の排他enumへ格納せず、既存の重ね表示を維持する。導出したtop_overlayは終了処理（その強制終了確認を含む）、確認、端末選択、モデル、操作、原文詳細、管理、help、Thread一覧の順で入力ownerを返す。composerが入力を所有する条件を共用し、animation/cursor/補完描画も同じmodal判定を使う。補完候補はcomposerの一部なので文字入力を妨げず、履歴mouse/linkだけは既存の補完表示除外も適用する。追加Core状態、DB、queue、workerは不要。

| 入力 | 所有・例外 |
|---|---|
| Ctrl+Q | 終了中は終了owner、それ以外では保存付き通常終了を要求 |
| Ctrl+C | 終了中は終了owner、それ以外は確認popupを含め現在Threadの中断意味を保持 |
| F2 / 選択中Esc | 確認・終了中を除いて端末選択modeを切替。選択中の通常入力は無視 |
| F3〜F5 | composerが所有するときだけ貼付けblockを表示/編集/除去。popupの背後は変更しない |
| F6 / F7 / F8 | 既存のモデル・操作・原文ownerを優先。それ以外では原文/リンク/操作への既存navigationを維持 |
| Paste | 最前面モデル/操作の入力field、またはcomposerだけが受け取る。確認/終了/選択/他popupの背後には届かない |
| Esc / 方向キーなど | 最前面ownerへ配送。終了確認・管理詳細など固有の戻り方は各ownerが保持 |

先に構造だけを分け、既存の例外を保つ。Ctrl+Cの確認popup透過、Pasteの確認/終了透過抑止、F3〜F5の背面draft変更抑止は別の挙動変更として表の回帰で固定する。

終了表示のquitting/stopped/forced/progress四fieldはUI-local LifecycleのRunning / Quitting(progress任意) / Stopped(forced)へまとめる。未受付の終了要求をqueue拒否したときだけRunningへ戻し、進捗受理後の重複拒否では終了を取り消さない。強制終了の事実を遅れた通常終了通知で消さない。Coreのshutdown、force、保存・回収責務は変更せず、既存表示状態の不正な組合せを防ぐための整理である。

Status一覧からInspect/Readへ進む場合、次の表示名からmodeを推測しない。既存Command結果と同じ有界worker messageに、実際のCommandがStatus一覧（operation照会以外）だったかのUI-only flagを付け、受理したstructured frameと同時にStatus navigationを更新する。一般結果では旧Status modeだけを解除し、Index/Attachments固有modeは維持する。follow検証・取得の失敗は現在frameを置換しないので、Statusの更新/次page導線を保持する。新しい待機state、Core event、公開JSONを追加しない。

Lifecycleの終端後は遅れた進捗でQuittingへ戻さず、入力も再開しない。保持する進捗tupleは従来と同じ一件だけで、終端へ移ると解放する。UIの終了結果読取はread accessorへ揃え、既存Coreの終了/force責務を増やさない。

## 中断と貼付け拒否の追加監査（U10、U11）

U10の確認popup中の中断はR4-2の全owner Ctrl+C/Ctrl+Q/F2表で再現・是正済み。global_keyは受信したCtrl+Cに必ず結果（対象なしなら空）を返すため、composer側の同一分岐は到達不能であり削除する。終了中の入力は既存shutdown ownerに属する例外を維持する。

U11はR4-2の優先順だけでは閉じない。選択/help/管理/詳細/確認/Thread一覧のPasteは入力へ反映しないが、それまで通知がなく、加えてpopupが既存status欄を消去し、端末選択modeは描画を止めている。通知の可視性と入力の保持を独立して回帰対象とする。

U11の拒否feedbackは最小幅でも結果が見えるよう「未反映」を先頭にし、貼付け内容を保持しない固定文をUIに一件だけ持ち、次のキー入力またはPasteで解除/置換する。workerのstatus更新では消さず、Threadのstatus/draftやreceiptへ保存しない。非入力popupと終了中では本文/fieldを変更せず、未反映と再操作方法を示す。端末選択modeは画面更新を止めるため、Pasteを反映せず選択modeだけを解除し、その自動動作と再貼付けが必要なことを実フレームで明示する。入力内容を推測して背面へ渡さず、追加cache/queue/通知worker/timerは作らない。

共通popup viewportは既存status二行を残した領域とする。全popupのdrawとresize前のpage計算は同じ幾何helperを使い、固定footerや末尾までのreader移動を保つ。これにより通常の非同期成否通知もpopupの背後へ消さない。幅/高さが不足してもfooterの先頭にある戻る/取消/待機などの主要keyを優先し、詳細な操作は画面を広げて確認できる。最小/幅広、reader末尾、拒否feedback中の固定footerを回帰対象とする。
