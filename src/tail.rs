use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const LIMIT: usize = 4096;

pub type SharedTails = Arc<Mutex<Tails>>;

#[derive(Default)]
pub struct Tails {
    stdout: Tail,
    stderr: Tail,
}

impl Tails {
    pub fn append(&mut self, stream: &str, bytes: &[u8]) {
        let tail = if stream == "stdout" {
            &mut self.stdout
        } else {
            &mut self.stderr
        };
        tail.append(bytes);
    }

    pub fn snapshot(&self) -> [(String, bool); 2] {
        [self.stdout.snapshot(), self.stderr.snapshot()]
    }
}

#[derive(Default)]
struct Tail {
    bytes: VecDeque<u8>,
    truncated: bool,
}

impl Tail {
    fn append(&mut self, bytes: &[u8]) {
        if bytes.len() >= LIMIT {
            self.truncated |= !self.bytes.is_empty() || bytes.len() > LIMIT;
            self.bytes.clear();
            self.bytes.extend(&bytes[bytes.len() - LIMIT..]);
        } else {
            let overflow = (self.bytes.len() + bytes.len()).saturating_sub(LIMIT);
            if overflow > 0 {
                self.bytes.drain(..overflow);
                self.truncated = true;
            }
            self.bytes.extend(bytes);
        }
    }

    fn snapshot(&self) -> (String, bool) {
        let bytes: Vec<_> = self.bytes.iter().copied().collect();
        let mut start = 0;
        if self.truncated {
            // The retained suffix may start in the middle of a valid UTF-8
            // character. Discard up to three continuation bytes at that boundary.
            while start < bytes.len().min(3) && bytes[start] & 0xc0 == 0x80 {
                start += 1;
            }
        }
        let mut text = String::from_utf8_lossy(&bytes[start..]).into_owned();
        let mut truncated = self.truncated;
        if text.len() > LIMIT {
            // Lossy decoding can expand each invalid byte to three UTF-8 bytes.
            // Bound the rendered field too and never slice inside a character.
            let mut start = text.len() - LIMIT;
            while !text.is_char_boundary(start) {
                start += 1;
            }
            text = text.split_off(start);
            truncated = true;
        }
        (text, truncated)
    }
}
