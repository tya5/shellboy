//! Task-oriented help and contextual readouts, rendered by the shared popup reader.
use super::theme;
use crate::config::Config;
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
    Note(String),
    Blank,
}
struct Guide {
    rows: Vec<Row>,
}

impl super::UiState {
    pub(super) fn show_help(&mut self, page: Page) -> Vec<super::UiAction> {
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
        let mut page = Guide { rows: Vec::new() };
        page.section("よく使う操作");
        page.entry("F1 / /help", "操作ガイドを開く");
        page.entry(&config.ui.send_key, "メッセージを送る");
        page.entry(&config.ui.newline_keys.join(" / "), "入力中に改行する");
        if !config.ui.send_key.eq_ignore_ascii_case("Ctrl+U")
            && !config
                .ui
                .newline_keys
                .iter()
                .any(|key| key.eq_ignore_ascii_case("Ctrl+U"))
        {
            page.entry(
                "Ctrl+U",
                "現在の下書きと添付参照を消去する。送信済み依頼は保持",
            );
        }
        if super::redraw::binding_available(config) {
            page.entry("Ctrl+L", "画面全体を描き直す。下書き・選択・表示位置は保持");
        } else {
            page.entry("Ctrl+L", "画面再描画は無効（送信・改行キーの設定と重複）");
        }
        page.entry("Ctrl+C", "現在の会話の処理を中断する");
        page.entry("Ctrl+Q / /quit", "保存・停止処理をしてアプリを終了する");
        page.entry("/", "コマンド候補を表示。↑↓で選び、Tabで補完");

        page.section("会話を読む・切り替える");
        page.entry("ホイール", "会話を上下にスクロールする");
        page.entry("PgUp / PgDn", "会話を画面単位で前後に読む");
        page.entry("Ctrl+End", "会話の末尾へ戻り、新しい出力に追従する");
        page.entry("Ctrl+T", "会話一覧を開く（/threads）");
        page.entry("Ctrl+N", "新しい会話の下書きを作る（/new）");
        page.entry("/switch ID", "指定した会話へ切り替える");
        page.entry("↑ / ↓", "入力の先頭行で送信履歴を呼び出す。↓で新しい履歴へ");
        page.note("送信履歴の最新からさらに↓で、書きかけの下書きに戻ります。");

        page.section("内容・処理状況を調べる");
        page.entry("F6", "会話の内容を選び、詳細・原文・IDを確認（/details）");
        page.entry("F7", "現在の項目のリンクを確認・完全URLをコピー（/links）");
        page.entry(
            "F8",
            "操作を選び、対象や引数をフォームで指定（/actions / /operations）",
        );
        page.entry(
            "/status thread",
            "現在の会話の処理・背景コマンド・子の状態を確認",
        );
        page.entry(
            "/status",
            "全会話の作業を確認。終了済みも見るには /status all",
        );
        page.entry("/search 検索語", "過去の会話を検索する");
        page.entry(
            "/received [ページ]",
            "失敗・ストリーム差分の受信記録を一覧し、原文を読む",
        );
        page.note("詳細画面: ↑↓で選択 → Enterで詳細。Oで原文、Cでコピー。");
        page.note("回答が終わっても背景処理が続く場合があります。受付と処理完了は別です。");

        page.section("失敗・復旧待ち");
        page.note("新しい復旧待ちは端末ベルで通知します（他の会話・子も対象、2秒以内はまとめます）。ui.bell = false で無効化できます。");
        page.note("音・点滅・タブ表示は端末設定に従います。tmux等ではベルの通過設定が必要な場合があります。");
        page.entry("/retry", "現在の復旧待ちを同じ内容で明示的に再試行する");
        page.entry(
            "入力して送信",
            "新しい指示で再開する。元の指示と区別して保存",
        );
        page.entry("/stop / /cancel / Ctrl+C", "現在の作業を停止する");
        page.note("原因と未反映指示は会話下部に残ります。F8で作業・応答処理の詳細を確認できます。");
        page.note("保存済みツール結果は再利用します。結果が不明な副作用は詳細で確認してください。");

        page.section("背景処理・待機");
        page.entry(
            "Ctrl+B",
            "手前で待っているshellコマンドを背景で継続（/background）",
        );
        page.entry(
            "/cancel-wait",
            "待機の観測だけをやめる。待機対象の処理は続く",
        );
        page.note("Ctrl+Cは処理の中断です。待機の観測をやめる操作とは異なります。");

        page.section("モデル・作業フォルダー");
        page.entry("/models", "モデル一覧を開き、選んで変更する");
        page.entry("/model", "現在の接続・モデル・推論強度を確認する");
        page.entry(
            "/model 接続名 モデル名",
            "接続とモデルを名前で指定して変更する",
        );
        page.entry("/effort 値", "推論強度を変更。指定解除は /effort none");
        page.entry("/cwd", "現在の作業フォルダーを確認する");
        page.entry("/cwd パス", "作業フォルダーを変更する");

        page.section("添付・貼付け・コピー");
        page.entry("/attach パス", "ファイルを添付する");
        page.entry("/attachments", "添付の一覧・詳細を確認する");
        page.entry("/detach 添付ID", "指定した添付を外す");
        page.entry(
            "/reload-attachments",
            "保存済みの添付を再読込する（モデルへは送信しない）",
        );
        page.entry("/image / /paste-image", "クリップボードの画像を添付する");
        page.entry("F3 / F4 / F5", "最新の長文貼付けを確認 / 編集 / 削除する");
        page.entry(
            "/paste ID show",
            "指定した貼付けを確認。編集はedit、削除はremove",
        );
        page.entry("/copy", "現在の項目の原文をコピーする");
        page.entry(
            "/copy 項目ID コード番号",
            "指定したコードをコピーする（番号は0始まり）",
        );
        page.entry("F2", "端末の範囲選択・コピーへ切り替える（/select）");
        page.note("長文の折りたたみ表示も、送信時は全文を送ります。");
        page.note("貼付けだけでは送信しません。貼付けた / から始まる文も本文として扱います。");

        page.section("履歴の整理・検索の管理");
        page.entry(
            "/fork [項目ID]",
            "指定位置から会話を分岐。省略時は閲覧位置を使う",
        );
        page.entry(
            "/edit [依頼の項目ID]",
            "以前の依頼を編集し、送信時に別の会話へ分岐する",
        );
        page.entry("/compact", "会話の要約・圧縮を依頼する");
        page.entry(
            "/reasoning [項目ID]",
            "モデルから返された推論テキストの表示を切り替える",
        );
        page.entry(
            "/restore 番号",
            "失敗時の下書きを復元する。番号は状態表示を参照",
        );
        page.entry("/index status", "検索索引の進み具合を確認する");
        page.entry("/index thread", "現在の会話を索引管理の対象にする");
        page.entry("/index all", "全会話を索引管理の対象にする");
        page.entry("/index threads ID…", "指定した会話を索引管理の対象にする");
        page.entry(
            "/index space ID [対象]",
            "指定した埋め込み空間の索引を管理する",
        );
        page.note("索引の更新は対象・費用を確認してから実施します。");

        page.section("詳しいガイドを読む");
        page.note("次のコマンドは端末のshellで使います。会話入力欄の / コマンドとは別です。");
        page.entry(
            "shellboy docs get --id shellboy/tui",
            "画面操作の詳しい説明を読む",
        );
        page.entry("shellboy docs list", "ほかのガイドを探す");
        page.note("[ ] は省略可能な引数です。項目ID・パスなどは実際の値に置き換えます。");

        page.section("参考：メモリに保持するデータの設定");
        let cache = &config.cache;
        for (label, bytes) in [
            ("本文・表示情報", cache.body_bytes),
            ("画面レイアウト", cache.layout_bytes),
            ("モデル送信用の履歴", cache.history_bytes),
            ("添付の送信用データ", cache.attachment_base64_bytes),
        ] {
            page.entry(label, &format!("{bytes} bytes"));
        }
        let total = u128::from(cache.body_bytes)
            + u128::from(cache.layout_bytes)
            + u128::from(cache.history_bytes)
            + u128::from(cache.attachment_base64_bytes);
        page.entry("合計", &format!("{total} bytes"));
        page.note("保持量の設定です。処理中の作業領域などを含むアプリ全体の使用量ではありません。");
        Self {
            title: "ヘルプ",
            context: super::HELP,
            content: Content::Guide(page.rows),
        }
    }
    pub(super) fn lines(&self, width: u16) -> std::borrow::Cow<'_, [Line<'static>]> {
        use unicode_width::UnicodeWidthStr;
        match &self.content {
            Content::Fixed(lines) => std::borrow::Cow::Borrowed(lines),
            Content::Guide(rows) => std::borrow::Cow::Owned(
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
                                let description_indent = if columns { 22 } else { 2 };
                                if columns && key.width() <= 18 {
                                    vec![Line::from(vec![
                                        Span::styled(
                                            format!("  {key}{}", " ".repeat(20 - key.width())),
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
                            Row::Note(text) => vec![Line::from(format!("  補足: {text}"))],
                        }
                    })
                    .collect(),
            ),
        }
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
        self.rows.push(Row::Note(text.to_owned()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{UiState, draw};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    use unicode_width::UnicodeWidthStr;

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
    fn guide_columns_align_and_narrow_layout_keeps_notes_distinct() {
        let page = Page {
            title: "fixture",
            context: "fixture",
            content: Content::Guide(vec![
                Row::Entry("F1".into(), "操作ガイド".into()),
                Row::Entry("Ctrl+Q".into(), "保存して終了".into()),
                Row::Note("補足の情報".into()),
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
    fn first_page_uses_current_bindings_and_help_scrolling_preserves_the_draft() {
        let mut config = Config::default();
        config.ui.send_key = "Ctrl+Enter".into();
        config.ui.newline_keys = vec!["Enter".into()];
        let mut ui = UiState::new(&config, std::env::temp_dir());
        ui.insert("保持する下書き", false);
        let draft = ui.view().draft.clone();
        let anchor = ui.view().anchor.clone();
        key(&mut ui, KeyCode::F(1));
        let first = screen(&mut ui, 80, 24);
        for expected in [
            "よく使う操作",
            "Ctrl+Enter",
            "メッセージを送る",
            "入力中に改行する",
            "Ctrl+C",
            "Ctrl+Q",
            "会話を読む・切り替える",
            "Esc",
        ] {
            assert!(first.contains(expected), "{expected}: {first}");
        }
        assert!(!first.contains("型付き") && !first.contains("cache"));
        for _ in 0..30 {
            key(&mut ui, KeyCode::PageDown);
            screen(&mut ui, 80, 24);
        }
        let end = screen(&mut ui, 80, 24);
        assert!(
            end.contains("メモリに保持するデータ") && end.contains("bytes"),
            "{end}"
        );
        assert!(end.contains("Esc"));
        key(&mut ui, KeyCode::Enter);
        assert!(!ui.help_visible);
        assert_eq!(ui.view().draft, draft);
        assert_eq!(ui.view().anchor, anchor);
        key(&mut ui, KeyCode::F(1));
        assert!(screen(&mut ui, 80, 24).contains("よく使う操作"));
        for (width, height) in [(32, 12), (8, 5)] {
            assert!(screen(&mut ui, width, height).contains("Esc"));
        }
    }
    #[test]
    fn large_help_windows_show_the_final_sentence_after_paging_and_resizing() {
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
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
                .filter(|ch| !ch.is_whitespace() && !matches!(ch, '│' | '║' | '█'))
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
        let mut ui = UiState::new(&Config::default(), std::env::temp_dir());
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
