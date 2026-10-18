use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, Focus};
use crate::editor::Editor;
use crate::highlight;

const SIDEBAR_WIDTH: u16 = 30;

pub fn gutter_width(line_count: usize) -> usize {
    line_count.to_string().len().max(3) + 1
}

fn build_status_prompt(app: &App) -> Option<String> {
    if let Some(digits) = &app.goto_line_input {
        return Some(format!(":{digits}_  (Enter to jump, Esc to cancel)"));
    }
    if let Some(query) = &app.find_input {
        let info = if query.is_empty() {
            String::new()
        } else {
            let matches = app.editor.document.find_occurrences(query);
            if matches.is_empty() {
                "  (no matches)".to_string()
            } else {
                format!("  ({} match{})", matches.len(), if matches.len() == 1 { "" } else { "es" })
            }
        };
        return Some(format!(
            "/{query}_{info}  (Enter/Down next, Shift+Enter/Up prev, Esc cancel)"
        ));
    }
    if let Some(name) = &app.new_file_input {
        let dir = app.new_file_dir();
        let dir_label = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| dir.display().to_string());
        return Some(format!(
            "New file in {dir_label}/: {name}_  (Enter to create, Esc to cancel)"
        ));
    }
    if app.esc_armed() {
        return Some("Press Esc again to quit (or keep working to cancel)".to_string());
    }
    None
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let status_prompt = build_status_prompt(app);
    let search_nav_active = app.search_nav_active;
    if app.sidebar_visible {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(1)])
            .split(area);
        app.tree_panel_area = Some(cols[0]);
        app.editor_panel_area = Some(cols[1]);
        draw_tree(frame, app, cols[0]);
        let focused = app.focus == Focus::Editor;
        draw_editor_area(
            frame,
            &mut app.editor,
            cols[1],
            focused,
            &mut app.editor_text_area,
            status_prompt.as_deref(),
            search_nav_active,
        );
    } else {
        app.tree_panel_area = None;
        app.editor_panel_area = Some(area);
        draw_editor_area(
            frame,
            &mut app.editor,
            area,
            true,
            &mut app.editor_text_area,
            status_prompt.as_deref(),
            search_nav_active,
        );
    }

    if app.open_file_input.is_some() {
        draw_open_file_popup(frame, app, area);
    }
}

/// "Go to File" popup: unlike the goto-line/find/new-file prompts, this
/// one needs more than a single status-bar line (the whole point is
/// seeing several candidate matches at once), so it overlays a bordered
/// box on top of everything else instead of reusing `build_status_prompt`.
fn draw_open_file_popup(frame: &mut Frame, app: &App, area: Rect) {
    let query = app.open_file_input.as_deref().unwrap_or("");
    let matches = app.open_file_matches();
    let root = app.open_file_root();

    let width = area.width.saturating_sub(4).min(70).max(20);
    let max_list_rows = 12u16;
    let list_rows = (matches.len() as u16).clamp(1, max_list_rows);
    let height = (list_rows + 3).min(area.height.saturating_sub(2));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + 1,
        width,
        height,
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Open File ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    frame.render_widget(
        Paragraph::new(format!("{query}_")).style(Style::default().fg(Color::White)),
        rows[0],
    );

    let visible = (rows[1].height as usize).max(1);
    let selected = app.open_file_selected();
    let scroll_top = selected.saturating_sub(visible.saturating_sub(1)).min(
        matches.len().saturating_sub(visible.min(matches.len().max(1))),
    );

    let mut lines: Vec<Line> = Vec::with_capacity(visible);
    if matches.is_empty() {
        lines.push(Line::from(Span::styled(
            "(no matches)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for idx in scroll_top..(scroll_top + visible).min(matches.len()) {
            let label = matches[idx].strip_prefix(root).unwrap_or(&matches[idx]).display().to_string();
            let style = if idx == selected {
                Style::default().bg(Color::White).fg(Color::Black).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(label, style)));
        }
    }
    frame.render_widget(Paragraph::new(lines), rows[1]);
}

fn draw_tree(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Tree;
    let border_style = if focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Project ")
        .border_style(border_style);
    let inner = block.inner(area);
    app.tree_rows_area = Some(inner);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    if height == 0 {
        return;
    }

    let selected_idx = {
        let rows = app.tree.visible_rows();
        rows.iter()
            .position(|(_, n)| n.path == app.tree.selected_path)
            .unwrap_or(0)
    };
    if selected_idx < app.tree_scroll_top {
        app.tree_scroll_top = selected_idx;
    } else if selected_idx >= app.tree_scroll_top + height {
        app.tree_scroll_top = selected_idx + 1 - height;
    }

    let rows = app.tree.visible_rows();
    let mut lines = Vec::with_capacity(height);
    for row in 0..height {
        let idx = app.tree_scroll_top + row;
        if idx >= rows.len() {
            lines.push(Line::from(""));
            continue;
        }
        let (depth, node) = rows[idx];
        let indent = "  ".repeat(depth);
        let marker = if node.is_dir {
            if node.expanded {
                "v "
            } else {
                "> "
            }
        } else {
            "  "
        };
        let label = format!("{indent}{marker}{}", node.name);

        let mut style = if node.is_dir {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        if idx == selected_idx {
            style = if focused {
                style.bg(Color::White).fg(Color::Black).add_modifier(Modifier::BOLD)
            } else {
                style.bg(Color::DarkGray)
            };
        }
        lines.push(Line::from(Span::styled(label, style)));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_editor_area(
    frame: &mut Frame,
    editor: &mut Editor,
    area: Rect,
    focused: bool,
    text_area_out: &mut Option<Rect>,
    status_prompt: Option<&str>,
    search_nav_active: bool,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);
    *text_area_out = Some(chunks[0]);

    adjust_viewport(editor, chunks[0]);
    draw_text(frame, editor, chunks[0], focused);
    draw_status_bar(frame, editor, chunks[1], status_prompt, search_nav_active);
}

fn adjust_viewport(editor: &mut Editor, area: Rect) {
    let height = area.height as usize;
    let (line, _) = editor.caret_line_col();
    if line < editor.viewport_top_line {
        editor.viewport_top_line = line;
    } else if height > 0 && line >= editor.viewport_top_line + height {
        editor.viewport_top_line = line + 1 - height;
    }
}

fn draw_text(frame: &mut Frame, editor: &Editor, area: Rect, focused: bool) {
    let height = area.height as usize;
    let gutter_width = gutter_width(editor.document.len_lines());
    let selection = editor.caret.selection_range();
    let (caret_line, caret_col) = editor.caret_line_col();
    let column_view = editor.column_block_view();
    let caret_style = if focused {
        Style::default().bg(Color::White).fg(Color::Black).add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(Color::DarkGray)
    };

    let lang = editor.language();
    let mut line_state = highlight::state_before_line(&editor.document, lang, editor.viewport_top_line);

    let mut lines: Vec<Line> = Vec::with_capacity(height);
    for row in 0..height {
        let line_idx = editor.viewport_top_line + row;
        if line_idx >= editor.document.len_lines() {
            lines.push(Line::from(""));
            continue;
        }
        let gutter = format!("{:>width$} ", line_idx + 1, width = gutter_width - 1);
        let text = editor.document.line(line_idx);
        let line_start_offset = editor.document.line_col_to_offset(line_idx, 0);

        let (tokens, next_state) = highlight::highlight_line(&text, lang, line_state);
        line_state = next_state;
        let char_count = text.chars().count();
        let mut syntax_styles = vec![Style::default(); char_count];
        for (start, end, style) in &tokens {
            for s in syntax_styles.iter_mut().take((*end).min(char_count)).skip(*start) {
                *s = *style;
            }
        }

        let mut spans = vec![Span::styled(gutter, Style::default().fg(Color::DarkGray))];

        for (col, ch) in text.chars().chain(std::iter::once(' ')).enumerate() {
            let offset = line_start_offset + col;
            let in_selection = selection.map(|(s, e)| offset >= s && offset < e).unwrap_or(false);

            // Column Selection Mode has no single caret: every line the
            // rectangle spans gets its own cursor marker at the active
            // corner's column, and the rectangle itself highlights like a
            // normal selection.
            let (in_block, is_block_caret) = match column_view {
                Some(((min_l, max_l), (min_c, max_c), cur_col)) => {
                    let in_rows = line_idx >= min_l && line_idx <= max_l;
                    (in_rows && min_c < max_c && col >= min_c && col < max_c, in_rows && col == cur_col)
                }
                None => (false, false),
            };
            let is_caret = column_view.is_none() && line_idx == caret_line && col == caret_col;

            let mut style = syntax_styles.get(col).copied().unwrap_or_default();
            if in_selection || in_block {
                style = style.bg(Color::Blue);
            }
            if is_caret || is_block_caret {
                style = caret_style;
            }
            let s = if ch == ' ' && col == text.chars().count() {
                " ".to_string()
            } else {
                ch.to_string()
            };
            spans.push(Span::styled(s, style));
        }
        lines.push(Line::from(spans));
    }

    let paragraph = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
    frame.render_widget(paragraph, area);
}

fn draw_status_bar(
    frame: &mut Frame,
    editor: &Editor,
    area: Rect,
    status_prompt: Option<&str>,
    search_nav_active: bool,
) {
    if let Some(prompt) = status_prompt {
        let text = format!(" {prompt}");
        let paragraph = Paragraph::new(text).style(Style::default().bg(Color::Yellow).fg(Color::Black));
        frame.render_widget(paragraph, area);
        return;
    }

    let (line, col) = editor.caret_line_col();
    let modified = if editor.document.is_modified() { "[+]" } else { "" };
    if editor.is_column_mode() {
        let text = format!(
            " [COLUMN] {} {}  Ln {}, Col {}   arrows resize block  type/Backspace/Delete edit all lines  Ctrl+8 exit",
            editor.file_name(),
            modified,
            line + 1,
            col + 1,
        );
        let paragraph = Paragraph::new(text).style(Style::default().bg(Color::Magenta).fg(Color::White));
        frame.render_widget(paragraph, area);
        return;
    }
    let text = format!(
        " {} {}  Ln {}, Col {}   ^W sidebar  ^N new file  ^O open file  :<n>/^G goto line  / or ^F find{}  Ctrl/Cmd+S save  +Z undo  +Shift+Z/+Y redo  Ctrl+8 column mode  Ctrl+Q quit",
        editor.file_name(),
        modified,
        line + 1,
        col + 1,
        if search_nav_active { "  [n/N: next/prev match]" } else { "" },
    );
    let paragraph = Paragraph::new(text).style(Style::default().bg(Color::DarkGray).fg(Color::White));
    frame.render_widget(paragraph, area);
}
