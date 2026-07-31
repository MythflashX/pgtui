use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
}

/// Result of feeding a key to the editor that the app may need to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorEvent {
    None,
    /// Text was yanked; the app may forward it to the system clipboard.
    Yanked(String),
}

pub struct Editor {
    pub name: String,
    pub lines: Vec<String>,
    /// Cursor position as (row, column) in characters.
    pub row: usize,
    pub col: usize,
    pub mode: Mode,
    pub scroll_row: usize,
    pub scroll_col: usize,
    /// Anchor of the visual selection, if in visual mode.
    pub anchor: Option<(usize, usize)>,
    /// Last search pattern for n/N.
    pub search_pattern: Option<String>,
    /// Viewport size, updated by the renderer each frame.
    pub view_h: usize,
    pub view_w: usize,
    vim: bool,
    pending: Option<char>,
    register: String,
    register_linewise: bool,
    undo_stack: Vec<(Vec<String>, usize, usize)>,
    redo_stack: Vec<(Vec<String>, usize, usize)>,
    /// Column the cursor "wants" for vertical movement over short lines.
    goal_col: usize,
}

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn byte_idx(s: &str, col: usize) -> usize {
    s.char_indices()
        .nth(col)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum CharClass {
    Space,
    Word,
    Punct,
}

fn class_of(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

impl Editor {
    pub fn new(name: String, vim: bool) -> Self {
        Self {
            name,
            lines: vec![String::new()],
            row: 0,
            col: 0,
            mode: if vim { Mode::Normal } else { Mode::Insert },
            scroll_row: 0,
            scroll_col: 0,
            anchor: None,
            search_pattern: None,
            view_h: 10,
            view_w: 40,
            vim,
            pending: None,
            register: String::new(),
            register_linewise: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            goal_col: 0,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].trim().is_empty()
    }

    pub fn set_text(&mut self, text: &str) {
        self.push_undo();
        self.lines = text.lines().map(|s| s.to_string()).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = 0;
        self.col = 0;
        self.scroll_row = 0;
        self.scroll_col = 0;
        self.anchor = None;
        if self.vim {
            self.mode = Mode::Normal;
        }
    }

    /// Text of the current visual selection, if any.
    pub fn selection_text(&self) -> Option<String> {
        let ((sr, sc), (er, ec)) = self.selection_range()?;
        let mut out = String::new();
        for r in sr..=er {
            let line = &self.lines[r];
            let from = if r == sr { sc } else { 0 };
            let to = if r == er {
                (ec + 1).min(char_len(line))
            } else {
                char_len(line)
            };
            let b0 = byte_idx(line, from);
            let b1 = byte_idx(line, to);
            out.push_str(&line[b0..b1]);
            if r != er {
                out.push('\n');
            }
        }
        Some(out)
    }

    /// Ordered, inclusive selection range ((start_row, start_col), (end_row, end_col)).
    pub fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.anchor?;
        let cur = (self.row, self.col);
        let (a, b) = if anchor <= cur {
            (anchor, cur)
        } else {
            (cur, anchor)
        };
        Some((a, b))
    }

    fn cur_line_len(&self) -> usize {
        char_len(&self.lines[self.row])
    }

    fn max_col(&self) -> usize {
        let len = self.cur_line_len();
        if self.mode == Mode::Insert {
            len
        } else {
            len.saturating_sub(1)
        }
    }

    fn clamp_col(&mut self) {
        let m = if self.mode == Mode::Insert {
            self.cur_line_len()
        } else {
            self.cur_line_len().saturating_sub(1)
        };
        if self.col > m {
            self.col = m;
        }
    }

    fn push_undo(&mut self) {
        self.undo_stack.push((self.lines.clone(), self.row, self.col));
        if self.undo_stack.len() > 200 {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    fn undo(&mut self) {
        if let Some((lines, row, col)) = self.undo_stack.pop() {
            self.redo_stack
                .push((self.lines.clone(), self.row, self.col));
            self.lines = lines;
            self.row = row.min(self.lines.len() - 1);
            self.col = col;
            self.clamp_col();
        }
    }

    fn redo(&mut self) {
        if let Some((lines, row, col)) = self.redo_stack.pop() {
            self.undo_stack
                .push((self.lines.clone(), self.row, self.col));
            self.lines = lines;
            self.row = row.min(self.lines.len() - 1);
            self.col = col;
            self.clamp_col();
        }
    }

    // ---- editing primitives -------------------------------------------------

    fn insert_char(&mut self, c: char) {
        let b = byte_idx(&self.lines[self.row], self.col);
        self.lines[self.row].insert(b, c);
        self.col += 1;
    }

    fn insert_newline(&mut self) {
        let line = self.lines[self.row].clone();
        let b = byte_idx(&line, self.col);
        let indent: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let rest = line[b..].to_string();
        self.lines[self.row] = line[..b].to_string();
        let new_line = format!("{indent}{rest}");
        self.lines.insert(self.row + 1, new_line);
        self.row += 1;
        self.col = char_len(&indent);
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            let b0 = byte_idx(&self.lines[self.row], self.col - 1);
            let b1 = byte_idx(&self.lines[self.row], self.col);
            self.lines[self.row].replace_range(b0..b1, "");
            self.col -= 1;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.cur_line_len();
            self.lines[self.row].push_str(&cur);
        }
    }

    fn delete_at_cursor(&mut self) -> Option<char> {
        let len = self.cur_line_len();
        if self.col < len {
            let b0 = byte_idx(&self.lines[self.row], self.col);
            let b1 = byte_idx(&self.lines[self.row], self.col + 1);
            let removed: String = self.lines[self.row][b0..b1].to_string();
            self.lines[self.row].replace_range(b0..b1, "");
            removed.chars().next()
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
            Some('\n')
        } else {
            None
        }
    }

    fn delete_range(&mut self, (sr, sc): (usize, usize), (er, ec): (usize, usize)) -> String {
        // Inclusive char-wise range delete.
        let mut removed = String::new();
        if sr == er {
            let line = &self.lines[sr];
            let to = (ec + 1).min(char_len(line));
            let b0 = byte_idx(line, sc);
            let b1 = byte_idx(line, to);
            removed.push_str(&line[b0..b1]);
            self.lines[sr].replace_range(b0..b1, "");
        } else {
            let first = self.lines[sr].clone();
            let last = self.lines[er].clone();
            let b0 = byte_idx(&first, sc);
            let to = (ec + 1).min(char_len(&last));
            let b1 = byte_idx(&last, to);
            removed.push_str(&first[b0..]);
            removed.push('\n');
            for line in &self.lines[sr + 1..er] {
                removed.push_str(line);
                removed.push('\n');
            }
            removed.push_str(&last[..b1]);
            let merged = format!("{}{}", &first[..b0], &last[b1..]);
            self.lines.splice(sr..=er, std::iter::once(merged));
        }
        self.row = sr;
        self.col = sc;
        self.clamp_col();
        removed
    }

    fn insert_text_at_cursor(&mut self, text: &str) {
        let parts: Vec<&str> = text.split('\n').collect();
        if parts.len() == 1 {
            let b = byte_idx(&self.lines[self.row], self.col);
            self.lines[self.row].insert_str(b, parts[0]);
            self.col += char_len(parts[0]);
        } else {
            let line = self.lines[self.row].clone();
            let b = byte_idx(&line, self.col);
            let tail = line[b..].to_string();
            self.lines[self.row] = format!("{}{}", &line[..b], parts[0]);
            let mut insert_at = self.row + 1;
            for part in &parts[1..parts.len() - 1] {
                self.lines.insert(insert_at, part.to_string());
                insert_at += 1;
            }
            let last = parts[parts.len() - 1];
            self.lines.insert(insert_at, format!("{last}{tail}"));
            self.row = insert_at;
            self.col = char_len(last);
        }
    }

    /// Byte offset of the cursor within `text()`.
    pub fn cursor_offset(&self) -> usize {
        let mut off = 0;
        for line in self.lines.iter().take(self.row) {
            off += line.len() + 1; // + newline
        }
        off + byte_idx(&self.lines[self.row], self.col)
    }

    /// Insert externally pasted text at the cursor (bracketed paste).
    pub fn paste_text(&mut self, text: &str) {
        self.push_undo();
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.insert_text_at_cursor(&text);
    }

    // ---- motions ------------------------------------------------------------

    fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        }
        self.goal_col = self.col;
    }

    fn move_right(&mut self) {
        if self.col < self.max_col() {
            self.col += 1;
        }
        self.goal_col = self.col;
    }

    fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.goal_col;
            self.clamp_col();
        }
    }

    fn move_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.goal_col;
            self.clamp_col();
        }
    }

    fn line_chars(&self, row: usize) -> Vec<char> {
        self.lines[row].chars().collect()
    }

    fn word_forward(&mut self) {
        let mut row = self.row;
        let mut col = self.col;
        let mut chars = self.line_chars(row);
        let start_class = chars.get(col).copied().map(class_of);
        // Skip the rest of the current word.
        if let Some(sc) = start_class {
            if sc != CharClass::Space {
                while col < chars.len() && class_of(chars[col]) == sc {
                    col += 1;
                }
            }
        }
        // Skip whitespace (possibly across lines).
        loop {
            if col >= chars.len() {
                if row + 1 < self.lines.len() {
                    row += 1;
                    col = 0;
                    chars = self.line_chars(row);
                    if chars.is_empty() {
                        break;
                    }
                } else {
                    break;
                }
            } else if class_of(chars[col]) == CharClass::Space {
                col += 1;
            } else {
                break;
            }
        }
        self.row = row;
        self.col = col;
        self.clamp_col();
        self.goal_col = self.col;
    }

    fn word_back(&mut self) {
        let mut row = self.row;
        let mut col = self.col;
        loop {
            if col == 0 {
                if row == 0 {
                    break;
                }
                row -= 1;
                col = char_len(&self.lines[row]);
                if col == 0 {
                    continue;
                }
            }
            let chars = self.line_chars(row);
            col -= 1;
            // Skip whitespace backwards.
            while col > 0 && class_of(chars[col]) == CharClass::Space {
                col -= 1;
            }
            if chars.is_empty() || class_of(chars[col]) == CharClass::Space {
                continue;
            }
            let cls = class_of(chars[col]);
            while col > 0 && class_of(chars[col - 1]) == cls {
                col -= 1;
            }
            break;
        }
        self.row = row;
        self.col = col;
        self.goal_col = self.col;
    }

    fn word_end(&mut self) {
        let mut row = self.row;
        let mut col = self.col;
        loop {
            let chars = self.line_chars(row);
            if col + 1 < chars.len() {
                col += 1;
            } else if row + 1 < self.lines.len() {
                row += 1;
                col = 0;
            } else {
                break;
            }
            let chars = self.line_chars(row);
            // Skip whitespace.
            while col < chars.len() && class_of(chars[col]) == CharClass::Space {
                if col + 1 < chars.len() {
                    col += 1;
                } else if row + 1 < self.lines.len() {
                    row += 1;
                    col = 0;
                } else {
                    self.row = row;
                    self.col = col.min(chars.len().saturating_sub(1));
                    return;
                }
            }
            let chars = self.line_chars(row);
            if chars.is_empty() {
                continue;
            }
            let cls = class_of(chars[col]);
            while col + 1 < chars.len() && class_of(chars[col + 1]) == cls {
                col += 1;
            }
            break;
        }
        self.row = row;
        self.col = col;
        self.clamp_col();
        self.goal_col = self.col;
    }

    fn first_non_blank(&mut self) {
        let idx = self.lines[self.row]
            .chars()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0);
        self.col = idx;
        self.clamp_col();
        self.goal_col = self.col;
    }

    pub fn goto_top(&mut self) {
        self.row = 0;
        self.col = 0;
        self.goal_col = 0;
    }

    pub fn goto_bottom(&mut self) {
        self.row = self.lines.len() - 1;
        self.col = 0;
        self.goal_col = 0;
    }

    fn scroll_half(&mut self, down: bool) {
        let half = (self.view_h / 2).max(1);
        if down {
            self.row = (self.row + half).min(self.lines.len() - 1);
        } else {
            self.row = self.row.saturating_sub(half);
        }
        self.clamp_col();
    }

    fn scroll_full(&mut self, down: bool) {
        let page = self.view_h.max(1);
        if down {
            self.row = (self.row + page).min(self.lines.len() - 1);
        } else {
            self.row = self.row.saturating_sub(page);
        }
        self.clamp_col();
    }

    // ---- search -------------------------------------------------------------

    pub fn search(&mut self, pattern: &str) {
        if pattern.is_empty() {
            return;
        }
        self.search_pattern = Some(pattern.to_string());
        self.search_next(true);
    }

    pub fn search_next(&mut self, forward: bool) {
        let Some(pat) = self.search_pattern.clone() else {
            return;
        };
        let pat = pat.to_lowercase();
        let n = self.lines.len();
        // Collect (row, col) matches lazily by scanning rows in order.
        let find_in = |line: &str, from: Option<usize>, rev: bool| -> Option<usize> {
            let lower = line.to_lowercase();
            let matches: Vec<usize> = lower
                .match_indices(&pat)
                .map(|(b, _)| line[..b].chars().count())
                .collect();
            if rev {
                matches.into_iter().rfind(|c| from.is_none_or(|f| *c < f))
            } else {
                matches.into_iter().find(|c| from.is_none_or(|f| *c > f))
            }
        };
        if forward {
            for step in 0..=n {
                let row = (self.row + step) % n;
                let from = if step == 0 { Some(self.col) } else { None };
                if let Some(col) = find_in(&self.lines[row], from, false) {
                    self.row = row;
                    self.col = col;
                    self.clamp_col();
                    return;
                }
            }
        } else {
            for step in 0..=n {
                let row = (self.row + n - (step % n)) % n;
                let from = if step == 0 { Some(self.col) } else { None };
                if let Some(col) = find_in(&self.lines[row], from, true) {
                    self.row = row;
                    self.col = col;
                    self.clamp_col();
                    return;
                }
            }
        }
    }

    // ---- scrolling ----------------------------------------------------------

    /// Keep the cursor inside the viewport; called before rendering.
    pub fn ensure_visible(&mut self) {
        if self.row < self.scroll_row {
            self.scroll_row = self.row;
        }
        if self.row >= self.scroll_row + self.view_h.max(1) {
            self.scroll_row = self.row + 1 - self.view_h.max(1);
        }
        if self.col < self.scroll_col {
            self.scroll_col = self.col;
        }
        if self.col >= self.scroll_col + self.view_w.max(1) {
            self.scroll_col = self.col + 1 - self.view_w.max(1);
        }
    }

    // ---- key handling -------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> EditorEvent {
        if !self.vim {
            self.handle_insert_key(key);
            return EditorEvent::None;
        }
        match self.mode {
            Mode::Insert => {
                if key.code == KeyCode::Esc {
                    self.mode = Mode::Normal;
                    if self.col > 0 {
                        self.col -= 1;
                    }
                    self.clamp_col();
                } else {
                    self.handle_insert_key(key);
                }
                EditorEvent::None
            }
            Mode::Normal => self.handle_normal_key(key),
            Mode::Visual => self.handle_visual_key(key),
        }
    }

    fn handle_insert_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char(c) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return;
                }
                self.insert_char(c);
            }
            KeyCode::Enter => self.insert_newline(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => {
                self.delete_at_cursor();
            }
            KeyCode::Tab => {
                for _ in 0..4 {
                    self.insert_char(' ');
                }
            }
            KeyCode::Left => self.move_left(),
            KeyCode::Right => {
                if self.col < self.cur_line_len() {
                    self.col += 1;
                }
                self.goal_col = self.col;
            }
            KeyCode::Up => self.move_up(),
            KeyCode::Down => self.move_down(),
            KeyCode::Home => {
                self.col = 0;
                self.goal_col = 0;
            }
            KeyCode::End => {
                self.col = self.cur_line_len();
                self.goal_col = self.col;
            }
            _ => {}
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) -> EditorEvent {
        // Multi-key sequences: g, d, y
        if let Some(p) = self.pending.take() {
            match (p, key.code) {
                ('g', KeyCode::Char('g')) => self.goto_top(),
                ('d', KeyCode::Char('d')) => {
                    self.push_undo();
                    let line = self.lines[self.row].clone();
                    self.register = line;
                    self.register_linewise = true;
                    if self.lines.len() == 1 {
                        self.lines[0].clear();
                        self.col = 0;
                    } else {
                        self.lines.remove(self.row);
                        if self.row >= self.lines.len() {
                            self.row = self.lines.len() - 1;
                        }
                        self.clamp_col();
                    }
                    return EditorEvent::Yanked(self.register.clone());
                }
                ('y', KeyCode::Char('y')) => {
                    self.register = self.lines[self.row].clone();
                    self.register_linewise = true;
                    return EditorEvent::Yanked(self.register.clone());
                }
                _ => {}
            }
            return EditorEvent::None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') => self.scroll_half(true),
                KeyCode::Char('u') => self.scroll_half(false),
                KeyCode::Char('f') => self.scroll_full(true),
                KeyCode::Char('b') => self.scroll_full(false),
                KeyCode::Char('r') => self.redo(),
                _ => {}
            }
            return EditorEvent::None;
        }

        match key.code {
            KeyCode::Char('h') | KeyCode::Left => self.move_left(),
            KeyCode::Char('j') | KeyCode::Down => self.move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_up(),
            KeyCode::Char('l') | KeyCode::Right => self.move_right(),
            KeyCode::Char('w') => self.word_forward(),
            KeyCode::Char('b') => self.word_back(),
            KeyCode::Char('e') => self.word_end(),
            KeyCode::Char('0') | KeyCode::Home => {
                self.col = 0;
                self.goal_col = 0;
            }
            KeyCode::Char('^') => self.first_non_blank(),
            KeyCode::Char('$') | KeyCode::End => {
                self.col = self.cur_line_len().saturating_sub(1);
                self.goal_col = usize::MAX / 2;
            }
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.goto_bottom(),
            KeyCode::Char('d') => self.pending = Some('d'),
            KeyCode::Char('y') => self.pending = Some('y'),
            KeyCode::Char('i') => {
                self.push_undo();
                self.mode = Mode::Insert;
            }
            KeyCode::Char('a') => {
                self.push_undo();
                self.mode = Mode::Insert;
                if self.cur_line_len() > 0 {
                    self.col += 1;
                }
            }
            KeyCode::Char('I') => {
                self.push_undo();
                self.first_non_blank();
                self.mode = Mode::Insert;
            }
            KeyCode::Char('A') => {
                self.push_undo();
                self.mode = Mode::Insert;
                self.col = self.cur_line_len();
            }
            KeyCode::Char('o') => {
                self.push_undo();
                let indent: String = self.lines[self.row]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                self.lines.insert(self.row + 1, indent.clone());
                self.row += 1;
                self.col = char_len(&indent);
                self.mode = Mode::Insert;
            }
            KeyCode::Char('O') => {
                self.push_undo();
                let indent: String = self.lines[self.row]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                self.lines.insert(self.row, indent.clone());
                self.col = char_len(&indent);
                self.mode = Mode::Insert;
            }
            KeyCode::Char('x') => {
                self.push_undo();
                if let Some(c) = self.delete_at_cursor() {
                    self.register = c.to_string();
                    self.register_linewise = false;
                }
                self.clamp_col();
            }
            KeyCode::Char('D') => {
                self.push_undo();
                let b = byte_idx(&self.lines[self.row], self.col);
                self.register = self.lines[self.row][b..].to_string();
                self.register_linewise = false;
                self.lines[self.row].truncate(b);
                self.clamp_col();
            }
            KeyCode::Char('p') => {
                self.push_undo();
                if self.register_linewise {
                    self.lines.insert(self.row + 1, self.register.clone());
                    self.row += 1;
                    self.first_non_blank();
                } else {
                    if self.cur_line_len() > 0 {
                        self.col += 1;
                    }
                    self.insert_text_at_cursor(&self.register.clone());
                    if self.col > 0 {
                        self.col -= 1;
                    }
                    self.clamp_col();
                }
            }
            KeyCode::Char('P') => {
                self.push_undo();
                if self.register_linewise {
                    self.lines.insert(self.row, self.register.clone());
                    self.first_non_blank();
                } else {
                    self.insert_text_at_cursor(&self.register.clone());
                    if self.col > 0 {
                        self.col -= 1;
                    }
                    self.clamp_col();
                }
            }
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('v') => {
                self.mode = Mode::Visual;
                self.anchor = Some((self.row, self.col));
            }
            KeyCode::Char('n') => self.search_next(true),
            KeyCode::Char('N') => self.search_next(false),
            KeyCode::Enter | KeyCode::Char('+') if self.row + 1 < self.lines.len() => {
                self.row += 1;
                self.first_non_blank();
            }
            _ => {}
        }
        EditorEvent::None
    }

    fn handle_visual_key(&mut self, key: KeyEvent) -> EditorEvent {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('d') => self.scroll_half(true),
                KeyCode::Char('u') => self.scroll_half(false),
                _ => {}
            }
            return EditorEvent::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('v') => {
                self.mode = Mode::Normal;
                self.anchor = None;
            }
            KeyCode::Char('h') | KeyCode::Left => self.move_left(),
            KeyCode::Char('j') | KeyCode::Down => self.move_down(),
            KeyCode::Char('k') | KeyCode::Up => self.move_up(),
            KeyCode::Char('l') | KeyCode::Right => self.move_right(),
            KeyCode::Char('w') => self.word_forward(),
            KeyCode::Char('b') => self.word_back(),
            KeyCode::Char('e') => self.word_end(),
            KeyCode::Char('0') => self.col = 0,
            KeyCode::Char('^') => self.first_non_blank(),
            KeyCode::Char('$') => self.col = self.cur_line_len().saturating_sub(1),
            KeyCode::Char('G') => self.goto_bottom(),
            KeyCode::Char('g') => {
                // Only gg is supported in visual mode; treat single g as pending-less gg.
                self.goto_top();
            }
            KeyCode::Char('y') => {
                let text = self.selection_text().unwrap_or_default();
                self.register = text.clone();
                self.register_linewise = false;
                self.mode = Mode::Normal;
                self.anchor = None;
                return EditorEvent::Yanked(text);
            }
            KeyCode::Char('d') | KeyCode::Char('x') => {
                if let Some((a, b)) = self.selection_range() {
                    self.push_undo();
                    let removed = self.delete_range(a, b);
                    self.register = removed.clone();
                    self.register_linewise = false;
                    self.mode = Mode::Normal;
                    self.anchor = None;
                    return EditorEvent::Yanked(removed);
                }
            }
            _ => {}
        }
        EditorEvent::None
    }
}
