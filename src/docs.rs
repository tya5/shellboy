//! Bundled, owner-independent product documentation.
use crate::digest::sha256_hex as hash;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::Subcommand;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Subcommand)]
pub enum DocsCommand {
    /// List immediate children of a documentation category.
    List {
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Read a bundled guide or one of its sections.
    Get {
        #[arg(long)]
        id: String,
        #[arg(long)]
        section: Option<String>,
        #[arg(long, default_value_t = 16384)]
        limit_bytes: usize,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Search literal text without an embedding service or owner.
    Search {
        #[arg(long)]
        query: String,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        cursor: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Document {
    pub id: String,
    pub parent_id: String,
    pub title: String,
    pub summary: String,
    pub text: String,
}

fn document(id: &str, parent: &str, title: &str, summary: &str, text: &str) -> Document {
    Document {
        id: id.into(),
        parent_id: parent.into(),
        title: title.into(),
        summary: summary.into(),
        text: text.into(),
    }
}

/// All actual guides, with stable logical IDs independent of repository paths.
pub fn documents() -> Vec<Document> {
    vec![
        document(
            "shellboy",
            "",
            "Shellboyガイド",
            "目的からガイドを探す入口と用語",
            include_str!("../docs/guides/README.md"),
        ),
        document(
            "shellboy/user-guide",
            "shellboy/start",
            "User Guide",
            "起動、会話、モデル変更、停止",
            include_str!("../docs/guides/user-guide.md"),
        ),
        document(
            "shellboy/agent-guide",
            "shellboy/start",
            "Agent Guide",
            "モデルの操作手順、文脈とID、受付と完了",
            include_str!("../docs/guides/agent-guide.md"),
        ),
        document(
            "shellboy/docs",
            "shellboy/start",
            "ガイドを見つける",
            "docsの階層、検索、版付き本文取得",
            include_str!("../docs/guides/docs.md"),
        ),
        document(
            "shellboy/delegation",
            "shellboy/work",
            "委譲と子エージェント",
            "子作成、依頼、途中報告、待機、所有",
            include_str!("../docs/guides/delegation.md"),
        ),
        document(
            "shellboy/shell",
            "shellboy/work",
            "shell",
            "foreground/background、execution、stdin、出力",
            include_str!("../docs/guides/shell.md"),
        ),
        document(
            "shellboy/history-and-search",
            "shellboy/work",
            "履歴と検索",
            "原文、fork、検索、索引、要約とspill",
            include_str!("../docs/guides/history-and-search.md"),
        ),
        document(
            "shellboy/attachments",
            "shellboy/work",
            "添付",
            "画像とfile、更新、出典、除外",
            include_str!("../docs/guides/attachments.md"),
        ),
        document(
            "shellboy/recovery-and-storage",
            "shellboy/work",
            "復旧とストレージ",
            "停止、再試行、操作照会、削除、GC",
            include_str!("../docs/guides/recovery-and-storage.md"),
        ),
        document(
            "shellboy/configuration",
            "shellboy/reference",
            "設定",
            "API接続、モデル、保存場所、TOML",
            include_str!("../docs/guides/configuration.md"),
        ),
        document(
            "shellboy/tui",
            "shellboy/reference",
            "画面操作",
            "キー、slash、補完、履歴、表示",
            include_str!("../docs/guides/tui.md"),
        ),
        document(
            "shellboy/rag",
            "shellboy/reference",
            "独立RAG",
            "資料登録、抽出、索引、横断検索、版付き取得",
            include_str!("../docs/guides/rag.md"),
        ),
        document(
            "shellboy/cli-reference",
            "shellboy/reference",
            "CLI reference",
            "コマンド構文、JSON、cursor、エラー",
            include_str!("../docs/guides/cli-reference.md"),
        ),
    ]
}
fn catalogue() -> Vec<Document> {
    let mut docs = documents();
    for (id, title, summary) in [
        ("start", "使い始める", "人間とモデルの入口"),
        ("work", "作業する", "委譲、shell、履歴、添付、復旧"),
        ("reference", "辞書", "設定、画面、CLIの参照"),
    ] {
        docs.push(document(&format!("shellboy/{id}"), "shellboy", title, summary, &format!("# {title}\n\n{summary}\n\n`shellboy docs list --parent shellboy/{id}` でガイドを選びます。\n")));
    }
    docs.sort_by(|a, b| a.id.cmp(&b.id));
    docs
}
fn version(docs: &[Document]) -> String {
    let mut data = String::new();
    for d in docs {
        data.push_str(&d.id);
        data.push('\0');
        data.push_str(&d.parent_id);
        data.push('\0');
        data.push_str(&d.title);
        data.push('\0');
        data.push_str(&d.summary);
        data.push('\0');
        data.push_str(&d.text);
        data.push('\0');
    }
    format!("{}:{}", env!("CARGO_PKG_VERSION"), hash(&data))
}
fn category(id: &str) -> bool {
    matches!(
        id,
        "shellboy/start" | "shellboy/work" | "shellboy/reference"
    )
}
fn summary(d: &Document, version: &str) -> Value {
    json!({"id":d.id,"parent_id":d.parent_id,"kind":if category(&d.id){"category"}else{"guide"},"title":d.title,"summary":d.summary,"version":version,"content_hash":hash(&d.text)})
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    v: u8,
    version: String,
    scope: String,
    offset: usize,
}
fn read_cursor(cursor: Option<&str>, version: &str, scope: &str) -> Result<usize, String> {
    let Some(cursor) = cursor else { return Ok(0) };
    if cursor.len() > 8192 {
        return Err("cursor_expired: Cursor is too large".into());
    }
    let c: Cursor = URL_SAFE_NO_PAD
        .decode(cursor)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or("cursor_expired: Invalid cursor")?;
    if c.v != 1 || c.version != version || c.scope != scope {
        return Err(
            "cursor_expired: Cursor belongs to different documentation or arguments".into(),
        );
    }
    Ok(c.offset)
}
fn next_cursor(version: &str, scope: &str, offset: usize) -> String {
    URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&Cursor {
            v: 1,
            version: version.into(),
            scope: scope.into(),
            offset,
        })
        .expect("cursor"),
    )
}
fn limit(value: usize, max: usize) -> Result<(), String> {
    if value == 0 || value > max {
        Err(format!(
            "invalid_argument: Limit must be between 1 and {max}"
        ))
    } else {
        Ok(())
    }
}
fn find<'a>(docs: &'a [Document], id: &str) -> Result<&'a Document, String> {
    if id.is_empty() || id.len() > 256 {
        return Err("invalid_argument: Document ID must contain 1 to 256 bytes".into());
    }
    docs.iter()
        .find(|d| d.id == id)
        .ok_or_else(|| format!("target_unavailable: Unknown document ID {id}"))
}
fn descendant(d: &Document, parent: &str, docs: &[Document]) -> bool {
    let mut current = d.parent_id.as_str();
    for _ in 0..docs.len() {
        if current == parent {
            return true;
        }
        if current.is_empty() {
            break;
        }
        let Some(p) = docs.iter().find(|d| d.id == current) else {
            break;
        };
        current = &p.parent_id;
    }
    false
}

#[derive(Debug, Serialize)]
struct Section {
    id: String,
    title: String,
    start: usize,
    end: usize,
    level: u8,
}
fn slug(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                Some(c)
            } else if c.is_whitespace() {
                Some('-')
            } else {
                None
            }
        })
        .collect()
}
fn sections(text: &str) -> Vec<Section> {
    let mut result: Vec<Section> = Vec::new();
    let mut active: Option<(String, usize, u8)> = None;
    for (event, range) in Parser::new(text).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                active = Some((String::new(), range.start, level as u8))
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((title, _, _)) = &mut active {
                    title.push_str(&t)
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((title, start, level)) = active.take() {
                    let base = slug(&title);
                    let mut id = base.clone();
                    let mut suffix = 1;
                    while result.iter().any(|s| s.id == id) {
                        id = format!("{base}-{suffix}");
                        suffix += 1;
                    }
                    result.push(Section {
                        id,
                        title,
                        start,
                        end: text.len(),
                        level,
                    });
                }
            }
            _ => {}
        }
    }
    for i in 0..result.len() {
        if let Some(end) = result[i + 1..]
            .iter()
            .find(|s| s.level <= result[i].level)
            .map(|s| s.start)
        {
            result[i].end = end;
        }
    }
    result
}
fn link_target(path: &str) -> Option<&'static str> {
    Some(match path {
        "README.md" => "shellboy",
        "user-guide.md" => "shellboy/user-guide",
        "agent-guide.md" => "shellboy/agent-guide",
        "docs.md" => "shellboy/docs",
        "delegation.md" => "shellboy/delegation",
        "shell.md" => "shellboy/shell",
        "history-and-search.md" => "shellboy/history-and-search",
        "attachments.md" => "shellboy/attachments",
        "recovery-and-storage.md" => "shellboy/recovery-and-storage",
        "configuration.md" => "shellboy/configuration",
        "tui.md" => "shellboy/tui",
        "cli-reference.md" => "shellboy/cli-reference",
        "rag.md" => "shellboy/rag",
        _ => return None,
    })
}
fn links(d: &Document, docs: &[Document]) -> Vec<Value> {
    Parser::new(&d.text).filter_map(|event|{let Event::Start(Tag::Link{dest_url,..})=event else{return None};let target=dest_url.as_ref();let(path,section)=target.split_once('#').map_or((target,None),|(p,s)|(p,Some(s)));
        let id=if path.is_empty(){Some(d.id.as_str())}else{link_target(path.strip_prefix("./").unwrap_or(path))};
        if let Some(id)=id {let resolved=docs.iter().find(|doc|doc.id==id).is_some_and(|doc|section.is_none_or(|s|sections(&doc.text).iter().any(|sec|sec.id==s)));Some(json!({"href":target,"kind":"bundled","target_id":id,"section":section,"resolved":resolved}))}
        else {Some(json!({"href":target,"kind":if target.starts_with("https://")||target.starts_with("http://"){"external"}else{"repository"},"available_offline":false}))}
    }).collect()
}
// A lowercase scalar may expand (İ → i + combining dot); include its original
// scalar whenever any lowered byte overlaps the literal match.
fn original_span(text: &str, lower_start: usize, lower_end: usize) -> (usize, usize) {
    let mut lower_offset = 0;
    let mut start = None;
    let mut end = 0;
    for (offset, c) in text.char_indices() {
        let next = lower_offset + c.to_lowercase().map(char::len_utf8).sum::<usize>();
        if next > lower_start && lower_offset < lower_end {
            start.get_or_insert(offset);
            end = offset + c.len_utf8();
        }
        lower_offset = next;
        if lower_offset >= lower_end {
            break;
        }
    }
    (start.unwrap_or(0), end)
}
fn paginated(mut page: Value, cursor: Option<String>) -> Value {
    if let Some(cursor) = cursor {
        page.as_object_mut()
            .expect("document page object")
            .insert("next_cursor".into(), json!(cursor));
    }
    page
}

/// Execute locally. Errors have a stable code prefix for the standalone envelope.
pub fn execute(command: DocsCommand) -> Result<Value, String> {
    let docs = catalogue();
    let version = version(&docs);
    match command {
        DocsCommand::List {
            parent,
            limit: count,
            cursor,
        } => {
            limit(count, 200)?;
            if let Some(p) = &parent {
                find(&docs, p)?;
            }
            let scope = serde_json::to_string(&json!(["list", parent])).expect("scope");
            let offset = read_cursor(cursor.as_deref(), &version, &scope)?;
            let items: Vec<_> = docs
                .iter()
                .filter(|d| {
                    parent
                        .as_ref()
                        .map_or(d.parent_id.is_empty(), |p| &d.parent_id == p)
                })
                .collect();
            if offset > items.len() {
                return Err("cursor_expired: Offset is outside list".into());
            }
            let end = offset.saturating_add(count).min(items.len());
            Ok(paginated(
                json!({"version":version,"items":items[offset..end].iter().map(|d|summary(d,&version)).collect::<Vec<_>>()}),
                (end < items.len()).then(|| next_cursor(&version, &scope, end)),
            ))
        }
        DocsCommand::Get {
            id,
            section,
            limit_bytes,
            cursor,
        } => {
            limit(limit_bytes, 65536)?;
            let d = find(&docs, &id)?;
            let sections = sections(&d.text);
            let (start, end) = if let Some(s) = &section {
                let sec = sections
                    .iter()
                    .find(|sec| &sec.id == s)
                    .ok_or("target_unavailable: Unknown section ID")?;
                (sec.start, sec.end)
            } else {
                (0, d.text.len())
            };
            let scope =
                serde_json::to_string(&json!(["get", id, section, hash(&d.text)])).expect("scope");
            let offset = if cursor.is_some() {
                read_cursor(cursor.as_deref(), &version, &scope)?
            } else {
                start
            };
            if offset < start || offset > end || !d.text.is_char_boundary(offset) {
                return Err("cursor_expired: Invalid text offset".into());
            }
            let page_end = d
                .text
                .floor_char_boundary(offset.saturating_add(limit_bytes).min(end));
            if page_end == offset && offset < end {
                return Err(
                    "invalid_argument: Byte limit cannot hold the next UTF-8 character".into(),
                );
            }
            Ok(paginated(
                json!({"id":d.id,"title":d.title,"version":version,"content_hash":hash(&d.text),"text":&d.text[offset..page_end],"start":offset,"end":page_end,"total_bytes":d.text.len(),"section":section,"sections":sections,"links":links(d,&docs)}),
                (page_end < end).then(|| next_cursor(&version, &scope, page_end)),
            ))
        }
        DocsCommand::Search {
            query,
            parent,
            limit: count,
            cursor,
        } => {
            limit(count, 100)?;
            if query.trim().is_empty() || query.len() > 4096 {
                return Err("invalid_argument: Query must contain 1 to 4096 bytes".into());
            }
            if let Some(p) = &parent {
                find(&docs, p)?;
            }
            let scope = serde_json::to_string(&json!(["search", query, parent])).expect("scope");
            let offset = read_cursor(cursor.as_deref(), &version, &scope)?;
            let needle = query.to_lowercase();
            let mut hits = Vec::new();
            for d in &docs {
                if category(&d.id)
                    || parent
                        .as_ref()
                        .is_some_and(|p| d.id != *p && !descendant(d, p, &docs))
                {
                    continue;
                }
                let lowered = d.text.to_lowercase();
                let Some(at) = lowered.find(&needle) else {
                    continue;
                };
                let (original_start, end) = original_span(&d.text, at, at + needle.len());
                let excerpt_start = d.text.ceil_char_boundary(original_start.saturating_sub(96));
                let excerpt_end = d
                    .text
                    .floor_char_boundary(end.saturating_add(192).min(d.text.len()));
                hits.push(json!({"id":d.id,"title":d.title,"parent_id":d.parent_id,"version":version,"content_hash":hash(&d.text),"excerpt":&d.text[excerpt_start..excerpt_end],"spans":[{"start":original_start,"end":end}],"score":lowered.matches(&needle).count()}));
            }
            hits.sort_by(|a, b| {
                b["score"]
                    .as_u64()
                    .cmp(&a["score"].as_u64())
                    .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
            });
            if offset > hits.len() {
                return Err("cursor_expired: Offset is outside search".into());
            }
            let end = offset.saturating_add(count).min(hits.len());
            Ok(paginated(
                json!({"version":version,"mode":"keyword","items":hits[offset..end]}),
                (end < hits.len()).then(|| next_cursor(&version, &scope, end)),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalogue_is_connected_and_links_resolve() {
        let docs = catalogue();
        for d in &docs {
            assert_eq!(docs.iter().filter(|x| x.id == d.id).count(), 1);
            if !d.parent_id.is_empty() {
                assert!(docs.iter().any(|p| p.id == d.parent_id));
                assert!(descendant(d, "shellboy", &docs));
            }
            for link in links(d, &docs) {
                if link["kind"] == "bundled" {
                    assert_eq!(link["resolved"], true, "{} {link}", d.id);
                }
            }
        }
    }
    #[test]
    fn pages_reconstruct_unicode_and_reject_cross_document_cursor() {
        let mut cursor = None;
        let mut text = String::new();
        loop {
            let page = execute(DocsCommand::Get {
                id: "shellboy/shell".into(),
                section: None,
                limit_bytes: 17,
                cursor: cursor.clone(),
            })
            .unwrap();
            text.push_str(page["text"].as_str().unwrap());
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
            assert!(
                execute(DocsCommand::Get {
                    id: "shellboy/delegation".into(),
                    section: None,
                    limit_bytes: 17,
                    cursor: cursor.clone()
                })
                .unwrap_err()
                .starts_with("cursor_expired")
            );
        }
        assert_eq!(
            text,
            documents()
                .into_iter()
                .find(|d| d.id == "shellboy/shell")
                .unwrap()
                .text
        );
    }
    #[test]
    fn japanese_keyword_and_nested_scope() {
        let page = execute(DocsCommand::Search {
            query: "子エージェント".into(),
            parent: Some("shellboy/work".into()),
            limit: 100,
            cursor: None,
        })
        .unwrap();
        assert!(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["id"] == "shellboy/delegation")
        );
        assert!(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .all(|d| d["parent_id"] == "shellboy/work")
        );
    }
    #[test]
    fn heading_parser_ignores_code_and_disambiguates() {
        let s = sections("# Same\n\n```\n# Hidden\n```\n## Same\n\n## 子\n");
        assert_eq!(
            s.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["same", "same-1", "子"]
        );
        assert_eq!(
            &"# Same\n\n```\n# Hidden\n```\n## Same\n\n## 子\n"[s[1].start..s[1].end],
            "## Same\n\n"
        );
    }
    #[test]
    fn section_pages_stay_inside_selected_heading() {
        let source = documents()
            .into_iter()
            .find(|d| d.id == "shellboy/delegation")
            .unwrap();
        let section = sections(&source.text)
            .into_iter()
            .find(|s| s.level == 2)
            .unwrap();
        let mut cursor = None;
        let mut text = String::new();
        loop {
            let page = execute(DocsCommand::Get {
                id: source.id.clone(),
                section: Some(section.id.clone()),
                limit_bytes: 31,
                cursor,
            })
            .unwrap();
            assert!(page["start"].as_u64().unwrap() >= section.start as u64);
            assert!(page["end"].as_u64().unwrap() <= section.end as u64);
            text.push_str(page["text"].as_str().unwrap());
            cursor = page["next_cursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(text, source.text[section.start..section.end]);
    }
    #[test]
    fn list_and_search_cursor_conditions_are_bound() {
        let first = execute(DocsCommand::List {
            parent: Some("shellboy/work".into()),
            limit: 1,
            cursor: None,
        })
        .unwrap();
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();
        assert!(
            execute(DocsCommand::List {
                parent: Some("shellboy/start".into()),
                limit: 1,
                cursor: Some(cursor.clone())
            })
            .is_err()
        );
        let second = execute(DocsCommand::List {
            parent: Some("shellboy/work".into()),
            limit: 2,
            cursor: Some(cursor),
        })
        .unwrap();
        assert_ne!(first["items"][0]["id"], second["items"][0]["id"]);
        let first = execute(DocsCommand::Search {
            query: "shellboy".into(),
            parent: None,
            limit: 1,
            cursor: None,
        })
        .unwrap();
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();
        assert!(
            execute(DocsCommand::Search {
                query: "shell".into(),
                parent: None,
                limit: 1,
                cursor: Some(cursor)
            })
            .is_err()
        );
        assert!(
            execute(DocsCommand::Search {
                query: " ".into(),
                parent: None,
                limit: 1,
                cursor: None
            })
            .is_err()
        );
    }
    #[test]
    fn lowercase_expansion_keeps_original_utf8_span() {
        let text = "xİZ";
        assert_eq!(original_span(text, 2, 4), (1, 3));
        assert_eq!(original_span(text, 1, 5), (1, 4));
    }
    #[test]
    fn revision_and_argument_cursor_binding() {
        let c = next_cursor("old", "scope", 1);
        assert!(read_cursor(Some(&c), "new", "scope").is_err());
        assert!(read_cursor(Some(&c), "old", "other").is_err());
        assert!(
            execute(DocsCommand::List {
                parent: None,
                limit: 201,
                cursor: None
            })
            .is_err()
        );
    }
}
