//! Bounded OSC7501 rev0.4 records. Only the UI's frame-boundary writer emits these.
use crate::activity::ActivityPhase;
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    collections::BTreeMap,
    io::{self, Write},
};
pub const CHILD_LIMIT: usize = 32;
pub const TITLE_BYTES: usize = 192;
pub const MSG_BYTES: usize = 2048;
pub const SEQUENCE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum State {
    Idle,
    Done,
    Working,
    Error,
    Blocked,
    Clear,
}
impl State {
    pub fn text(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Done => "done",
            Self::Working => "working",
            Self::Error => "error",
            Self::Blocked => "blocked",
            Self::Clear => "clear",
        }
    }
}
pub fn map_phase(phase: ActivityPhase, unseen: bool) -> State {
    use ActivityPhase::*;
    match phase {
        Idle | Stopped => State::Idle,
        Completed => {
            if unseen {
                State::Done
            } else {
                State::Idle
            }
        }
        Failed => State::Error,
        Recovery => State::Blocked,
        Sending | Reflecting | Preparing | AwaitingModel | Retrying | Receiving | Tools
        | Running | Waiting | Background | Continuing | Stopping => State::Working,
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub id: Option<String>,
    pub state: State,
    pub auth: bool,
    pub title: String,
    pub msg: String,
}
#[derive(Clone, Debug, Default)]
pub struct Counts {
    pub blocked: u64,
    pub auth: u64,
    pub error: u64,
    pub working: u64,
    pub done: u64,
    pub idle: u64,
}
impl Counts {
    pub fn add(&mut self, state: State, auth: bool) {
        match state {
            State::Blocked => {
                self.blocked += 1;
                if auth {
                    self.auth += 1;
                }
            }
            State::Error => self.error += 1,
            State::Working => self.working += 1,
            State::Done => self.done += 1,
            State::Idle => self.idle += 1,
            State::Clear => {}
        }
    }
    pub fn state(&self) -> State {
        if self.blocked > 0 {
            State::Blocked
        } else if self.error > 0 {
            State::Error
        } else if self.working > 0 {
            State::Working
        } else if self.done > 0 {
            State::Done
        } else {
            State::Idle
        }
    }
    pub fn root(&self) -> Record {
        Record {
            id: None,
            state: self.state(),
            auth: self.blocked > 0 && self.blocked == self.auth,
            title: format!(
                "blocked {}, error {}, working {}, done {}, idle {}",
                self.blocked, self.error, self.working, self.done, self.idle
            ),
            msg: if self.blocked > 0 {
                format!("{} conversations need recovery", self.blocked)
            } else {
                String::new()
            },
        }
    }
}
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub selected: Option<String>,
    pub viewed: Option<(String, String)>,
    pub counts: Counts,
    pub children: Vec<Record>,
}

pub fn thread_id(id: &str) -> String {
    let short = uuid::Uuid::parse_str(id)
        .map(|id| id.simple().to_string())
        .unwrap_or_else(|_| crate::digest::sha256_hex(id)[..32].to_owned());
    format!("thread/{short}")
}
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.split('/').count() <= 8
        && id.split('/').all(|segment| {
            !segment.is_empty()
                && segment.len() <= 32
                && segment
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_.+-".contains(&c))
        })
}
fn format_char(c: char) -> bool {
    // Unicode format/default-ignorable controls are never needed in a single-line label.
    matches!(
        c,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{06dd}'
            | '\u{070f}'
            | '\u{08e2}'
            | '\u{180e}'
            | '\u{3164}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{110bd}'
            | '\u{110cd}'
    ) || matches!(c as u32,0x600..=0x605|0x890..=0x891|0x115f..=0x1160|0x17b4..=0x17b5|0x180b..=0x180f|0x200b..=0x200f|0x2028..=0x202e|0x2060..=0x206f|0xfe00..=0xfe0f|0xfff9..=0xfffb|0x13430..=0x1345f|0x1bca0..=0x1bca3|0x1d173..=0x1d17a|0xe0000..=0xe0fff)
}
pub fn clean(text: &str, limit: usize) -> String {
    let plain = crate::presentation::sanitize_plain(text, 0).0;
    let mut out = String::new();
    for c in plain
        .chars()
        .filter(|c| !c.is_control() && !format_char(*c))
    {
        if out.len() + c.len_utf8() > limit {
            break;
        }
        out.push(c);
    }
    out
}
pub fn encode(record: &Record) -> io::Result<Vec<u8>> {
    if record.id.as_deref().is_some_and(|id| !valid_id(id)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid OSC7501 id",
        ));
    }
    let mut out = format!("\x1b]7501;state={}:app=shellboy", record.state.text());
    if let Some(id) = &record.id {
        out.push_str(":id=");
        out.push_str(id);
    }
    if record.state == State::Blocked && record.auth {
        out.push_str(":kind=auth");
    }
    if record.state != State::Clear {
        out.push_str(":title=");
        out.push_str(&STANDARD.encode(clean(&record.title, TITLE_BYTES)));
        if !record.msg.is_empty() {
            out.push_str(":msg=");
            out.push_str(&STANDARD.encode(clean(&record.msg, MSG_BYTES)));
        }
    }
    out.push_str("\x1b\\");
    if out.len() > SEQUENCE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "OSC7501 sequence too large",
        ));
    }
    Ok(out.into_bytes())
}
/// A bounded last-successful-write cache; no timer, task or terminal feature query.
pub struct Publisher {
    enabled: bool,
    sent: BTreeMap<Option<String>, Vec<u8>>,
}
impl Publisher {
    pub fn new(mode: crate::config::ProgramStatus) -> Self {
        Self {
            enabled: mode == crate::config::ProgramStatus::On,
            sent: BTreeMap::new(),
        }
    }
    pub fn sync(&mut self, snapshot: &Snapshot, writer: &mut impl Write) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let mut desired = BTreeMap::new();
        let mut root = snapshot.counts.root();
        if root.state == State::Blocked
            && let Some(reason) = snapshot
                .children
                .iter()
                .find(|record| record.state == State::Blocked && !record.msg.is_empty())
        {
            root.msg = reason.msg.clone();
        }
        desired.insert(None, encode(&root)?);
        let mut children = snapshot
            .children
            .iter()
            .filter(|r| r.id.is_some() && !matches!(r.state, State::Idle | State::Clear))
            .collect::<Vec<_>>();
        children.sort_by(|a, b| b.state.cmp(&a.state).then(a.id.cmp(&b.id)));
        for record in children.into_iter().take(CHILD_LIMIT) {
            desired.insert(record.id.clone(), encode(record)?);
        }
        let mut bytes = Vec::new();
        for id in self
            .sent
            .keys()
            .filter(|id| id.is_some() && !desired.contains_key(*id))
        {
            bytes.extend(encode(&Record {
                id: id.clone(),
                state: State::Clear,
                auth: false,
                title: String::new(),
                msg: String::new(),
            })?);
        }
        for (id, value) in &desired {
            if self.sent.get(id) != Some(value) {
                bytes.extend_from_slice(value);
            }
        }
        if !bytes.is_empty() {
            writer.write_all(&bytes)?;
            writer.flush()?;
        }
        self.sent = desired;
        Ok(())
    }
    pub fn clear(&mut self, writer: &mut impl Write) -> io::Result<()> {
        if self.enabled {
            writer.write_all(&encode(&Record {
                id: None,
                state: State::Clear,
                auth: false,
                title: String::new(),
                msg: String::new(),
            })?)?;
            writer.flush()?;
            self.sent.clear();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(id: Option<String>, state: State) -> Record {
        Record {
            id,
            state,
            auth: false,
            title: "title".into(),
            msg: "message".into(),
        }
    }
    fn fields(bytes: &[u8]) -> BTreeMap<String, String> {
        let text = std::str::from_utf8(bytes)
            .unwrap()
            .strip_prefix("\x1b]7501;")
            .unwrap()
            .strip_suffix("\x1b\\")
            .unwrap();
        text.split(':')
            .map(|pair| {
                let (key, value) = pair.split_once('=').unwrap();
                assert!(key.len() <= 16 && key.bytes().all(|c| c.is_ascii_lowercase()));
                assert!(
                    value
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.,+/=-".contains(&c))
                );
                (key.into(), value.into())
            })
            .collect()
    }
    #[test]
    fn encoder_cleans_text_and_bounds_every_wire_value_without_splitting_utf8() {
        let mut item = record(Some(thread_id("synthetic")), State::Blocked);
        item.auth = true;
        item.title = format!(
            "a\n\t\x1b]7501;state=error:app=evil\x1b\\\u{0085}\u{202e}\u{2066}\u{200b}\u{feff}{}",
            "🙂".repeat(1000)
        );
        item.msg = format!("msg\u{2028}\u{2029}\u{2069}{}", "界".repeat(2000));
        let bytes = encode(&item).unwrap();
        assert!(bytes.len() <= SEQUENCE_BYTES);
        let values = fields(&bytes);
        assert_eq!(values["state"], "blocked");
        assert_eq!(values["kind"], "auth");
        assert_eq!(values["app"], "shellboy");
        assert!(!values.contains_key("progress"));
        for (key, limit) in [("title", TITLE_BYTES), ("msg", MSG_BYTES)] {
            let text = String::from_utf8(STANDARD.decode(&values[key]).unwrap()).unwrap();
            assert!(text.len() <= limit);
            assert!(!text.chars().any(|c| c.is_control() || format_char(c)));
            assert!(!text.contains("app=evil"));
        }
        assert!(values["title"].len() <= 256 && values["msg"].len() <= 2732);
        item.state = State::Working;
        assert!(!fields(&encode(&item).unwrap()).contains_key("kind"));
    }
    #[test]
    fn ids_are_strict_and_never_fall_back_to_root() {
        let max = [
            "a".repeat(32),
            "b".repeat(32),
            "c".repeat(32),
            "d".repeat(29),
        ]
        .join("/");
        assert_eq!(max.len(), 128);
        assert!(valid_id(&max));
        assert!(valid_id("A_+.-/b"));
        assert!(valid_id(&["x"; 8].join("/")));
        for bad in [
            "".into(),
            "/a".into(),
            "a/".into(),
            "a//b".into(),
            "a:b".into(),
            "a;b".into(),
            "a=b".into(),
            "x".repeat(33),
            ["x"; 9].join("/"),
            format!("{max}x"),
            "日本語".into(),
        ] {
            assert!(!valid_id(&bad));
            assert!(encode(&record(Some(bad), State::Working)).is_err());
        }
        assert_eq!(
            thread_id("550e8400-e29b-41d4-a716-446655440000"),
            "thread/550e8400e29b41d4a716446655440000"
        );
    }
    #[test]
    fn every_shared_activity_maps_and_root_priority_is_explicit() {
        use ActivityPhase::*;
        for phase in [
            Sending,
            Reflecting,
            Preparing,
            AwaitingModel,
            Retrying,
            Receiving,
            Tools,
            Running,
            Waiting,
            Background,
            Continuing,
            Stopping,
        ] {
            assert_eq!(map_phase(phase, false), State::Working);
        }
        for phase in [Idle, Stopped] {
            assert_eq!(map_phase(phase, true), State::Idle);
        }
        assert_eq!(map_phase(Completed, true), State::Done);
        assert_eq!(map_phase(Completed, false), State::Idle);
        assert_eq!(map_phase(Recovery, false), State::Blocked);
        assert_eq!(map_phase(Failed, false), State::Error);
        let mut counts = Counts::default();
        assert_eq!(counts.state(), State::Idle);
        for state in [State::Done, State::Working, State::Error, State::Blocked] {
            counts.add(state, false);
            assert_eq!(counts.state(), state);
        }
    }
    #[test]
    fn publisher_diffs_clear_idle_and_overflow_and_disabled_emits_nothing() {
        let mut snapshot = Snapshot {
            selected: None,
            viewed: None,
            counts: Counts {
                working: 40,
                ..Default::default()
            },
            children: (0..40)
                .map(|i| record(Some(thread_id(&i.to_string())), State::Working))
                .collect(),
        };
        let mut publisher = Publisher::new(crate::config::ProgramStatus::On);
        let mut bytes = Vec::new();
        publisher.sync(&snapshot, &mut bytes).unwrap();
        assert_eq!(publisher.sent.len(), CHILD_LIMIT + 1);
        assert_eq!(
            bytes
                .windows(7)
                .filter(|bytes| *bytes == b"\x1b]7501;")
                .count(),
            33
        );
        let at = bytes.len();
        publisher.sync(&snapshot, &mut bytes).unwrap();
        assert_eq!(bytes.len(), at);
        let excluded = snapshot
            .children
            .iter()
            .position(|record| !publisher.sent.contains_key(&record.id))
            .unwrap();
        snapshot.children[excluded].state = State::Blocked;
        snapshot.children[excluded].auth = true;
        snapshot.counts.blocked = 1;
        snapshot.counts.auth = 1;
        snapshot.counts.working = 39;
        publisher.sync(&snapshot, &mut bytes).unwrap();
        assert_eq!(publisher.sent.len(), 33);
        let added = std::str::from_utf8(&bytes[at..]).unwrap();
        assert!(added.contains("state=clear:app=shellboy:id=thread/"));
        assert!(
            publisher
                .sent
                .values()
                .any(|bytes| fields(bytes).get("state") == Some(&"blocked".into()))
        );
        snapshot.children.clear();
        snapshot.counts = Counts {
            idle: 40,
            ..Default::default()
        };
        publisher.sync(&snapshot, &mut bytes).unwrap();
        assert_eq!(publisher.sent.len(), 1);
        publisher.clear(&mut bytes).unwrap();
        assert!(publisher.sent.is_empty());
        let rootclear = encode(&record(None, State::Clear)).unwrap();
        assert!(bytes.ends_with(&rootclear));
        let mut disabled = Publisher::new(crate::config::ProgramStatus::Off);
        let mut empty = Vec::new();
        disabled.sync(&snapshot, &mut empty).unwrap();
        disabled.clear(&mut empty).unwrap();
        assert!(empty.is_empty());
    }
}

#[cfg(test)]
mod setting_tests {
    #[test]
    fn setting_is_on_by_default_and_off_is_strict_string_configuration() {
        use crate::config::{Config, ProgramStatus};
        assert_eq!(Config::default().ui.program_status, ProgramStatus::On);
        assert_eq!(
            Config::parse("[ui]\nprogram_status='off'")
                .unwrap()
                .ui
                .program_status,
            ProgramStatus::Off
        );
        for bad in ["true", "false", "'auto'", "'ON'", "1"] {
            assert!(Config::parse(&format!("[ui]\nprogram_status={bad}")).is_err());
        }
    }
}
