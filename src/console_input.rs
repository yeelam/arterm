use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

pub(crate) const MAX_BUFFERED_INPUT: usize = 64 * 1024;
const MAX_WIN32_SEQUENCE: usize = 128;
const ESCAPE_TIMEOUT: Duration = Duration::from_millis(60);
const ESC: u8 = 0x1b;
const GROUP_SEPARATOR: u8 = 0x1d;
const VK_OEM_6: u32 = 0xdd;
const RIGHT_CTRL_PRESSED: u32 = 0x0004;
const LEFT_CTRL_PRESSED: u32 = 0x0008;

#[derive(Default)]
pub(crate) struct ConsoleInput {
    candidate: Vec<u8>,
    candidate_since: Option<Instant>,
    ready: VecDeque<u8>,
}

impl ConsoleInput {
    pub(crate) fn push(&mut self, bytes: &[u8], now: Instant) -> bool {
        self.expire(now);
        for &byte in bytes {
            if byte == GROUP_SEPARATOR {
                self.clear();
                return true;
            }

            if self.candidate.is_empty() {
                if byte == ESC {
                    self.candidate.push(byte);
                    self.candidate_since = Some(now);
                } else {
                    self.ready.push_back(byte);
                }
                continue;
            }

            self.candidate.push(byte);
            match candidate_state(&self.candidate) {
                CandidateState::Pending if self.candidate.len() <= MAX_WIN32_SEQUENCE => {}
                CandidateState::Complete => {
                    if is_ctrl_close_bracket(&self.candidate) {
                        self.clear();
                        return true;
                    }
                    self.flush_candidate();
                }
                CandidateState::Invalid | CandidateState::Pending => self.flush_candidate(),
            }
        }
        false
    }

    pub(crate) fn expire(&mut self, now: Instant) {
        if self
            .candidate_since
            .is_some_and(|started| now.saturating_duration_since(started) >= ESCAPE_TIMEOUT)
        {
            self.flush_candidate();
        }
    }

    pub(crate) fn finish(&mut self) {
        self.flush_candidate();
    }

    pub(crate) fn take(&mut self, limit: usize) -> Option<Vec<u8>> {
        if self.ready.is_empty() {
            return None;
        }
        let count = limit.min(self.ready.len());
        Some(self.ready.drain(..count).collect())
    }

    pub(crate) fn buffered_len(&self) -> usize {
        self.ready.len() + self.candidate.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ready.is_empty() && self.candidate.is_empty()
    }

    fn flush_candidate(&mut self) {
        self.ready.extend(self.candidate.drain(..));
        self.candidate_since = None;
    }

    fn clear(&mut self) {
        self.ready.clear();
        self.candidate.clear();
        self.candidate_since = None;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CandidateState {
    Pending,
    Complete,
    Invalid,
}

fn candidate_state(bytes: &[u8]) -> CandidateState {
    match bytes {
        [ESC] | [ESC, b'['] => CandidateState::Pending,
        [ESC, b'[', body @ ..] => match body.last() {
            Some(b'_')
                if body[..body.len() - 1]
                    .iter()
                    .all(|byte| byte.is_ascii_digit() || *byte == b';') =>
            {
                CandidateState::Complete
            }
            Some(byte) if byte.is_ascii_digit() || *byte == b';' => CandidateState::Pending,
            _ => CandidateState::Invalid,
        },
        _ => CandidateState::Invalid,
    }
}

fn is_ctrl_close_bracket(bytes: &[u8]) -> bool {
    let Some(body) = bytes
        .strip_prefix(b"\x1b[")
        .and_then(|value| value.strip_suffix(b"_"))
    else {
        return false;
    };
    let mut fields = [0u32; 6];
    fields[5] = 1;
    let mut count = 0;
    for (index, value) in body.split(|byte| *byte == b';').enumerate() {
        if index >= fields.len() {
            return false;
        }
        count = index + 1;
        if !value.is_empty() {
            let Ok(text) = std::str::from_utf8(value) else {
                return false;
            };
            let Ok(number) = text.parse() else {
                return false;
            };
            fields[index] = number;
        }
    }
    count >= 4
        && fields[0] == VK_OEM_6
        && fields[2] == GROUP_SEPARATOR as u32
        && fields[3] == 1
        && fields[4] & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_group_separator_detaches_without_forwarding_its_chunk() {
        let mut input = ConsoleInput::default();
        assert!(input.push(b"abc\x1ddef", Instant::now()));
        assert!(input.take(4096).is_none());
    }

    #[test]
    fn win32_ctrl_oem6_press_detaches_but_release_is_preserved() {
        let mut input = ConsoleInput::default();
        assert!(input.push(b"\x1b[221;27;29;1;8;1_", Instant::now()));
        assert!(input.take(4096).is_none());

        let release = b"\x1b[221;27;29;0;8;1_";
        assert!(!input.push(release, Instant::now()));
        assert_eq!(input.take(4096).as_deref(), Some(release.as_slice()));
    }

    #[test]
    fn win32_detach_accepts_either_control_key_flag() {
        for state in [LEFT_CTRL_PRESSED, RIGHT_CTRL_PRESSED] {
            let mut input = ConsoleInput::default();
            let sequence = format!("\x1b[221;27;29;1;{state};1_");
            assert!(input.push(sequence.as_bytes(), Instant::now()));
        }
    }

    #[test]
    fn regular_keys_utf8_and_control_sequences_are_unchanged() {
        let mut input = ConsoleInput::default();
        let bytes = b"hello \xe2\x98\x83\r\n\x1b[A\x1b[65;30;97;1;0;1_";
        assert!(!input.push(bytes, Instant::now()));
        assert_eq!(input.take(4096).as_deref(), Some(bytes.as_slice()));
    }

    #[test]
    fn fragmented_win32_sequence_is_recognized() {
        let mut input = ConsoleInput::default();
        let started = Instant::now();
        assert!(!input.push(b"\x1b[221;27;", started));
        assert!(input.take(4096).is_none());
        assert!(input.push(b"29;1;8;1_", started + Duration::from_millis(10)));
    }

    #[test]
    fn incomplete_escape_is_released_after_timeout() {
        let mut input = ConsoleInput::default();
        let started = Instant::now();
        assert!(!input.push(b"\x1b[221;", started));
        input.expire(started + ESCAPE_TIMEOUT);
        assert_eq!(input.take(4096).as_deref(), Some(b"\x1b[221;".as_slice()));
    }

    #[test]
    fn oversized_candidate_is_forwarded_unchanged() {
        let mut input = ConsoleInput::default();
        let mut bytes = b"\x1b[".to_vec();
        bytes.extend(std::iter::repeat_n(b'1', MAX_WIN32_SEQUENCE));
        assert!(!input.push(&bytes, Instant::now()));
        assert_eq!(input.take(4096).as_deref(), Some(bytes.as_slice()));
    }
}
