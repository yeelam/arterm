use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Console::{INPUT_RECORD, KEY_EVENT};

pub(super) struct Character {
    pub byte: u8,
    pub raw: Vec<u8>,
    pub record: Option<INPUT_RECORD>,
}

pub(super) struct Replies {
    pub outstanding: BTreeSet<u16>,
    pub states: BTreeMap<u16, bool>,
    pub preserved: Vec<INPUT_RECORD>,
    pub wake_pending: bool,
    pending: Vec<Character>,
    encoded: Vec<u8>,
    last_input: Instant,
}

impl Replies {
    pub fn new(modes: &[u16]) -> Self {
        Self { outstanding: modes.iter().copied().collect(), states: BTreeMap::new(), preserved: Vec::new(), wake_pending: false,
            pending: Vec::new(), encoded: Vec::new(), last_input: Instant::now() }
    }

    fn character(&mut self, value: Character, output: &mut Vec<Character>) {
        if value.byte == 27 && !self.pending.is_empty() {
            output.append(&mut self.pending);
        }
        self.pending.push(value);
        let text: Vec<u8> = self.pending.iter().map(|value| value.byte).collect();
        let mut possible = false;
        for &mode in &self.outstanding {
            for state in 0..=4 {
                let reply = format!("\x1b[?{mode};{state}$y");
                if reply.as_bytes() == text {
                    self.outstanding.remove(&mode);
                    if state == 1 || state == 2 { self.states.insert(mode, state == 1); }
                    self.pending.clear();
                    return;
                }
                possible |= reply.as_bytes().starts_with(&text);
            }
        }
        if !possible { output.append(&mut self.pending); }
    }

    pub fn record(&mut self, record: INPUT_RECORD) -> Vec<INPUT_RECORD> {
        self.last_input = Instant::now();
        let mut output = Vec::new();
        let key = unsafe { record.Event.KeyEvent };
        if record.EventType == KEY_EVENT as u16 && key.bKeyDown != 0
            && unsafe { key.uChar.UnicodeChar } < 128
        {
            let byte = unsafe { key.uChar.UnicodeChar } as u8;
            self.character(Character { byte, raw: vec![byte], record: Some(record) }, &mut output);
            output.into_iter().map(Self::to_record).collect()
        } else {
            // Do not hold or discard unrelated mouse, focus, resize or key-up records.
            vec![record]
        }
    }

    fn to_record(value: Character) -> INPUT_RECORD {
        value.record.unwrap_or_else(|| {
            let mut record: INPUT_RECORD = unsafe { std::mem::zeroed() };
            record.EventType = KEY_EVENT as u16;
            record.Event.KeyEvent.bKeyDown = 1;
            record.Event.KeyEvent.wRepeatCount = 1;
            record.Event.KeyEvent.uChar.UnicodeChar = value.byte as u16;
            record
        })
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.last_input = Instant::now();
        if self.outstanding.is_empty() && self.pending.is_empty() && self.encoded.is_empty() {
            return bytes.to_vec();
        }
        let mut output = Vec::new();
        for &byte in bytes {
            if self.encoded.is_empty() && byte != 27 {
                self.character(Character { byte, raw: vec![byte], record: None }, &mut output);
                continue;
            }
            self.encoded.push(byte);
            let body = self.encoded.strip_prefix(b"\x1b[");
            if self.encoded == b"\x1b" || body.is_some_and(|body|
                body.iter().all(|b| b.is_ascii_digit() || *b == b';') && body.len() < 128)
            {
                continue;
            }
            let encoded = std::mem::take(&mut self.encoded);
            if let Some(fields) = super::console_input::win32_fields(&encoded) {
                if fields[3] == 1 && fields[2] < 128 {
                    self.character(Character { byte: fields[2] as u8, raw: encoded, record: None }, &mut output);
                    continue;
                }
            }
            for byte in encoded {
                self.character(Character { byte, raw: vec![byte], record: None }, &mut output);
            }
        }
        output.into_iter().flat_map(|value| value.raw).collect()
    }

    pub fn expire_ambiguous(&mut self, now: Instant) -> Vec<u8> {
        if now.saturating_duration_since(self.last_input) < Duration::from_millis(60) { return Vec::new(); }
        // An unfinished encoded character can continue an owned reply. Its ESC
        // is framing, not a new raw Escape that cancels the correlated prefix.
        if self.pending.len() >= 4 { return Vec::new(); }
        let encoded = std::mem::take(&mut self.encoded);
        let mut output = Vec::new();
        for byte in encoded {
            self.character(Character { byte, raw: vec![byte], record: None }, &mut output);
        }
        if self.pending.len() < 4 { output.append(&mut self.pending); }
        output.into_iter().flat_map(|value| value.raw).collect()
    }

    pub fn take_preserved_bytes(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut other = Vec::new();
        for record in self.preserved.drain(..) {
            if record.EventType == KEY_EVENT as u16 {
                let key = unsafe { record.Event.KeyEvent };
                bytes.extend_from_slice(format!("\x1b[{};{};{};{};{};{}_", key.wVirtualKeyCode,
                    key.wVirtualScanCode, unsafe { key.uChar.UnicodeChar }, key.bKeyDown,
                    key.dwControlKeyState, key.wRepeatCount).as_bytes());
            } else {
                other.push(record);
            }
        }
        self.preserved = other;
        bytes
    }

    pub fn finish(&mut self) -> Vec<INPUT_RECORD> {
        if self.pending.len() >= 4 { self.encoded.clear(); }
        let encoded = std::mem::take(&mut self.encoded);
        let mut released = Vec::new();
        for byte in encoded {
            self.character(Character { byte, raw: vec![byte], record: None }, &mut released);
        }
        // Only a correlated DECRPM prefix is ours. A lone Escape / CSI belongs
        // back to the caller if no identifying mode number ever arrived.
        if self.pending.len() < 4 {
            released.append(&mut self.pending);
        } else if !self.pending.is_empty() {
            crate::statusln!("[console] Discarding an incomplete local mode reply.");
            self.pending.clear();
        }
        self.preserved.extend(released.into_iter().map(Self::to_record));
        std::mem::take(&mut self.preserved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delayed_split_replies_are_not_input_and_unrelated_bytes_survive() {
        let mut replies = Replies::new(&[9001, 1004]);
        assert!(replies.bytes(b"\x1b[?9001;").is_empty());
        assert_eq!(replies.bytes(b"1$yecho ok\r"), b"echo ok\r");
        assert_eq!(replies.bytes(b"human\x1b[?1004;2$ymore"), b"humanmore");
        assert!(replies.outstanding.is_empty());
        assert_eq!(replies.states.get(&9001), Some(&true));
    }

    #[test]
    fn win32_encoded_query_replies_preserve_other_key_records() {
        let mut replies = Replies::new(&[9001]);
        let encoded: String = b"\x1b[?9001;1$y".iter().map(|byte|
            format!("\x1b[0;0;{byte};1;0;1_")).collect();
        for part in encoded.as_bytes().chunks(7) { assert!(replies.bytes(part).is_empty()); }
        let key = b"\x1b[65;30;97;1;0;1_";
        assert_eq!(replies.bytes(key), key);
        assert!(replies.outstanding.is_empty());
    }

    #[test]
    fn non_reply_sequences_and_unsolicited_reports_are_preserved() {
        let mut replies = Replies::new(&[9001]);
        let bytes = b"\x1b[A\x1b[I\x1b[?1004;1$yhello";
        assert_eq!(replies.bytes(bytes), bytes);
    }

    #[test]
    fn unsupported_queries_do_not_hold_a_human_escape_indefinitely() {
        let mut replies = Replies::new(&[9001]);
        assert!(replies.bytes(b"\x1b").is_empty());
        assert_eq!(replies.expire_ambiguous(Instant::now() + Duration::from_millis(61)), b"\x1b");
        assert_eq!(replies.bytes(b"normal"), b"normal");
    }

    #[test]
    fn partial_encoded_reply_survives_expiry_then_continues_without_leaking() {
        let mut replies = Replies::new(&[9001]);
        assert_eq!(replies.bytes(b"q"), b"q");
        assert!(replies.bytes(b"\x1b[?9001;").is_empty());
        assert!(replies.bytes(b"\x1b[0;0;49;").is_empty());
        for elapsed in [61, 150, 500] {
            assert!(replies.expire_ambiguous(Instant::now() + Duration::from_millis(elapsed)).is_empty());
        }
        assert!(replies.bytes(b"1;0;1_").is_empty());
        assert_eq!(replies.bytes(b"$yz"), b"z");
        assert!(replies.outstanding.is_empty());
        assert_eq!(replies.states.get(&9001), Some(&true));
        assert!(replies.finish().is_empty());
    }

    #[test]
    fn partial_encoded_reply_is_discarded_at_finish_preserving_typeahead() {
        let mut replies = Replies::new(&[9001]);
        for byte in b"qz" {
            let record = Replies::to_record(Character { byte: *byte, raw: vec![*byte], record: None });
            let preserved = replies.record(record);
            replies.preserved.extend(preserved);
        }
        assert!(replies.bytes(b"\x1b[?9001;").is_empty());
        assert!(replies.bytes(b"\x1b[0;0;49;").is_empty());
        let records = replies.finish();
        let text: Vec<u16> = records.iter().map(|record| unsafe { record.Event.KeyEvent.uChar.UnicodeChar }).collect();
        assert_eq!(text, vec![b'q' as u16, b'z' as u16]);
        assert!(replies.finish().is_empty());
    }
}
