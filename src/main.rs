mod app;
mod document;
mod editor;
mod highlight;
mod tree;
mod ui;
mod vfs;

use std::io;
use std::path::PathBuf;

use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use app::{App, Focus};
use editor::Editor;

fn main() -> Result<()> {
    let arg = std::env::args().nth(1).map(PathBuf::from);
    let (root, initial_file) = match arg {
        Some(p) if p.is_dir() => (p, None),
        Some(p) => {
            let root = p
                .parent()
                .map(|parent| if parent.as_os_str().is_empty() { PathBuf::from(".") } else { parent.to_path_buf() })
                .unwrap_or_else(|| PathBuf::from("."));
            (root, Some(p))
        }
        None => (std::env::current_dir()?, None),
    };
    let app = App::new(root, initial_file)?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    // Cmd (Super) is only ever visible to us if the terminal speaks this
    // escape-code protocol (Kitty, WezTerm, some iTerm2 setups) - stock
    // Terminal.app does not, and intercepts most Cmd+key combos itself
    // before they reach us regardless. Ctrl always works as a fallback.
    let cmd_supported = supports_keyboard_enhancement().unwrap_or(false);
    if cmd_supported {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, app);

    if cmd_supported {
        execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    }
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, mut app: App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                let shift = key.modifiers.contains(KeyModifiers::SHIFT);
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                let cmd = key.modifiers.contains(KeyModifiers::SUPER);
                let modkey = ctrl || cmd;

                if app.goto_line_input.is_some() {
                    handle_goto_line_key(&mut app, key.code);
                    continue;
                }
                if app.find_input.is_some() {
                    handle_find_key(&mut app, key.code, shift);
                    continue;
                }
                if app.new_file_input.is_some() {
                    handle_new_file_key(&mut app, key.code)?;
                    continue;
                }
                if app.open_file_input.is_some() {
                    handle_open_file_key(&mut app, key.code)?;
                    continue;
                }

                // Vim's "n/N repeat last search" - only right after a
                // confirmed search (see `search_nav_active`'s doc comment
                // for why this can't just be a permanent 'n' binding).
                if app.search_nav_active {
                    if !modkey {
                        match key.code {
                            KeyCode::Char('n') => {
                                app.find_next();
                                continue;
                            }
                            KeyCode::Char('N') => {
                                app.find_prev();
                                continue;
                            }
                            _ => app.search_nav_active = false,
                        }
                    } else {
                        app.search_nav_active = false;
                    }
                }

                if key.code != KeyCode::Esc {
                    app.clear_esc_arm();
                }

                // Global keys, regardless of which panel has focus.
                match key.code {
                    KeyCode::Char('q') if ctrl => break,
                    KeyCode::Esc => {
                        if app.confirm_quit_on_esc() {
                            break;
                        }
                        continue;
                    }
                    KeyCode::Char('b') if ctrl => {
                        app.toggle_sidebar();
                        continue;
                    }
                    KeyCode::Char('g') | KeyCode::Char('l') if modkey => {
                        app.open_goto_line();
                        continue;
                    }
                    KeyCode::Char('f') if modkey => {
                        app.open_find();
                        continue;
                    }
                    KeyCode::Char('n') if modkey => {
                        app.open_new_file();
                        continue;
                    }
                    KeyCode::Char('o') if modkey => {
                        app.open_open_file();
                        continue;
                    }
                    // IntelliJ's own default-Mac shortcut for Column
                    // Selection Mode is Cmd+Shift+8; this app keeps the
                    // plain "8" key, Ctrl or Cmd, like its other bindings.
                    KeyCode::Char('8') if modkey => {
                        app.editor.toggle_column_mode();
                        continue;
                    }
                    _ => {}
                }

                match app.focus {
                    Focus::Tree => handle_tree_key(&mut app, key.code)?,
                    Focus::Editor => handle_editor_key(&mut app, key.code, shift, modkey)?,
                }
            }
            Event::Mouse(mouse) => {
                if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                    handle_click(&mut app, mouse.column, mouse.row);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn point_in_rect(x: u16, y: u16, rect: Rect) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

fn handle_click(app: &mut App, x: u16, y: u16) {
    if let Some(area) = app.tree_panel_area {
        if point_in_rect(x, y, area) {
            app.focus = Focus::Tree;
            if let Some(rows_area) = app.tree_rows_area {
                if point_in_rect(x, y, rows_area) {
                    let row_idx = app.tree_scroll_top + (y - rows_area.y) as usize;
                    app.tree.select_row(row_idx);
                }
            }
            return;
        }
    }
    if let Some(area) = app.editor_panel_area {
        if point_in_rect(x, y, area) {
            app.focus = Focus::Editor;
            if let Some(text_area) = app.editor_text_area {
                if point_in_rect(x, y, text_area) {
                    place_caret_at_click(app, x, y, text_area);
                }
            }
        }
    }
}

fn place_caret_at_click(app: &mut App, x: u16, y: u16, text_area: Rect) {
    let doc = &app.editor.document;
    let gutter = ui::gutter_width(doc.len_lines());
    let line = (app.editor.viewport_top_line + (y - text_area.y) as usize)
        .min(doc.len_lines().saturating_sub(1));
    let col = (x as isize - text_area.x as isize - gutter as isize)
        .max(0) as usize;
    let col = col.min(doc.line_len(line));
    app.editor.caret.offset = doc.line_col_to_offset(line, col);
    app.editor.caret.clear_selection();
}

fn handle_find_key(app: &mut App, code: KeyCode, shift: bool) {
    match code {
        KeyCode::Char(c) => {
            if let Some(buf) = &mut app.find_input {
                buf.push(c);
            }
            app.update_find();
        }
        KeyCode::Backspace => {
            if let Some(buf) = &mut app.find_input {
                buf.pop();
            }
            app.update_find();
        }
        KeyCode::Enter | KeyCode::Down if shift => {
            app.find_prev();
            app.confirm_find();
        }
        KeyCode::Enter | KeyCode::Down => {
            app.find_next();
            app.confirm_find();
        }
        KeyCode::Up => {
            app.find_prev();
            app.confirm_find();
        }
        KeyCode::Esc => app.close_find(),
        _ => {}
    }
}

fn handle_new_file_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char(c) => {
            if let Some(buf) = &mut app.new_file_input {
                buf.push(c);
            }
        }
        KeyCode::Backspace => {
            if let Some(buf) = &mut app.new_file_input {
                buf.pop();
            }
        }
        KeyCode::Enter => app.confirm_new_file()?,
        KeyCode::Esc => app.cancel_new_file(),
        _ => {}
    }
    Ok(())
}

fn handle_open_file_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Char(c) => {
            if let Some(buf) = &mut app.open_file_input {
                buf.push(c);
            }
            app.update_open_file_matches();
        }
        KeyCode::Backspace => {
            if let Some(buf) = &mut app.open_file_input {
                buf.pop();
            }
            app.update_open_file_matches();
        }
        KeyCode::Down => app.move_open_file_selection(1),
        KeyCode::Up => app.move_open_file_selection(-1),
        KeyCode::Enter => app.confirm_open_file()?,
        KeyCode::Esc => app.cancel_open_file(),
        _ => {}
    }
    Ok(())
}

fn handle_goto_line_key(app: &mut App, code: KeyCode) {
    match code {
        KeyCode::Char(c) if c.is_ascii_digit() => {
            if let Some(buf) = &mut app.goto_line_input {
                buf.push(c);
            }
        }
        KeyCode::Backspace => {
            if let Some(buf) = &mut app.goto_line_input {
                buf.pop();
            }
        }
        KeyCode::Enter => app.confirm_goto_line(),
        KeyCode::Esc => app.cancel_goto_line(),
        _ => {}
    }
}

/// Column Selection Mode's own key dispatch, swapped in for the normal
/// one while active (see `handle_editor_key`). Arrows grow/shrink the
/// rectangle instead of moving a single caret; typing, Backspace, and
/// Delete apply to every line it spans at once. Enter and Home/End are
/// left unhandled - a per-line newline/line-start/line-end in a block
/// edit has no single sensible meaning here, so they're simply no-ops;
/// Ctrl+8 to leave the mode first if you need them.
fn handle_column_mode_key(editor: &mut Editor, code: KeyCode, modkey: bool) -> Result<()> {
    match code {
        KeyCode::Up => editor.column_move(-1, 0),
        KeyCode::Down => editor.column_move(1, 0),
        KeyCode::Left => editor.column_move(0, -1),
        KeyCode::Right => editor.column_move(0, 1),
        KeyCode::Char('s') if modkey => editor.save()?,
        KeyCode::Char('z') if modkey => editor.undo()?,
        KeyCode::Char('y') if modkey => editor.redo()?,
        KeyCode::Char(c) if !modkey => editor.column_insert(&c.to_string())?,
        KeyCode::Tab => editor.column_insert("    ")?,
        KeyCode::Backspace => editor.column_backspace()?,
        KeyCode::Delete => editor.column_delete_forward()?,
        _ => {}
    }
    Ok(())
}

fn handle_tree_key(app: &mut App, code: KeyCode) -> Result<()> {
    match code {
        KeyCode::Up => app.tree.move_selection(-1),
        KeyCode::Down => app.tree.move_selection(1),
        KeyCode::Right => app.tree.expand_selected(),
        KeyCode::Left => app.tree.collapse_selected(),
        KeyCode::Enter => app.activate_selected()?,
        KeyCode::Tab => app.focus = Focus::Editor,
        _ => {}
    }
    Ok(())
}

fn handle_editor_key(app: &mut App, code: KeyCode, shift: bool, modkey: bool) -> Result<()> {
    if app.editor.is_column_mode() {
        return handle_column_mode_key(&mut app.editor, code, modkey);
    }

    match code {
        KeyCode::Char('c') if modkey => {
            app.copy_selection();
            return Ok(());
        }
        KeyCode::Char('x') if modkey => return app.cut_selection(),
        KeyCode::Char('v') if modkey => return app.paste(),
        // Vim-style ": then a line number" - only when the caret is at the
        // start of an empty line, so ':' still inserts normally anywhere
        // it'd otherwise be real typing (this editor has no modal
        // insert/normal split like vim's, so there's no other way to tell
        // "command" apart from "text" for an already-typed-on line).
        KeyCode::Char(':') if !modkey => {
            let (line, col) = app.editor.caret_line_col();
            if col == 0 && app.editor.document.line_len(line) == 0 {
                app.open_goto_line();
                return Ok(());
            }
        }
        // Vim-style "/" to search - same empty-line-only heuristic as
        // ':' above. Note this one bites harder: a bare "//" comment
        // starting a fresh blank line (common in Rust/C/JS) will have its
        // first '/' open find instead of typing the comment. Esc backs
        // out, or use Ctrl+F/Cmd+F, which always inserts safely.
        KeyCode::Char('/') if !modkey => {
            let (line, col) = app.editor.caret_line_col();
            if col == 0 && app.editor.document.line_len(line) == 0 {
                app.open_find();
                return Ok(());
            }
        }
        _ => {}
    }

    let editor = &mut app.editor;
    match code {
        KeyCode::Char('s') if modkey => editor.save()?,
        // Cmd+Shift+Z (Mac convention) and Ctrl+Y (terminal convention) both redo.
        KeyCode::Char('z') if modkey && shift => editor.redo()?,
        KeyCode::Char('z') if modkey => editor.undo()?,
        KeyCode::Char('y') if modkey => editor.redo()?,
        KeyCode::Char(c) if !modkey => editor.insert_text(&c.to_string())?,
        KeyCode::Enter => editor.insert_text("\n")?,
        KeyCode::Tab => editor.insert_text("    ")?,
        KeyCode::Backspace => editor.backspace()?,
        KeyCode::Delete => editor.delete_forward()?,
        KeyCode::Left => editor.move_left(shift),
        KeyCode::Right => editor.move_right(shift),
        KeyCode::Up => editor.move_up(shift),
        KeyCode::Down => editor.move_down(shift),
        KeyCode::Home => editor.move_home(shift),
        KeyCode::End => editor.move_end(shift),
        _ => {}
    }
    Ok(())
}
