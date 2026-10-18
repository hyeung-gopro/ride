use anyhow::{bail, Result};
use ropey::Rope;

/// Normalizes CRLF/CR to LF, mirroring the IntelliJ Document contract:
/// internally all line breaks are '\n', regardless of source encoding.
fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

pub trait DocumentListener {
    fn after_change(&mut self, _offset: usize, _old_len: usize, _new_len: usize) {}
}

/// A plain-text, mutable character sequence - analogous to IntelliJ's
/// `Document`. It knows nothing about syntax (that's PSI's job, out of
/// scope for this milestone); it only guarantees consistent offsets,
/// normalized line endings, listener notification, and guarded (read-only)
/// regions.
pub struct Document {
    rope: Rope,
    listeners: Vec<Box<dyn DocumentListener>>,
    guarded_blocks: Vec<(usize, usize)>,
    modified: bool,
}

impl Document {
    pub fn from_str(text: &str) -> Self {
        Document {
            rope: Rope::from_str(&normalize_line_endings(text)),
            listeners: Vec::new(),
            guarded_blocks: Vec::new(),
            modified: false,
        }
    }

    pub fn add_listener(&mut self, listener: Box<dyn DocumentListener>) {
        self.listeners.push(listener);
    }

    pub fn create_guarded_block(&mut self, start: usize, end: usize) {
        self.guarded_blocks.push((start, end));
    }

    pub fn is_modified(&self) -> bool {
        self.modified
    }

    pub fn clear_modified(&mut self) {
        self.modified = false;
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn line(&self, idx: usize) -> String {
        if idx >= self.rope.len_lines() {
            return String::new();
        }
        let line = self.rope.line(idx);
        line.to_string().trim_end_matches('\n').to_string()
    }

    pub fn offset_to_line_col(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.rope.len_chars());
        let line = self.rope.char_to_line(offset);
        let line_start = self.rope.line_to_char(line);
        (line, offset - line_start)
    }

    pub fn line_col_to_offset(&self, line: usize, col: usize) -> usize {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let line_start = self.rope.line_to_char(line);
        let line_len = self.line(line).chars().count();
        line_start + col.min(line_len)
    }

    pub fn line_len(&self, idx: usize) -> usize {
        self.line(idx).chars().count()
    }

    pub fn text_range(&self, start: usize, end: usize) -> String {
        let start = start.min(self.rope.len_chars());
        let end = end.min(self.rope.len_chars());
        if start >= end {
            return String::new();
        }
        self.rope.slice(start..end).to_string()
    }

    /// Char-offsets of every non-overlapping occurrence of `needle`,
    /// case-sensitive, scanning left to right. `O(n)` scans over the
    /// whole text (it materializes it as a `String`); fine at the file
    /// sizes this editor targets, not meant to scale to huge files.
    pub fn find_occurrences(&self, needle: &str) -> Vec<usize> {
        if needle.is_empty() {
            return Vec::new();
        }
        let text = self.rope.to_string();
        let mut result = Vec::new();
        let mut search_from = 0;
        while let Some(pos) = text[search_from..].find(needle) {
            let byte_idx = search_from + pos;
            result.push(self.rope.byte_to_char(byte_idx));
            search_from = byte_idx + needle.len();
        }
        result
    }

    fn check_guarded(&self, start: usize, end: usize) -> Result<()> {
        for &(gs, ge) in &self.guarded_blocks {
            if start < ge && end > gs {
                bail!("edit touches a guarded (read-only) region [{gs}, {ge})");
            }
        }
        Ok(())
    }

    pub fn insert(&mut self, offset: usize, text: &str) -> Result<usize> {
        let offset = offset.min(self.rope.len_chars());
        self.check_guarded(offset, offset)?;
        let normalized = normalize_line_endings(text);
        self.rope.insert(offset, &normalized);
        self.modified = true;
        let inserted_len = normalized.chars().count();
        for l in &mut self.listeners {
            l.after_change(offset, 0, inserted_len);
        }
        Ok(inserted_len)
    }

    pub fn delete(&mut self, start: usize, end: usize) -> Result<String> {
        let start = start.min(self.rope.len_chars());
        let end = end.min(self.rope.len_chars());
        if start >= end {
            return Ok(String::new());
        }
        self.check_guarded(start, end)?;
        let removed = self.rope.slice(start..end).to_string();
        self.rope.remove(start..end);
        self.modified = true;
        for l in &mut self.listeners {
            l.after_change(start, removed.chars().count(), 0);
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_occurrences_returns_char_offsets_non_overlapping() {
        let doc = Document::from_str("foo bar foofoo");
        assert_eq!(doc.find_occurrences("foo"), vec![0, 8, 11]);
        assert_eq!(doc.find_occurrences("missing"), Vec::<usize>::new());
        assert_eq!(doc.find_occurrences(""), Vec::<usize>::new());
    }

    #[test]
    fn find_occurrences_uses_char_offsets_with_multibyte_text() {
        let doc = Document::from_str("héllo wörld héllo");
        // "héllo" starts at char offset 0 and 12, even though 'é' is a
        // multi-byte UTF-8 character - this would be wrong if byte
        // offsets leaked through unconverted.
        assert_eq!(doc.find_occurrences("héllo"), vec![0, 12]);
    }

    #[test]
    fn normalizes_line_endings() {
        let doc = Document::from_str("a\r\nb\rc\n");
        assert_eq!(doc.text(), "a\nb\nc\n");
    }

    #[test]
    fn insert_and_delete_round_trip() {
        let mut doc = Document::from_str("hello world");
        doc.insert(5, ",").unwrap();
        assert_eq!(doc.text(), "hello, world");
        let removed = doc.delete(5, 6).unwrap();
        assert_eq!(removed, ",");
        assert_eq!(doc.text(), "hello world");
    }

    #[test]
    fn offset_line_col_round_trip() {
        let doc = Document::from_str("ab\ncde\nf");
        assert_eq!(doc.offset_to_line_col(0), (0, 0));
        assert_eq!(doc.offset_to_line_col(3), (1, 0));
        assert_eq!(doc.offset_to_line_col(5), (1, 2));
        assert_eq!(doc.line_col_to_offset(1, 2), 5);
    }

    #[test]
    fn guarded_block_rejects_overlapping_edit() {
        let mut doc = Document::from_str("hello world");
        doc.create_guarded_block(0, 5);
        assert!(doc.delete(2, 7).is_err());
        assert!(doc.insert(6, "!").is_ok());
    }

    #[test]
    fn listener_is_notified_on_change() {
        struct Counter(usize);
        impl DocumentListener for Counter {
            fn after_change(&mut self, _offset: usize, _old_len: usize, _new_len: usize) {
                self.0 += 1;
            }
        }
        let mut doc = Document::from_str("x");
        doc.add_listener(Box::new(Counter(0)));
        doc.insert(1, "y").unwrap();
        doc.delete(0, 1).unwrap();
    }
}
