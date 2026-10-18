use std::path::PathBuf;

use anyhow::Result;

use crate::document::Document;
use crate::highlight::Language;
use crate::vfs::VirtualFile;

/// A single undoable edit, in the spirit of IntelliJ's CommandProcessor:
/// every change is recorded so it can be inverted. Each keystroke is its
/// own command here; a real CommandProcessor groups a whole user action
/// (e.g. "replace all") into one undo entry.
enum EditCommand {
    Insert { offset: usize, text: String },
    Delete { offset: usize, text: String },
}

/// A rectangular multi-line selection, analogous to IntelliJ's Column
/// Selection Mode: `anchor_*` is where it was entered (or last widened
/// from), `line`/`col` is the active corner that movement keys drive.
/// Edits (insert/backspace/delete) apply at the same column across every
/// line the rectangle spans, rather than at a single caret.
struct ColumnBlock {
    anchor_line: usize,
    anchor_col: usize,
    line: usize,
    col: usize,
}

impl ColumnBlock {
    fn lines(&self) -> (usize, usize) {
        (self.anchor_line.min(self.line), self.anchor_line.max(self.line))
    }

    fn cols(&self) -> (usize, usize) {
        (self.anchor_col.min(self.col), self.anchor_col.max(self.col))
    }
}

#[derive(Default)]
pub struct Caret {
    pub offset: usize,
    pub selection_anchor: Option<usize>,
}

impl Caret {
    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.selection_anchor.map(|a| {
            if a < self.offset {
                (a, self.offset)
            } else {
                (self.offset, a)
            }
        })
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }
}

/// Analogous to IntelliJ's Editor: a view over a Document that adds a
/// caret/selection and offset <-> (line, col) navigation, plus the
/// mutating actions a user triggers through the keyboard.
pub struct Editor {
    pub document: Document,
    pub caret: Caret,
    pub file: Option<VirtualFile>,
    pub viewport_top_line: usize,
    /// `Some` while Column Selection Mode is active (Ctrl/Cmd+8). Lives
    /// alongside `caret` rather than replacing it: `caret.offset` stays
    /// wherever it was when the mode was entered, and is only brought back
    /// in sync (to the block's active corner) on exit.
    column_block: Option<ColumnBlock>,
    undo_stack: Vec<EditCommand>,
    redo_stack: Vec<EditCommand>,
}

impl Editor {
    pub fn new(document: Document, file: Option<VirtualFile>) -> Self {
        Editor {
            document,
            caret: Caret::default(),
            file,
            viewport_top_line: 0,
            column_block: None,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn open(path: PathBuf) -> Result<Self> {
        let file = VirtualFile::new(path);
        let document = file.load()?;
        Ok(Editor::new(document, Some(file)))
    }

    pub fn file_name(&self) -> String {
        match &self.file {
            Some(f) => f.path.display().to_string(),
            None => "[untitled]".to_string(),
        }
    }

    /// The syntax lexer to highlight this buffer with, chosen by the open
    /// file's extension (an untitled buffer gets no highlighting).
    pub fn language(&self) -> Language {
        match &self.file {
            Some(f) => Language::from_path(&f.path),
            None => Language::PlainText,
        }
    }

    pub fn save(&mut self) -> Result<()> {
        if let Some(file) = &self.file {
            file.save(&mut self.document)?;
        }
        Ok(())
    }

    /// The position the UI should render the caret/viewport at: the
    /// column block's active corner while Column Selection Mode is on
    /// (its own line/col, not `caret.offset`), otherwise the normal caret.
    pub fn caret_line_col(&self) -> (usize, usize) {
        match &self.column_block {
            Some(block) => (block.line, block.col),
            None => self.document.offset_to_line_col(self.caret.offset),
        }
    }

    pub fn is_column_mode(&self) -> bool {
        self.column_block.is_some()
    }

    /// For rendering: the block's (min_line, max_line), (min_col, max_col),
    /// and the active corner's column (to draw a caret on every line in
    /// the span, not just the highlighted rectangle).
    pub fn column_block_view(&self) -> Option<((usize, usize), (usize, usize), usize)> {
        self.column_block.as_ref().map(|b| (b.lines(), b.cols(), b.col))
    }

    pub fn toggle_column_mode(&mut self) {
        if self.column_block.is_some() {
            self.exit_column_mode();
        } else {
            self.enter_column_mode();
        }
    }

    fn enter_column_mode(&mut self) {
        let (line, col) = self.caret_line_col();
        self.column_block = Some(ColumnBlock {
            anchor_line: line,
            anchor_col: col,
            line,
            col,
        });
        self.caret.clear_selection();
    }

    /// No-op if Column Selection Mode isn't active - safe to call
    /// unconditionally from anywhere that wants to guarantee it's off
    /// (e.g. before jumping the caret via a different mechanism).
    pub fn exit_column_mode(&mut self) {
        if let Some(block) = self.column_block.take() {
            self.caret.offset = self.document.line_col_to_offset(block.line, block.col);
            self.caret.clear_selection();
        }
    }

    /// Arrow keys in Column Selection Mode: grows/shrinks the rectangle by
    /// moving its active corner, keeping the anchor corner fixed.
    pub fn column_move(&mut self, d_line: isize, d_col: isize) {
        let max_line = self.document.len_lines().saturating_sub(1);
        if let Some(block) = &mut self.column_block {
            if d_line != 0 {
                block.line = (block.line as isize + d_line).clamp(0, max_line as isize) as usize;
            }
            if d_col != 0 {
                block.col = (block.col as isize + d_col).max(0) as usize;
            }
        }
    }

    /// Types `text` at the block's column on every line it spans. If the
    /// block has nonzero width, each line's slice of that column range is
    /// replaced first (block overwrite); a zero-width block just inserts.
    /// A line shorter than the block's column clamps to that line's own
    /// end, so short lines fall out of alignment with the block's column
    /// rather than being padded out to it - a deliberate simplification.
    pub fn column_insert(&mut self, text: &str) -> Result<()> {
        let Some(((min_line, max_line), (min_col, max_col))) =
            self.column_block.as_ref().map(|b| (b.lines(), b.cols()))
        else {
            return Ok(());
        };
        for line in min_line..=max_line {
            let line_len = self.document.line_len(line);
            let del_start = min_col.min(line_len);
            let del_end = max_col.min(line_len);
            if del_end > del_start {
                let start = self.document.line_col_to_offset(line, del_start);
                let end = self.document.line_col_to_offset(line, del_end);
                let removed = self.document.delete(start, end)?;
                self.undo_stack.push(EditCommand::Delete { offset: start, text: removed });
            }
            let insert_col = min_col.min(self.document.line_len(line));
            let offset = self.document.line_col_to_offset(line, insert_col);
            self.document.insert(offset, text)?;
            self.undo_stack.push(EditCommand::Insert { offset, text: text.to_string() });
        }
        self.redo_stack.clear();
        let new_col = min_col + text.chars().count();
        if let Some(block) = &mut self.column_block {
            block.anchor_col = new_col;
            block.col = new_col;
        }
        Ok(())
    }

    /// Backspace in Column Selection Mode: with a nonzero-width block,
    /// deletes the block itself (same as Delete); with a zero-width
    /// block, deletes the character before the column on every line.
    pub fn column_backspace(&mut self) -> Result<()> {
        let Some(((min_line, max_line), (min_col, max_col))) =
            self.column_block.as_ref().map(|b| (b.lines(), b.cols()))
        else {
            return Ok(());
        };
        if min_col < max_col {
            return self.column_delete_block(min_line, max_line, min_col, max_col);
        }
        if min_col == 0 {
            return Ok(());
        }
        for line in min_line..=max_line {
            let line_len = self.document.line_len(line);
            let col = min_col.min(line_len);
            if col == 0 {
                continue;
            }
            let start = self.document.line_col_to_offset(line, col - 1);
            let end = self.document.line_col_to_offset(line, col);
            let removed = self.document.delete(start, end)?;
            self.undo_stack.push(EditCommand::Delete { offset: start, text: removed });
        }
        self.redo_stack.clear();
        let new_col = min_col - 1;
        if let Some(block) = &mut self.column_block {
            block.anchor_col = new_col;
            block.col = new_col;
        }
        Ok(())
    }

    /// Delete (forward) in Column Selection Mode: with a nonzero-width
    /// block, deletes the block itself; with a zero-width block, deletes
    /// the character at the column on every line.
    pub fn column_delete_forward(&mut self) -> Result<()> {
        let Some(((min_line, max_line), (min_col, max_col))) =
            self.column_block.as_ref().map(|b| (b.lines(), b.cols()))
        else {
            return Ok(());
        };
        if min_col < max_col {
            return self.column_delete_block(min_line, max_line, min_col, max_col);
        }
        for line in min_line..=max_line {
            let line_len = self.document.line_len(line);
            if min_col >= line_len {
                continue;
            }
            let start = self.document.line_col_to_offset(line, min_col);
            let end = self.document.line_col_to_offset(line, min_col + 1);
            let removed = self.document.delete(start, end)?;
            self.undo_stack.push(EditCommand::Delete { offset: start, text: removed });
        }
        self.redo_stack.clear();
        Ok(())
    }

    fn column_delete_block(
        &mut self,
        min_line: usize,
        max_line: usize,
        min_col: usize,
        max_col: usize,
    ) -> Result<()> {
        for line in min_line..=max_line {
            let line_len = self.document.line_len(line);
            let start_col = min_col.min(line_len);
            let end_col = max_col.min(line_len);
            if end_col > start_col {
                let start = self.document.line_col_to_offset(line, start_col);
                let end = self.document.line_col_to_offset(line, end_col);
                let removed = self.document.delete(start, end)?;
                self.undo_stack.push(EditCommand::Delete { offset: start, text: removed });
            }
        }
        self.redo_stack.clear();
        if let Some(block) = &mut self.column_block {
            block.anchor_col = min_col;
            block.col = min_col;
        }
        Ok(())
    }

    fn delete_selection_if_any(&mut self) -> Result<bool> {
        if let Some((start, end)) = self.caret.selection_range() {
            let removed = self.document.delete(start, end)?;
            self.undo_stack.push(EditCommand::Delete {
                offset: start,
                text: removed,
            });
            self.redo_stack.clear();
            self.caret.offset = start;
            self.caret.clear_selection();
            return Ok(true);
        }
        Ok(false)
    }

    pub fn selected_text(&self) -> Option<String> {
        self.caret
            .selection_range()
            .map(|(start, end)| self.document.text_range(start, end))
    }

    /// Deletes the current selection and returns the removed text, for
    /// clipboard cut. `None` if there's no selection.
    pub fn cut_selection(&mut self) -> Result<Option<String>> {
        let Some((start, end)) = self.caret.selection_range() else {
            return Ok(None);
        };
        let removed = self.document.delete(start, end)?;
        self.undo_stack.push(EditCommand::Delete {
            offset: start,
            text: removed.clone(),
        });
        self.redo_stack.clear();
        self.caret.offset = start;
        self.caret.clear_selection();
        Ok(Some(removed))
    }

    /// Cuts the entire current line, including its trailing newline (or
    /// up to the document's end, on the last line) - same as IntelliJ's
    /// Cut action when there's no selection: Ctrl/Cmd+X with nothing
    /// selected removes the whole line instead of doing nothing.
    pub fn cut_current_line(&mut self) -> Result<String> {
        let (line, _) = self.caret_line_col();
        let start = self.document.line_col_to_offset(line, 0);
        let end = if line + 1 < self.document.len_lines() {
            self.document.line_col_to_offset(line + 1, 0)
        } else {
            self.document.len_chars()
        };
        let removed = self.document.delete(start, end)?;
        self.undo_stack.push(EditCommand::Delete {
            offset: start,
            text: removed.clone(),
        });
        self.redo_stack.clear();
        self.caret.offset = start;
        self.caret.clear_selection();
        Ok(removed)
    }

    /// Moves the caret to the start of the given 1-based line number,
    /// clamped to the document's line range.
    pub fn goto_line(&mut self, line_number: usize) {
        let target = line_number
            .saturating_sub(1)
            .min(self.document.len_lines().saturating_sub(1));
        self.caret.offset = self.document.line_col_to_offset(target, 0);
        self.caret.clear_selection();
    }

    pub fn insert_text(&mut self, text: &str) -> Result<()> {
        self.delete_selection_if_any()?;
        let offset = self.caret.offset;
        let inserted_len = self.document.insert(offset, text)?;
        self.undo_stack.push(EditCommand::Insert {
            offset,
            text: text.to_string(),
        });
        self.redo_stack.clear();
        self.caret.offset = offset + inserted_len;
        Ok(())
    }

    pub fn backspace(&mut self) -> Result<()> {
        if self.delete_selection_if_any()? {
            return Ok(());
        }
        if self.caret.offset == 0 {
            return Ok(());
        }
        let start = self.caret.offset - 1;
        let end = self.caret.offset;
        let removed = self.document.delete(start, end)?;
        self.undo_stack.push(EditCommand::Delete {
            offset: start,
            text: removed,
        });
        self.redo_stack.clear();
        self.caret.offset = start;
        Ok(())
    }

    pub fn delete_forward(&mut self) -> Result<()> {
        if self.delete_selection_if_any()? {
            return Ok(());
        }
        let start = self.caret.offset;
        let end = (start + 1).min(self.document.len_chars());
        if start == end {
            return Ok(());
        }
        let removed = self.document.delete(start, end)?;
        self.undo_stack.push(EditCommand::Delete {
            offset: start,
            text: removed,
        });
        self.redo_stack.clear();
        Ok(())
    }

    pub fn undo(&mut self) -> Result<()> {
        if let Some(cmd) = self.undo_stack.pop() {
            match cmd {
                EditCommand::Insert { offset, text } => {
                    let len = text.chars().count();
                    self.document.delete(offset, offset + len)?;
                    self.caret.offset = offset;
                    self.redo_stack.push(EditCommand::Insert { offset, text });
                }
                EditCommand::Delete { offset, text } => {
                    self.document.insert(offset, &text)?;
                    self.caret.offset = offset + text.chars().count();
                    self.redo_stack.push(EditCommand::Delete { offset, text });
                }
            }
        }
        Ok(())
    }

    pub fn redo(&mut self) -> Result<()> {
        if let Some(cmd) = self.redo_stack.pop() {
            match cmd {
                EditCommand::Insert { offset, text } => {
                    let len = self.document.insert(offset, &text)?;
                    self.caret.offset = offset + len;
                    self.undo_stack.push(EditCommand::Insert { offset, text });
                }
                EditCommand::Delete { offset, text } => {
                    let len = text.chars().count();
                    self.document.delete(offset, offset + len)?;
                    self.caret.offset = offset;
                    self.undo_stack.push(EditCommand::Delete { offset, text });
                }
            }
        }
        Ok(())
    }

    pub fn move_left(&mut self, extend_selection: bool) {
        self.begin_or_extend(extend_selection);
        if self.caret.offset > 0 {
            self.caret.offset -= 1;
        }
    }

    pub fn move_right(&mut self, extend_selection: bool) {
        self.begin_or_extend(extend_selection);
        if self.caret.offset < self.document.len_chars() {
            self.caret.offset += 1;
        }
    }

    pub fn move_up(&mut self, extend_selection: bool) {
        self.begin_or_extend(extend_selection);
        let (line, col) = self.caret_line_col();
        if line > 0 {
            self.caret.offset = self.document.line_col_to_offset(line - 1, col);
        }
    }

    pub fn move_down(&mut self, extend_selection: bool) {
        self.begin_or_extend(extend_selection);
        let (line, col) = self.caret_line_col();
        if line + 1 < self.document.len_lines() {
            self.caret.offset = self.document.line_col_to_offset(line + 1, col);
        }
    }

    pub fn move_home(&mut self, extend_selection: bool) {
        self.begin_or_extend(extend_selection);
        let (line, _) = self.caret_line_col();
        self.caret.offset = self.document.line_col_to_offset(line, 0);
    }

    pub fn move_end(&mut self, extend_selection: bool) {
        self.begin_or_extend(extend_selection);
        let (line, _) = self.caret_line_col();
        let len = self.document.line_len(line);
        self.caret.offset = self.document.line_col_to_offset(line, len);
    }

    fn begin_or_extend(&mut self, extend_selection: bool) {
        if extend_selection {
            if self.caret.selection_anchor.is_none() {
                self.caret.selection_anchor = Some(self.caret.offset);
            }
        } else {
            self.caret.clear_selection();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    fn editor(text: &str) -> Editor {
        Editor::new(Document::from_str(text), None)
    }

    #[test]
    fn selected_text_and_cut_selection_round_trip() {
        let mut ed = editor("hello world");
        ed.caret.offset = 0;
        ed.caret.selection_anchor = Some(5); // "hello" selected
        assert_eq!(ed.selected_text(), Some("hello".to_string()));
        let cut = ed.cut_selection().unwrap();
        assert_eq!(cut, Some("hello".to_string()));
        assert_eq!(ed.document.text(), " world");
        assert_eq!(ed.caret.offset, 0);
        assert!(ed.caret.selection_anchor.is_none());
    }

    #[test]
    fn cut_selection_is_none_without_selection() {
        let mut ed = editor("hello");
        assert_eq!(ed.selected_text(), None);
        assert_eq!(ed.cut_selection().unwrap(), None);
    }

    #[test]
    fn goto_line_moves_caret_and_clamps() {
        let mut ed = editor("a\nb\nc");
        ed.goto_line(2);
        assert_eq!(ed.caret_line_col(), (1, 0));
        ed.goto_line(999);
        assert_eq!(ed.caret_line_col(), (2, 0));
        ed.goto_line(0);
        assert_eq!(ed.caret_line_col(), (0, 0));
    }

    #[test]
    fn insert_moves_caret_forward() {
        let mut ed = editor("");
        ed.insert_text("hi").unwrap();
        assert_eq!(ed.document.text(), "hi");
        assert_eq!(ed.caret.offset, 2);
    }

    #[test]
    fn backspace_removes_preceding_char() {
        let mut ed = editor("hi");
        ed.caret.offset = 2;
        ed.backspace().unwrap();
        assert_eq!(ed.document.text(), "h");
        assert_eq!(ed.caret.offset, 1);
    }

    #[test]
    fn typing_then_undo_restores_text_and_caret() {
        let mut ed = editor("hello");
        ed.caret.offset = 5;
        ed.insert_text("!").unwrap();
        assert_eq!(ed.document.text(), "hello!");
        ed.undo().unwrap();
        assert_eq!(ed.document.text(), "hello");
        assert_eq!(ed.caret.offset, 5);
    }

    #[test]
    fn undo_then_redo_reapplies_change() {
        let mut ed = editor("hello");
        ed.caret.offset = 5;
        ed.insert_text("!").unwrap();
        ed.undo().unwrap();
        ed.redo().unwrap();
        assert_eq!(ed.document.text(), "hello!");
    }

    #[test]
    fn selection_delete_replaces_range() {
        let mut ed = editor("hello world");
        ed.caret.offset = 0;
        ed.caret.selection_anchor = Some(5); // "hello" selected
        ed.insert_text("bye").unwrap();
        assert_eq!(ed.document.text(), "bye world");
    }

    #[test]
    fn cut_current_line_removes_line_and_newline() {
        let mut ed = editor("one\ntwo\nthree");
        ed.caret.offset = 5; // line 1 ("two"), col 1
        let cut = ed.cut_current_line().unwrap();
        assert_eq!(cut, "two\n");
        assert_eq!(ed.document.text(), "one\nthree");
        assert_eq!(ed.caret_line_col(), (1, 0));
    }

    #[test]
    fn cut_current_line_on_last_line_has_no_trailing_newline() {
        let mut ed = editor("one\ntwo");
        ed.caret.offset = 5; // line 1 ("two")
        let cut = ed.cut_current_line().unwrap();
        assert_eq!(cut, "two");
        assert_eq!(ed.document.text(), "one\n");
    }

    #[test]
    fn toggle_column_mode_enters_and_exits() {
        let mut ed = editor("abc\ndef");
        ed.caret.offset = 1; // line 0, col 1
        assert!(!ed.is_column_mode());
        ed.toggle_column_mode();
        assert!(ed.is_column_mode());
        assert_eq!(ed.caret_line_col(), (0, 1));
        ed.toggle_column_mode();
        assert!(!ed.is_column_mode());
        assert_eq!(ed.caret.offset, 1);
    }

    #[test]
    fn column_mode_insert_types_into_every_selected_line() {
        let mut ed = editor("ab\ncd\nef");
        ed.caret.offset = 1; // line 0, col 1
        ed.toggle_column_mode();
        ed.column_move(1, 0);
        ed.column_move(1, 0); // rectangle now spans lines 0-2, col 1
        ed.column_insert("X").unwrap();
        assert_eq!(ed.document.text(), "aXb\ncXd\neXf");
    }

    #[test]
    fn column_mode_backspace_removes_from_every_selected_line() {
        let mut ed = editor("ab\ncd\nef");
        ed.caret.offset = 1; // col 1
        ed.toggle_column_mode();
        ed.column_move(1, 0);
        ed.column_backspace().unwrap();
        assert_eq!(ed.document.text(), "b\nd\nef");
    }

    #[test]
    fn column_mode_clamps_to_each_lines_own_length() {
        let mut ed = editor("abc\nx\ndefg");
        ed.caret.offset = 2; // line 0, col 2
        ed.toggle_column_mode();
        ed.column_move(2, 0); // rectangle spans lines 0-2, col 2
        ed.column_insert("!").unwrap();
        // "x" is only 1 char long, so col 2 clamps to its end (col 1).
        assert_eq!(ed.document.text(), "ab!c\nx!\nde!fg");
    }

    #[test]
    fn move_down_then_up_preserves_column() {
        let mut ed = editor("ab\ncdef\ng");
        ed.caret.offset = 1; // line 0, col 1
        ed.move_down(false);
        assert_eq!(ed.caret_line_col(), (1, 1));
        ed.move_up(false);
        assert_eq!(ed.caret_line_col(), (0, 1));
    }
}
