use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;

use crate::document::Document;
use crate::editor::Editor;
use crate::tree::FileTree;

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Focus {
    Tree,
    Editor,
}

pub struct App {
    pub editor: Editor,
    pub tree: FileTree,
    pub focus: Focus,
    pub sidebar_visible: bool,
    pub tree_scroll_top: usize,
    /// Screen rects from the last render, used to hit-test mouse clicks.
    pub tree_panel_area: Option<Rect>,
    pub tree_rows_area: Option<Rect>,
    pub editor_panel_area: Option<Rect>,
    pub editor_text_area: Option<Rect>,
    /// `None` if the OS clipboard couldn't be opened (e.g. no display
    /// server); copy/cut/paste then silently no-op instead of failing.
    clipboard: Option<arboard::Clipboard>,
    /// `Some(digits)` while the "Go to Line" prompt is open and capturing
    /// input; `None` otherwise. Modal: while open, all keys route to it
    /// regardless of `focus`.
    pub goto_line_input: Option<String>,
    /// `Some(query)` while incremental search is open. Modal, same as
    /// `goto_line_input`; the two are mutually exclusive.
    pub find_input: Option<String>,
    /// Offset the current search started from (or last jumped to), used
    /// to find "the next occurrence from here" rather than always
    /// restarting from the top of the document.
    find_anchor: usize,
    /// The last search query, kept after the find prompt closes so n/N
    /// can repeat it (vim's "/pattern<Enter>" then "n" convention).
    last_query: Option<String>,
    /// True right after confirming a search (Enter/Down/Up in the find
    /// prompt); while true, a bare 'n'/'N' repeats the search instead of
    /// inserting that letter. Cleared the instant any other key is
    /// pressed, so typing the letter "n" normally is never blocked - this
    /// editor has no vim-style normal/insert mode to hang a real binding
    /// on, so this short-lived window is the closest safe approximation.
    pub search_nav_active: bool,
    /// When the first Esc of a quit was pressed, if a second is still
    /// pending. Requiring two presses avoids an accidental quit from a
    /// single stray Esc (e.g. meant to cancel something else).
    esc_armed_at: Option<Instant>,
    /// `Some(name)` while the "New File" prompt is open and capturing the
    /// filename. Modal, same as `goto_line_input`/`find_input`.
    pub new_file_input: Option<String>,
    /// Directory the new file will be created in, captured when the
    /// prompt opens (the selected tree node if it's a directory, else its
    /// parent) - fixed for the lifetime of that one prompt even if the
    /// selection somehow changed while it's open.
    new_file_dir: PathBuf,
    /// `Some(query)` while the "Open File" prompt is open. Modal, same as
    /// the other prompts.
    pub open_file_input: Option<String>,
    /// Every file under the tree root, walked once when the prompt opens
    /// (not re-walked per keystroke - only the filter below re-runs).
    open_file_candidates: Vec<PathBuf>,
    /// `open_file_candidates` filtered by the current query (case-
    /// insensitive substring match on the path relative to the root).
    open_file_matches: Vec<PathBuf>,
    /// Index into `open_file_matches` currently highlighted.
    open_file_selected: usize,
}

impl App {
    const DOUBLE_ESC_WINDOW: Duration = Duration::from_millis(1000);
    pub fn new(root: PathBuf, initial_file: Option<PathBuf>) -> Result<Self> {
        let mut tree = FileTree::new(root.clone());
        let editor = match &initial_file {
            Some(p) => Editor::open(p.clone())?,
            None => Editor::new(Document::from_str(""), None),
        };
        if let Some(p) = &initial_file {
            if p.parent() == Some(root.as_path()) {
                tree.selected_path = p.clone();
            }
        }
        Ok(App {
            editor,
            tree,
            focus: Focus::Editor,
            sidebar_visible: true,
            tree_scroll_top: 0,
            tree_panel_area: None,
            tree_rows_area: None,
            editor_panel_area: None,
            editor_text_area: None,
            clipboard: arboard::Clipboard::new().ok(),
            goto_line_input: None,
            find_input: None,
            find_anchor: 0,
            last_query: None,
            search_nav_active: false,
            esc_armed_at: None,
            new_file_input: None,
            new_file_dir: PathBuf::new(),
            open_file_input: None,
            open_file_candidates: Vec::new(),
            open_file_matches: Vec::new(),
            open_file_selected: 0,
        })
    }

    pub fn open_goto_line(&mut self) {
        self.find_input = None;
        self.new_file_input = None;
        self.open_file_input = None;
        self.editor.exit_column_mode();
        self.goto_line_input = Some(String::new());
    }

    pub fn confirm_goto_line(&mut self) {
        if let Some(buf) = self.goto_line_input.take() {
            if let Ok(line) = buf.parse::<usize>() {
                self.editor.goto_line(line);
            }
            self.focus = Focus::Editor;
        }
    }

    pub fn cancel_goto_line(&mut self) {
        self.goto_line_input = None;
    }

    pub fn open_find(&mut self) {
        self.goto_line_input = None;
        self.new_file_input = None;
        self.open_file_input = None;
        self.editor.exit_column_mode();
        self.find_anchor = self.editor.caret.offset;
        self.find_input = Some(String::new());
        self.search_nav_active = false;
        self.focus = Focus::Editor;
    }

    /// Esc: cancel outright, no lingering n/N repeat.
    pub fn close_find(&mut self) {
        self.find_input = None;
        self.search_nav_active = false;
    }

    /// Enter/Down/Up on the find prompt: lock in the query, close the
    /// prompt (vim returns to normal mode here), and open the n/N repeat
    /// window.
    pub fn confirm_find(&mut self) {
        self.find_input = None;
        self.search_nav_active = self.last_query.is_some();
    }

    /// Re-runs the search for the current `find_input` buffer and jumps
    /// to the first occurrence at or after `find_anchor` (wrapping to the
    /// document's first match if none follows it). Called on every
    /// keystroke while the find prompt is open, for live/incremental feel.
    pub fn update_find(&mut self) {
        let Some(query) = self.find_input.clone() else {
            return;
        };
        if query.is_empty() {
            self.editor.caret.clear_selection();
            return;
        }
        self.last_query = Some(query.clone());
        let matches = self.editor.document.find_occurrences(&query);
        let Some(&target) = matches.iter().find(|&&m| m >= self.find_anchor).or(matches.first()) else {
            self.editor.caret.clear_selection();
            return;
        };
        self.jump_to_match(target, query.chars().count());
    }

    /// Next/previous match for the active query - the find prompt's
    /// buffer while it's open, or the last confirmed query afterward (for
    /// n/N repeat).
    pub fn find_next(&mut self) {
        self.step_match(true);
    }

    pub fn find_prev(&mut self) {
        self.step_match(false);
    }

    fn step_match(&mut self, forward: bool) {
        let Some(query) = self.find_input.clone().or_else(|| self.last_query.clone()) else {
            return;
        };
        if query.is_empty() {
            return;
        }
        let matches = self.editor.document.find_occurrences(&query);
        if matches.is_empty() {
            return;
        }
        let current = self.editor.caret.selection_range().map(|(s, _)| s).unwrap_or(self.editor.caret.offset);
        let target = if forward {
            matches.iter().find(|&&m| m > current).or(matches.first())
        } else {
            matches.iter().rev().find(|&&m| m < current).or(matches.last())
        };
        if let Some(&target) = target {
            self.jump_to_match(target, query.chars().count());
        }
    }

    fn jump_to_match(&mut self, start: usize, len: usize) {
        self.find_anchor = start;
        self.editor.caret.selection_anchor = Some(start);
        self.editor.caret.offset = start + len;
    }

    pub fn copy_selection(&mut self) {
        if let Some(text) = self.editor.selected_text() {
            if let Some(cb) = &mut self.clipboard {
                let _ = cb.set_text(text);
            }
        }
    }

    /// Ctrl/Cmd+X: cuts the selection, or - with nothing selected - the
    /// entire current line, matching IntelliJ's own Cut action.
    pub fn cut_selection(&mut self) -> Result<()> {
        let text = if self.editor.caret.selection_range().is_some() {
            self.editor.cut_selection()?
        } else {
            Some(self.editor.cut_current_line()?)
        };
        if let Some(text) = text {
            if let Some(cb) = &mut self.clipboard {
                let _ = cb.set_text(text);
            }
        }
        Ok(())
    }

    pub fn paste(&mut self) -> Result<()> {
        if let Some(cb) = &mut self.clipboard {
            if let Ok(text) = cb.get_text() {
                self.editor.insert_text(&text)?;
            }
        }
        Ok(())
    }

    /// Ctrl+N: prompts for a filename, created inside the selected tree
    /// node's directory (the node itself if it's a directory, its parent
    /// if it's a file).
    pub fn open_new_file(&mut self) {
        self.goto_line_input = None;
        self.find_input = None;
        self.open_file_input = None;
        self.editor.exit_column_mode();
        self.new_file_dir = match self.tree.selected_node() {
            Some(node) if node.is_dir => node.path.clone(),
            Some(node) => node
                .path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| self.tree.root.path.clone()),
            None => self.tree.root.path.clone(),
        };
        self.new_file_input = Some(String::new());
    }

    pub fn new_file_dir(&self) -> &std::path::Path {
        &self.new_file_dir
    }

    /// Enter on the new-file prompt: creates the file on disk, opens it in
    /// the editor, and moves focus there - same as activating it from the
    /// tree. A blank name cancels instead of creating nothing.
    pub fn confirm_new_file(&mut self) -> Result<()> {
        if let Some(name) = self.new_file_input.take() {
            let name = name.trim();
            if !name.is_empty() {
                let path = self.tree.create_file(&self.new_file_dir, name)?;
                self.editor = Editor::open(path)?;
                self.focus = Focus::Editor;
            }
        }
        Ok(())
    }

    pub fn cancel_new_file(&mut self) {
        self.new_file_input = None;
    }

    /// Ctrl+O: prompts for a filename and live-filters every file under
    /// the tree root as you type - IntelliJ's "Go to File" (Cmd+Shift+O),
    /// minus the fuzzy scoring (plain case-insensitive substring match on
    /// the relative path is enough at this project's scale).
    pub fn open_open_file(&mut self) {
        self.goto_line_input = None;
        self.find_input = None;
        self.new_file_input = None;
        self.editor.exit_column_mode();
        self.open_file_candidates = Self::walk_files(&self.tree.root.path);
        self.open_file_selected = 0;
        self.open_file_input = Some(String::new());
        self.update_open_file_matches();
    }

    /// Recursively lists every file (not directory) under `root`, skipping
    /// a few directories that are huge and never worth opening by name.
    fn walk_files(root: &std::path::Path) -> Vec<PathBuf> {
        const SKIP: &[&str] = &[".git", "target", "node_modules"];
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_dir() {
                    let name = entry.file_name();
                    if !SKIP.iter().any(|s| name == std::ffi::OsStr::new(s)) {
                        stack.push(path);
                    }
                } else {
                    out.push(path);
                }
            }
        }
        out.sort();
        out
    }

    /// Re-filters `open_file_candidates` against the current query. Called
    /// on every keystroke while the prompt is open, for live/incremental
    /// feel (same pattern as `update_find`).
    pub fn update_open_file_matches(&mut self) {
        let Some(query) = &self.open_file_input else {
            return;
        };
        let query = query.to_lowercase();
        let root = &self.tree.root.path;
        self.open_file_matches = self
            .open_file_candidates
            .iter()
            .filter(|p| {
                p.strip_prefix(root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_lowercase()
                    .contains(&query)
            })
            .cloned()
            .collect();
        self.open_file_selected = self
            .open_file_selected
            .min(self.open_file_matches.len().saturating_sub(1));
    }

    pub fn open_file_matches(&self) -> &[PathBuf] {
        &self.open_file_matches
    }

    pub fn open_file_selected(&self) -> usize {
        self.open_file_selected
    }

    pub fn open_file_root(&self) -> &std::path::Path {
        &self.tree.root.path
    }

    pub fn move_open_file_selection(&mut self, delta: isize) {
        if self.open_file_matches.is_empty() {
            return;
        }
        let len = self.open_file_matches.len() as isize;
        let cur = self.open_file_selected as isize;
        self.open_file_selected = (cur + delta).clamp(0, len - 1) as usize;
    }

    /// Enter on the open-file prompt: opens the highlighted match in the
    /// editor and closes the prompt. A query with no matches just closes
    /// it with nothing opened.
    pub fn confirm_open_file(&mut self) -> Result<()> {
        if let Some(path) = self.open_file_matches.get(self.open_file_selected).cloned() {
            self.editor = Editor::open(path.clone())?;
            if path.parent() == Some(self.tree.root.path.as_path()) {
                self.tree.selected_path = path;
            }
            self.focus = Focus::Editor;
        }
        self.open_file_input = None;
        Ok(())
    }

    pub fn cancel_open_file(&mut self) {
        self.open_file_input = None;
    }

    /// Call on every Esc press outside a modal prompt. Returns true if
    /// this is the confirming second press (within `DOUBLE_ESC_WINDOW` of
    /// the first) and the app should quit; otherwise arms the window and
    /// returns false.
    pub fn confirm_quit_on_esc(&mut self) -> bool {
        if let Some(armed_at) = self.esc_armed_at {
            if armed_at.elapsed() < Self::DOUBLE_ESC_WINDOW {
                self.esc_armed_at = None;
                return true;
            }
        }
        self.esc_armed_at = Some(Instant::now());
        false
    }

    /// Call on any key other than Esc, so a stray Esc much earlier can't
    /// combine with an unrelated later one.
    pub fn clear_esc_arm(&mut self) {
        self.esc_armed_at = None;
    }

    pub fn esc_armed(&self) -> bool {
        self.esc_armed_at.is_some()
    }

    /// Mirrors IntelliJ's tool-window shortcuts (e.g. Cmd+1 for Project):
    /// jumps focus to the tree, showing it first if it's hidden; pressing
    /// again while it's already focused hides it and returns focus to the
    /// editor.
    pub fn toggle_sidebar(&mut self) {
        if self.sidebar_visible && self.focus == Focus::Tree {
            self.sidebar_visible = false;
            self.focus = Focus::Editor;
        } else {
            self.sidebar_visible = true;
            self.focus = Focus::Tree;
        }
    }

    /// Enter on the tree: open the selected file, or toggle the selected
    /// directory's expansion.
    pub fn activate_selected(&mut self) -> Result<()> {
        let Some(node) = self.tree.selected_node() else {
            return Ok(());
        };
        if node.is_dir {
            self.tree.toggle_selected();
        } else {
            let path = node.path.clone();
            self.editor = Editor::open(path)?;
            self.focus = Focus::Editor;
        }
        Ok(())
    }
}
