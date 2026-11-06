//! Typed standalone forms. No terminal stdin, subprocess, file or DB access.
use super::super::i18n::local;
use super::*;
use crate::{
    cli::ThreadCreateOptions,
    config_setup::ConfigCommand,
    docs::DocsCommand,
    exec_client::ExecOptions,
    rag::{IndexMode, IngestKind, RagCommand, RagOptions, SearchMode, SourceCommand},
};

type ClientBuilder = fn(&Form, &Path) -> Result<ClientOperation, String>;

pub(super) fn new(operation: OpKind, c: &Context) -> Form {
    use Kind::*;
    let f = |name, label, value: &str, kind| Field::new(name, label, value, kind);
    let db = || f("db", "独立RAG DB（空=起動時data-dir/rag.db）", "", Text);
    let limit = |value| f("limit", "取得件数", value, Number);
    let cursor = || f("cursor", "続きcursor（空=先頭）", "", Text);
    let docparent = || f("doc_parent", "ガイド階層ID（空=起点）", "", Text);
    let (fields, build): (Vec<Field>, ClientBuilder) = match operation {
        OpKind::ConfigInit => (Vec::new(), |_, _| {
            Ok(ClientOperation::Config(ConfigCommand::Init))
        }),
        OpKind::ConfigCheck => (Vec::new(), |_, _| {
            Ok(ClientOperation::Config(ConfigCommand::Check))
        }),
        OpKind::DocsList => (vec![docparent(), limit("50"), cursor()], |form, _| {
            let limit = form.number("limit")?;
            bounded(limit, 1, 200, "件数", "Count", form.language)?;
            Ok(ClientOperation::Docs(DocsCommand::List {
                parent: form.optional("doc_parent")?,
                limit,
                cursor: form.optional("cursor")?,
            }))
        }),
        OpKind::DocsGet => (
            vec![
                f("document", "文書ID", "", Text),
                f("section", "見出し（任意）", "", Text),
                f("limit", "取得byte数", "16384", Number),
                cursor(),
            ],
            |form, _| {
                let limit_bytes = form.number("limit")?;
                bounded(
                    limit_bytes,
                    1,
                    65536,
                    "取得byte数",
                    "Read size in bytes",
                    form.language,
                )?;
                Ok(ClientOperation::Docs(DocsCommand::Get {
                    id: required(form, "document")?,
                    section: form.optional("section")?,
                    limit_bytes,
                    cursor: form.optional("cursor")?,
                }))
            },
        ),
        OpKind::DocsSearch => (
            vec![
                f("query", "検索文字列", "", Text),
                docparent(),
                limit("10"),
                cursor(),
            ],
            |form, _| {
                let limit = form.number("limit")?;
                bounded(limit, 1, 100, "件数", "Count", form.language)?;
                Ok(ClientOperation::Docs(DocsCommand::Search {
                    query: required(form, "query")?,
                    parent: form.optional("doc_parent")?,
                    limit,
                    cursor: form.optional("cursor")?,
                }))
            },
        ),
        OpKind::Exec => (
            vec![
                f("schema", "JSON Schemaファイル（64KiBまで）", "", Text),
                f(
                    "thread",
                    "既存Thread（空=新規作成）",
                    c.thread.as_str(),
                    Text,
                ),
                f(
                    "agents",
                    "新規Threadの指示ファイル（既存Thread時は空）",
                    "",
                    Text,
                ),
                f("cwd", "新規Threadのcwd（空=起動場所）", "", Text),
                f("connection", "新規Threadの接続（任意）", "", Text),
                f("model", "新規Threadのモデル（任意）", "", Text),
                f("effort", "新規Threadの推論設定（none=解除）", "", Text),
                f("parent", "新規Threadの親（任意）", "", Text),
                f(
                    "text",
                    "依頼本文（CLI stdin相当もここへ入力）",
                    "",
                    Multiline,
                ),
                f("exec_result", "結果モード", "reply", Text),
                f(
                    "max_repairs",
                    "修復メッセージの最大回数（0〜5）",
                    "2",
                    Number,
                ),
                f(
                    "timeout",
                    "全体期限ms（対象を停止しません）",
                    "300000",
                    Number,
                ),
                f("sender_label", "送信者の表示名", "shellboy exec", Text),
            ],
            |form, _| {
                let thread_id = form.optional("thread")?;
                let create = ThreadCreateOptions {
                    agents: raw_path(form, "agents")?,
                    cwd: raw_path(form, "cwd")?,
                    connection: form.optional("connection")?,
                    model: form.optional("model")?,
                    effort: form.effort()?,
                    parent_id: form.optional("parent")?,
                };
                if thread_id.is_some()
                    && (create.agents.is_some()
                        || create.cwd.is_some()
                        || create.connection.is_some()
                        || create.model.is_some()
                        || create.effort.is_some()
                        || create.parent_id.is_some())
                {
                    return Err(local!(form.language, "既存Threadと新規Thread設定は併用できません。対象又は新規設定を消去してください", "An existing Thread cannot be combined with new Thread settings. Clear the target or the new settings.").into());
                }
                let text = required(form, "text")?;
                if text.trim().is_empty() || text.len() > 128 * 1024 {
                    return Err(local!(form.language, "依頼本文は空白以外を含む128KiB以内です。Schema込みの上限は実行前にも検証します", "The request must contain non-whitespace text and fit within 128 KiB. The combined schema limit is checked again before execution.").into());
                }
                let max_repairs = form.number("max_repairs")?;
                if max_repairs > 5 {
                    return Err(local!(
                        form.language,
                        "修復回数は0〜5です",
                        "The repair count must be 0 through 5."
                    )
                    .into());
                }
                let timeout_ms = form.number("timeout")?;
                if !(1..=86_400_000).contains(&timeout_ms) {
                    return Err(local!(
                        form.language,
                        "全体期限は1〜86400000msです",
                        "The total deadline must be 1 through 86400000 ms."
                    )
                    .into());
                }
                Ok(ClientOperation::Exec(ExecOptions {
                    result: match required(form, "exec_result")?.as_str() {
                        "reply" => crate::exec_client::ResultMode::Reply,
                        "turn-end" => crate::exec_client::ResultMode::TurnEnd,
                        "run-end" => crate::exec_client::ResultMode::RunEnd,
                        _ => {
                            return Err(local!(
                                form.language,
                                "結果モードはreply、turn-end、run-endです",
                                "Result mode must be reply, turn-end, or run-end."
                            )
                            .into());
                        }
                    },
                    schema: PathBuf::from(required(form, "schema")?),
                    thread_id,
                    create,
                    text: Some(text),
                    text_stdin: false,
                    max_repairs,
                    timeout_ms,
                    sender_label: required(form, "sender_label")?,
                }))
            },
        ),
        OpKind::RagInit => (vec![db()], |form, _| rag(form, RagCommand::Init, None)),
        OpKind::RagSourceAdd => (
            vec![
                db(),
                f("path", "資料フォルダ／ファイル", "", Text),
                f("source_name", "登録名", "", Text),
                f("excludes", "除外glob（1行1パターン）", "", Multiline),
                f("clear_excludes", "既存の除外を解除", "false", Boolean),
            ],
            |form, _| {
                let exclude = lines(form, "excludes")?;
                let clear_excludes = form.boolean("clear_excludes")?;
                if clear_excludes && !exclude.is_empty() {
                    return Err(local!(
                        form.language,
                        "除外globの指定と既存除外の解除は併用できません",
                        "Exclusion globs and clearing existing exclusions cannot be combined."
                    )
                    .into());
                }
                rag(
                    form,
                    RagCommand::Source {
                        command: SourceCommand::Add {
                            path: PathBuf::from(required(form, "path")?),
                            name: required(form, "source_name")?,
                            exclude,
                            clear_excludes,
                        },
                    },
                    None,
                )
            },
        ),
        OpKind::RagSourceList => (vec![db()], |form, _| {
            rag(
                form,
                RagCommand::Source {
                    command: SourceCommand::List,
                },
                None,
            )
        }),
        OpKind::RagSourceRemove => (
            vec![db(), f("source", "解除するソースID／登録名", "", Text)],
            |form, _| {
                rag(
                    form,
                    RagCommand::Source {
                        command: SourceCommand::Remove {
                            source: required(form, "source")?,
                        },
                    },
                    None,
                )
            },
        ),
        OpKind::RagIndex => (
            vec![
                db(),
                f(
                    "source",
                    "ソースID／名（空=登録資料、docs=同梱ガイド）",
                    "",
                    Text,
                ),
                f(
                    "dry_run",
                    "対象確認だけ（索引を変更しない）",
                    "false",
                    Boolean,
                ),
                f(
                    "search_mode",
                    "索引方式",
                    "keyword",
                    Choice(&["keyword", "semantic"]),
                ),
            ],
            |form, _| {
                let mode = match form.value("search_mode")? {
                    "keyword" => IndexMode::Keyword,
                    "semantic" => IndexMode::Semantic,
                    _ => {
                        return Err(local!(
                            form.language,
                            "索引方式を選択してください",
                            "Select an indexing mode."
                        )
                        .into());
                    }
                };
                rag(
                    form,
                    RagCommand::Index {
                        source: form.optional("source")?,
                        dry_run: form.boolean("dry_run")?,
                        mode,
                    },
                    None,
                )
            },
        ),
        OpKind::RagIngest => (
            vec![
                db(),
                f("document", "登録する文書ID", "", Text),
                f("source_file", "元ファイル", "", Text),
                f(
                    "ingest_kind",
                    "本文の種類",
                    "extracted",
                    Choice(&["extracted", "llm-explanation", "summary"]),
                ),
                f(
                    "input_mode",
                    "抽出済み本文の入力元",
                    "file",
                    Choice(&["file", "inline"]),
                ),
                f(
                    "text_file",
                    "抽出済み本文ファイル（本文入力時は空）",
                    "",
                    Text,
                ),
                f(
                    "text",
                    "抽出済み本文（CLI stdin相当・ファイル時は空）",
                    "",
                    Multiline,
                ),
                f(
                    "metadata_file",
                    "JSON metadataファイル（64KiBまで、任意）",
                    "",
                    Text,
                ),
            ],
            |form, _| {
                let kind = match form.value("ingest_kind")? {
                    "extracted" => IngestKind::Extracted,
                    "llm-explanation" => IngestKind::LlmExplanation,
                    "summary" => IngestKind::Summary,
                    _ => {
                        return Err(local!(
                            form.language,
                            "本文の種類を選択してください",
                            "Select a text kind."
                        )
                        .into());
                    }
                };
                let (text_file, text_stdin, owned) = match form.value("input_mode")? {
                    "file" => {
                        if !form.value("text")?.is_empty() {
                            return Err(local!(
                                form.language,
                                "本文ファイルを使うときは本文入力を消去してください",
                                "Clear the inline text when using a text file."
                            )
                            .into());
                        }
                        (
                            Some(PathBuf::from(required(form, "text_file")?)),
                            false,
                            None,
                        )
                    }
                    "inline" => {
                        if form.optional("text_file")?.is_some() {
                            return Err(local!(
                                form.language,
                                "本文を入力するときは本文ファイル欄を消去してください",
                                "Clear the text file field when entering inline text."
                            )
                            .into());
                        }
                        (None, true, Some(required(form, "text")?))
                    }
                    _ => {
                        return Err(local!(
                            form.language,
                            "本文の入力元を選択してください",
                            "Select a text input source."
                        )
                        .into());
                    }
                };
                rag(
                    form,
                    RagCommand::Ingest {
                        id: required(form, "document")?,
                        source_file: PathBuf::from(required(form, "source_file")?),
                        kind,
                        text_file,
                        text_stdin,
                        metadata_file: raw_path(form, "metadata_file")?,
                    },
                    owned,
                )
            },
        ),
        OpKind::RagStatus => (vec![db()], |form, _| rag(form, RagCommand::Status, None)),
        OpKind::RagInspect => (
            vec![db(), f("document", "文書ID", "", Text)],
            |form, _| {
                rag(
                    form,
                    RagCommand::Inspect {
                        id: required(form, "document")?,
                    },
                    None,
                )
            },
        ),
        OpKind::RagSearch => (
            vec![
                db(),
                f("query", "検索文字列", "", Text),
                f(
                    "rag_scopes",
                    "検索範囲（空=ガイドと登録資料、1行1指定）",
                    "",
                    Multiline,
                ),
                f(
                    "search_mode",
                    "検索方式",
                    "keyword",
                    Choice(&["keyword", "semantic", "hybrid"]),
                ),
                limit("10"),
                cursor(),
            ],
            |form, _| {
                let mode = match form.value("search_mode")? {
                    "keyword" => SearchMode::Keyword,
                    "semantic" => SearchMode::Semantic,
                    "hybrid" => SearchMode::Hybrid,
                    _ => {
                        return Err(local!(
                            form.language,
                            "検索方式を選択してください",
                            "Select a search mode."
                        )
                        .into());
                    }
                };
                let limit = form.number("limit")?;
                if !(1..=100).contains(&limit) {
                    return Err(local!(
                        form.language,
                        "検索件数は1〜100です",
                        "The search limit must be 1 through 100."
                    )
                    .into());
                }
                rag(
                    form,
                    RagCommand::Search {
                        query: required(form, "query")?,
                        scope: lines(form, "rag_scopes")?,
                        mode,
                        limit,
                        cursor: form.optional("cursor")?,
                    },
                    None,
                )
            },
        ),
        OpKind::RagGet => (
            vec![
                db(),
                f("reference", "検索結果のref", "", Text),
                f("limit", "取得byte数", "16384", Number),
                cursor(),
            ],
            |form, _| {
                let limit_bytes = form.number("limit")?;
                if !(1..=65536).contains(&limit_bytes) {
                    return Err(local!(
                        form.language,
                        "取得byte数は1〜65536です",
                        "The read size must be 1 through 65536 bytes."
                    )
                    .into());
                }
                rag(
                    form,
                    RagCommand::Get {
                        reference: required(form, "reference")?,
                        limit_bytes,
                        cursor: form.optional("cursor")?,
                    },
                    None,
                )
            },
        ),
        // This branch builds the actual owner form; it never returns a client
        // stub or attempts a terminal/subprocess fallback.
        _ => return Form::new(operation, c),
    };
    Form {
        language: UiLanguage::Ja,
        operation,
        fields,
        build: FormBuilder::Client(build),
        selected: 0,
        conditions: Vec::new(),
        error: None,
        follows_current: false,
        help: false,
        help_scroll: std::cell::Cell::new(0),
        help_page: std::cell::Cell::new(1),
        client_confirmation: None,
        reader: std::cell::RefCell::new(popup::ReaderCache::default()),
        help_revision: 0,
    }
}
fn required(form: &Form, name: &str) -> Result<String, String> {
    let value = form.value(name)?;
    if value.is_empty() {
        Err(match form.language {
            UiLanguage::Ja => format!("{name}: 入力が必要です。F1で条件を確認してください"),
            UiLanguage::En => format!("{name}: A value is required. Press F1 for requirements."),
        })
    } else {
        Ok(value.into())
    }
}
fn raw_path(form: &Form, name: &str) -> Result<Option<PathBuf>, String> {
    Ok(form.optional(name)?.map(PathBuf::from))
}
fn lines(form: &Form, name: &str) -> Result<Vec<String>, String> {
    Ok(form
        .value(name)?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}
fn bounded(
    value: usize,
    min: usize,
    max: usize,
    ja_label: &str,
    en_label: &str,
    language: UiLanguage,
) -> Result<(), String> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(match language {
            UiLanguage::Ja => format!("{ja_label}は{min}〜{max}です"),
            UiLanguage::En => format!("{en_label} must be {min} through {max}"),
        })
    }
}
fn rag(
    form: &Form,
    command: RagCommand,
    owned_stdin: Option<String>,
) -> Result<ClientOperation, String> {
    Ok(ClientOperation::Rag {
        options: RagOptions {
            db: raw_path(form, "db")?,
            command,
        },
        owned_stdin,
    })
}

fn safe(value: &str) -> String {
    crate::presentation::sanitize_plain(&value.chars().take(256).collect::<String>(), 0).0
}
pub(super) fn confirmation_lines_for(
    operation: &ClientOperation,
    language: UiLanguage,
) -> Vec<Line<'static>> {
    let id = operation.catalogue();
    let mut lines = vec![
        Line::from(super::metadata::display_label(id, language)),
        Line::from(super::metadata::text_for(id.meaning_text(), language)),
        Line::from(super::metadata::text_for(
            operation.effect().text(),
            language,
        )),
        Line::from(""),
    ];
    match operation {
        ClientOperation::Config(_) => lines.extend([
            Line::from(local!(language,
                "起動時に選択した設定ファイルへ保存します。既存ファイルは上書きしません。",
                "Write to the configuration file selected at launch. Existing files are not overwritten.")),
            Line::from(local!(language,
                "現在の所有者の設定は変えません。編集した設定の反映は再起動時です。",
                "The running owner's settings do not change. Edited configuration takes effect on restart.")),
        ]),
        ClientOperation::Exec(options) => {
            let thread = match (language, options.thread_id.as_deref()) {
                (UiLanguage::Ja, Some(id)) => format!("既存Thread: {}", safe(id)),
                (UiLanguage::En, Some(id)) => format!("Existing Thread: {}", safe(id)),
                (_, None) => local!(language, "既存Thread: なし（新規作成）", "Existing Thread: none; create a new Thread").into(),
            };
            let repairs = options.max_repairs;
            let timeout = options.timeout_ms;
            let sep = crate::ui::text_width::glyphs::separator();
            let limits = match language {
                UiLanguage::Ja => format!("追加修復: 最大{repairs}回{sep}全体期限{timeout}ms"),
                UiLanguage::En => format!("Additional repairs: at most {repairs}; total deadline: {timeout} ms"),
            };
            lines.extend([
                Line::from(thread),
                Line::from(limits),
                Line::from(local!(language,
                    "モデルAPIとtoolの費用・副作用が発生する場合があります。修復は完了済み作用を巻き戻しません。",
                    "Model API calls and tools may incur costs or side effects. Repairs do not undo completed effects.")),
                Line::from(local!(language,
                    "期限や表示を閉じる操作だけでは対象Runを停止しません。受付IDは結果に個別に示します。",
                    "The deadline or closing this screen does not stop the target Run. Its receipt ID is shown separately in the result.")),
            ]);
        }
        ClientOperation::Rag { options, .. } => {
            let database = match (language, &options.db) {
                (UiLanguage::Ja, Some(path)) => format!("独立RAG DB: {}", safe(&path.to_string_lossy())),
                (UiLanguage::En, Some(path)) => format!("Independent RAG DB: {}", safe(&path.to_string_lossy())),
                (_, None) => local!(language, "独立RAG DB: 起動時data-dir/rag.db", "Independent RAG DB: launch data-dir/rag.db").into(),
            };
            lines.push(Line::from(database));
            if matches!(options.command, crate::rag::RagCommand::Index { mode: crate::rag::IndexMode::Semantic, .. }) {
                lines.push(Line::from(local!(language,
                    "semantic索引は埋め込みAPIを使用し、費用が発生する場合があります。",
                    "Semantic indexing uses the embedding API and may incur costs.")));
            }
            lines.push(Line::from(local!(language,
                "資料ファイル自体は削除しません。登録・索引の変更は独立RAG DBへ保存します。",
                "Source documents are not deleted. Registration and index changes are saved in the independent RAG database.")));
        }
        ClientOperation::Docs(_) => {}
    }
    lines.extend([
        Line::from(""),
        Line::from(local!(language,
            "相対パスはTUI起動場所を基準とします。設定・data-dirは起動時の選択を使います。",
            "Relative paths use the TUI launch directory. Configuration and data-dir are the selections made at launch.")),
        Line::from(local!(language,
            "Yで要求します。N/Escで入力内容を保ったまま戻ります。受付済みの処理は画面を閉じても取り消しません。",
            "Y submits the request. N/Esc returns with inputs intact. Closing the screen does not cancel accepted work.")),
    ]);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context() -> Context {
        Context {
            thread: "current".into(),
            cwd: PathBuf::from("/thread-cwd"),
            ..Default::default()
        }
    }
    fn set(form: &mut Form, name: &str, value: &str) {
        form.fields
            .iter_mut()
            .find(|field| field.name == name)
            .unwrap()
            .value = value.into();
    }
    fn fixture(kind: OpKind) -> Form {
        let mut form = Form::new(kind, &context());
        for (name, value) in [
            ("schema", "schema.json"),
            ("text", "hello"),
            ("document", "document-id"),
            ("query", "needle"),
            ("path", "relative/materials"),
            ("source_name", "source name"),
            ("source", "source-id"),
            ("source_file", "source.pdf"),
            ("text_file", "extracted.txt"),
            ("reference", "saved-ref"),
        ] {
            if form.fields.iter().any(|field| field.name == name)
                && !(kind == OpKind::RagIngest && name == "text")
            {
                set(&mut form, name, value);
            }
        }
        form
    }
    fn client(form: &Form) -> ClientOperation {
        match form.invocation(Path::new("/thread-cwd")).unwrap() {
            Outcome::Client(operation) => operation,
            _ => panic!("expected actual typed client operation"),
        }
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn draw(operations: &Operations, width: u16, height: u16) {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| operations.draw(frame, frame.area()))
            .unwrap();
    }
    #[test]
    fn every_catalogue_identity_has_one_real_form_and_all_sixteen_client_builders_match_their_identity()
     {
        let ids = OpKind::ALL
            .iter()
            .map(|kind| kind.catalogue())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 65);
        assert_eq!(ids, OperationId::ALL.iter().copied().collect());
        assert_eq!(
            crate::operations::CLI_BINDINGS.len(),
            66,
            "attach is a CLI shorthand, not another operation"
        );
        let clients = OpKind::ALL
            .iter()
            .copied()
            .filter(|kind| !OpKind::OWNER.contains(kind))
            .collect::<Vec<_>>();
        assert_eq!(clients.len(), 16);
        for kind in clients {
            let mut form = fixture(kind);
            assert_eq!(client(&form).catalogue(), kind.catalogue(), "{kind:?}");
            for index in 0..form.fields.len() {
                form.selected = index;
                assert!(
                    form.selected_metadata().is_some() || form.fields[index].name == "db",
                    "{kind:?}: {}",
                    form.fields[index].name
                );
                assert!(!form.help_lines().is_empty());
            }
        }
    }
    #[test]
    fn every_client_form_field_has_an_english_label_without_changing_values() {
        for kind in OpKind::ALL
            .iter()
            .copied()
            .filter(|kind| !OpKind::OWNER.contains(kind))
        {
            let mut form = fixture(kind);
            let values: Vec<_> = form
                .fields
                .iter()
                .map(|field| field.value.clone())
                .collect();
            form.language = UiLanguage::En;
            for field in &form.fields {
                let label = form.field_label_for(field);
                assert!(label.is_ascii(), "{kind:?} {}: {label}", field.name);
            }
            assert_eq!(
                values,
                form.fields
                    .iter()
                    .map(|field| field.value.clone())
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn bounded_validation_selects_paired_labels_without_changing_bounds() {
        assert_eq!(
            bounded(0, 1, 200, "件数", "Count", UiLanguage::Ja).unwrap_err(),
            "件数は1〜200です"
        );
        assert_eq!(
            bounded(0, 1, 200, "件数", "Count", UiLanguage::En).unwrap_err(),
            "Count must be 1 through 200"
        );
        assert!(bounded(1, 1, 200, "件数", "Count", UiLanguage::En).is_ok());
        assert!(bounded(200, 1, 200, "件数", "Count", UiLanguage::Ja).is_ok());
    }
    #[test]
    fn exec_preserves_owned_text_raw_relative_paths_and_all_new_thread_options() {
        let mut form = fixture(OpKind::Exec);
        set(&mut form, "thread", "");
        for (field, value) in [
            ("agents", "AGENTS.md"),
            ("cwd", "work"),
            ("connection", "proxy"),
            ("model", "model"),
            ("effort", "none"),
            ("parent", "parent"),
            ("max_repairs", "5"),
            ("timeout", "1234"),
            ("sender_label", "operator"),
        ] {
            set(&mut form, field, value);
        }
        let ClientOperation::Exec(options) = client(&form) else {
            panic!()
        };
        assert_eq!(options.schema, PathBuf::from("schema.json"));
        assert_eq!(options.create.cwd, Some(PathBuf::from("work")));
        assert_eq!(options.create.agents, Some(PathBuf::from("AGENTS.md")));
        assert_eq!(options.create.connection.as_deref(), Some("proxy"));
        assert_eq!(options.create.model.as_deref(), Some("model"));
        assert_eq!(options.create.parent_id.as_deref(), Some("parent"));
        assert_eq!(options.create.effort.as_deref(), Some(""));
        assert_eq!(options.text.as_deref(), Some("hello"));
        assert!(!options.text_stdin);
        assert_eq!(options.max_repairs, 5);
        assert_eq!(options.timeout_ms, 1234);
        assert_eq!(options.sender_label, "operator");
        set(&mut form, "thread", "existing");
        assert!(form.invocation(Path::new("/")).is_err());
        set(&mut form, "thread", "");
        set(&mut form, "max_repairs", "6");
        assert!(form.invocation(Path::new("/")).is_err());
    }
    #[test]
    fn rag_file_and_owned_inline_ingest_never_generate_unowned_stdin_or_silently_ignore_conflicts()
    {
        let mut form = fixture(OpKind::RagIngest);
        set(&mut form, "db", "knowledge.db");
        let ClientOperation::Rag {
            options,
            owned_stdin,
        } = client(&form)
        else {
            panic!()
        };
        assert_eq!(options.db, Some(PathBuf::from("knowledge.db")));
        assert!(owned_stdin.is_none());
        assert!(
            matches!(options.command,RagCommand::Ingest{text_stdin:false,text_file:Some(path),..}if path.as_path()==Path::new("extracted.txt"))
        );
        set(&mut form, "input_mode", "inline");
        set(&mut form, "text", "derived\nbody");
        assert!(form.invocation(Path::new("/")).is_err());
        set(&mut form, "text_file", "");
        set(&mut form, "metadata_file", "metadata.json");
        set(&mut form, "ingest_kind", "llm-explanation");
        let ClientOperation::Rag {
            options,
            owned_stdin,
        } = client(&form)
        else {
            panic!()
        };
        assert_eq!(owned_stdin.as_deref(), Some("derived\nbody"));
        assert!(
            matches!(options.command,RagCommand::Ingest{text_stdin:true,text_file:None,kind:IngestKind::LlmExplanation,metadata_file:Some(path),..}if path.as_path()==Path::new("metadata.json"))
        );
        set(&mut form, "input_mode", "file");
        assert!(form.invocation(Path::new("/")).is_err());
    }
    #[test]
    fn rag_modes_exclusions_repeated_scopes_and_dry_run_effect_match_actual_arguments() {
        let mut add = fixture(OpKind::RagSourceAdd);
        set(&mut add, "excludes", "a folder/**\nprivate/**");
        let ClientOperation::Rag { options, .. } = client(&add) else {
            panic!()
        };
        assert!(
            matches!(options.command,RagCommand::Source{command:SourceCommand::Add{exclude,..}}if exclude==["a folder/**","private/**"])
        );
        set(&mut add, "clear_excludes", "true");
        assert!(add.invocation(Path::new("/")).is_err());
        let mut search = fixture(OpKind::RagSearch);
        set(
            &mut search,
            "rag_scopes",
            "docs\nhistory:thread\nsource:folder name",
        );
        set(&mut search, "search_mode", "hybrid");
        set(&mut search, "limit", "17");
        let ClientOperation::Rag { options, .. } = client(&search) else {
            panic!()
        };
        assert!(
            matches!(options.command,RagCommand::Search{scope,mode:SearchMode::Hybrid,limit:17, cursor: None,..}if scope==["docs","history:thread","source:folder name"])
        );
        let mut continuation = fixture(OpKind::RagSearch);
        set(&mut continuation, "query", "previous query");
        set(&mut continuation, "rag_scopes", "history:thread");
        set(&mut continuation, "search_mode", "keyword");
        set(&mut continuation, "cursor", "cursor-token");
        let ClientOperation::Rag { options, .. } = client(&continuation) else {
            panic!()
        };
        assert!(
            matches!(options.command, RagCommand::Search { cursor: Some(cursor), .. } if cursor == "cursor-token")
        );
        let mut index = fixture(OpKind::RagIndex);
        set(&mut index, "dry_run", "true");
        set(&mut index, "search_mode", "semantic");
        assert_eq!(index.effect(), Effect::Inspect);
        assert_eq!(client(&index).effect(), Effect::Inspect);
        assert_eq!(index.verb(), "表示");
    }
    #[test]
    fn mutations_require_explicit_confirmation_read_only_and_zero_field_forms_do_not_fake_inputs() {
        let mut operations = Operations::new(context());
        operations.select(OpKind::ConfigInit);
        assert!(operations.form.as_ref().unwrap().fields.is_empty());
        operations.key(key(KeyCode::Up));
        operations.paste("ignored");
        assert!(matches!(operations.key(key(KeyCode::Enter)), Outcome::None));
        assert!(
            operations
                .form
                .as_ref()
                .unwrap()
                .client_confirmation
                .is_some()
        );
        assert!(matches!(operations.key(key(KeyCode::Enter)), Outcome::None));
        assert!(matches!(operations.key(key(KeyCode::Esc)), Outcome::None));
        assert!(operations.form.is_some());
        assert!(
            operations
                .form
                .as_ref()
                .unwrap()
                .client_confirmation
                .is_none()
        );
        operations.key(key(KeyCode::Enter));
        assert!(matches!(
            operations.key(key(KeyCode::Char('y'))),
            Outcome::Client(ClientOperation::Config(ConfigCommand::Init))
        ));
        operations.select(OpKind::ConfigCheck);
        assert!(matches!(
            operations.key(key(KeyCode::Enter)),
            Outcome::Client(ClientOperation::Config(ConfigCommand::Check))
        ));
        let mut exec = fixture(OpKind::Exec);
        assert!(matches!(
            exec.key(key(KeyCode::Enter), Path::new("/")),
            Outcome::None
        ));
        exec.insert("ignored during confirmation");
        assert_eq!(exec.value("text").unwrap(), "hello");
        assert!(matches!(
            exec.key(key(KeyCode::Char('n')), Path::new("/")),
            Outcome::None
        ));
        assert_eq!(exec.value("text").unwrap(), "hello");
    }
    #[test]
    fn form_input_is_bounded_before_dispatch_and_wheel_only_moves_cached_focus_or_reader() {
        let mut ingest = fixture(OpKind::RagIngest);
        set(&mut ingest, "input_mode", "inline");
        set(&mut ingest, "text_file", "");
        set(&mut ingest, "text", &"x".repeat(1024 * 1024));
        assert!(ingest.invocation(Path::new("/")).is_err());
        let mut operations = Operations::new(context());
        draw(&operations, 80, 24);
        assert!(operations.paint_bounds().is_some());
        let initial = operations.selected;
        assert!(!operations.wheel(-100));
        assert_eq!(operations.selected, initial);
        assert!(operations.wheel(2));
        assert!(operations.form.is_none());
        assert!(!operations.wheel(0));
        operations.select(OpKind::Exec);
        let values = operations
            .form
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .map(|field| field.value.clone())
            .collect::<Vec<_>>();
        assert!(operations.wheel(1));
        assert_eq!(
            operations
                .form
                .as_ref()
                .unwrap()
                .fields
                .iter()
                .map(|field| field.value.clone())
                .collect::<Vec<_>>(),
            values
        );
        operations.key(key(KeyCode::F(1)));
        draw(&operations, 60, 14);
        assert!(operations.wheel(isize::MAX));
        let maximum = operations
            .form
            .as_ref()
            .unwrap()
            .reader
            .borrow()
            .bounds()
            .maximum;
        assert_eq!(operations.form.as_ref().unwrap().help_scroll.get(), maximum);
        assert!(!operations.wheel(1));
        assert!(operations.wheel(isize::MIN));
        assert_eq!(operations.form.as_ref().unwrap().help_scroll.get(), 0);
    }
}
