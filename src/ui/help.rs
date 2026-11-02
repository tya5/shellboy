//! Task-oriented help and contextual readouts, rendered by the shared popup reader.
use super::theme;
use crate::config::{Config, UiLanguage};

use super::i18n::local;
use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

pub(super) struct Page {
    pub title: &'static str,
    pub context: &'static str,
    content: Content,
}
enum Content {
    Fixed(Vec<Line<'static>>),
    Guide(Vec<Row>),
}
enum Row {
    Section(String),
    Entry(String, String),
    Note(String, UiLanguage),
    Blank,
}
struct Guide {
    rows: Vec<Row>,
    language: UiLanguage,
}

impl super::UiState {
    pub(super) fn show_help(&mut self, page: Page) -> Vec<super::UiAction> {
        self.help_cache.clear();
        self.invalidate_popup_paint();
        self.help_above_popups = false;
        self.help_page = page;
        self.help_scroll = 0;
        self.help_visible = true;
        vec![]
    }
}

impl Page {
    pub fn info(title: &'static str, context: &'static str, text: String) -> Self {
        let text = crate::presentation::sanitize_plain(&text, 0).0;
        Self {
            title,
            context,
            content: Content::Fixed(
                text.lines()
                    .map(|line| Line::from(line.to_owned()))
                    .collect(),
            ),
        }
    }

    pub fn guide(config: &Config) -> Self {
        let mut page = Guide {
            rows: Vec::new(),
            language: config.ui.language,
        };
        page.section(local!(config.ui.language, "よく使う操作", "Common actions"));
        use super::key_hints::Action;
        for action in [
            Action::Help,
            Action::Send,
            Action::Newline,
            Action::Commands,
            Action::History,
            Action::Threads,
            Action::Interrupt,
            Action::Stop,
        ] {
            page.entry(
                &action.binding(config),
                action.description(config.ui.language),
            );
        }
        page.entry(
            "/help",
            local!(
                config.ui.language,
                "操作ガイドを開く（F1と同じ）",
                "Open the operation guide (same as F1)"
            ),
        );
        page.section(local!(
            config.ui.language,
            "回答をコピーする",
            "Copy an answer"
        ));
        page.entry(
            &crate::ui::text_width::glyphs::formatted!(
                "{} → {}",
                "{} > {}",
                Action::History.binding(config),
                Action::Copy.binding(config)
            ),
            local!(
                config.ui.language,
                "焦点を当てた項目の原文を丸ごとコピーする",
                "Copy the complete original of the focused item"
            ),
        );
        page.entry(
            &crate::ui::text_width::glyphs::formatted!(
                "{} → {}",
                "{} > {}",
                Action::History.binding(config),
                Action::Code.binding(config)
            ),
            Action::Code.description(config.ui.language),
        );
        page.entry(
            crate::ui::text_width::glyphs::select("o → c", "o > c"),
            local!(
                config.ui.language,
                "詳細から原文を開き、表示中の原文ページをコピーする",
                "Open the original from details and copy the displayed page"
            ),
        );
        page.entry(
            "/copy",
            local!(config.ui.language, "焦点（なければ読んでいる位置）のコードが1つならコピー、複数なら番号を選ぶ。コードなしなら本文全体", "Copy the only code block in the focused item (or reading position); choose a number if there are several, or copy the full text if there are none"),
        );
        page.entry(
            "/copy all",
            local!(
                config.ui.language,
                "コードだけでなく本文全体をコピーする",
                "Copy the full text, not only code"
            ),
        );
        page.note(local!(config.ui.language, "番号の表示は1始まり。/copy 項目ID 番号の指定は0始まりです。64KiBを超える本文はoで原文、nで続き、pで先頭を確認します。F2は見えている画面だけの範囲選択です。", "Displayed numbers start at 1; /copy item ID and number start at 0. For text over 64 KiB, use o for original, n for the next page, and p for the first. F2 selects only the visible terminal screen."));
        page.section(local!(
            config.ui.language,
            "画像やファイルを添付する",
            "Attach images and files"
        ));
        page.entry(
            "/image",
            local!(
                config.ui.language,
                "クリップボードの画像を下書きに添付する（送信はしない）",
                "Attach a clipboard image to the draft without sending it"
            ),
        );
        page.entry(
            local!(config.ui.language, "/attach パス", "/attach PATH"),
            local!(
                config.ui.language,
                "画像やファイルを下書きに添付する",
                "Attach an image or file to the draft"
            ),
        );
        page.note(local!(config.ui.language, "普通の貼り付けは文字です。画像を取り出せない環境では /attach パス を使います。添付の印・状態は下書きで確認し、上限は設定のlimitsに従います。", "Ordinary paste is text. If clipboard images are unavailable, use /attach PATH. Check attachment marks and status in the draft; configured limits apply."));
        if !config.ui.send_key.eq_ignore_ascii_case("Ctrl+U")
            && !config
                .ui
                .newline_keys
                .iter()
                .any(|key| key.eq_ignore_ascii_case("Ctrl+U"))
        {
            page.entry(
                "Ctrl+U",
                local!(
                    config.ui.language,
                    "現在の下書きと添付参照を消去する。送信済み依頼は保持",
                    "Clear the current draft and attachment references; sent requests are retained"
                ),
            );
        }
        if super::redraw::binding_available(config) {
            page.entry("Ctrl+L", local!(config.ui.language, "画面全体を描き直す。下書き・選択・表示位置は保持", "Redraw the entire screen without changing draft, selection, or reading position"));
        } else {
            page.entry(
                "Ctrl+L",
                local!(
                    config.ui.language,
                    "画面再描画は無効（送信・改行キーの設定と重複）",
                    "Redraw is disabled because its key conflicts with send or newline"
                ),
            );
        }
        page.entry(
            "Ctrl+C",
            local!(
                config.ui.language,
                "Runと所有する背景の作業を停止する",
                "Stop the Run and its background work"
            ),
        );
        page.entry(
            "Ctrl+Q / /quit",
            local!(
                config.ui.language,
                "保存・停止処理をしてアプリを終了する",
                "Save and stop work before quitting the application"
            ),
        );
        page.entry(
            "/",
            crate::ui::text_width::glyphs::localized!(
                config.ui.language,
                "コマンド候補を表示。↑↓で選び、Tabで補完",
                "コマンド候補を表示。^vで選び、Tabで補完",
                "Show commands; select with ↑↓ and complete with Tab",
                "Show commands; select with ^v and complete with Tab"
            ),
        );

        page.section(local!(
            config.ui.language,
            "会話を読む・切り替える",
            "Read and switch conversations"
        ));
        page.entry(
            local!(config.ui.language, "ホイール", "Mouse wheel"),
            local!(
                config.ui.language,
                "会話を上下にスクロールする",
                "Scroll the conversation up or down"
            ),
        );
        page.entry(
            "PgUp / PgDn",
            local!(
                config.ui.language,
                "会話を画面単位で前後に読む",
                "Read the conversation one screen at a time"
            ),
        );
        page.entry(
            "Ctrl+End",
            local!(
                config.ui.language,
                "会話の末尾へ戻り、新しい出力に追従する",
                "Return to the end and follow new output"
            ),
        );
        page.entry(
            "Ctrl+T",
            local!(
                config.ui.language,
                "一覧/Watchを直前のモードで開く（/threadsは一覧）",
                "Open the last list or Watch mode (/threads opens the list)"
            ),
        );
        page.entry(
            local!(config.ui.language, "一覧内 Tab", "List Tab"),
            local!(
                config.ui.language,
                "一覧とWatchを切り替える。文字入力は絞り込み",
                "Switch between list and Watch; typing filters the list"
            ),
        );
        page.entry(
            "/threads active",
            local!(
                config.ui.language,
                "復旧待ち・失敗・未読完了・実行中の作業を監視",
                "Watch recovery, failures, unread completions, and running work"
            ),
        );
        page.entry(
            "/threads descendants",
            local!(
                config.ui.language,
                "現在の会話を親とする子孫の作業を監視",
                "Watch descendants of the current conversation"
            ),
        );
        let descendant_key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('d'),
            crossterm::event::KeyModifiers::ALT,
        );
        if !super::key_matches(&config.ui.send_key, descendant_key)
            && !config
                .ui
                .newline_keys
                .iter()
                .any(|binding| super::key_matches(binding, descendant_key))
        {
            page.entry(
                "Alt+D",
                local!(
                    config.ui.language,
                    "現在の会話の子孫をWatchで開く",
                    "Open descendants of the current conversation in Watch"
                ),
            );
        } else {
            page.entry(
                "Alt+D",
                local!(config.ui.language, "子孫監視の短縮キーは無効（送信・改行と重複）。/threads descendants を使用", "Descendant Watch shortcut is disabled by a send/newline conflict; use /threads descendants"),
            );
        }
        page.entry(
            local!(
                config.ui.language,
                "Watch内 Ctrl+S / Alt+R",
                "Watch Ctrl+S / Alt+R"
            ),
            local!(
                config.ui.language,
                "表示中のRunを停止／復旧待ちを再試行。確認対象が変われば未適用",
                "Stop a displayed Run or retry recovery; a changed target is not applied"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "Watch内 Alt+O / Enter",
                "Watch Alt+O / Enter"
            ),
            local!(
                config.ui.language,
                "保存出力を読む／会話を開く。削除は一覧側で操作",
                "Read saved output or open a conversation; delete from the list"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "一覧内 Delete / Ctrl+D",
                "List Delete / Ctrl+D"
            ),
            local!(
                config.ui.language,
                "選んだ会話の削除影響を確認する。まだ削除しない",
                "Review deletion effects for the selected conversation; this does not delete yet"
            ),
        );
        page.note(local!(config.ui.language, "削除確認ではD/Spaceで子孫も含めるか変更。影響の取得中や実行中の作業がある場合はYで実行できません。Yで削除、N/Escで一覧へ戻ります。文字入力とBackspaceは一覧の絞り込みです。", "In deletion confirmation, D/Space toggles descendants. Y cannot execute while effects are loading or work is running. Y deletes; N/Esc returns to the list. Typing and Backspace filter the list."));
        page.note(local!(
            config.ui.language,
            "macOSのノートのdeleteはBackspaceです。削除確認にはCtrl+Dかfn+deleteを使ってください。",
            "On macOS laptops delete is Backspace. Use Ctrl+D or fn+delete to confirm deletion."
        ));
        page.entry(
            "Ctrl+N",
            local!(
                config.ui.language,
                "新しい会話の下書きを作る（/new）",
                "Create a new conversation draft (/new)"
            ),
        );
        page.entry(
            "/switch ID",
            local!(
                config.ui.language,
                "指定した会話へ切り替える",
                "Switch to the specified conversation"
            ),
        );
        page.entry(
            crate::ui::text_width::glyphs::select("↑ / ↓", "^ / v"),
            crate::ui::text_width::glyphs::localized!(
                config.ui.language,
                "入力の先頭行で送信履歴を呼び出す。↓で新しい履歴へ",
                "入力の先頭行で送信履歴を呼び出す。vで新しい履歴へ",
                "Recall sent history on the first input line; ↓ selects newer history",
                "Recall sent history on the first input line; v selects newer history"
            ),
        );
        page.note(crate::ui::text_width::glyphs::localized!(
            config.ui.language,
            "送信履歴の最新からさらに↓で、書きかけの下書きに戻ります。",
            "送信履歴の最新からさらにvで、書きかけの下書きに戻ります。",
            "Press ↓ after the newest sent entry to restore the unfinished draft.",
            "Press v after the newest sent entry to restore the unfinished draft."
        ));

        page.section(local!(
            config.ui.language,
            "反映待ち・中断・復元",
            "Queued input, interruption, and recovery"
        ));
        if !config.ui.send_key.eq_ignore_ascii_case("Ctrl+Enter") {
            page.entry(
                "Ctrl+Enter",
                local!(
                    config.ui.language,
                    "すぐ渡す（応答生成は中断しない）",
                    "Deliver now without interrupting generation"
                ),
            );
        }
        page.entry(
            crate::ui::text_width::glyphs::select("Ctrl+X → Ctrl+S", "Ctrl+X > Ctrl+S"),
            local!(
                config.ui.language,
                "すぐ渡す。前景shellの待機だけを解除する",
                "Deliver now and release only the foreground shell wait"
            ),
        );
        page.entry(
            "/queue",
            local!(
                config.ui.language,
                "保存済みの反映待ち・保留入力を確認／取消",
                "Inspect or cancel saved queued and held input"
            ),
        );
        page.entry(
            "/restore list / /restore draft ID",
            local!(
                config.ui.language,
                "保存した取消入力・未確定送信を復元",
                "Restore saved cancelled input or an unconfirmed send"
            ),
        );
        page.entry(
            "Esc",
            local!(
                config.ui.language,
                "画面を閉じる。通常の会話ではTurnだけ中断",
                "Close a screen; in the conversation, interrupt only the Turn"
            ),
        );

        page.section(row_label(
            crate::presentation::rows::RowKind::LiveWork,
            config.ui.language,
        ));
        page.entry(
            local!(
                config.ui.language,
                "Shift+Tab / クリック",
                "Shift+Tab / click"
            ),
            crate::ui::text_width::glyphs::localized!(
                config.ui.language,
                "入力欄の上にある作業を選ぶ。↑↓で移動",
                "入力欄の上にある作業を選ぶ。^vで移動",
                "Select work above the input; use ↑↓ to move",
                "Select work above the input; use ^v to move"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "Alt+W / 見出しクリック",
                "Alt+W / click heading"
            ),
            local!(
                config.ui.language,
                "作業の中身を開閉。開いた高さは ui.live_work_rows 行で固定",
                "Toggle work details; expanded height is fixed by ui.live_work_rows"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "作業内 Space / F6 / o",
                "Within work Space / F6 / o"
            ),
            local!(
                config.ui.language,
                "shell出力の開閉／詳細フォーム／子の会話へ",
                "Toggle shell output, open details, or open a child conversation"
            ),
        );
        page.note(local!(config.ui.language, "Escで入力へ戻ります。見出しには最古の作業と最後の出力の経過を表示し、折りたたみ中も残ります。画面が低く区画がない場合は状態欄に表示します。回答後も残る作業は保存時刻が基準です。抜粋を開いても実行・停止しません。", "Esc returns to input. The heading shows the oldest work and last output ages even when folded; when the screen is too short for the section, the status bar shows them instead. Work that remains after an answer uses saved timestamps. Opening an excerpt does not execute or stop work."));

        page.section(local!(
            config.ui.language,
            "内容・処理状況を調べる",
            "Inspect content and activity"
        ));
        page.entry(
            "F6",
            local!(
                config.ui.language,
                "会話の内容を選び、詳細・原文・IDを確認（/details）",
                "Select conversation content to inspect details, original, and ID (/details)"
            ),
        );
        page.entry(
            "F7",
            local!(
                config.ui.language,
                "現在の項目のリンクを確認・完全URLをコピー（/links）",
                "Inspect links in the current item and copy complete URLs (/links)"
            ),
        );
        page.entry(
            "F8",
            local!(
                config.ui.language,
                "操作を選び、対象や引数をフォームで指定（/actions / /operations）",
                "Select an action and enter target and arguments in a form (/actions / /operations)"
            ),
        );
        page.entry(
            "/status thread",
            local!(
                config.ui.language,
                "現在の会話の処理・背景コマンド・子の状態を確認",
                "Inspect the current conversation's work, background commands, and children"
            ),
        );
        page.entry(
            "/status",
            local!(
                config.ui.language,
                "全会話の作業を確認。終了済みも見るには /status all",
                "Inspect work across conversations; use /status all to include finished work"
            ),
        );
        page.entry(
            local!(config.ui.language, "/search 検索語", "/search QUERY"),
            local!(
                config.ui.language,
                "過去の会話を検索する",
                "Search past conversations"
            ),
        );
        page.entry(
            local!(config.ui.language, "/received [ページ]", "/received [PAGE]"),
            local!(
                config.ui.language,
                "失敗・ストリーム差分の受信記録を一覧し、原文を読む",
                "List failed and stream-difference receipts and read their originals"
            ),
        );
        page.note(crate::ui::text_width::glyphs::localized!(
            config.ui.language,
            "詳細画面: ↑↓で選択 → Enterで詳細。Oで原文、Cでコピー。",
            "詳細画面: ^vで選択 > Enterで詳細。Oで原文、Cでコピー。",
            "Details: select with ↑↓ and open with Enter. O opens the original; C copies.",
            "Details: select with ^v and open with Enter. O opens the original; C copies."
        ));
        page.note(local!(
            config.ui.language,
            "回答が終わっても背景処理が続く場合があります。受付と処理完了は別です。",
            "Background work can continue after an answer. Acceptance and completion are distinct."
        ));

        page.section(local!(
            config.ui.language,
            "失敗・復旧待ち",
            "Failures and recovery"
        ));
        page.note(local!(config.ui.language, "新しい復旧待ちは端末ベルで通知します（他の会話・子も対象、2秒以内はまとめます）。ui.bell = false で無効化できます。", "A terminal bell signals new recovery work, including other conversations and children; events within two seconds are grouped. Disable with ui.bell = false."));
        page.note(local!(config.ui.language, "音・点滅・タブ表示は端末設定に従います。tmux等ではベルの通過設定が必要な場合があります。", "Sound, flashing, and tab indicators depend on terminal settings. Multiplexers such as tmux may need bell passthrough."));
        page.entry(
            "/retry",
            local!(
                config.ui.language,
                "現在の復旧待ちを同じ内容で明示的に再試行する",
                "Explicitly retry the current recovery work with the same input"
            ),
        );
        page.entry(
            local!(config.ui.language, "入力して送信", "Type and send"),
            local!(
                config.ui.language,
                "新しい指示で再開する。元の指示と区別して保存",
                "Resume with a new instruction, saved separately from the original"
            ),
        );
        page.entry(
            "/stop / /cancel / Ctrl+C",
            local!(
                config.ui.language,
                "現在の作業を停止する",
                "Stop the current work"
            ),
        );
        page.note(local!(config.ui.language, "原因と未反映指示は会話下部に残ります。F8で作業・応答処理の詳細を確認できます。", "The cause and pending instruction remain at the bottom of the conversation. F8 shows work and response details."));
        page.note(local!(
            config.ui.language,
            "保存済みツール結果は再利用します。結果が不明な副作用は詳細で確認してください。",
            "Saved tool results are reused. Check details for side effects with unknown results."
        ));

        page.section(local!(
            config.ui.language,
            "背景処理・待機",
            "Background work and waiting"
        ));
        page.entry(
            "Ctrl+B",
            local!(
                config.ui.language,
                "手前で待っているshellコマンドを背景で継続（/background）",
                "Continue the foreground shell command in the background (/background)"
            ),
        );
        page.entry(
            "/cancel-wait",
            local!(
                config.ui.language,
                "待機の観測だけをやめる。待機対象の処理は続く",
                "Stop observing the wait only; the target work continues"
            ),
        );
        page.note(local!(
            config.ui.language,
            "Ctrl+Cは処理の中断です。待機の観測をやめる操作とは異なります。",
            "Ctrl+C interrupts work; it is different from stopping wait observation."
        ));

        page.section(local!(
            config.ui.language,
            "モデル・作業フォルダー",
            "Model and working folder"
        ));
        page.entry(
            "/models",
            local!(
                config.ui.language,
                "モデル一覧を開き、選んで変更する",
                "Open the model list and select a model"
            ),
        );
        page.entry(
            "/model",
            local!(
                config.ui.language,
                "現在の接続・モデル・推論強度を確認する",
                "Inspect the current connection, model, and reasoning effort"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "/model 接続名 モデル名",
                "/model CONNECTION MODEL"
            ),
            local!(
                config.ui.language,
                "接続とモデルを名前で指定して変更する",
                "Change connection and model by name"
            ),
        );
        page.entry(
            local!(config.ui.language, "/effort 値", "/effort VALUE"),
            local!(
                config.ui.language,
                "推論強度を変更。指定解除は /effort none",
                "Change reasoning effort; /effort none clears it"
            ),
        );
        page.entry(
            local!(config.ui.language, "/agents [パス]", "/agents [PATH]"),
            local!(
                config.ui.language,
                "指示ファイル参照を確認／変更する",
                "Inspect or change the instruction-file reference"
            ),
        );
        page.entry(
            "/cwd",
            local!(
                config.ui.language,
                "現在の作業フォルダーを確認する",
                "Inspect the current working folder"
            ),
        );
        page.entry(
            local!(config.ui.language, "/cwd パス", "/cwd PATH"),
            local!(
                config.ui.language,
                "作業フォルダーを変更する",
                "Change the working folder"
            ),
        );

        page.section(local!(
            config.ui.language,
            "添付・貼付け・コピー",
            "Attachments, paste, and copy"
        ));
        page.entry(
            local!(config.ui.language, "/attach パス", "/attach PATH"),
            local!(config.ui.language, "ファイルを添付する", "Attach a file"),
        );
        page.entry(
            "/attachments",
            local!(
                config.ui.language,
                "添付の一覧・詳細を確認する",
                "Inspect the attachment list and details"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "/detach 添付ID",
                "/detach ATTACHMENT_ID"
            ),
            local!(
                config.ui.language,
                "指定した添付を外す",
                "Remove the specified attachment"
            ),
        );
        page.entry(
            "/reload-attachments",
            local!(
                config.ui.language,
                "保存済みの添付を再読込する（モデルへは送信しない）",
                "Reload saved attachments without sending them to the model"
            ),
        );
        page.entry(
            "/image / /paste-image",
            local!(
                config.ui.language,
                "クリップボードの画像を添付する",
                "Attach a clipboard image"
            ),
        );
        page.entry(
            "F3 / F4 / F5",
            local!(
                config.ui.language,
                "最新の長文貼付けを確認 / 編集 / 削除する",
                "Inspect, edit, or remove the latest long paste"
            ),
        );
        page.entry(
            "/paste ID show",
            local!(
                config.ui.language,
                "指定した貼付けを確認。編集はedit、削除はremove",
                "Inspect the specified paste; use edit to change it and remove to delete it"
            ),
        );
        page.entry(
            "/copy",
            local!(
                config.ui.language,
                "現在の項目の原文をコピーする",
                "Copy the original of the current item"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "/copy 項目ID コード番号",
                "/copy ITEM_ID CODE_NUMBER"
            ),
            local!(
                config.ui.language,
                "指定したコードをコピーする（番号は0始まり）",
                "Copy the specified code block (numbers start at 0)"
            ),
        );
        page.entry(
            "F2",
            local!(
                config.ui.language,
                "端末の範囲選択・コピーへ切り替える（/select）",
                "Switch to terminal selection and copy (/select)"
            ),
        );
        page.note(local!(
            config.ui.language,
            "長文の折りたたみ表示も、送信時は全文を送ります。",
            "Collapsed long pastes are sent in full."
        ));
        page.note(local!(
            config.ui.language,
            "貼付けだけでは送信しません。貼付けた / から始まる文も本文として扱います。",
            "Pasting does not send. Pasted text beginning with / remains message text."
        ));

        page.section(local!(
            config.ui.language,
            "会話の行の印",
            "Conversation row marks"
        ));
        for kind in crate::presentation::rows::RowKind::ALL {
            let (mark, _, _) = kind.spec();
            page.entry(mark, row_label(kind, config.ui.language));
        }
        page.note(local!(config.ui.language, "背景は並行して続く shell です。経過は保存された開始時刻が基準です。通知を選んで Enter で元の行・子の会話へ移れます。", "Background means a shell continuing concurrently. Elapsed time uses the saved start time. Select a notice and press Enter to open its original row or child conversation."));
        page.note(
            local!(config.ui.language, "種類の境目は空行で分け、続く tool は詰めて表示します。空行は選択対象ではありません。", "Blank lines separate kinds; consecutive tool rows stay together. Blank lines are not selectable."),
        );

        page.section(local!(
            config.ui.language,
            "履歴の整理・検索の管理",
            "History and search management"
        ));
        page.entry(
            local!(config.ui.language, "/fork [項目ID]", "/fork [ITEM_ID]"),
            local!(
                config.ui.language,
                "指定位置から会話を分岐。省略時は閲覧位置を使う",
                "Fork the conversation at the specified position; defaults to the reading position"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "/edit [依頼の項目ID]",
                "/edit [REQUEST_ITEM_ID]"
            ),
            local!(
                config.ui.language,
                "以前の依頼を編集し、送信時に別の会話へ分岐する",
                "Edit a previous request and fork into a new conversation when sent"
            ),
        );
        page.entry(
            "/compact-max",
            local!(
                config.ui.language,
                "APIなしで保護外を最大縮小（再試行は別操作）",
                "Shrink unprotected history without an API call; retry is separate"
            ),
        );
        page.entry(
            "/compact",
            local!(
                config.ui.language,
                "会話の要約・圧縮を依頼する",
                "Request conversation summary and compaction"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "/restore（引数なし）",
                "/restore (no arguments)"
            ),
            local!(
                config.ui.language,
                "現在の会話で、縮小した入力範囲を元に戻す。後から付いた履歴も残す",
                "Restore compacted input in the current conversation while retaining later history"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "/reasoning [項目ID]",
                "/reasoning [ITEM_ID]"
            ),
            local!(
                config.ui.language,
                "モデルから返された推論テキストの表示を切り替える",
                "Toggle model-returned reasoning text"
            ),
        );
        page.entry(
            local!(config.ui.language, "/restore 番号", "/restore NUMBER"),
            local!(
                config.ui.language,
                "失敗時の下書きを復元する。番号は状態表示を参照",
                "Restore a failed-send draft; see status for its number"
            ),
        );
        page.entry(
            "/index status",
            local!(
                config.ui.language,
                "検索索引の進み具合を確認する",
                "Inspect search indexing progress"
            ),
        );
        page.entry(
            "/index thread",
            local!(
                config.ui.language,
                "現在の会話を索引管理の対象にする",
                "Manage indexing for the current conversation"
            ),
        );
        page.entry(
            "/index all",
            local!(
                config.ui.language,
                "全会話を索引管理の対象にする",
                "Manage indexing for all conversations"
            ),
        );
        page.entry(
            crate::ui::text_width::glyphs::select("/index threads ID…", "/index threads ID."),
            local!(
                config.ui.language,
                "指定した会話を索引管理の対象にする",
                "Manage indexing for the specified conversation"
            ),
        );
        page.entry(
            local!(
                config.ui.language,
                "/index space ID [対象]",
                "/index space ID [TARGET]"
            ),
            local!(
                config.ui.language,
                "指定した埋め込み空間の索引を管理する",
                "Manage indexing in the specified embedding space"
            ),
        );
        page.note(local!(
            config.ui.language,
            "索引の更新は対象・費用を確認してから実施します。",
            "Review target and cost before updating the index."
        ));
        page.note(local!(
            config.ui.language,
            "⌕ 索引停止中は /index を開き、T で再試行します。",
            "When ⌕ shows an index pause, open /index and press T to retry."
        ));
        page.note(local!(
            config.ui.language,
            "取りこぼしの復旧には preview と backfill を使います。",
            "Use preview and backfill to recover missed conversations."
        ));

        page.section(local!(
            config.ui.language,
            "詳しいガイドを読む",
            "Read detailed guides"
        ));
        page.note(local!(
            config.ui.language,
            "次のコマンドは端末のshellで使います。会話入力欄の / コマンドとは別です。",
            "Run these commands in a terminal shell, not in the conversation input."
        ));
        page.entry(
            "shellboy docs get --id shellboy/tui",
            local!(
                config.ui.language,
                "画面操作の詳しい説明を読む",
                "Read detailed screen-operation documentation"
            ),
        );
        page.entry(
            "shellboy docs list",
            local!(
                config.ui.language,
                "ほかのガイドを探す",
                "Find other guides"
            ),
        );
        page.note(local!(
            config.ui.language,
            "[ ] は省略可能な引数です。項目ID・パスなどは実際の値に置き換えます。",
            "[ ] denotes optional arguments. Replace item IDs and paths with actual values."
        ));

        page.section(local!(
            config.ui.language,
            "参考：メモリに保持するデータの設定",
            "Reference: retained in-memory data settings"
        ));
        let cache = &config.cache;
        for (label, bytes) in [
            (
                local!(
                    config.ui.language,
                    "本文・表示情報",
                    "Bodies and display information"
                ),
                cache.body_bytes,
            ),
            (
                local!(config.ui.language, "画面レイアウト", "Screen layout"),
                cache.layout_bytes,
            ),
            (
                local!(
                    config.ui.language,
                    "モデル送信用の履歴",
                    "History for model requests"
                ),
                cache.history_bytes,
            ),
            (
                local!(
                    config.ui.language,
                    "添付の送信用データ",
                    "Attachment data for sending"
                ),
                cache.attachment_base64_bytes,
            ),
        ] {
            page.entry(label, &format!("{bytes} bytes"));
        }
        let total = u128::from(cache.body_bytes)
            + u128::from(cache.layout_bytes)
            + u128::from(cache.history_bytes)
            + u128::from(cache.attachment_base64_bytes);
        page.entry(
            local!(config.ui.language, "合計", "Total"),
            &format!("{total} bytes"),
        );
        page.note(local!(
            config.ui.language,
            "保持量の設定です。処理中の作業領域などを含むアプリ全体の使用量ではありません。",
            "These are retention settings, not total application memory including work in progress."
        ));
        page.section(&super::i18n::Message::TerminalWidthHeading.render(config.ui.language));
        page.note(
            &super::i18n::Message::TerminalWidth {
                observed: crate::ui::text_width::terminal_width_observation(),
            }
            .render(config.ui.language),
        );
        Self {
            title: local!(config.ui.language, "ヘルプ", "Help"),
            context: crate::ui::text_width::glyphs::localized!(
                config.ui.language,
                "操作するときはEscで会話へ戻る · / でコマンド候補を表示",
                "操作するときはEscで会話へ戻る | / でコマンド候補を表示",
                "Esc returns to the conversation · / shows commands",
                "Esc returns to the conversation | / shows commands"
            ),
            content: Content::Guide(page.rows),
        }
    }
    pub(super) fn lines(&self, width: u16) -> std::borrow::Cow<'_, [Line<'static>]> {
        use crate::ui::text_width::UnicodeWidthStr;
        match &self.content {
            Content::Fixed(lines) => std::borrow::Cow::Borrowed(lines),
            Content::Guide(rows) => {
                let column = rows
                    .iter()
                    .filter_map(|row| match row {
                        Row::Entry(key, _) => Some(key.width()),
                        _ => None,
                    })
                    .max()
                    .unwrap_or(0)
                    .min(usize::from(width) / 5);
                std::borrow::Cow::Owned(
                    rows.iter()
                        .flat_map(|row| {
                            let key_style = Style::default()
                                .fg(theme::EMPHASIS)
                                .add_modifier(Modifier::BOLD);
                            match row {
                                Row::Blank => vec![Line::default()],
                                Row::Section(title) => vec![Line::from(Span::styled(
                                    title.clone(),
                                    Style::default()
                                        .fg(theme::SECTION_HEADING)
                                        .add_modifier(Modifier::BOLD),
                                ))],
                                Row::Entry(key, description) => {
                                    let columns = width >= 46;
                                    let description_indent = if columns { column + 4 } else { 2 };
                                    if columns && key.width() <= column {
                                        vec![Line::from(vec![
                                            Span::styled(
                                                format!(
                                                    "  {key}{}",
                                                    " ".repeat(column + 2 - key.width())
                                                ),
                                                key_style,
                                            ),
                                            Span::styled(
                                                description.clone(),
                                                Style::default().fg(theme::TEXT),
                                            ),
                                        ])]
                                    } else {
                                        vec![
                                            Line::from(Span::styled(format!("  {key}"), key_style)),
                                            Line::from(format!(
                                                "{}{description}",
                                                " ".repeat(description_indent)
                                            )),
                                        ]
                                    }
                                }
                                Row::Note(text, language) => vec![Line::from(format!(
                                    "  {}{text}",
                                    match language {
                                        UiLanguage::En => "Note: ",
                                        UiLanguage::Ja => "補足: ",
                                    }
                                ))],
                            }
                        })
                        .collect(),
                )
            }
        }
    }
}
pub(super) fn footer(language: UiLanguage) -> &'static str {
    crate::ui::text_width::glyphs::localized!(
        language,
        "Esc / Enter / F1 戻る · ↑↓ / PgUp / PgDn 閲覧",
        "Esc / Enter / F1 戻る | ^v / PgUp / PgDn 閲覧",
        "Esc / Enter / F1 Back · ↑↓ / PgUp / PgDn Scroll",
        "Esc / Enter / F1 Back | ^v / PgUp / PgDn Scroll"
    )
}
fn row_label(kind: crate::presentation::rows::RowKind, language: UiLanguage) -> &'static str {
    use crate::presentation::rows::RowKind::*;
    if language == UiLanguage::Ja {
        return kind.spec().1;
    }
    match kind {
        Human => "User",
        External => "External",
        Parent => "Parent",
        SelfModel => "Self",
        OtherModel => "Model",
        Revisit => "Revisit",
        Assistant => "Answer",
        ToolCall => "Tool call",
        Running => "Running",
        Unknown => "Unknown",
        Success => "Completed",
        Failure => "Failed",
        Cancelled => "Cancelled / not started",
        Notice => "Notice",
        Summary => "Summary",
        Spill => "Spilled",
        LiveWork => "Live work",
    }
}

impl Guide {
    fn section(&mut self, title: &str) {
        if !self.rows.is_empty() {
            self.rows.push(Row::Blank);
        }
        self.rows.push(Row::Section(title.to_owned()));
    }
    fn entry(&mut self, key: &str, description: &str) {
        self.rows
            .push(Row::Entry(key.to_owned(), description.to_owned()));
    }
    fn note(&mut self, text: &str) {
        self.rows.push(Row::Note(text.to_owned(), self.language));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::text_width::UnicodeWidthStr;
    use crate::ui::{UiState, draw};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};

    fn japanese_config() -> Config {
        let mut config = Config::default();
        config.ui.language = UiLanguage::Ja;
        config
    }
    fn key(ui: &mut UiState, code: KeyCode) {
        assert!(
            ui.event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                .is_empty()
        );
    }
    fn screen(ui: &mut UiState, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, ui)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                let mut line = String::new();
                let mut x = 0;
                while x < width {
                    let symbol = buffer[(x, y)].symbol();
                    line.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn guide_keys_follow_measured_width_in_both_languages() {
        use crate::ui::text_width;
        for policy in [Some(1), Some(2), None] {
            text_width::set_terminal_width(policy);
            for language in [UiLanguage::En, UiLanguage::Ja] {
                let mut config = Config::default();
                config.ui.language = language;
                for width in [80, 120] {
                    let page = Page::guide(&config);
                    let lines = page.lines(width);
                    let text = lines
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join("\n");
                    let (chain, arrows) = if policy == Some(1) {
                        ("o → c", "↑↓")
                    } else {
                        ("o > c", "^v")
                    };
                    assert!(text.contains("preview"), "{policy:?} {language:?}: {text}");
                    assert!(text.contains("backfill"), "{policy:?} {language:?}: {text}");
                    for line in lines.iter().map(ToString::to_string).filter(|line| {
                        line.contains("preview") || (line.contains("⌕") && line.contains("/index"))
                    }) {
                        assert!(
                            line.width() <= usize::from(width),
                            "{policy:?} {language:?}: {line}"
                        );
                    }
                    assert!(text.contains(chain), "{policy:?} {language:?}: {text}");
                    assert!(text.contains(arrows), "{policy:?} {language:?}: {text}");
                    assert!(footer(language).contains(arrows));
                    // Guide rows are wrapped by the popup reader, not Page::lines.
                    let mut ui = UiState::new(&config, std::env::temp_dir());
                    key(&mut ui, KeyCode::F(1));
                    let shown = screen(&mut ui, width, 24);
                    assert!(shown.contains(arrows), "{policy:?} {language:?}: {shown}");
                    for row in shown.lines() {
                        assert!(
                            row.width() <= usize::from(width),
                            "{policy:?} {language:?} {width}: {row}"
                        );
                    }
                }
            }
        }
        text_width::clear_terminal_width();
    }

    #[test]
    fn thread_deletion_help_distinguishes_confirmation_from_filter_editing() {
        let shown = Page::guide(&japanese_config())
            .lines(160)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        for expected in [
            "Delete / Ctrl+D",
            "まだ削除しない",
            "D/Space",
            "Yで実行できません",
            "N/Esc",
            "fn+delete",
            "Backspace",
        ] {
            assert!(shown.contains(expected), "{expected}: {shown}");
        }
    }

    #[test]
    fn owned_work_help_describes_heading_and_short_screen_fallback() {
        for (language, phrases) in [
            (UiLanguage::Ja, ["最古の作業", "折りたたみ中", "状態欄"]),
            (
                UiLanguage::En,
                ["oldest work", "even when folded", "status bar"],
            ),
        ] {
            let mut config = Config::default();
            config.ui.language = language;
            let text = Page::guide(&config)
                .lines(160)
                .iter()
                .map(ToString::to_string)
                .collect::<String>();
            for phrase in phrases {
                assert!(text.contains(phrase), "{language:?}: missing {phrase}");
            }
        }
    }

    #[test]
    fn guide_columns_align_and_narrow_layout_keeps_notes_distinct() {
        let page = Page {
            title: "fixture",
            context: "fixture",
            content: Content::Guide(vec![
                Row::Entry("F1".into(), "操作ガイド".into()),
                Row::Entry("Ctrl+Q".into(), "保存して終了".into()),
                Row::Note("補足の情報".into(), UiLanguage::Ja),
            ]),
        };
        let wide = page.lines(80);
        let offsets = wide[..2]
            .iter()
            .map(|line| line.spans[0].width())
            .collect::<Vec<_>>();
        assert_eq!(offsets[0], offsets[1], "description columns must align");
        let narrow = page.lines(16);
        assert_eq!(
            narrow.len(),
            5,
            "narrow keys and explanations use separate rows"
        );
        assert!(
            narrow[4]
                .spans
                .iter()
                .any(|span| span.content.contains("補足:")),
            "notes are not keyboard bindings"
        );
    }
    #[test]
    fn help_renders_in_both_languages_at_standard_widths() {
        fn show(ui: &mut UiState) {
            let config = ui.config.clone();
            ui.show_help(Page::guide(&config));
        }
        fn expected(language: UiLanguage) -> &'static [&'static str] {
            match language {
                UiLanguage::En => &["Common actions", "Copy an answer"],
                UiLanguage::Ja => &["よく使う操作", "回答をコピーする"],
            }
        }
        super::super::i18n::verify_screen(show, expected);
    }
    #[test]
    fn first_page_uses_current_bindings_and_help_scrolling_preserves_the_draft() {
        let mut config = japanese_config();
        config.ui.send_key = "Ctrl+Enter".into();
        config.ui.newline_keys = vec!["Enter".into()];
        let mut ui = UiState::new(&config, std::env::temp_dir());
        ui.insert("保持する下書き", false);
        let draft = ui.view().draft.clone();
        let anchor = ui.view().navigation.anchor.clone();
        key(&mut ui, KeyCode::F(1));
        let first = screen(&mut ui, 80, 24);
        for expected in [
            "よく使う操作",
            "Ctrl+Enter",
            "次の区切りでメッセージを渡す",
            "改行",
            "Ctrl+C",
            "回答をコピーする",
            "Esc",
        ] {
            assert!(first.contains(expected), "{expected}: {first}");
        }
        assert!(!first.contains("型付き") && !first.contains("cache"));
        let all = ui
            .help_page
            .lines(76)
            .iter()
            .flat_map(|line| &line.spans)
            .map(|span| span.content.as_ref())
            .collect::<String>();
        for expected in [
            "Ctrl+Q",
            "会話を読む・切り替える",
            "画像やファイルを添付する",
            "/image",
            "/attach",
        ] {
            assert!(all.contains(expected), "{expected}");
        }
        for _ in 0..30 {
            key(&mut ui, KeyCode::PageDown);
            screen(&mut ui, 80, 24);
        }
        let end = screen(&mut ui, 80, 24);
        assert!(
            all.contains("メモリに保持するデータ")
                && end.contains("端末の表示")
                && end.contains("bytes"),
            "{end}"
        );
        assert!(end.contains("Esc"));
        key(&mut ui, KeyCode::Enter);
        assert!(!ui.help_visible);
        assert_eq!(ui.view().draft, draft);
        assert_eq!(ui.view().navigation.anchor, anchor);
        key(&mut ui, KeyCode::F(1));
        assert!(screen(&mut ui, 80, 24).contains("よく使う操作"));
        for (width, height) in [(32, 12), (8, 5)] {
            assert!(screen(&mut ui, width, height).contains("Esc"));
        }
    }
    #[test]
    fn large_help_windows_show_the_final_sentence_after_paging_and_resizing() {
        let mut ui = UiState::new(&japanese_config(), std::env::temp_dir());
        key(&mut ui, KeyCode::F(1));
        for (width, height) in [(80, 24), (120, 40), (180, 65), (80, 24)] {
            screen(&mut ui, width, height);
            for _ in 0..30 {
                key(&mut ui, KeyCode::PageDown);
                screen(&mut ui, width, height);
            }
            let text = screen(&mut ui, width, height);
            // Ignore frame/scrollbar cells between the sentence's wrapped rows.
            let compact = text
                .chars()
                .filter(|ch| {
                    !ch.is_whitespace() && !matches!(ch, '|' | '-' | '#' | '│' | '─' | '█')
                })
                .collect::<String>();
            assert!(
                compact.contains("アプリ全体の使用量ではありません。"),
                "{width}x{height}: {text}"
            );
            assert!(text.contains("Esc"));
        }
    }
    #[test]
    fn information_readouts_have_their_own_titles_and_return_to_the_operation_guide() {
        let mut ui = UiState::new(&japanese_config(), std::env::temp_dir());
        for (command, title) in [("/model", "モデル設定"), ("/cwd", "作業フォルダー")] {
            assert!(ui.slash(command).is_empty());
            let text = screen(&mut ui, 80, 24);
            assert!(
                text.contains(title) && !text.contains("よく使う操作"),
                "{text}"
            );
            key(&mut ui, KeyCode::Esc);
        }
        key(&mut ui, KeyCode::F(1));
        assert!(screen(&mut ui, 80, 24).contains("よく使う操作"));
    }
}
