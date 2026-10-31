//! Opt-in, offline persisted histories for reviewing the real TUI in a terminal.
//! Creates a new directory only. Never touches a user's data or calls a provider.
use serde_json::{Value, json};
use shellboy::{history::append_node, storage::DbWorker};
use std::{error::Error, path::PathBuf};

type VisualCase<'a> = (&'a str, &'a str, Vec<(&'a str, Value)>);

fn answer(text: &str) -> Value {
    json!({"output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text}]}],"reasoning_epoch":0})
}
fn event(kind: &str, payload: Value) -> Value {
    json!({"role":"developer","content":[{"type":"input_text","text":format!("Shellboy event v1: {}", json!({"event_id":format!("visual-{kind}"),"kind":kind,"payload":payload}))}]})
}
fn context(kind: &str, change: Value) -> Value {
    event(
        "context_changed",
        json!({"kind":kind,"actor":{"kind":"shellboy"},"change":change,"observed_at":"2026-10-24T12:00:00+09:00","timezone":"+09:00"}),
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("Usage: visual_fixture NEW_DIRECTORY")?;
    let untitled = std::env::args_os().skip(2).any(|arg| arg == "--untitled");
    // The caller must choose a new directory, so an existing data-dir cannot be
    // accidentally overwritten or populated with synthetic history.
    std::fs::create_dir(&root)?;
    let root = std::fs::canonicalize(root)?;
    std::fs::write(
        root.join(".shellboy-visual-fixture"),
        "synthetic-history-v1\n",
    )?;
    let data = root.join("data");
    std::fs::create_dir(&data)?;
    std::fs::write(
        root.join("config.toml"),
        "default_connection='visual'\ndefault_model='offline-fixture'\n[connections.visual]\nbase_url='http://127.0.0.1:9/v1'\n",
    )?;
    let database = DbWorker::open(data.join("shellboy.db")).await?;
    let cwd = root.to_string_lossy().into_owned();
    let agents = root.join("AGENTS.md").to_string_lossy().into_owned();
    database.call(move |db| {
        let tx = db.transaction()?;
        let cases: Vec<VisualCase<'_>> = vec![
            ("visual-conversation", "会話と状態変更", vec![
                ("user", json!({"role":"user","content":[{"type":"input_text","text":"状態遷移の調査結果を、日本語で簡潔にまとめてください。"}]})),
                ("event",context("run_started",json!({"run_id":"visual-run-conversation"}))),
                ("event",context("turn_started",json!({"run_id":"visual-run-conversation","turn_id":"visual-turn-conversation","previous_turn_ended_at":null}))),
                ("event",context("instructions_observed",json!({"before":null,"after":{"path":"/workspace/AGENTS.md","hash":"a1b2c3d4e5f6","present":true}}))),
                ("event",context("thread_settings",json!({"before":{"model":"model-a","effort":"low"},"after":{"model":"model-b","effort":"medium"}}))),
                ("assistant_response", answer("## 調査結果\n\n受付と完了は別の状態です。**受付済み**でも、処理が完了したとは限りません。\n\n- 保存後に実行を開始します。\n- 背景処理が残る間はRunが生存します。\n- 失敗時は原因を確認して明示的に再試行します。\n\n次は[設計メモ](https://example.com/design)を確認できます。")),
            ]),
            ("visual-tools", "コマンドと実行結果", vec![
                ("user",json!({"role":"user","content":"テストを実行し、失敗した箇所を説明してください。"})),
                ("assistant_response",json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"対象のテストを実行します。"}]},{"type":"function_call","name":"shell","call_id":"visual-call-test","arguments":"{\"command\":\"cargo test --locked --test parser\",\"background\":false}"}]})),
                ("tool_result",json!({"item":{"type":"function_call_output","call_id":"visual-call-test","output":json!({"execution_id":"visual-shell-test","status":"exited","exit_code":101,"output_complete":true,"elapsed_ms":1240,"stdout":"running 3 tests\ntest accepts_unicode ... ok\ntest handles_empty_input ... ok\ntest rejects_unknown_state ... FAILED\n","stderr":"assertion failed: unexpected state transition\nexpected: recovery_wait\nactual: running\n"}).to_string()}})),
                ("assistant_response",answer("### 結果\n\n3件中 **2件成功・1件失敗** でした。`rejects_unknown_state` の遷移先が期待値と異なります。\n\n```text\nexpected: recovery_wait\nactual:   running\n```\n\n原文のstderrと状態遷移の条件を確認します。")),
            ]),
            ("visual-prose", "長い本文と段落の密度", vec![
                ("user", json!({"role":"user","content":"長い説明でも段落と字下げを保ち、元の本文へ戻れるように表示してください。"})),
                ("assistant_response", answer(&(1..=24).map(|index| format!("第{index}段落。受付と処理の完了は異なる観測です。保存された依頼と実行結果を区別し、必要なときに原文と識別子を確認します。長い説明は内容を削らず端末幅で折り返し、段落の区切りから続きの位置を追えるようにします。日本語、English、結合文字e\u{301}と絵文字🙂を含みます。")).collect::<Vec<_>>().join("\n\n"))),
            ]),
            ("visual-markdown", "Markdown・表・リンク", vec![
                ("user",json!({"role":"user","content":"# この見出し記号は入力原文です\n* 箇条書きに変換せず表示してください。"})),
                ("assistant_response",answer("# 表示の確認 🙂\n\n**重要な結論** と *補足説明* を区別します。`inline_code()` は本文と違う表示です。\n\n> 引用された日本語の文章です。\n\n1. 最初の項目\n2. 二番目の項目\n\n| 対象 | 状態 | 次の操作 |\n|---|---|---|\n| 日本語の入力 | 保存済み | 回答を待つ |\n| shellの長い出力 | 取得可能 | 原文を開く |\n\n```rust\nfn main() {\n    println!(\"日本語・emoji 🙂\");\n}\n```\n\n[表示名のあるリンク](https://example.com/docs?q=unicode%20text) と裸URL https://example.com/status を確認します。\n\n長いURL: https://example.com/oauth/authorize?client_id=synthetic-visual-fixture&redirect_uri=https%3A%2F%2Fexample.com%2Fcallback&scope=read%20write%20history&state=this-is-a-synthetic-value-for-wrap-and-copy-check-not-a-credential\n")),
            ]),
            ("visual-recovery", "子の復旧待ちと途中報告", vec![
                ("user",json!({"role":"user","content":"子に調査を依頼しています。状態と対応方法を教えてください。"})),
                ("event",event("child_turn_completed",json!({"thread_id":"visual-child","run_id":"visual-child-run","turn_id":"visual-child-turn","state":"completed","answer_node_id":"visual-child-answer"}))),
                ("event",event("child_recovery_wait",json!({"thread_id":"visual-child","run_id":"visual-child-run","turn_id":"visual-child-turn","state":"recovery_wait","error":{"code":"io_failed","message":"資料ファイルを読み込めません","hint":"参照先とアクセス権を確認してください"}}))),
                ("event",event("shell_ended",json!({"execution_id":"visual-shell-report","state":"failed","exit_code":1,"output_complete":false}))),
                ("assistant_response",answer("子の調査が**復旧待ち**になりました。参照先を確認してから、直属の親または人間が再試行を判断します。\n\n未完了の出力を成功結果として扱わないでください。")),
            ]),
            ("visual-table", "狭い幅での表", vec![
                ("user",json!({"role":"user","content":"狭い画面でも各列の意味と値を読めるようにしてください。"})),
                ("assistant_response",answer("## 確認結果\n\n| 調査対象 | 現在の状態 | 次に必要な操作 |\n|---|---|---|\n| 日本語と絵文字 🙂 | 保存済み | 回答を待つ |\n| shellの長い出力 | 原文を取得可能 | 出力の続きとエラー理由を確認する |\n\n列の情報を保持して表示しています。")),
            ]),
            ("visual-summary", "要約と原文参照", vec![
                ("user",json!({"role":"user","content":"表示契約の確認結果を表で引き継いでください。"})),
                ("assistant_response", json!({"output":[
                    {"type":"reasoning","summary":[{"type":"summary_text","text":"これは推論の原文です。回答や要約には混ぜません。"}]},
                    {"type":"message","role":"assistant","content":[{"type":"output_text","text":"> 引用と **強調**。\n\n| 対象 | 状態 |\n|---|---|\n| 元の回答 | 確認済み🙂 |\n\n[設計メモ](https://example.com/design?q=unicode%20text)"}]}
                ]})),
                ("summary",json!({"item":{"role":"developer","content":[{"type":"input_text","text":"### Part 1\n> 引用と **強調**。\n\n| 対象 | 状態 |\n|---|---|\n| 引継ぎ | 原文を保持🙂 |\n\n[設計メモ](https://example.com/design?q=unicode%20text)\n\nShellboy source nodes: visual-summary-node-0, visual-summary-node-1"}]},"source_node_ids":["visual-summary-node-0","visual-summary-node-1"],"summary_part_keys":["visual-part"],"summary_format":"shellboy-summary-v1"})),
            ]),
            ("visual-unknown", "未知の項目と長い原文", vec![
                ("future_visual_node",json!({"version":2,"status":"pending","details":{"reason":"未知の項目も消さずに参照できます","items":["日本語","English",123]}})),
                ("tool_result",json!({"item":{"type":"function_call_output","call_id":"visual-large-call","output":json!({"status":"exited","exit_code":0,"output_complete":true,"stdout":"日本語の長い原文を保持し、表示範囲だけを読み込みます。\n".repeat(1500)}).to_string()}})),
                ("assistant_response",answer("長い原文と未対応種別を、通常の回答から区別できることを確認します。")),
            ]),
            ("visual-composer", "入力補助・保存済みの依頼", vec![
                ("user", json!({"role":"user","content":"H01 はじめの依頼"})),
                ("assistant_response", answer("最初の合成応答です。")),
                ("user", json!({"role":"user","content":"/help"})),
                ("assistant_response", answer("slash本文の合成応答です。")),
                ("user", json!({"role":"user","content":"H03 最新の依頼🙂\n次の行も復元"})),
                ("assistant_response", answer("最後の合成応答です。")),
            ]),
            ("visual-scroll-lines", "スクロール・番号付き160行", vec![
                ("user", json!({"role":"user","content":"番号付きの160行をスクロールして確認します。"})),
                ("assistant_response", answer(&format!("```text\n{}\n```", (1..=160).map(|i| format!("R{i:03}")).collect::<Vec<_>>().join("\n")))),
            ]),
            ("visual-scroll-messages", "スクロール・短い120メッセージ",
                (1..=120).map(|i| ("assistant_response",answer(&format!("メッセージ M{i:03}")))).collect()),
            ("visual-scroll-wrap", "スクロール・日本語の折り返し", vec![
                ("assistant_response", answer(&(1..=40).map(|i| format!("P{i:03} 日本語の折り返し位置を確認します。幅が変わっても、ホイールは文字数ではなく実際に表示される行で動きます。絵文字🙂と結合文字e\u{301}も含めます。")).collect::<Vec<_>>().join("\n\n"))),
            ]),
            ("visual-shell-metadata", "Shell metadata palette fixture", vec![
                ("tool_result", json!({"item":{"type":"function_call_output","call_id":"metadata-success","output":json!({"command":"METADATA_SUCCESS","status":"exited","exit_code":0,"elapsed_ms":12400}).to_string()}})),
                ("tool_result", json!({"item":{"type":"function_call_output","call_id":"metadata-nonzero","output":json!({"command":"METADATA_NONZERO","status":"exited","exit_code":7,"elapsed_ms":12400}).to_string()}})),
                ("tool_result", json!({"item":{"type":"function_call_output","call_id":"metadata-failed","output":json!({"command":"METADATA_FAILED","status":"failed","elapsed_ms":12400}).to_string()}})),
                ("tool_result", json!({"item":{"type":"function_call_output","call_id":"metadata-cancelled","output":json!({"command":"METADATA_CANCELLED","status":"cancelled","elapsed_ms":12400}).to_string()}})),
            ]),
            ("visual-row-kinds", "Row vocabulary synthetic fixture", vec![
                ("user", json!({"role":"user","content":"ROW_FIXTURE_USER"})),
                ("summary", json!({"source_node_ids":["visual-row-kinds-node-0"],"item":{"role":"developer","content":"ROW_FIXTURE_SUMMARY"}})),
                ("event", event("child_turn_completed", json!({"thread_id":"visual-tree-child-39","title":"Fixture child","answer_node_id":"visual-tree-child-39-answer"}))),
                ("tool_result", json!({"item":{"type":"function_call_output","call_id":"unknown","output":json!({"command":"UNKNOWN_OBSERVATION","status":"observation_unavailable"}).to_string()}})),
            ]),
            ("visual-scroll-body", "スクロール・本文取得window境界", vec![
                ("assistant_response", answer(&format!("```text\n{}\n```", (1..=2000).map(|i| format!("B{i:05} bounded source window preserves the visible rows while loading more. ")).collect::<Vec<_>>().join("\n")))),
            ]),
        ];
        for (thread,title,nodes) in cases {
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,?2,?3,?4,'visual','offline-fixture','2026-10-24T12:00:00+09:00')",rusqlite::params![thread,title,cwd,agents])?;
            for (index,(kind,payload)) in nodes.into_iter().enumerate() {
                append_node(&tx,thread,&format!("{thread}-node-{index}"),None,kind,&payload,"2026-10-24T12:00:00+09:00")?;
            }
        }
        // Keep real source relations for the summary original/provenance view.
        // Both source nodes stay visible in this comparison scene on purpose.
        tx.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at) VALUES('visual-summary-replacement','visual-summary-node-2','visual-input','shellboy-summary-v1','visual-prompt','completed','2026-10-24T12:00:00+09:00')",[])?;
        for ordinal in 0..2 {
            tx.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision,source_hash) SELECT 'visual-summary-replacement',?1,id,payload_revision,body_hash FROM node WHERE id=?2",rusqlite::params![ordinal,format!("visual-summary-node-{ordinal}")])?;
        }
        tx.execute("INSERT INTO thread_replacement(thread_id,replacement_id) VALUES('visual-summary','visual-summary-replacement')",[])?;
        // Dedicated browser scenes: enough siblings for paging, nested children,
        // and a parent that intentionally arrives on the second page.
        tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('visual-tree-root','Shellboyの表示改善',?1,?2,'visual','offline-fixture','2026-10-25T12:00:00+09:00')",rusqlite::params![cwd,agents])?;
        for i in 0..40 {
            let id=format!("visual-tree-child-{i:02}");
            let title=match i {39=>"スクロールの検証 🙂".into(),38=>"親子表示とUnicodeの列幅を確認する長いタイトル".into(),_=>format!("調査タスク {i:02}")};
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,parent_thread_id,updated_at) VALUES(?1,?2,?3,?4,'visual','offline-fixture','visual-tree-root','2026-10-25T11:00:00+09:00')",rusqlite::params![id,title,format!("{cwd}/projects/component-{i:02}"),agents])?;
            append_node(&tx,&id,&format!("{id}-answer"),None,"assistant_response",&answer("# 確認結果\n\n**構造化データ**を保ち、一覧と詳細を分離します。\n\n```rust\nfn synthetic() {}\n```"),"2026-10-25T11:00:00+09:00")?;
        }
        tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,parent_thread_id,updated_at) VALUES('visual-tree-grandchild','折り返しと結合文字 é',?1,?2,'visual','offline-fixture','visual-tree-child-39','2026-10-25T11:30:00+09:00')",rusqlite::params![cwd,agents])?;
        tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES('visual-parent-later','別プロジェクトの親',?1,?2,'visual','offline-fixture','2026-10-20T12:00:00+09:00')",rusqlite::params![cwd,agents])?;
        tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,parent_thread_id,updated_at) VALUES('visual-child-first','親が次ページにある子',?1,?2,'visual','offline-fixture','visual-parent-later','2026-10-25T12:30:00+09:00')",rusqlite::params![cwd,agents])?;
        if untitled {
            tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('visual-untitled-root',?1,?2,'visual','offline-fixture','2026-10-25T12:59:00+09:00')",rusqlite::params![cwd,agents])?;
            append_node(&tx,"visual-untitled-root","visual-untitled-root-user",None,"user",&json!({"role":"user","content":[{"type":"input_text","text":"保存タイトルがない会話を、最初の依頼から表示してください。\n改行も1行に整えます。"}]}),"2026-10-25T12:59:00+09:00")?;
            append_node(&tx,"visual-untitled-root","visual-untitled-root-answer",None,"assistant_response",&answer("最後の回答をタイトルと取り違えないための合成データです。"),"2026-10-25T12:59:00+09:00")?;
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,parent_thread_id,updated_at) VALUES('visual-untitled-child','   ',?1,?2,'visual','offline-fixture','visual-untitled-root','2026-10-25T12:58:00+09:00')",rusqlite::params![cwd,agents])?;
            append_node(&tx,"visual-untitled-child","visual-untitled-child-user",None,"user",&json!({"role":"user","content":"子のスレッドも、保存titleが空なら依頼を表示してください。"}),"2026-10-25T12:58:00+09:00")?;
            tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('visual-untitled-empty',?1,?2,'visual','offline-fixture','2026-10-25T12:57:00+09:00')",rusqlite::params![cwd,agents])?;
        }
        tx.execute("INSERT INTO view_state(thread_id,anchor_node_id,anchor_offset,follow_tail) VALUES('visual-scroll-body','visual-scroll-body-node-0',65000,0)",[])?;
        tx.commit()?;
        Ok(())
    }).await?;
    database.shutdown().await?;
    println!("Offline visual fixture created: {}", root.display());
    println!(
        "shellboy --config {:?} --data-dir {:?}",
        root.join("config.toml"),
        data
    );
    println!("Open /threads and select a visual fixture. It contains synthetic history only.");
    Ok(())
}
