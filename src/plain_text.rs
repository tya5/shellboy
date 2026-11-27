//! Terminal-control stripping and bare/OSC 8 link recognition for plain text.
//! Owner-neutral: control and presentation both import from here.
use crate::terminal_links::valid_link_target as http;
use serde::{Deserialize, Serialize};

pub const MAX_METADATA_BYTES: usize = 64 * 1024;
pub(crate) const MAX_LINKS: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresentationLink {
    pub label: String,
    pub url: String,
    pub source_start: usize,
    pub source_end: usize,
}
pub(crate) fn add_link(
    out: &mut Vec<PresentationLink>,
    label: &str,
    url: &str,
    start: usize,
    end: usize,
) {
    if http(url)
        && out.len() < MAX_LINKS
        && !out.iter().any(|l| l.url == url && l.source_start == start)
        && out
            .iter()
            .map(|l| l.url.len() + l.label.len())
            .sum::<usize>()
            + url.len()
            + label.len()
            <= MAX_METADATA_BYTES
    {
        out.push(PresentationLink {
            label: label.into(),
            url: url.into(),
            source_start: start,
            source_end: end,
        });
    }
}
// A broken string stops at its next line or escape, so retrying that escape
// cannot repeatedly scan the remaining input. Only a complete OSC owns payload.
fn osc_control_end(chars: &[char], mut at: usize) -> Option<(usize, usize)> {
    while let Some(&c) = chars.get(at) {
        match c {
            '\u{7}' => return Some((at, at + 1)),
            '\u{1b}' if chars.get(at + 1) == Some(&'\\') => return Some((at, at + 2)),
            '\n' | '\r' | '\u{1b}' => return None,
            _ => at += 1,
        }
    }
    None
}

fn csi_control_end(chars: &[char], mut at: usize) -> Option<usize> {
    let mut intermediate = false;
    while let Some(&c) = chars.get(at) {
        match c {
            ' '..='/' => intermediate = true,
            '0'..='?' if !intermediate => {}
            '@'..='~' => return Some(at + 1),
            _ => return None,
        }
        at += 1;
    }
    None
}

/// Strip terminal control sequences; OSC 8 URLs become metadata, never terminal bytes.
pub fn sanitize_plain(text: &str, base: usize) -> (String, Vec<PresentationLink>) {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut links = vec![];
    let mut i = 0;
    let mut open: Option<(String, usize, usize)> = None;
    let mut controls = Vec::new();
    while i < chars.len() {
        if chars[i] == '\u{1b}' {
            let control_start = i;
            if chars.get(i + 1) == Some(&']') {
                let start = i;
                let begin = i + 2;
                if let Some((end, sequence_end)) = osc_control_end(&chars, begin) {
                    let payload: String = chars[begin..end].iter().collect();
                    if let Some(rest) = payload.strip_prefix("8;")
                        && let Some((_, url)) = rest.split_once(';')
                    {
                        if url.is_empty() {
                            if let Some((url, at, label_byte)) = open.take() {
                                add_link(
                                    &mut links,
                                    &out[label_byte..],
                                    &url,
                                    base + at,
                                    base + start,
                                );
                            }
                        } else if http(url) {
                            open = Some((url.into(), sequence_end, out.len()));
                        }
                    }
                    i = sequence_end;
                } else {
                    // Drop the unsafe introducer, then preserve its payload as
                    // ordinary sanitized text instead of hiding the remainder.
                    i = begin;
                }
            } else if chars.get(i + 1) == Some(&'[') {
                i = csi_control_end(&chars, i + 2).unwrap_or(i + 2);
            } else {
                i = (i + 2).min(chars.len());
            }
            controls.push((control_start, i));
        } else {
            let c = chars[i];
            if c == '\n' || c == '\t' || (!c.is_control() && c != '\u{7f}') {
                out.push(c);
            }
            i += 1;
        }
    }
    if let Some((url, at, label_byte)) = open {
        add_link(
            &mut links,
            &out[label_byte..],
            &url,
            base + at,
            base + chars.len(),
        );
    }
    // Bare URL offsets refer to the unmodified input. Recognition performs no I/O.
    for (at, _) in text.match_indices("http") {
        if links.len() >= MAX_LINKS {
            break;
        }
        if !text[at..].starts_with("http://") && !text[at..].starts_with("https://") {
            continue;
        }
        let raw = text[at..]
            .split(|c: char| {
                c.is_whitespace() || c.is_control() || ['<', '>', '"', '\'', '`'].contains(&c)
            })
            .next()
            .unwrap_or("");
        let mut url = raw.trim_end_matches([
            '.', ',', ';', ':', '。', '、', '！', '？', '）', '】', '」', '』',
        ]);
        for (left, right) in [('(', ')'), ('[', ']'), ('{', '}')] {
            while url.ends_with(right) && url.matches(right).count() > url.matches(left).count() {
                url = url.strip_suffix(right).unwrap();
            }
        }
        if http(url) {
            let local = text[..at].chars().count();
            let seq = controls.partition_point(|&(_, end)| end <= local);
            if controls
                .get(seq)
                .is_some_and(|&(begin, end)| local >= begin && local < end)
            {
                continue;
            }
            let start = base + local;
            add_link(&mut links, url, url, start, start + url.chars().count());
        }
    }
    (out, links)
}
