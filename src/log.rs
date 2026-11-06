//! Turning a raw stream of terminal output into log lines.
//!
//! A tiny terminal emulator: `\n` commits the current line, `\r` means "the next text overwrites
//! the current line" (progress bars), and ANSI escape sequences are dropped. Text arrives in
//! arbitrary chunks, so every piece of state survives between calls.

/// Keep memory bounded: older lines are discarded.
pub const MAX_LINES: usize = 5000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Escape {
    #[default]
    None,
    /// Saw ESC.
    Started,
    /// Saw ESC `[`: skipping parameters until the final byte.
    Csi,
}

#[derive(Debug, Clone, Default)]
pub struct LogBuffer {
    lines: Vec<String>,
    /// The line being written; it has not seen `\n` yet.
    current: String,
    /// A `\r` was seen: the next visible character starts the line over.
    overwrite_next: bool,
    escape: Escape,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_str(&mut self, text: &str) {
        for c in text.chars() {
            match self.escape {
                Escape::Started => {
                    self.escape = if c == '[' { Escape::Csi } else { Escape::None };
                }
                Escape::Csi => {
                    // A CSI sequence ends with a byte in '@'..='~'.
                    if ('@'..='~').contains(&c) {
                        self.escape = Escape::None;
                    }
                }
                Escape::None => match c {
                    '\x1b' => self.escape = Escape::Started,
                    '\r' => self.overwrite_next = true,
                    '\n' => self.commit(),
                    c => {
                        if self.overwrite_next {
                            self.current.clear();
                            self.overwrite_next = false;
                        }
                        self.current.push(c);
                    }
                },
            }
        }
    }

    /// The process ended: whatever is on the current line is final.
    pub fn finish(&mut self) {
        if !self.current.is_empty() {
            self.commit();
        }
        self.overwrite_next = false;
    }

    fn commit(&mut self) {
        self.lines.push(std::mem::take(&mut self.current));
        self.overwrite_next = false;
        if self.lines.len() > MAX_LINES {
            let excess = self.lines.len() - MAX_LINES;
            self.lines.drain(..excess);
        }
    }

    /// Committed lines followed by the line still being written (if any).
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        let pending = (!self.current.is_empty()).then_some(self.current.as_str());
        self.lines.iter().map(String::as_str).chain(pending)
    }

    pub fn len(&self) -> usize {
        self.lines.len() + usize::from(!self.current.is_empty())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(chunks: &[&str]) -> Vec<String> {
        let mut log = LogBuffer::new();
        for chunk in chunks {
            log.push_str(chunk);
        }
        log.lines().map(str::to_owned).collect()
    }

    #[test]
    fn newline_separates_lines() {
        assert_eq!(lines_of(&["one\ntwo\n"]), ["one", "two"]);
    }

    #[test]
    fn carriage_return_replaces_the_current_line() {
        assert_eq!(
            lines_of(&["progress 20%\rprogress 60%\rprogress 100%\n"]),
            ["progress 100%"]
        );
        // tqdm style: the \r comes first.
        assert_eq!(lines_of(&["\rp1\rp2\rp3\n"]), ["p3"]);
    }

    #[test]
    fn crlf_keeps_the_text() {
        assert_eq!(lines_of(&["hello\r\nworld\r\n"]), ["hello", "world"]);
    }

    #[test]
    fn overwriting_with_shorter_text_leaves_no_tail() {
        assert_eq!(lines_of(&["long line here\rshort\n"]), ["short"]);
    }

    #[test]
    fn state_survives_chunk_boundaries() {
        assert_eq!(lines_of(&["abc\r", "de\n"]), ["de"]);
        assert_eq!(lines_of(&["ab", "c\nd", "ef\n"]), ["abc", "def"]);
    }

    #[test]
    fn ansi_sequences_are_removed_even_when_split() {
        assert_eq!(lines_of(&["\x1b[31mred\x1b[0m done\n"]), ["red done"]);
        assert_eq!(lines_of(&["\x1b[3", "1mred\n"]), ["red"]);
        assert_eq!(lines_of(&["a\x1b[2K\x1b[1Gb\n"]), ["ab"]);
    }

    #[test]
    fn unfinished_line_is_visible_and_finish_commits_it() {
        let mut log = LogBuffer::new();
        log.push_str("done\nworking 50%");
        assert_eq!(log.lines().collect::<Vec<_>>(), ["done", "working 50%"]);
        log.push_str("\rworking 80%");
        assert_eq!(log.lines().last(), Some("working 80%"));
        log.finish();
        assert_eq!(log.lines().collect::<Vec<_>>(), ["done", "working 80%"]);
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn old_lines_are_dropped_beyond_the_cap() {
        let mut log = LogBuffer::new();
        for i in 0..MAX_LINES + 100 {
            log.push_str(&format!("line {i}\n"));
        }
        assert_eq!(log.len(), MAX_LINES);
        assert_eq!(log.lines().next(), Some("line 100"));
    }
}
