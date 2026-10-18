# ride

A Rust text editor whose core is modeled on IntelliJ Platform's layering —
not a port or decompile of IntelliJ (which is closed-source for the
Ultimate edition and off-limits to reverse-engineer under its EULA; the
Community Edition is already open source at `JetBrains/intellij-community`
if you want to read the real thing).

## Architecture

Mirrors three IntelliJ Platform concepts, each with a much smaller surface:

- `document.rs` — **Document**: a mutable Unicode character sequence
  (rope-backed via `ropey`), not syntax-aware. Line endings are normalized
  to `\n` internally. Supports change listeners and guarded (read-only)
  regions, same as the real thing.
- `vfs.rs` — **VirtualFile/VFS**: minimal load/save between a `Document`
  and disk. The real VFS adds an app-level cache snapshot decoupled from
  disk and from any open `Document`; not needed until more than one file
  is open at a time.
- `editor.rs` — **Editor**: caret + selection on top of a `Document`,
  offset↔(line, col) coordinate conversion, and the edit actions
  (insert/backspace/delete/undo/redo) a keystroke triggers. Undo here is
  one command per keystroke; IntelliJ's `CommandProcessor` groups a whole
  user action into one undo entry — that's the next thing worth porting
  over if multi-character operations (e.g. "replace all") are added.
- `highlight.rs` — **Lexer**: fast, line-oriented syntax highlighting for
  JSON, YAML, TypeScript/JavaScript, and Rust, chosen by file extension -
  the lighter of IntelliJ's two highlighting layers (the heavier one,
  PSI, is still the "not yet implemented" item below). Hand-rolled per
  language rather than tree-sitter-backed, to avoid a grammar dependency
  (and, going by this project's past runs-in with `arboard`/`darling`,
  real risk of needing a newer rustc) for what's ultimately just coloring
  spans of text. Known gaps: YAML flow collections/anchors/tags/block
  scalars render as plain text; TypeScript doesn't distinguish a regex
  literal from the division operator (needs tracking whether the
  previous token could end an expression, which this lexer doesn't do);
  Rust block comments aren't nested (unlike real Rust's), type names are
  detected by the CamelCase convention rather than actually knowing
  what's a type (so a `SCREAMING_CASE` const gets colored the same way),
  and the char-literal-vs-lifetime split (`'c'` vs `'a`) is a heuristic,
  not a real parse.
- `tree.rs` — **Project View**: a directory tree mirroring the IntelliJ
  Project panel, backed by a minimal VFS-style lazy load (a directory's
  children are only read from disk the first time it's expanded).
- `app.rs` — ties the editor and tree together with a `Focus` (which
  panel keyboard input goes to) and sidebar visibility.
- `ui.rs` + `main.rs` — terminal UI (`ratatui` + `crossterm`), the
  two-panel layout, and the keybinding → action event loop.

## Run

```
cargo run -- path/to/file.txt   # opens the file, tree rooted at its directory
cargo run -- path/to/dir        # opens the tree only, no file
cargo run                       # tree rooted at the current directory
```

**Editor keys** (when the editor panel has focus): arrows to move
(+Shift to select), type to insert, Backspace/Delete, Ctrl/Cmd+S save,
+Z undo, +Shift+Z or Ctrl+Y redo, +C copy selection, +X cut selection,
+V paste. Cut (+X) with nothing selected cuts the entire current line
(including its trailing newline) instead of doing nothing, same as
IntelliJ's own Cut action. Copy/cut/paste use the real OS clipboard
(via `arboard`), since
enabling mouse capture (below) takes over the terminal's native
click-drag copy.

**Jump to line**: type `:` followed by digits, then Enter — vim-style
(e.g. `:42<Enter>`). Esc cancels. Ctrl+G / Cmd+L opens the same prompt.
Since this editor has no vim-style normal/insert mode split, `:` only
opens the prompt when the caret is at the start of an already-empty
line — everywhere else it inserts a literal `:` as normal text.

**Find**: type `/` to open incremental search, vim-style - same
empty-line-only heuristic as `:` above, so `/` elsewhere still inserts
normally. Ctrl+F / Cmd+F always opens it too, with no such restriction -
use that (or Esc to back out) if `/` fires when you meant to type a
`//` comment on a blank line, since that's the one real collision this
heuristic doesn't dodge (unlike `:`, starting a blank line with `//` is
common). Once open: type to search live (the first match jumps into view
and gets highlighted via the normal selection styling), Enter/Down for
the next match, Shift+Enter/Up for the previous - any of those also
confirms the search (closes the prompt, vim's "back to normal mode"),
after which **n**/**N** repeat the same search forward/backward, vim-style.
That n/N binding is only live for a short window right after confirming
a search; the moment you press anything else, it lapses and `n`/`N` go
back to being ordinary letters - this editor has no real normal/insert
mode split to hang a permanent binding on, so a literal always-on 'n'
would make it impossible to ever type the letter again. Esc instead of
confirming cancels outright, with no n/N window afterward. Case-sensitive;
only the current match is highlighted (not every occurrence at once); no
regex.

**Tree keys** (when the project panel has focus): Up/Down to move the
selection, Right expands a directory, Left collapses it, Enter opens the
selected file (or toggles a directory), Tab moves focus back to the editor.

**New file**: Ctrl/Cmd+N (works regardless of which panel has focus)
prompts for a filename, created inside the selected tree node's directory
(or that node's parent, if the selection is a file rather than a
directory), then opens it in the editor. Enter creates it, Esc cancels.

**Open file**: Ctrl/Cmd+O opens a "Go to File" popup (IntelliJ's
Cmd+Shift+O, minus the fuzzy scoring) - it walks every file under the
tree root once when the popup opens, then live-filters that list by
case-insensitive substring match on the path as you type. Up/Down moves
the highlighted match, Enter opens it, Esc cancels. `.git`, `target`, and
`node_modules` are skipped during the walk since they're large and never
what you're looking for by name.

**Mouse**: clicking the tree panel focuses it and selects the row under
the cursor; clicking the editor panel focuses it and moves the caret to
the click position. Mouse capture is enabled for the whole session, which
means the terminal's own click-drag text selection/copy is unavailable
while the app is running (all mouse events are routed to it instead).

**Column Selection Mode**: Ctrl/Cmd+8 toggles it (IntelliJ's own default
Mac shortcut is Cmd+Shift+8; this app keeps the plain "8" like its other
Ctrl/Cmd bindings). While active, the status bar turns magenta and reads
`[COLUMN]`: arrows grow or shrink a rectangle instead of moving a single
caret, and typing, Backspace, and Delete apply to every line the
rectangle spans at the same column, not just one. A line shorter than
the rectangle's column clamps to its own end rather than being padded out
to match - so very ragged text can fall out of alignment with the
rectangle as you type. Enter and Home/End are unhandled in this mode (no
single sensible per-line meaning); Ctrl+8 again to leave, then they work
normally. Undo/redo are still per-line, not per-block-edit - undoing a
column insert across 5 lines takes 5 undos, same underlying limitation as
the lack of CommandProcessor-style grouping noted above.

**Global**: Ctrl+W jumps focus to the tree (showing it first if hidden);
pressing it again while the tree already has focus hides it and returns
focus to the editor — same convention as IntelliJ's own tool-window
shortcuts (e.g. Cmd+1 for Project). Ctrl+Q quits immediately; Esc needs
two presses within about a second (any other key in between resets it) -
the status bar shows "Press Esc again to quit" after the first, so a
single stray Esc (e.g. a missed cancel elsewhere) can't accidentally
close the app.

## Test

```
cargo test
```

## Not yet implemented (natural next milestones)

- PSI-equivalent: syntax tree over the Document (tree-sitter is the
  obvious Rust fit) — needed for anything structure-aware.
- Real VFS cache shared across multiple open files (right now, opening a
  file from the tree replaces the current editor buffer outright, even
  if it has unsaved changes).
- CommandProcessor-style undo grouping instead of per-keystroke.
- A GUI front end instead of terminal-only.
