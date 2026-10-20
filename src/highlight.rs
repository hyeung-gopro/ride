use std::path::Path;

use ratatui::style::{Color, Modifier, Style};

use crate::document::Document;

/// The "Lexer" layer of IntelliJ's architecture: fast, line-oriented
/// token classification for display only. Deliberately far simpler than
/// the "PSI" full-syntax-tree layer the README's "not yet implemented"
/// list still calls for - no real parsing, no error recovery, no
/// cross-reference awareness, just enough tokenizing to color text.
/// Hand-rolled rather than tree-sitter-backed on purpose: a grammar
/// dependency is a lot of build weight (and, going by this project's
/// past run-ins with `arboard`/`darling`, real risk of needing a newer
/// rustc) for a feature that's ultimately just coloring spans of text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Language {
    PlainText,
    Json,
    Yaml,
    TypeScript,
    Rust,
}

impl Language {
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => Language::Json,
            Some("yaml") | Some("yml") => Language::Yaml,
            Some("ts") | Some("tsx") | Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => {
                Language::TypeScript
            }
            Some("rs") => Language::Rust,
            _ => Language::PlainText,
        }
    }
}

/// Lexer state carried from the end of one line to the start of the
/// next. Only TypeScript and Rust need this (an unterminated block
/// comment, template literal, plain string, or raw string can span many
/// lines); JSON and YAML strings can't contain a literal newline, so
/// every line lexes independently there. `RawString` carries the raw
/// string's `#` delimiter count (`r##"..."##` needs as many closing `#`s
/// as it opened with).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum LineState {
    #[default]
    Normal,
    BlockComment,
    TemplateLiteral,
    StringLiteral,
    RawString(usize),
}

fn keyword_style() -> Style {
    Style::default().fg(Color::Magenta)
}
fn string_style() -> Style {
    Style::default().fg(Color::Green)
}
fn comment_style() -> Style {
    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)
}
fn number_style() -> Style {
    Style::default().fg(Color::LightYellow)
}
fn key_style() -> Style {
    Style::default().fg(Color::Cyan)
}
fn type_style() -> Style {
    Style::default().fg(Color::Blue)
}
fn macro_style() -> Style {
    Style::default().fg(Color::LightMagenta)
}
fn attribute_style() -> Style {
    Style::default().fg(Color::LightBlue)
}

/// Tokenizes one line, returning (char-offset-range, style) spans plus
/// the state the *next* line should start with. `state_in` is ignored by
/// every language but TypeScript.
pub fn highlight_line(text: &str, lang: Language, state_in: LineState) -> (Vec<(usize, usize, Style)>, LineState) {
    match lang {
        Language::PlainText => (Vec::new(), LineState::Normal),
        Language::Json => (highlight_json(text), LineState::Normal),
        Language::Yaml => (highlight_yaml(text), LineState::Normal),
        Language::TypeScript => highlight_typescript(text, state_in),
        Language::Rust => highlight_rust(text, state_in),
    }
}

/// The state the line at `line` (0-based) starts in, found by replaying
/// every line above it. A no-op for languages without cross-line state.
/// For TypeScript this is a full rescan from the top of the file on
/// every call - fine at the file sizes this editor targets (same
/// trade-off `Document::find_occurrences` already makes), but a real
/// incremental lexer would cache per-line states instead of redoing this
/// on every render.
pub fn state_before_line(document: &Document, lang: Language, line: usize) -> LineState {
    if !matches!(lang, Language::TypeScript | Language::Rust) {
        return LineState::Normal;
    }
    let mut state = LineState::Normal;
    for l in 0..line.min(document.len_lines()) {
        let (_, next) = highlight_line(&document.line(l), lang, state);
        state = next;
    }
    state
}

fn highlight_json(text: &str) -> Vec<(usize, usize, Style)> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '"' {
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let is_key = j < chars.len() && chars[j] == ':';
            tokens.push((start, i, if is_key { key_style() } else { string_style() }));
            continue;
        }
        if c == '-' || c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_digit() || matches!(chars[i], '.' | 'e' | 'E' | '+' | '-')) {
                i += 1;
            }
            tokens.push((start, i, number_style()));
            continue;
        }
        if c.is_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_alphanumeric() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if matches!(word.as_str(), "true" | "false" | "null") {
                tokens.push((start, i, keyword_style()));
            }
            continue;
        }
        i += 1;
    }
    tokens
}

/// Covers the common case well (comments, quoted strings, plain mapping
/// keys, booleans/null, numbers) but isn't a real YAML parser: flow
/// collections (`{a: 1}`, `[1, 2]`), anchors/aliases (`&x`, `*x`), tags
/// (`!!str`), and multi-line block scalars (`|`, `>`) aren't specially
/// tokenized - they just render as plain text.
fn highlight_yaml(text: &str) -> Vec<(usize, usize, Style)> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '#' {
            tokens.push((i, chars.len(), comment_style()));
            break;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if quote == '"' && chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            tokens.push((start, i, string_style()));
            continue;
        }
        if c == '-' && (i + 1 >= chars.len() || chars[i + 1].is_whitespace()) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i] != '#' && !chars[i].is_whitespace() {
            if chars[i] == ':' && (i + 1 == chars.len() || chars[i + 1].is_whitespace()) {
                break;
            }
            i += 1;
        }
        if i < chars.len() && chars[i] == ':' {
            tokens.push((start, i, key_style()));
            i += 1;
            continue;
        }
        if i == start {
            // A ':' that isn't a key separator (no trailing whitespace, e.g.
            // "a:b") stops the scan above without being consumed by it, so
            // without this, `i` would never advance and the outer loop would
            // spin forever on the same index.
            i += 1;
            continue;
        }
        let word: String = chars[start..i].iter().collect();
        if matches!(word.as_str(), "true" | "false" | "null" | "~") {
            tokens.push((start, i, keyword_style()));
        } else if !word.is_empty()
            && word.chars().any(|ch| ch.is_ascii_digit())
            && word.chars().all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '+'))
        {
            tokens.push((start, i, number_style()));
        }
    }
    tokens
}

const TS_KEYWORDS: &[&str] = &[
    "abstract", "any", "as", "asserts", "async", "await", "boolean", "break", "case", "catch",
    "class", "const", "continue", "debugger", "declare", "default", "delete", "do", "else",
    "enum", "export", "extends", "false", "finally", "for", "from", "function", "get", "if",
    "implements", "import", "in", "infer", "instanceof", "interface", "is", "keyof", "let",
    "module", "namespace", "never", "new", "null", "number", "object", "of", "package",
    "private", "protected", "public", "readonly", "return", "satisfies", "set", "static",
    "string", "super", "switch", "symbol", "this", "throw", "true", "try", "type", "typeof",
    "undefined", "unique", "unknown", "var", "void", "while", "with", "yield",
];

/// Also used for .js/.jsx/.mjs/.cjs, which are a subset of this grammar
/// as far as a lexer is concerned. One known gap: regex literals
/// (`/foo/g`) aren't distinguished from the division operator - that
/// requires knowing whether the previous token could end an expression,
/// which this lexer doesn't track - so a `/` is always just punctuation
/// here.
fn highlight_typescript(text: &str, state_in: LineState) -> (Vec<(usize, usize, Style)>, LineState) {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut state = state_in;

    if state == LineState::BlockComment {
        while i < chars.len() {
            if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                i += 2;
                state = LineState::Normal;
                break;
            }
            i += 1;
        }
        tokens.push((0, i, comment_style()));
        if state == LineState::BlockComment {
            return (tokens, state);
        }
    } else if state == LineState::TemplateLiteral {
        while i < chars.len() {
            if chars[i] == '\\' && i + 1 < chars.len() {
                i += 2;
                continue;
            }
            if chars[i] == '`' {
                i += 1;
                state = LineState::Normal;
                break;
            }
            i += 1;
        }
        tokens.push((0, i, string_style()));
        if state == LineState::TemplateLiteral {
            return (tokens, state);
        }
    }

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            tokens.push((i, chars.len(), comment_style()));
            break;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            tokens.push((start, i, comment_style()));
            if !closed {
                state = LineState::BlockComment;
            }
            continue;
        }
        if c == '"' || c == '\'' {
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != quote {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            tokens.push((start, i, string_style()));
            continue;
        }
        if c == '`' {
            let start = i;
            i += 1;
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == '`' {
                    i += 1;
                    closed = true;
                    break;
                }
                i += 1;
            }
            tokens.push((start, i, string_style()));
            if !closed {
                state = LineState::TemplateLiteral;
            }
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '.' | '_')) {
                i += 1;
            }
            tokens.push((start, i, number_style()));
            continue;
        }
        if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if TS_KEYWORDS.contains(&word.as_str()) {
                tokens.push((start, i, keyword_style()));
            }
            continue;
        }
        i += 1;
    }
    (tokens, state)
}

const RUST_KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true",
    "try", "type", "union", "unsafe", "use", "where", "while", "yield",
];

const RUST_PRIMITIVE_TYPES: &[&str] = &[
    "bool", "char", "str", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64",
    "u128", "usize", "f32", "f64",
];

/// Tries to parse a byte string (`b"..."`), raw string (`r"..."`,
/// `r#"..."#`, with any number of `#`s), or raw byte string
/// (`br"..."`/`br#"..."#`) starting at `i`. Returns the end index and,
/// if it runs off the end of the line unterminated, the state the next
/// line should resume in. `None` if `i` isn't actually the start of one
/// of these (e.g. an ordinary identifier like `brown` or `r2d2`).
fn try_rust_byte_or_raw_string(chars: &[char], i: usize) -> Option<(usize, Option<LineState>)> {
    let mut j = i;
    if chars.get(j) == Some(&'b') {
        j += 1;
    }
    let is_raw = chars.get(j) == Some(&'r');
    if is_raw {
        j += 1;
    }
    let hashes = {
        let mut n = 0;
        while chars.get(j + n) == Some(&'#') {
            n += 1;
        }
        n
    };
    if is_raw {
        j += hashes;
    } else if hashes > 0 || j == i {
        // "b#.." isn't a thing, and we need at least a 'b' or 'r' consumed.
        return None;
    }
    if chars.get(j) != Some(&'"') {
        return None;
    }
    j += 1;
    if !is_raw {
        while j < chars.len() {
            if chars[j] == '\\' && j + 1 < chars.len() {
                j += 2;
                continue;
            }
            if chars[j] == '"' {
                return Some((j + 1, None));
            }
            j += 1;
        }
        return Some((chars.len(), Some(LineState::StringLiteral)));
    }
    while j < chars.len() {
        if chars[j] == '"' && (0..hashes).all(|k| chars.get(j + 1 + k) == Some(&'#')) {
            return Some((j + 1 + hashes, None));
        }
        j += 1;
    }
    Some((chars.len(), Some(LineState::RawString(hashes))))
}

/// Distinguishes a char literal (`'c'`, `'\n'`, `'\u{41}'`) from a
/// lifetime (`'a`, `'static`) by checking whether a closing `'` appears
/// right where a single (possibly-escaped) character would end. Not
/// airtight against every pathological case, but right for the common
/// shape of each.
fn try_rust_char_literal(chars: &[char], i: usize) -> Option<usize> {
    let mut j = i + 1;
    chars.get(j)?;
    if chars[j] == '\\' {
        j += 1;
        if chars.get(j) == Some(&'u') && chars.get(j + 1) == Some(&'{') {
            j += 2;
            while matches!(chars.get(j), Some(c) if *c != '}') {
                j += 1;
            }
            if chars.get(j) == Some(&'}') {
                j += 1;
            }
        } else {
            j += 1;
        }
    } else {
        j += 1;
    }
    if chars.get(j) == Some(&'\'') {
        Some(j + 1)
    } else {
        None
    }
}

/// One known gap on top of the ones `try_rust_byte_or_raw_string` and
/// `try_rust_char_literal` already call out: block comments aren't
/// nested here, even though Rust's actually are (`/* outer /* inner */
/// still outer */` closes early in this lexer). Type names are detected
/// by the CamelCase convention rather than by knowing what's actually a
/// type, so a `SCREAMING_CASE` constant gets colored as if it were one.
fn highlight_rust(text: &str, state_in: LineState) -> (Vec<(usize, usize, Style)>, LineState) {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut state = state_in;

    match state {
        LineState::BlockComment => {
            while i < chars.len() {
                if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    i += 2;
                    state = LineState::Normal;
                    break;
                }
                i += 1;
            }
            tokens.push((0, i, comment_style()));
            if state == LineState::BlockComment {
                return (tokens, state);
            }
        }
        LineState::StringLiteral => {
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    closed = true;
                    break;
                }
                i += 1;
            }
            tokens.push((0, i, string_style()));
            if !closed {
                return (tokens, state);
            }
            state = LineState::Normal;
        }
        LineState::RawString(hashes) => {
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '"' && (0..hashes).all(|k| chars.get(i + 1 + k) == Some(&'#')) {
                    i += 1 + hashes;
                    closed = true;
                    break;
                }
                i += 1;
            }
            tokens.push((0, i, string_style()));
            if !closed {
                return (tokens, state);
            }
            state = LineState::Normal;
        }
        _ => {}
    }

    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            tokens.push((i, chars.len(), comment_style()));
            break;
        }
        if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            let start = i;
            i += 2;
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    i += 2;
                    closed = true;
                    break;
                }
                i += 1;
            }
            tokens.push((start, i, comment_style()));
            if !closed {
                state = LineState::BlockComment;
            }
            continue;
        }
        if c == '#' {
            let start = i;
            let mut j = i + 1;
            if chars.get(j) == Some(&'!') {
                j += 1;
            }
            if chars.get(j) == Some(&'[') {
                let mut depth = 0;
                while j < chars.len() {
                    match chars[j] {
                        '[' => depth += 1,
                        ']' => {
                            depth -= 1;
                            j += 1;
                            if depth == 0 {
                                break;
                            }
                            continue;
                        }
                        _ => {}
                    }
                    j += 1;
                }
                tokens.push((start, j, attribute_style()));
                i = j;
                continue;
            }
            i += 1;
            continue;
        }
        if c == 'b' || c == 'r' {
            if let Some((end, pending)) = try_rust_byte_or_raw_string(&chars, i) {
                tokens.push((i, end, string_style()));
                if let Some(st) = pending {
                    state = st;
                    return (tokens, state);
                }
                i = end;
                continue;
            }
        }
        if c == '"' {
            let start = i;
            i += 1;
            let mut closed = false;
            while i < chars.len() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 2;
                    continue;
                }
                if chars[i] == '"' {
                    i += 1;
                    closed = true;
                    break;
                }
                i += 1;
            }
            tokens.push((start, i, string_style()));
            if !closed {
                state = LineState::StringLiteral;
                return (tokens, state);
            }
            continue;
        }
        if c == '\'' {
            if let Some(end) = try_rust_char_literal(&chars, i) {
                tokens.push((i, end, string_style()));
                i = end;
            } else {
                let start = i;
                i += 1;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                tokens.push((start, i, type_style()));
            }
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '.' | '_')) {
                i += 1;
            }
            tokens.push((start, i, number_style()));
            continue;
        }
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            if chars.get(i) == Some(&'!') {
                tokens.push((start, i, macro_style()));
            } else if RUST_KEYWORDS.contains(&word.as_str()) {
                tokens.push((start, i, keyword_style()));
            } else if RUST_PRIMITIVE_TYPES.contains(&word.as_str())
                || word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
            {
                tokens.push((start, i, type_style()));
            }
            continue;
        }
        i += 1;
    }
    (tokens, state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_distinguishes_keys_from_string_values() {
        let (tokens, _) = highlight_line(r#"  "name": "ride""#, Language::Json, LineState::Normal);
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0], (2, 8, key_style()));
        assert_eq!(tokens[1], (10, 16, string_style()));
    }

    #[test]
    fn json_highlights_booleans_and_numbers() {
        let (tokens, _) = highlight_line("[true, 42, -1.5e3, null]", Language::Json, LineState::Normal);
        let kinds: Vec<Style> = tokens.iter().map(|(_, _, s)| *s).collect();
        assert!(kinds.contains(&keyword_style()));
        assert!(kinds.contains(&number_style()));
    }

    #[test]
    fn yaml_highlights_key_comment_and_string() {
        let (tokens, _) = highlight_line("name: \"ride\" # a tool", Language::Yaml, LineState::Normal);
        assert_eq!(tokens[0], (0, 4, key_style()));
        assert_eq!(tokens[1], (6, 12, string_style()));
        assert_eq!(tokens[2].2, comment_style());
    }

    #[test]
    fn yaml_terminates_on_a_colon_not_followed_by_whitespace() {
        // Regression test for a hang: a ':' inside a value (not a key
        // separator) must not stall the scan at the same index forever.
        // Each of these must return rather than spin; the assertions just
        // double check the spans make forward progress and cover the line.
        for line in [
            "path: a:b",
            "x: ${{ a }}:${{ b }}",
            "url: http://example.com",
            "k: v:",
        ] {
            let (tokens, _) = highlight_line(line, Language::Yaml, LineState::Normal);
            for (start, end, _) in &tokens {
                assert!(end > start, "token span must advance: {:?}", (start, end));
            }
        }
    }

    #[test]
    fn typescript_tracks_block_comment_across_lines() {
        let (_, state) = highlight_line("/* start", Language::TypeScript, LineState::Normal);
        assert_eq!(state, LineState::BlockComment);
        let (tokens, state) = highlight_line("still a comment", Language::TypeScript, state);
        assert_eq!(tokens, vec![(0, 15, comment_style())]);
        assert_eq!(state, LineState::BlockComment);
        let (_, state) = highlight_line("end */ let x = 1;", Language::TypeScript, state);
        assert_eq!(state, LineState::Normal);
    }

    #[test]
    fn typescript_highlights_keywords_and_strings() {
        let (tokens, state) = highlight_line("const x: string = 'hi';", Language::TypeScript, LineState::Normal);
        assert_eq!(state, LineState::Normal);
        let kinds: Vec<Style> = tokens.iter().map(|(_, _, s)| *s).collect();
        assert!(kinds.iter().filter(|s| **s == keyword_style()).count() >= 2); // const, string
        assert!(kinds.contains(&string_style()));
    }

    #[test]
    fn language_from_path_matches_known_extensions() {
        assert_eq!(Language::from_path(Path::new("a.json")), Language::Json);
        assert_eq!(Language::from_path(Path::new("a.yaml")), Language::Yaml);
        assert_eq!(Language::from_path(Path::new("a.yml")), Language::Yaml);
        assert_eq!(Language::from_path(Path::new("a.ts")), Language::TypeScript);
        assert_eq!(Language::from_path(Path::new("a.tsx")), Language::TypeScript);
        assert_eq!(Language::from_path(Path::new("a.rs")), Language::Rust);
        assert_eq!(Language::from_path(Path::new("a.txt")), Language::PlainText);
    }

    #[test]
    fn rust_highlights_keyword_type_and_macro() {
        let (tokens, state) = highlight_line("println!(\"hi\");", Language::Rust, LineState::Normal);
        assert_eq!(state, LineState::Normal);
        assert_eq!(tokens[0], (0, 7, macro_style()));
        assert_eq!(tokens[1], (9, 13, string_style()));

        let (tokens, _) = highlight_line("fn main() -> Foo {", Language::Rust, LineState::Normal);
        let styles: Vec<Style> = tokens.iter().map(|(_, _, s)| *s).collect();
        assert!(styles.contains(&keyword_style())); // fn
        assert!(styles.contains(&type_style())); // Foo (CamelCase)
    }

    #[test]
    fn rust_distinguishes_char_literal_from_lifetime() {
        let (tokens, _) = highlight_line("let x: &'a str = 'c';", Language::Rust, LineState::Normal);
        let styles: Vec<Style> = tokens.iter().map(|(_, _, s)| *s).collect();
        assert!(styles.contains(&type_style())); // 'a lifetime + str primitive
        assert!(styles.contains(&string_style())); // 'c' char literal
        assert!(styles.contains(&keyword_style())); // let
    }

    #[test]
    fn rust_tracks_raw_string_across_lines() {
        let (_, state) = highlight_line("let s = r#\"start", Language::Rust, LineState::Normal);
        assert_eq!(state, LineState::RawString(1));
        let (tokens, state) = highlight_line("end\"# ;", Language::Rust, state);
        assert_eq!(state, LineState::Normal);
        assert_eq!(tokens[0], (0, 5, string_style()));
    }

    #[test]
    fn rust_highlights_attribute() {
        let (tokens, _) = highlight_line("#[derive(Debug, Clone)]", Language::Rust, LineState::Normal);
        assert_eq!(tokens[0], (0, 23, attribute_style()));
    }
}
