//! On-disk index key spelling. Profiles are existing identity hashes, not new IDs.
#[derive(Clone, Copy)]
pub(crate) struct IndexKeys<'a>(&'a str);
impl<'a> IndexKeys<'a> {
    pub(super) const ACTIVE_SPACE: &'static str = "index.active_space";
    pub(super) const AUTO_PREFIX: &'static str = "index.auto.";
    pub(super) const INPUT_PREFIX: &'static str = "index.input.";
    pub(super) const BOOT_PREFIX: &'static str = "index.bootstrap.";
    pub(super) const PREVIEW_PREFIX: &'static str = "index.preview.";
    pub(super) fn new(profile: &'a str) -> Self {
        Self(profile)
    }
    pub(super) fn pause(self) -> String {
        format!("index.pause.{}", self.0)
    }
    pub(super) fn rebuild(self) -> String {
        format!("index.rebuild.{}", self.0)
    }
    pub(super) fn group(self, group: &str) -> String {
        format!("index.group.{}.{group}", self.0)
    }
    pub(super) fn enqueueing(self, group: &str) -> String {
        format!("index.enqueueing.{}.{group}", self.0)
    }
    pub(super) fn common_failures(self) -> String {
        format!("index.common_failures.{}", self.0)
    }
    pub(super) fn auto(self, node: &str) -> String {
        format!("{}{}.{node}", Self::AUTO_PREFIX, self.0)
    }
    pub(super) fn auto_pattern(self) -> String {
        format!("{}{}.%", Self::AUTO_PREFIX, self.0)
    }
    pub(super) fn bootstrap(self, digest: &str) -> String {
        format!("{}{}.{digest}", Self::BOOT_PREFIX, self.0)
    }
    pub(super) fn bootstrap_pattern(self) -> String {
        format!("{}{}.%", Self::BOOT_PREFIX, self.0)
    }
    pub(super) fn splitter(self) -> String {
        format!("{}:{}", super::SPLITTER, self.0)
    }
    pub(super) fn input(job: &str) -> String {
        format!("{}{job}", Self::INPUT_PREFIX)
    }
    pub(super) fn query_profile(space: &str) -> String {
        format!("index.query_profile.{space}")
    }
    pub(super) fn preview(chunk: &str) -> String {
        format!("{}{chunk}", Self::PREVIEW_PREFIX)
    }
    pub(crate) fn observed(thread: &str) -> String {
        format!("index.observed.{thread}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_index_key_spellings_are_byte_exact() {
        let keys = IndexKeys::new("profile:日本");
        for (actual, expected) in [
            (keys.pause(), "index.pause.profile:日本"),
            (keys.rebuild(), "index.rebuild.profile:日本"),
            (
                keys.group("candidate:1"),
                "index.group.profile:日本.candidate:1",
            ),
            (
                keys.enqueueing("candidate:1"),
                "index.enqueueing.profile:日本.candidate:1",
            ),
            (keys.common_failures(), "index.common_failures.profile:日本"),
            (keys.auto("node:1"), "index.auto.profile:日本.node:1"),
            (keys.auto_pattern(), "index.auto.profile:日本.%"),
            (
                keys.bootstrap("hash:1"),
                "index.bootstrap.profile:日本.hash:1",
            ),
            (keys.bootstrap_pattern(), "index.bootstrap.profile:日本.%"),
            (keys.splitter(), "unicode-paragraph-v1:profile:日本"),
            (IndexKeys::input("job:1"), "index.input.job:1"),
            (IndexKeys::preview("chunk:1"), "index.preview.chunk:1"),
            (
                IndexKeys::query_profile("space:1"),
                "index.query_profile.space:1",
            ),
            (IndexKeys::observed("thread:1"), "index.observed.thread:1"),
        ] {
            assert_eq!(actual.as_bytes(), expected.as_bytes());
        }
        assert_eq!(IndexKeys::ACTIVE_SPACE, "index.active_space");
        assert_eq!(IndexKeys::new("").group("%_"), "index.group..%_");
    }
    #[test]
    fn authorized_record_json_is_the_existing_wire_shape() {
        use crate::index::{AutoAdmission, RawWitness, Segment, Witness, Work};
        let work = Work::pending(
            Witness {
                node_id: "node".into(),
                revision: 7,
                body_hash: Some("source".into()),
                raw: Some(RawWitness {
                    execution: "shell".into(),
                    generation: 3,
                    saved_sequence: 5,
                    encoding: "UTF-8".into(),
                }),
            },
            "tool_raw".into(),
            Segment {
                start: 2,
                end: 4,
                text: "A界".into(),
            },
            "group",
            "explicit",
        );
        assert_eq!(
            serde_json::to_string(&work).unwrap(),
            r#"{"witness":{"node_id":"node","revision":7,"body_hash":"source","raw":{"execution":"shell","generation":3,"saved_sequence":5,"encoding":"UTF-8"}},"body_kind":"tool_raw","segment":{"start":2,"end":4,"text":"A界"},"group":"group","authorization":"explicit","attempts":0,"state":"pending","next_at":null,"error":null}"#
        );
        assert_eq!(
            crate::index::bootstrap_key(IndexKeys::new("profile"), &work),
            "index.bootstrap.profile.1155439556c2f5e8b80beacab9aa222a2f46aec8c81a025b891917121448d7cd"
        );
        assert_eq!(
            crate::index::encode_json(&work).unwrap(),
            serde_json::to_string(&work).unwrap()
        );
        let auto = AutoAdmission {
            node_id: "node".into(),
            state: "pending".into(),
            error: None,
        };
        assert_eq!(
            crate::index::encode_json(&auto).unwrap(),
            serde_json::to_string(&auto).unwrap()
        );
        assert_eq!(
            serde_json::to_string(&auto).unwrap(),
            r#"{"node_id":"node","state":"pending","error":null}"#
        );
    }
}
