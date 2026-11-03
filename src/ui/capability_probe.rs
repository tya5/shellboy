//! Incremental classification of terminal capability replies.
//!
//! The caller owns the input stream and forwards `input` unchanged to the key
//! decoder. Paste delimiters and contents are never interpreted as replies.

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Capability<T> {
    Unmeasured,
    Measured(T),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Cursor,
    DeviceAttributes,
    Keyboard,
}

impl Kind {
    fn index(self) -> usize {
        match self {
            Self::Cursor => 0,
            Self::DeviceAttributes => 1,
            Self::Keyboard => 2,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Reply {
    pub kind: Kind,
    pub private: bool,
    pub values: Vec<u16>,
}

#[derive(Default)]
pub(crate) struct ReplyFilter {
    pending: Vec<u8>,
    paste: bool,
    outstanding: [bool; 3],
}

impl ReplyFilter {
    pub fn expect(&mut self, kind: Kind) {
        self.outstanding[kind.index()] = true;
    }

    pub fn feed(&mut self, bytes: &[u8], input: &mut Vec<u8>, replies: &mut Vec<Reply>) {
        for &byte in bytes {
            if self.pending.is_empty() && byte != 0x1b {
                input.push(byte);
                continue;
            }
            self.pending.push(byte);
            let p = self.pending.as_slice();
            if p == b"\x1b[200~" && !self.paste {
                self.paste = true;
            } else if p == b"\x1b[201~" && self.paste {
                self.paste = false;
            } else if p == b"\x1b" || p == b"\x1b[" {
                continue;
            } else if p.starts_with(b"\x1b[")
                && p.len() <= 64
                && p.last().is_some_and(|v| (0x20..=0x3f).contains(v))
            {
                continue;
            } else if !self.paste
                && let Some(reply) = parse_reply(p)
                && self.outstanding[reply.kind.index()]
            {
                self.outstanding[reply.kind.index()] = false;
                replies.push(reply);
                self.pending.clear();
                continue;
            }
            input.append(&mut self.pending);
        }
    }

    pub fn flush_input(&mut self, input: &mut Vec<u8>) {
        input.append(&mut self.pending);
    }
}

fn parse_reply(bytes: &[u8]) -> Option<Reply> {
    let bytes = bytes.strip_prefix(b"\x1b[")?;
    let (&last, body) = bytes.split_last()?;
    let (private, body) = match body.strip_prefix(b"?") {
        Some(body) => (true, body),
        None => (false, body),
    };
    let kind = match (last, private) {
        (b'R', _) => Kind::Cursor,
        (b'c', true) => Kind::DeviceAttributes,
        (b'u', true) => Kind::Keyboard,
        _ => return None,
    };
    let text = std::str::from_utf8(body).ok()?;
    let values: Vec<u16> = text
        .split(';')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if values.is_empty()
        || (kind == Kind::Cursor && (values.len() != 2 || values.contains(&0)))
        || (kind == Kind::Keyboard && values.len() != 1)
    {
        return None;
    }
    Some(Reply {
        kind,
        private,
        values,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn paste_keeps_response_like_bytes_even_if_query_is_outstanding() {
        let mut filter = ReplyFilter::default();
        filter.expect(Kind::Cursor);
        let paste = b"\x1b[200~a\x1b[1;2Rb\x1b[201~";
        let mut input = Vec::new();
        let mut replies = Vec::new();
        for byte in paste {
            filter.feed(&[*byte], &mut input, &mut replies);
        }
        assert_eq!(input, paste);
        assert!(replies.is_empty());
    }

    #[test]
    fn paste_decodes_identically_with_and_without_filter() {
        for payload in [
            b"before\x1b[1;2R\x1b[?uafter".as_slice(),
            b"bad\xfftext".as_slice(),
        ] {
            let mut encoded = b"\x1b[200~".to_vec();
            encoded.extend_from_slice(payload);
            encoded.extend_from_slice(b"\x1b[201~");
            let mut filter = ReplyFilter::default();
            filter.expect(Kind::Cursor);
            filter.expect(Kind::Keyboard);
            let mut filtered = Vec::new();
            let mut replies = Vec::new();
            for fragment in encoded.chunks(2) {
                filter.feed(fragment, &mut filtered, &mut replies);
            }
            assert_eq!(filtered, encoded);
            assert!(replies.is_empty());
            // Crossterm's existing paste decoder applies UTF-8 loss replacement.
            assert_eq!(
                String::from_utf8_lossy(&filtered[6..filtered.len() - 6]),
                String::from_utf8_lossy(payload)
            );
        }
    }

    #[test]
    fn typing_and_delayed_replies_are_filtered_exactly_once() {
        let (tx, rx) = mpsc::channel();
        tx.send(b"hel".as_slice()).unwrap();
        tx.send(b"\x1b[?1;3Rlo\x1b[1;2R".as_slice()).unwrap();
        drop(tx);
        let mut filter = ReplyFilter::default();
        filter.expect(Kind::Cursor);
        let mut input = Vec::new();
        let mut replies = Vec::new();
        for bytes in rx {
            filter.feed(bytes, &mut input, &mut replies);
        }
        assert_eq!(input, b"hello\x1b[1;2R");
        assert_eq!(replies.len(), 1);
        assert!(replies[0].private);
        assert_eq!(replies[0].values, [1, 3]);
    }

    #[test]
    fn unanswered_query_preserves_unrelated_input() {
        let mut filter = ReplyFilter::default();
        filter.expect(Kind::Cursor);
        let mut input = Vec::new();
        let mut replies = Vec::new();
        filter.feed(b"hello\x1b[A world\x1b[?1u", &mut input, &mut replies);
        assert_eq!(input, b"hello\x1b[A world\x1b[?1u");
        assert!(replies.is_empty());
    }
}
