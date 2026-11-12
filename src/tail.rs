use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const LIMIT: usize = 4096;

pub type SharedTails = Arc<Mutex<Tails>>;

/// A job's bounded output: the tail of each stream, and the head and tail of
/// both streams together in the order they were read.
#[derive(Default)]
pub struct Tails {
    stdout: Tail,
    stderr: Tail,
    head: Vec<u8>,
    /// Combined output after `head`.
    rest: Tail,
    /// Bytes read from both streams.
    total: usize,
}

/// The bounded output of a failed job, as its result reports it.
pub struct Snapshot {
    pub stdout: (String, bool),
    pub stderr: (String, bool),
    pub output: Excerpt,
}

/// The first and last bytes of a job's stdout and stderr together, in the
/// order goodflows read them.
#[derive(Clone, Debug, Serialize)]
pub struct Excerpt {
    #[serde(rename = "output_head")]
    pub head: String,
    /// The output after `head`, or its end when `truncated`.
    #[serde(rename = "output_tail")]
    pub tail: String,
    /// Output between `head` and `tail` was omitted.
    #[serde(rename = "output_truncated")]
    pub truncated: bool,
    /// How many bytes of output were omitted between `head` and `tail`.
    #[serde(rename = "output_omitted_bytes")]
    pub omitted: usize,
}

impl Tails {
    pub fn append(&mut self, stream: &str, mut bytes: &[u8]) {
        let tail = if stream == "stdout" {
            &mut self.stdout
        } else {
            &mut self.stderr
        };
        tail.append(bytes);
        self.total += bytes.len();
        let room = LIMIT - self.head.len();
        let (head, rest) = bytes.split_at(room.min(bytes.len()));
        self.head.extend_from_slice(head);
        bytes = rest;
        if !bytes.is_empty() {
            self.rest.append(bytes);
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            stdout: self.stdout.snapshot(),
            stderr: self.stderr.snapshot(),
            output: self.excerpt(),
        }
    }

    fn excerpt(&self) -> Excerpt {
        if !self.rest.truncated {
            // Head and tail are contiguous: decode them together so a character
            // that spans the boundary survives, then split on a character.
            let mut bytes = self.head.clone();
            bytes.extend(&self.rest.bytes);
            let mut head = String::from_utf8_lossy(&bytes).into_owned();
            let mut tail = head.split_off(floor_char_boundary(&head, LIMIT));
            let mut omitted = 0;
            if tail.len() > LIMIT {
                // Only lossy decoding makes contiguous output this long.
                let kept = tail.split_off(ceil_char_boundary(&tail, tail.len() - LIMIT));
                omitted = tail.len();
                tail = kept;
            }
            return Excerpt {
                head,
                tail,
                truncated: omitted > 0,
                omitted,
            };
        }
        // Drop an incomplete character at the end of the head; its remaining
        // bytes were omitted.
        let end = match std::str::from_utf8(&self.head) {
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            _ => self.head.len(),
        };
        let mut head = String::from_utf8_lossy(&self.head[..end]).into_owned();
        // Lossy decoding can expand each invalid byte to three UTF-8 bytes.
        head.truncate(floor_char_boundary(&head, LIMIT));
        Excerpt {
            head,
            tail: self.rest.snapshot().0,
            truncated: true,
            // The bytes neither `head` nor the retained tail holds.
            omitted: self.total - self.head.len() - self.rest.bytes.len(),
        }
    }
}

/// The largest character boundary of `text` at or below `index`.
fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The smallest character boundary of `text` at or above `index`.
fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index += 1;
    }
    index
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
            text = text.split_off(ceil_char_boundary(&text, text.len() - LIMIT));
            truncated = true;
        }
        (text, truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn excerpt(chunks: &[(&str, &[u8])]) -> Excerpt {
        let mut tails = Tails::default();
        for (stream, bytes) in chunks {
            tails.append(stream, bytes);
        }
        tails.snapshot().output
    }

    #[test]
    fn excerpts_keep_the_head_and_tail_of_both_streams_in_order() {
        let short = excerpt(&[("stderr", b"error: first\n"), ("stdout", b"then out\n")]);
        assert_eq!(short.head, "error: first\nthen out\n");
        assert_eq!((short.tail.as_str(), short.truncated), ("", false));

        // Contiguous: the head and tail together are the whole output.
        let a = "a".repeat(LIMIT - 1);
        let whole = excerpt(&[("stdout", a.as_bytes()), ("stderr", "é-end".as_bytes())]);
        assert_eq!(whole.head, a);
        assert_eq!((whole.tail.as_str(), whole.truncated), ("é-end", false));

        // Too long: the middle is omitted, and a character cut at either edge
        // is dropped rather than replaced.
        let long = excerpt(&[
            ("stderr", format!("{a}é").as_bytes()),
            ("stdout", "x".repeat(2 * LIMIT).as_bytes()),
            ("stdout", "€-end".as_bytes()),
        ]);
        assert_eq!(long.head, a);
        assert_eq!(long.tail, format!("{}€-end", "x".repeat(LIMIT - 7)));
        assert!(long.truncated);
        // 4097 + 8192 + 7 bytes, less the 4096 kept at each end.
        assert_eq!(long.omitted, 4104);
        assert_eq!((short.omitted, whole.omitted), (0, 0));
    }

    #[test]
    fn excerpts_bound_lossy_expansion() {
        let invalid = [0xffu8; 3 * LIMIT];
        let contiguous = excerpt(&[("stdout", &invalid[..LIMIT + 10])]);
        assert_eq!(contiguous.head, "\u{fffd}".repeat(LIMIT / 3));
        assert_eq!(contiguous.tail, "\u{fffd}".repeat(LIMIT / 3));
        assert!(contiguous.truncated);
        // 4106 replacement characters of 3 bytes each, less the 2 * 1365 kept.
        assert_eq!(contiguous.omitted, 3 * (LIMIT + 10 - 2 * (LIMIT / 3)));
        let cut = excerpt(&[("stdout", &invalid)]);
        assert_eq!(cut.head, "\u{fffd}".repeat(LIMIT / 3));
        assert_eq!(cut.tail, "\u{fffd}".repeat(LIMIT / 3));
        assert!(cut.truncated);
    }
}
