use std::collections::HashMap;
use std::time::Instant;

use chrono::Local;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{lookup_pgpass, Config};
use crate::db::{
    self, ConnKey, DbEvent, DbHandle, DbRequest, Purpose, LIST_DATABASES_SQL, LIST_SCHEMAS_SQL,
};
use crate::editor::{Editor, EditorEvent, Mode};
use crate::history::{self, HistoryEntry};
use crate::results::Grid;
use crate::tree::{NodeKind, Tree};
use crate::util::{human_duration, osc52_copy, quote_ident};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Editor,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultsTab {
    Results,
    Messages,
    History,
}

#[derive(Debug, Clone)]
pub enum Overlay {
    Help,
    Picker { sel: usize },
}

#[derive(Debug, Clone)]
pub enum AfterConnect {
    SetActive,
    ListDatabases,
    ListSchemas { db: String },
    ListRelations { db: String, schema: String },
    /// Run this SQL once the connection is live (used by quick reconnect).
    RunQuery { sql: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnState {
    Connecting,
    Connected,
    Failed(String),
}

#[derive(Debug, Clone)]
pub enum MiniKind {
    Command,
    SearchEditor,
    SearchTree,
    SearchHistory,
    Password {
        conn: String,
        db: String,
        intents: Vec<AfterConnect>,
    },
}

#[derive(Debug, Clone)]
pub struct Minibuffer {
    pub kind: MiniKind,
    pub prompt: String,
    pub input: String,
    pub cursor: usize,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub time: String,
    pub level: MessageLevel,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageLevel {
    Info,
    Notice,
    Error,
}

pub struct App {
    pub cfg: Config,
    pub tabs: Vec<Editor>,
    pub active_tab: usize,
    pub tree: Tree,
    pub show_tree: bool,
    pub focus: Focus,
    pub results_tab: ResultsTab,
    pub grid: Option<Grid>,
    pub messages: Vec<Message>,
    /// Scroll offset from the bottom of the messages list.
    pub messages_scroll: usize,
    pub history: Vec<HistoryEntry>,
    pub history_sel: usize,
    pub history_filter: String,
    pub overlay: Option<Overlay>,
    pub minibuffer: Option<Minibuffer>,
    pub conns: HashMap<ConnKey, DbHandle>,
    pub conn_states: HashMap<ConnKey, ConnState>,
    pub pending: HashMap<ConnKey, Vec<AfterConnect>>,
    pub active: Option<ConnKey>,
    pub busy_since: Option<Instant>,
    pub status: Option<(String, bool)>,
    pub last_stats: Option<(u64, std::time::Duration)>,
    pub txn_open: bool,
    pw_cache: HashMap<String, String>,
    pub db_tx: UnboundedSender<DbEvent>,
    pub should_quit: bool,
    tab_counter: usize,
    pub spinner_idx: usize,
    // Layout rects captured at render time, used for mouse hit testing.
    pub tree_rect: Rect,
    pub editor_rect: Rect,
    pub editor_text_origin: (u16, u16, u16), // (x, y, gutter_width)
    pub results_rect: Rect,
    /// Pending 'g' prefix for gg in tree/results panes.
    pending_g: bool,
}

impl App {
    pub fn new(cfg: Config, db_tx: UnboundedSender<DbEvent>, startup_msg: Option<String>) -> Self {
        let names: Vec<String> = cfg.connections.iter().map(|c| c.name.clone()).collect();
        let vim = cfg.settings.vim_mode;
        let mut history = history::load();
        let limit = cfg.settings.history_limit;
        if history.len() > limit {
            history::compact(&history, limit);
            history = history.split_off(history.len() - limit);
        }
        let mut app = Self {
            tree: Tree::new(&names),
            cfg,
            tabs: vec![Editor::new("query1".into(), vim)],
            active_tab: 0,
            show_tree: true,
            focus: Focus::Editor,
            results_tab: ResultsTab::Results,
            grid: None,
            messages: Vec::new(),
            messages_scroll: 0,
            history,
            history_sel: 0,
            history_filter: String::new(),
            overlay: None,
            minibuffer: None,
            conns: HashMap::new(),
            conn_states: HashMap::new(),
            pending: HashMap::new(),
            active: None,
            busy_since: None,
            status: None,
            last_stats: None,
            txn_open: false,
            pw_cache: HashMap::new(),
            db_tx,
            should_quit: false,
            tab_counter: 1,
            spinner_idx: 0,
            tree_rect: Rect::default(),
            editor_rect: Rect::default(),
            editor_text_origin: (0, 0, 0),
            results_rect: Rect::default(),
            pending_g: false,
        };
        if let Some(msg) = startup_msg {
            app.set_status(msg, false);
        }
        if app.cfg.connections.is_empty() {
            app.set_status(
                format!(
                    "No connections configured. Edit {} and restart.",
                    crate::config::config_path().display()
                ),
                true,
            );
        }
        app
    }

    pub fn editor(&mut self) -> &mut Editor {
        &mut self.tabs[self.active_tab]
    }

    pub fn set_status(&mut self, msg: impl Into<String>, is_err: bool) {
        self.status = Some((msg.into(), is_err));
    }

    fn push_message(&mut self, level: MessageLevel, text: impl Into<String>) {
        self.messages.push(Message {
            time: Local::now().format("%H:%M:%S").to_string(),
            level,
            text: text.into(),
        });
        self.messages_scroll = 0;
        if self.messages.len() > 500 {
            self.messages.drain(..self.messages.len() - 500);
        }
    }

    pub fn vim(&self) -> bool {
        self.cfg.settings.vim_mode
    }

    // ---- events -------------------------------------------------------------

    pub fn on_term_event(&mut self, ev: Event) {
        match ev {
            Event::Key(key) => self.on_key(key),
            Event::Mouse(m) if self.cfg.settings.mouse => self.on_mouse(m),
            Event::Paste(text)
                if self.focus == Focus::Editor && self.minibuffer.is_none() =>
            {
                self.editor().paste_text(&text)
            }
            _ => {}
        }
    }

    pub fn on_tick(&mut self) {
        if self.busy_since.is_some() {
            self.spinner_idx = (self.spinner_idx + 1) % crate::util::SPINNER.len();
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
            return;
        }

        if self.overlay.is_some() {
            self.on_overlay_key(key);
            return;
        }
        if self.minibuffer.is_some() {
            self.on_minibuffer_key(key);
            return;
        }

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Global shortcuts.
        if (ctrl && key.code == KeyCode::Enter) || key.code == KeyCode::F(5) {
            self.run_query(None);
            return;
        }
        if key.code == KeyCode::F(9) {
            self.run_statement_under_cursor();
            return;
        }
        if ctrl {
            match key.code {
                KeyCode::Char('q') => {
                    self.should_quit = true;
                    return;
                }
                KeyCode::Char('c') => {
                    if self.busy_since.is_some() {
                        self.cancel_query();
                    } else {
                        self.set_status("Press Ctrl+q or :q to quit", false);
                    }
                    return;
                }
                KeyCode::Char('p') => {
                    self.overlay = Some(Overlay::Picker { sel: 0 });
                    return;
                }
                KeyCode::Char('e') => {
                    if self.show_tree && self.focus == Focus::Tree {
                        self.show_tree = false;
                        self.focus = Focus::Editor;
                    } else {
                        self.show_tree = true;
                        self.focus = Focus::Tree;
                    }
                    return;
                }
                KeyCode::Char('t') => {
                    self.new_tab();
                    return;
                }
                KeyCode::Char('w') => {
                    self.close_tab();
                    return;
                }
                KeyCode::Char('h') => {
                    self.focus_dir(Dir::Left);
                    return;
                }
                KeyCode::Char('j') => {
                    self.focus_dir(Dir::Down);
                    return;
                }
                KeyCode::Char('k') => {
                    self.focus_dir(Dir::Up);
                    return;
                }
                KeyCode::Char('l') => {
                    self.focus_dir(Dir::Right);
                    return;
                }
                KeyCode::Char('g') => {
                    self.results_tab = ResultsTab::History;
                    self.focus = Focus::Results;
                    return;
                }
                KeyCode::Char('o') => {
                    self.cycle_focus();
                    return;
                }
                _ => {}
            }
        }

        // Tab switching (outside insert mode).
        let in_insert = self.focus == Focus::Editor && self.tabs[self.active_tab].mode == Mode::Insert;
        if !in_insert {
            match key.code {
                KeyCode::Tab => {
                    self.active_tab = (self.active_tab + 1) % self.tabs.len();
                    return;
                }
                KeyCode::BackTab => {
                    self.active_tab = (self.active_tab + self.tabs.len() - 1) % self.tabs.len();
                    return;
                }
                _ => {}
            }
        }

        match self.focus {
            Focus::Editor => self.on_editor_key(key),
            Focus::Tree => self.on_tree_key(key),
            Focus::Results => self.on_results_key(key),
        }
    }

    fn on_editor_key(&mut self, key: KeyEvent) {
        let mode = self.tabs[self.active_tab].mode;
        let vim = self.vim();
        if (!vim || mode == Mode::Normal) && !key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char(':') if vim && mode == Mode::Normal => {
                    self.open_minibuffer(MiniKind::Command, ":");
                    return;
                }
                KeyCode::Char('/') if vim && mode == Mode::Normal => {
                    self.open_minibuffer(MiniKind::SearchEditor, "/");
                    return;
                }
                KeyCode::Char('?') if vim && mode == Mode::Normal => {
                    self.overlay = Some(Overlay::Help);
                    return;
                }
                _ => {}
            }
        }
        let ev = self.editor().handle_key(key);
        if let EditorEvent::Yanked(text) = ev {
            if !text.is_empty() {
                osc52_copy(&text);
                self.set_status("Yanked to clipboard", false);
            }
        }
    }

    fn on_tree_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if self.pending_g {
            self.pending_g = false;
            if key.code == KeyCode::Char('g') {
                self.tree.goto_top();
            }
            return;
        }
        if ctrl {
            match key.code {
                KeyCode::Char('d') => {
                    let h = self.tree.view_h;
                    self.tree.move_sel((h / 2).max(1) as i32);
                }
                KeyCode::Char('u') => {
                    let h = self.tree.view_h;
                    self.tree.move_sel(-((h / 2).max(1) as i32));
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.tree.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.tree.move_sel(-1),
            KeyCode::Char('h') | KeyCode::Left => self.tree.collapse_or_parent(),
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Char('G') => self.tree.goto_bottom(),
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => {
                self.activate_tree_node()
            }
            KeyCode::Char('r') => self.refresh_tree_node(),
            KeyCode::Char('/') => self.open_minibuffer(MiniKind::SearchTree, "/"),
            KeyCode::Char('n') => self.tree.search_jump(true),
            KeyCode::Char('N') => self.tree.search_jump(false),
            KeyCode::Char('?') => self.overlay = Some(Overlay::Help),
            KeyCode::Char(':') => self.open_minibuffer(MiniKind::Command, ":"),
            KeyCode::Esc => self.focus = Focus::Editor,
            _ => {}
        }
    }

    fn on_results_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char(']') => {
                self.results_tab = match self.results_tab {
                    ResultsTab::Results => ResultsTab::Messages,
                    ResultsTab::Messages => ResultsTab::History,
                    ResultsTab::History => ResultsTab::Results,
                };
                return;
            }
            KeyCode::Char('[') => {
                self.results_tab = match self.results_tab {
                    ResultsTab::Results => ResultsTab::History,
                    ResultsTab::Messages => ResultsTab::Results,
                    ResultsTab::History => ResultsTab::Messages,
                };
                return;
            }
            KeyCode::Esc => {
                self.focus = Focus::Editor;
                return;
            }
            KeyCode::Char(':') => {
                self.open_minibuffer(MiniKind::Command, ":");
                return;
            }
            KeyCode::Char('?') => {
                self.overlay = Some(Overlay::Help);
                return;
            }
            _ => {}
        }
        match self.results_tab {
            ResultsTab::Results => self.on_grid_key(key, ctrl),
            ResultsTab::Messages => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.messages_scroll = self.messages_scroll.saturating_sub(1)
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.messages_scroll =
                        (self.messages_scroll + 1).min(self.messages.len().saturating_sub(1))
                }
                KeyCode::Char('G') => self.messages_scroll = 0,
                _ => {}
            },
            ResultsTab::History => self.on_history_key(key),
        }
    }

    fn on_grid_key(&mut self, key: KeyEvent, ctrl: bool) {
        if self.pending_g {
            self.pending_g = false;
            if key.code == KeyCode::Char('g') {
                if let Some(g) = &mut self.grid {
                    g.goto_top();
                }
            }
            return;
        }
        let Some(grid) = &mut self.grid else { return };
        if ctrl {
            match key.code {
                KeyCode::Char('d') => grid.half_page(true),
                KeyCode::Char('u') => grid.half_page(false),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => grid.move_sel(1, 0),
            KeyCode::Char('k') | KeyCode::Up => grid.move_sel(-1, 0),
            KeyCode::Char('h') | KeyCode::Left => grid.move_sel(0, -1),
            KeyCode::Char('l') | KeyCode::Right => grid.move_sel(0, 1),
            KeyCode::Char('0') | KeyCode::Home => grid.first_col(),
            KeyCode::Char('$') | KeyCode::End => grid.last_col(),
            KeyCode::Char('g') => self.pending_g = true,
            KeyCode::Char('G') => grid.goto_bottom(),
            KeyCode::PageDown => grid.half_page(true),
            KeyCode::PageUp => grid.half_page(false),
            KeyCode::Char('y') => {
                if let Some(text) = grid.selected_cell_text() {
                    osc52_copy(&text);
                    self.set_status("Cell copied to clipboard", false);
                }
            }
            KeyCode::Char('Y') => {
                if let Some(text) = grid.selected_row_tsv() {
                    osc52_copy(&text);
                    self.set_status("Row copied to clipboard", false);
                }
            }
            _ => {}
        }
    }

    fn filtered_history(&self) -> Vec<usize> {
        let filter = self.history_filter.to_lowercase();
        let mut idx: Vec<usize> = (0..self.history.len())
            .filter(|&i| {
                filter.is_empty() || self.history[i].sql.to_lowercase().contains(&filter)
            })
            .collect();
        idx.reverse(); // newest first
        idx
    }

    fn on_history_key(&mut self, key: KeyEvent) {
        let filtered = self.filtered_history();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.history_sel =
                    (self.history_sel + 1).min(filtered.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.history_sel = self.history_sel.saturating_sub(1);
            }
            KeyCode::Char('G') => {
                self.history_sel = filtered.len().saturating_sub(1);
            }
            KeyCode::Char('g') => {
                self.history_sel = 0;
            }
            KeyCode::Char('/') => {
                self.open_minibuffer(MiniKind::SearchHistory, "filter: ");
            }
            KeyCode::Enter => {
                if let Some(&i) = filtered.get(self.history_sel) {
                    let sql = self.history[i].sql.clone();
                    self.load_sql_into_editor(&sql);
                    self.set_status("History entry loaded into editor", false);
                }
            }
            KeyCode::Char('r') => {
                if let Some(&i) = filtered.get(self.history_sel) {
                    let sql = self.history[i].sql.clone();
                    self.run_query(Some(sql));
                }
            }
            KeyCode::Char('y') => {
                if let Some(&i) = filtered.get(self.history_sel) {
                    osc52_copy(&self.history[i].sql.clone());
                    self.set_status("Query copied to clipboard", false);
                }
            }
            _ => {}
        }
    }

    fn load_sql_into_editor(&mut self, sql: &str) {
        if !self.tabs[self.active_tab].is_empty() {
            self.new_tab();
        }
        self.editor().set_text(sql);
        self.focus = Focus::Editor;
    }

    // ---- overlay / minibuffer ----------------------------------------------

    fn on_overlay_key(&mut self, key: KeyEvent) {
        let overlay = self.overlay.clone();
        match overlay {
            Some(Overlay::Help) => {
                self.overlay = None;
            }
            Some(Overlay::Picker { sel }) => match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Char('j') | KeyCode::Down => {
                    let max = self.cfg.connections.len().saturating_sub(1);
                    self.overlay = Some(Overlay::Picker {
                        sel: (sel + 1).min(max),
                    });
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.overlay = Some(Overlay::Picker {
                        sel: sel.saturating_sub(1),
                    });
                }
                KeyCode::Enter => {
                    self.overlay = None;
                    let order = self.connections_by_recency();
                    if let Some(cc) = order.get(sel).and_then(|&i| self.cfg.connections.get(i)) {
                        let name = cc.name.clone();
                        let db = cc.dbname();
                        self.ensure_connected(
                            &name,
                            &db,
                            vec![AfterConnect::SetActive, AfterConnect::ListDatabases],
                        );
                    }
                }
                _ => {}
            },
            None => {}
        }
    }

    fn open_minibuffer(&mut self, kind: MiniKind, prompt: &str) {
        self.minibuffer = Some(Minibuffer {
            kind,
            prompt: prompt.to_string(),
            input: String::new(),
            cursor: 0,
        });
    }

    fn on_minibuffer_key(&mut self, key: KeyEvent) {
        let Some(mb) = &mut self.minibuffer else { return };
        match key.code {
            KeyCode::Esc => {
                self.minibuffer = None;
            }
            KeyCode::Enter => {
                let mb = self.minibuffer.take().unwrap();
                self.submit_minibuffer(mb);
            }
            KeyCode::Backspace => {
                if mb.cursor > 0 {
                    let idx = mb
                        .input
                        .char_indices()
                        .nth(mb.cursor - 1)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    mb.input.remove(idx);
                    mb.cursor -= 1;
                } else if mb.input.is_empty() {
                    self.minibuffer = None;
                }
            }
            KeyCode::Left => mb.cursor = mb.cursor.saturating_sub(1),
            KeyCode::Right => mb.cursor = (mb.cursor + 1).min(mb.input.chars().count()),
            KeyCode::Home => mb.cursor = 0,
            KeyCode::End => mb.cursor = mb.input.chars().count(),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                mb.input.clear();
                mb.cursor = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                let idx = mb
                    .input
                    .char_indices()
                    .nth(mb.cursor)
                    .map(|(i, _)| i)
                    .unwrap_or(mb.input.len());
                mb.input.insert(idx, c);
                mb.cursor += 1;
            }
            _ => {}
        }
    }

    fn submit_minibuffer(&mut self, mb: Minibuffer) {
        match mb.kind {
            MiniKind::Command => self.run_command(mb.input.trim().to_string()),
            MiniKind::SearchEditor => {
                self.editor().search(&mb.input);
            }
            MiniKind::SearchTree => {
                self.tree.search = Some(mb.input);
                self.tree.search_jump(true);
            }
            MiniKind::SearchHistory => {
                self.history_filter = mb.input;
                self.history_sel = 0;
            }
            MiniKind::Password { conn, db, intents } => {
                self.pw_cache.insert(conn.clone(), mb.input.clone());
                self.connect_with_password(&conn, &db, Some(mb.input), intents);
            }
        }
    }

    // ---- commands -----------------------------------------------------------

    fn run_command(&mut self, cmd: String) {
        let mut parts = cmd.splitn(2, ' ');
        let head = parts.next().unwrap_or("").to_lowercase();
        let rest = parts.next().unwrap_or("").trim().to_string();
        match head.as_str() {
            "" => {}
            "q" | "quit" => self.should_quit = true,
            "run" => self.run_query(None),
            "stmt" => self.run_statement_under_cursor(),
            "explain" => {
                let analyze = rest.eq_ignore_ascii_case("analyze");
                // EXPLAIN takes exactly one statement, so never send the whole
                // buffer: use the selection, else the statement under cursor.
                let ed = &self.tabs[self.active_tab];
                let target = if ed.mode == Mode::Visual {
                    ed.selection_text()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                } else {
                    let text = ed.text();
                    crate::sqlsplit::statement_at(&text, ed.cursor_offset())
                };
                match target {
                    Some(sql) => {
                        let prefix = if analyze {
                            "EXPLAIN (ANALYZE, BUFFERS) "
                        } else {
                            "EXPLAIN "
                        };
                        self.run_query(Some(format!("{prefix}{}", sql.trim_end_matches(';'))));
                    }
                    None => self.set_status("Nothing to explain", true),
                }
            }
            "export" => {
                if rest.is_empty() {
                    self.set_status("Usage: :export <path.csv>", true);
                } else {
                    self.export_csv(&rest);
                }
            }
            "begin" | "commit" | "rollback" => {
                self.run_query(Some(head.to_uppercase()));
            }
            "connect" => {
                if rest.is_empty() {
                    self.overlay = Some(Overlay::Picker { sel: 0 });
                } else if let Some(cc) =
                    self.cfg.connections.iter().find(|c| c.name == rest).cloned()
                {
                    self.ensure_connected(
                        &cc.name,
                        &cc.dbname(),
                        vec![AfterConnect::SetActive, AfterConnect::ListDatabases],
                    );
                } else {
                    self.set_status(format!("Unknown connection '{rest}'"), true);
                }
            }
            "reconnect" => match self.active.clone() {
                Some((conn, db)) => {
                    self.conns.remove(&(conn.clone(), db.clone()));
                    self.conn_states.remove(&(conn.clone(), db.clone()));
                    self.ensure_connected(&conn, &db, vec![AfterConnect::SetActive]);
                }
                None => self.set_status("Not connected", true),
            },
            "disconnect" => {
                if let Some(key) = self.active.take() {
                    self.conns.remove(&key);
                    self.conn_states.remove(&key);
                    self.set_status(format!("Disconnected from {}/{}", key.0, key.1), false);
                } else {
                    self.set_status("Not connected", true);
                }
            }
            "new" => self.new_tab(),
            "close" => self.close_tab(),
            "clear" => {
                self.grid = None;
                self.messages.clear();
                self.last_stats = None;
            }
            "history" => {
                self.results_tab = ResultsTab::History;
                self.focus = Focus::Results;
            }
            "help" => self.overlay = Some(Overlay::Help),
            _ => self.set_status(format!("Unknown command: {head}"), true),
        }
    }

    fn export_csv(&mut self, path: &str) {
        let Some(grid) = &self.grid else {
            self.set_status("No results to export", true);
            return;
        };
        let csv = grid.to_csv();
        let path = shellexpand_home(path);
        match std::fs::write(&path, csv) {
            Ok(()) => self.set_status(
                format!("Exported {} rows to {}", grid.rs.rows.len(), path),
                false,
            ),
            Err(e) => self.set_status(format!("Export failed: {e}"), true),
        }
    }

    // ---- tabs ---------------------------------------------------------------

    fn new_tab(&mut self) {
        self.tab_counter += 1;
        let name = format!("query{}", self.tab_counter);
        self.tabs.push(Editor::new(name, self.vim()));
        self.active_tab = self.tabs.len() - 1;
        self.focus = Focus::Editor;
    }

    fn close_tab(&mut self) {
        if self.tabs.len() == 1 {
            let vim = self.vim();
            self.tabs[0] = Editor::new("query1".into(), vim);
            self.tab_counter = 1;
        } else {
            self.tabs.remove(self.active_tab);
            if self.active_tab >= self.tabs.len() {
                self.active_tab = self.tabs.len() - 1;
            }
        }
    }

    /// Directional pane movement, tmux/vim-style. The explorer spans the full
    /// height on the left; the editor sits above the results on the right.
    ///
    ///     +----------+----------+
    ///     |          |  Editor  |
    ///     | Explorer +----------+
    ///     |          |  Results |
    ///     +----------+----------+
    ///
    /// Moving left onto a hidden explorer reveals it, so the key never feels
    /// dead.
    fn focus_dir(&mut self, dir: Dir) {
        self.focus = match (self.focus, dir) {
            (Focus::Editor | Focus::Results, Dir::Left) => {
                self.show_tree = true;
                Focus::Tree
            }
            (Focus::Tree, Dir::Right) => Focus::Editor,
            (Focus::Editor, Dir::Down) => Focus::Results,
            (Focus::Results, Dir::Up) => Focus::Editor,
            // Everything else is an edge: stay put rather than wrap around.
            (focus, _) => focus,
        };
    }

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Editor => Focus::Results,
            Focus::Results => {
                if self.show_tree {
                    Focus::Tree
                } else {
                    Focus::Editor
                }
            }
            Focus::Tree => Focus::Editor,
        };
    }

    // ---- query execution ----------------------------------------------------

    fn current_sql(&mut self) -> Option<String> {
        let ed = &mut self.tabs[self.active_tab];
        let sql = if ed.mode == Mode::Visual {
            ed.selection_text()
        } else {
            Some(ed.text())
        };
        let sql = sql.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        if sql.is_none() {
            self.set_status("Nothing to execute", true);
        }
        sql
    }

    /// Execute only the statement the cursor sits in.
    fn run_statement_under_cursor(&mut self) {
        let ed = &self.tabs[self.active_tab];
        let text = ed.text();
        match crate::sqlsplit::statement_at(&text, ed.cursor_offset()) {
            Some(stmt) => self.run_query(Some(stmt)),
            None => self.set_status("No statement under cursor", true),
        }
    }

    pub fn run_query(&mut self, sql_override: Option<String>) {
        if self.busy_since.is_some() {
            self.set_status("A query is already running (Ctrl+c to cancel)", true);
            return;
        }
        let Some(active) = self.active.clone() else {
            self.set_status("Not connected — press Ctrl+p to pick a connection", true);
            return;
        };
        let sql = match sql_override {
            Some(s) => s,
            None => match self.current_sql() {
                Some(s) => s,
                None => return,
            },
        };
        // Leave visual mode after grabbing the selection.
        let ed = &mut self.tabs[self.active_tab];
        if ed.mode == Mode::Visual {
            ed.mode = Mode::Normal;
            ed.anchor = None;
        }
        let Some(handle) = self.conns.get(&active) else {
            // The connection dropped (server restart, idle timeout). Rebuild it
            // and run the query as soon as it is live.
            self.set_status("Reconnecting…", false);
            let (conn, db) = (active.0.clone(), active.1.clone());
            self.ensure_connected(
                &conn,
                &db,
                vec![AfterConnect::SetActive, AfterConnect::RunQuery { sql }],
            );
            return;
        };
        handle.send(DbRequest::Query {
            sql: sql.clone(),
            purpose: Purpose::User { sql },
        });
        self.busy_since = Some(Instant::now());
        self.status = None;
    }

    fn cancel_query(&mut self) {
        if let Some(active) = &self.active {
            if let Some(handle) = self.conns.get(active) {
                handle.cancel_running();
                self.set_status("Cancel requested", false);
            }
        }
    }

    // ---- connections --------------------------------------------------------

    fn conn_config(&self, name: &str) -> Option<crate::config::ConnectionConfig> {
        self.cfg.connections.iter().find(|c| c.name == name).cloned()
    }

    pub fn ensure_connected(&mut self, conn: &str, db: &str, intents: Vec<AfterConnect>) {
        let key: ConnKey = (conn.to_string(), db.to_string());
        match self.conn_states.get(&key) {
            Some(ConnState::Connected) => {
                for intent in intents {
                    self.apply_intent(&key, intent);
                }
            }
            Some(ConnState::Connecting) => {
                self.pending.entry(key).or_default().extend(intents);
            }
            _ => {
                let Some(cc) = self.conn_config(conn) else {
                    self.set_status(format!("Unknown connection '{conn}'"), true);
                    return;
                };
                // Resolve password: config -> session cache -> ~/.pgpass.
                // If none is found we still try: trust/peer auth need no
                // password, and the server tells us when it does — that comes
                // back as ConnectFailed and prompts there.
                let password = cc
                    .password
                    .clone()
                    .or_else(|| self.pw_cache.get(conn).cloned())
                    .or_else(|| lookup_pgpass(&cc.host, cc.port, db, &cc.user));
                self.connect_with_password(conn, db, password, intents);
            }
        }
    }

    fn connect_with_password(
        &mut self,
        conn: &str,
        db: &str,
        password: Option<String>,
        intents: Vec<AfterConnect>,
    ) {
        let Some(cc) = self.conn_config(conn) else {
            return;
        };
        let key: ConnKey = (conn.to_string(), db.to_string());
        let handle = db::spawn_connection(
            key.clone(),
            cc,
            db.to_string(),
            password,
            self.cfg.settings.row_limit,
            self.db_tx.clone(),
        );
        self.conns.insert(key.clone(), handle);
        self.conn_states.insert(key.clone(), ConnState::Connecting);
        self.pending.entry(key.clone()).or_default().extend(intents);
        self.set_status(format!("Connecting to {conn}/{db}…"), false);
    }

    fn apply_intent(&mut self, key: &ConnKey, intent: AfterConnect) {
        match intent {
            AfterConnect::SetActive => {
                self.active = Some(key.clone());
                self.txn_open = false;
            }
            AfterConnect::ListDatabases => {
                if let Some(h) = self.conns.get(key) {
                    h.send(DbRequest::Query {
                        sql: LIST_DATABASES_SQL.to_string(),
                        purpose: Purpose::ListDatabases,
                    });
                }
            }
            AfterConnect::ListSchemas { db } => {
                if let Some(h) = self.conns.get(key) {
                    h.send(DbRequest::Query {
                        sql: LIST_SCHEMAS_SQL.to_string(),
                        purpose: Purpose::ListSchemas { db },
                    });
                }
            }
            AfterConnect::ListRelations { db, schema } => {
                if let Some(h) = self.conns.get(key) {
                    h.send(DbRequest::Query {
                        sql: db::list_relations_sql(&schema),
                        purpose: Purpose::ListRelations { db, schema },
                    });
                }
            }
            AfterConnect::RunQuery { sql } => {
                if let Some(h) = self.conns.get(key) {
                    h.send(DbRequest::Query {
                        sql: sql.clone(),
                        purpose: Purpose::User { sql },
                    });
                    self.busy_since = Some(Instant::now());
                    self.status = None;
                }
            }
        }
    }

    // ---- tree actions -------------------------------------------------------

    fn activate_tree_node(&mut self) {
        let Some(row) = self.tree.selected_row() else {
            return;
        };
        match row.kind.clone() {
            NodeKind::Server { conn } => {
                let Some(cc) = self.conn_config(&conn) else {
                    return;
                };
                let db = cc.dbname();
                if row.loaded {
                    if let Some(node) = self.tree.node_mut(&row.path) {
                        node.expanded = !node.expanded;
                    }
                    self.tree.reveal_children();
                    self.ensure_connected(&conn, &db, vec![AfterConnect::SetActive]);
                } else {
                    self.ensure_connected(
                        &conn,
                        &db,
                        vec![AfterConnect::SetActive, AfterConnect::ListDatabases],
                    );
                }
            }
            NodeKind::Database { conn, db } => {
                if row.loaded {
                    if let Some(node) = self.tree.node_mut(&row.path) {
                        node.expanded = !node.expanded;
                    }
                    self.tree.reveal_children();
                    self.ensure_connected(&conn, &db, vec![AfterConnect::SetActive]);
                } else {
                    self.ensure_connected(
                        &conn,
                        &db,
                        vec![
                            AfterConnect::SetActive,
                            AfterConnect::ListSchemas { db: db.clone() },
                        ],
                    );
                }
            }
            NodeKind::Schema { conn, db, schema } => {
                if row.loaded {
                    if let Some(node) = self.tree.node_mut(&row.path) {
                        node.expanded = !node.expanded;
                    }
                    self.tree.reveal_children();
                } else {
                    self.ensure_connected(
                        &conn,
                        &db,
                        vec![AfterConnect::ListRelations {
                            db: db.clone(),
                            schema: schema.clone(),
                        }],
                    );
                }
            }
            NodeKind::TablesFolder { .. } | NodeKind::ViewsFolder { .. } => {
                if let Some(node) = self.tree.node_mut(&row.path) {
                    node.expanded = !node.expanded;
                }
                self.tree.reveal_children();
            }
            NodeKind::Table {
                conn,
                db,
                schema,
                name,
            }
            | NodeKind::View {
                conn,
                db,
                schema,
                name,
            } => {
                let sql = format!(
                    "SELECT *\nFROM {}.{}\nLIMIT 100;",
                    quote_ident(&schema),
                    quote_ident(&name)
                );
                self.ensure_connected(&conn, &db, vec![AfterConnect::SetActive]);
                self.load_sql_into_editor(&sql);
            }
        }
    }

    fn refresh_tree_node(&mut self) {
        let Some(row) = self.tree.selected_row() else {
            return;
        };
        match row.kind.clone() {
            NodeKind::Server { conn } => {
                let Some(cc) = self.conn_config(&conn) else {
                    return;
                };
                self.ensure_connected(&conn, &cc.dbname(), vec![AfterConnect::ListDatabases]);
            }
            NodeKind::Database { conn, db } => {
                self.ensure_connected(
                    &conn,
                    &db,
                    vec![AfterConnect::ListSchemas { db: db.clone() }],
                );
            }
            NodeKind::Schema { conn, db, schema }
            | NodeKind::TablesFolder { conn, db, schema }
            | NodeKind::ViewsFolder { conn, db, schema } => {
                self.ensure_connected(
                    &conn,
                    &db,
                    vec![AfterConnect::ListRelations {
                        db: db.clone(),
                        schema: schema.clone(),
                    }],
                );
            }
            _ => {}
        }
    }

    // ---- db events ----------------------------------------------------------

    pub fn on_db_event(&mut self, ev: DbEvent) {
        match ev {
            DbEvent::Connected { key } => {
                self.conn_states.insert(key.clone(), ConnState::Connected);
                self.push_message(
                    MessageLevel::Info,
                    format!("Connected to {}/{}", key.0, key.1),
                );
                self.set_status(format!("Connected to {}/{}", key.0, key.1), false);
                if let Some(intents) = self.pending.remove(&key) {
                    for intent in intents {
                        self.apply_intent(&key, intent);
                    }
                }
            }
            DbEvent::ConnectFailed { key, error } => {
                self.conns.remove(&key);
                let intents = self.pending.remove(&key).unwrap_or_default();
                let missing = db::is_password_missing(&error);
                let rejected = db::is_password_rejected(&error);
                if missing || rejected {
                    // A bad cached password shouldn't poison future attempts.
                    self.pw_cache.remove(&key.0);
                    self.conn_states.remove(&key);
                    if rejected {
                        self.push_message(MessageLevel::Error, error.clone());
                        self.set_status("Authentication failed — try again", true);
                    }
                    let user = self
                        .conn_config(&key.0)
                        .map(|c| c.user)
                        .unwrap_or_else(|| "?".into());
                    let prompt = format!("password for {user}@{}: ", key.0);
                    self.open_minibuffer(
                        MiniKind::Password {
                            conn: key.0.clone(),
                            db: key.1.clone(),
                            intents,
                        },
                        &prompt,
                    );
                    return;
                }
                self.conn_states
                    .insert(key.clone(), ConnState::Failed(error.clone()));
                self.push_message(
                    MessageLevel::Error,
                    format!("Connection to {}/{} failed: {error}", key.0, key.1),
                );
                self.set_status(format!("Connect failed: {error}"), true);
                self.results_tab = ResultsTab::Messages;
            }
            DbEvent::Closed { key, error } => {
                if self.conns.remove(&key).is_some() {
                    self.conn_states.remove(&key);
                    let msg = match &error {
                        Some(e) => format!("Connection {}/{} closed: {e}", key.0, key.1),
                        None => format!("Connection {}/{} closed", key.0, key.1),
                    };
                    self.push_message(MessageLevel::Error, msg);
                    if self.active.as_ref() == Some(&key) {
                        self.busy_since = None;
                        self.set_status(
                            format!("Connection to {}/{} lost", key.0, key.1),
                            true,
                        );
                    }
                }
            }
            DbEvent::Notice {
                key,
                severity,
                message,
            } => {
                self.push_message(
                    MessageLevel::Notice,
                    format!("[{}/{}] {severity}: {message}", key.0, key.1),
                );
            }
            DbEvent::QueryDone {
                key,
                purpose,
                outcome,
                elapsed,
            } => match purpose {
                Purpose::User { sql } => {
                    self.busy_since = None;
                    self.finish_user_query(&key, sql, outcome, elapsed);
                }
                Purpose::ListDatabases => {
                    if let Ok(out) = outcome {
                        let names = first_column(&out);
                        self.tree.set_databases(&key.0, names);
                    }
                }
                Purpose::ListSchemas { db } => {
                    if let Ok(out) = outcome {
                        let names = first_column(&out);
                        self.tree.set_schemas(&key.0, &db, names);
                        self.tree.reveal_children();
                    }
                }
                Purpose::ListRelations { db, schema } => {
                    if let Ok(out) = outcome {
                        let mut tables = Vec::new();
                        let mut views = Vec::new();
                        if let Some(rs) = out.result_sets.first() {
                            for row in &rs.rows {
                                let name = row
                                    .first()
                                    .and_then(|c| c.clone())
                                    .unwrap_or_default();
                                let kind = row
                                    .get(1)
                                    .and_then(|c| c.clone())
                                    .unwrap_or_default();
                                match kind.as_str() {
                                    "v" | "m" => views.push(name),
                                    _ => tables.push(name),
                                }
                            }
                        }
                        self.tree.set_relations(&key.0, &db, &schema, tables, views);
                        self.tree.reveal_children();
                    }
                }
            },
        }
    }

    fn finish_user_query(
        &mut self,
        key: &ConnKey,
        sql: String,
        outcome: Result<db::QueryOutput, String>,
        elapsed: std::time::Duration,
    ) {
        match outcome {
            Ok(out) => {
                // Follow the transaction across every statement in the batch.
                for kw in crate::sqlsplit::leading_keywords(&sql) {
                    match kw.as_str() {
                        "BEGIN" | "START" => self.txn_open = true,
                        "COMMIT" | "ROLLBACK" | "END" => self.txn_open = false,
                        _ => {}
                    }
                }
                let display = out
                    .result_sets
                    .iter()
                    .rev()
                    .find(|rs| !rs.columns.is_empty())
                    .cloned();
                let mut total: u64 = 0;
                for (i, n) in out.commands.iter().enumerate() {
                    total = *n;
                    self.push_message(
                        MessageLevel::Info,
                        format!("Statement {}: OK, {} row(s), {}", i + 1, n, human_duration(elapsed)),
                    );
                }
                let rows_shown = display.as_ref().map(|rs| rs.total_rows).unwrap_or(total);
                self.last_stats = Some((rows_shown, elapsed));
                self.history_append(&sql, key, true, elapsed, Some(rows_shown));
                match display {
                    Some(rs) => {
                        if rs.truncated {
                            let shown = rs.rows.len();
                            self.push_message(
                                MessageLevel::Notice,
                                format!(
                                    "Result truncated to {shown} rows (row_limit); total {}",
                                    rs.total_rows
                                ),
                            );
                        }
                        self.grid = Some(Grid::new(rs));
                        self.results_tab = ResultsTab::Results;
                    }
                    None => {
                        self.results_tab = ResultsTab::Messages;
                    }
                }
                // The status bar already shows rows/time from last_stats.
                self.status = None;
            }
            Err(e) => {
                self.push_message(MessageLevel::Error, e.clone());
                self.history_append(&sql, key, false, elapsed, None);
                self.results_tab = ResultsTab::Messages;
                self.set_status(
                    e.lines().next().unwrap_or("Query failed").to_string(),
                    true,
                );
            }
        }
    }

    fn history_append(
        &mut self,
        sql: &str,
        key: &ConnKey,
        ok: bool,
        elapsed: std::time::Duration,
        rows: Option<u64>,
    ) {
        let entry = HistoryEntry {
            sql: sql.to_string(),
            connection: key.0.clone(),
            database: key.1.clone(),
            timestamp: Local::now(),
            duration_ms: Some(elapsed.as_millis() as u64),
            ok,
            rows,
        };
        history::append(&entry);
        self.history.push(entry);
    }

    // ---- mouse --------------------------------------------------------------

    fn on_mouse(&mut self, m: MouseEvent) {
        let pos = (m.column, m.row);
        let hit = |r: Rect| -> bool {
            pos.0 >= r.x && pos.0 < r.x + r.width && pos.1 >= r.y && pos.1 < r.y + r.height
        };
        match m.kind {
            MouseEventKind::Down(_) => {
                if self.show_tree && hit(self.tree_rect) {
                    self.focus = Focus::Tree;
                    let row = (m.row.saturating_sub(self.tree_rect.y + 1)) as usize
                        + self.tree.scroll;
                    let count = self.tree.flatten().len();
                    if row < count {
                        self.tree.selected = row;
                    }
                } else if hit(self.editor_rect) {
                    self.focus = Focus::Editor;
                    let (ox, oy, gutter) = self.editor_text_origin;
                    let ed = self.editor();
                    if m.row >= oy && m.column >= ox + gutter {
                        let row = (m.row - oy) as usize + ed.scroll_row;
                        let col = (m.column - ox - gutter) as usize + ed.scroll_col;
                        if row < ed.lines.len() {
                            ed.row = row;
                            ed.col = col.min(ed.lines[row].chars().count());
                            let insert = ed.mode == Mode::Insert;
                            if !insert {
                                ed.col = ed
                                    .col
                                    .min(ed.lines[row].chars().count().saturating_sub(1));
                            }
                        }
                    }
                } else if hit(self.results_rect) {
                    self.focus = Focus::Results;
                }
            }
            MouseEventKind::ScrollDown => self.mouse_scroll(pos, 3),
            MouseEventKind::ScrollUp => self.mouse_scroll(pos, -3),
            _ => {}
        }
    }

    fn mouse_scroll(&mut self, pos: (u16, u16), delta: i32) {
        let hit = |r: Rect| -> bool {
            pos.0 >= r.x && pos.0 < r.x + r.width && pos.1 >= r.y && pos.1 < r.y + r.height
        };
        if self.show_tree && hit(self.tree_rect) {
            self.tree.move_sel(delta);
        } else if hit(self.editor_rect) {
            let ed = self.editor();
            if delta > 0 {
                for _ in 0..delta {
                    if ed.row + 1 < ed.lines.len() {
                        ed.row += 1;
                    }
                }
            } else {
                ed.row = ed.row.saturating_sub((-delta) as usize);
            }
            let max = ed.lines[ed.row].chars().count();
            if ed.col > max {
                ed.col = max;
            }
        } else if hit(self.results_rect) {
            match self.results_tab {
                ResultsTab::Results => {
                    if let Some(g) = &mut self.grid {
                        g.move_sel(delta, 0);
                    }
                }
                ResultsTab::Messages => {
                    if delta > 0 {
                        self.messages_scroll = self.messages_scroll.saturating_sub(delta as usize);
                    } else {
                        self.messages_scroll = (self.messages_scroll + (-delta) as usize)
                            .min(self.messages.len().saturating_sub(1));
                    }
                }
                ResultsTab::History => {
                    let len = self.filtered_history().len();
                    if delta > 0 {
                        self.history_sel =
                            (self.history_sel + delta as usize).min(len.saturating_sub(1));
                    } else {
                        self.history_sel = self.history_sel.saturating_sub((-delta) as usize);
                    }
                }
            }
        }
    }

    /// Connection indices ordered most-recently-used first, using the last
    /// query timestamp per connection. Unused connections keep config order.
    pub fn connections_by_recency(&self) -> Vec<usize> {
        let mut last_used: HashMap<&str, chrono::DateTime<Local>> = HashMap::new();
        for e in &self.history {
            let slot = last_used.entry(e.connection.as_str()).or_insert(e.timestamp);
            if e.timestamp > *slot {
                *slot = e.timestamp;
            }
        }
        let mut idx: Vec<usize> = (0..self.cfg.connections.len()).collect();
        idx.sort_by(|&a, &b| {
            let ta = last_used.get(self.cfg.connections[a].name.as_str());
            let tb = last_used.get(self.cfg.connections[b].name.as_str());
            match (ta, tb) {
                (Some(x), Some(y)) => y.cmp(x),               // newest first
                (Some(_), None) => std::cmp::Ordering::Less,  // used before unused
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.cmp(&b),                    // else config order
            }
        });
        idx
    }

    pub fn history_view(&self) -> (Vec<usize>, usize) {
        let filtered = self.filtered_history();
        let sel = self.history_sel.min(filtered.len().saturating_sub(1));
        (filtered, sel)
    }
}

fn first_column(out: &db::QueryOutput) -> Vec<String> {
    out.result_sets
        .first()
        .map(|rs| {
            rs.rows
                .iter()
                .filter_map(|r| r.first().and_then(|c| c.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn shellexpand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    path.to_string()
}
