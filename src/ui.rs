use ratatui::{
    layout::{Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Tabs},
    Frame,
};
use unicode_width::UnicodeWidthChar;

use crate::app::{App, ConnState, Focus, MessageLevel, MiniKind, Overlay, ResultsTab};
use crate::editor::Mode;
use crate::highlight::{line_styles, LexState};
use crate::tree::NodeKind;
use crate::util::{centered_rect, human_duration, SPINNER};

const ACCENT: Color = Color::Cyan;
const DIM: Color = Color::DarkGray;
const SEL_BG: Color = Color::Rgb(50, 60, 80);

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(DIM)
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(1)])
        .split(f.area());
    let main = root[0];
    let status_area = root[1];

    let (tree_area, right_area) = if app.show_tree {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(30), Constraint::Min(20)])
            .split(main);
        (Some(cols[0]), cols[1])
    } else {
        (None, main)
    };

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Percentage(38),
        ])
        .split(right_area);

    if let Some(area) = tree_area {
        app.tree_rect = area;
        draw_tree(f, app, area);
    } else {
        app.tree_rect = Rect::default();
    }
    draw_tab_bar(f, app, right[0]);
    app.editor_rect = right[1];
    draw_editor(f, app, right[1]);
    app.results_rect = right[2];
    draw_results(f, app, right[2]);
    draw_status(f, app, status_area);

    match app.overlay.clone() {
        Some(Overlay::Help) => draw_help(f),
        Some(Overlay::Picker { sel }) => draw_picker(f, app, sel),
        None => {}
    }
}

// ---- explorer tree ----------------------------------------------------------

fn draw_tree(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Tree;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Explorer ")
        .border_style(border_style(focused));
    let inner = block.inner(area);
    f.render_widget(block, area);

    app.tree.view_h = inner.height as usize;
    app.tree.ensure_visible();

    let flat = app.tree.flatten();
    let mut lines: Vec<Line> = Vec::new();
    for (i, row) in flat
        .iter()
        .enumerate()
        .skip(app.tree.scroll)
        .take(inner.height as usize)
    {
        let indent = "  ".repeat(row.depth);
        let arrow = if row.is_leaf {
            "  "
        } else if row.expanded {
            "▾ "
        } else {
            "▸ "
        };
        let (icon, icon_style, label_style) = match &row.kind {
            NodeKind::Server { conn } => {
                let connected = app
                    .conn_states
                    .iter()
                    .any(|((c, _), s)| c == conn && *s == ConnState::Connected);
                let connecting = app
                    .conn_states
                    .iter()
                    .any(|((c, _), s)| c == conn && *s == ConnState::Connecting);
                let (ic, st) = if connected {
                    ("● ", Style::default().fg(Color::Green))
                } else if connecting {
                    ("◌ ", Style::default().fg(Color::Yellow))
                } else {
                    ("○ ", Style::default().fg(DIM))
                };
                (ic, st, Style::default().add_modifier(Modifier::BOLD))
            }
            NodeKind::Database { conn, db } => {
                let active = app
                    .active
                    .as_ref()
                    .map(|(c, d)| c == conn && d == db)
                    .unwrap_or(false);
                let st = if active {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                ("🗄 ", Style::default().fg(Color::Yellow), st)
            }
            NodeKind::Schema { .. } => ("◇ ", Style::default().fg(Color::Magenta), Style::default()),
            NodeKind::TablesFolder { .. } | NodeKind::ViewsFolder { .. } => {
                ("", Style::default().fg(DIM), Style::default().fg(DIM))
            }
            NodeKind::Table { .. } => ("▤ ", Style::default().fg(Color::Blue), Style::default()),
            NodeKind::View { .. } => ("▥ ", Style::default().fg(Color::Green), Style::default()),
        };
        let selected = i == app.tree.selected;
        let base = if selected {
            if focused {
                Style::default().bg(SEL_BG)
            } else {
                Style::default().bg(Color::Rgb(35, 38, 46))
            }
        } else {
            Style::default()
        };
        let mut spans = vec![
            Span::styled(format!("{indent}{arrow}"), base.patch(Style::default().fg(DIM))),
            Span::styled(icon.to_string(), base.patch(icon_style)),
            Span::styled(row.label.clone(), base.patch(label_style)),
        ];
        // Pad the selected row so the highlight spans the full width.
        if selected {
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            let pad = (inner.width as usize).saturating_sub(used);
            spans.push(Span::styled(" ".repeat(pad), base));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

// ---- editor -----------------------------------------------------------------

fn draw_tab_bar(f: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = app
        .tabs
        .iter()
        .map(|t| Line::from(format!(" {} ", t.name)))
        .collect();
    let tabs = Tabs::new(titles)
        .select(app.active_tab)
        .style(Style::default().fg(DIM))
        .highlight_style(
            Style::default()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider("│");
    f.render_widget(tabs, area);
}

fn draw_editor(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Editor;
    let active_tab = app.active_tab;
    let mode = app.tabs[active_tab].mode;
    let title = format!(" SQL — {} ", app.tabs[active_tab].name);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let editor = &mut app.tabs[active_tab];
    let gutter_w = (editor.lines.len().to_string().len() + 1).max(3) as u16;
    let text_w = inner.width.saturating_sub(gutter_w) as usize;
    editor.view_h = inner.height as usize;
    editor.view_w = text_w.max(1);
    editor.ensure_visible();

    app.editor_text_origin = (inner.x, inner.y, gutter_w);

    // Lex from the top of the buffer so multi-line constructs highlight right.
    let mut state = LexState::Normal;
    let mut per_line_styles: Vec<Option<Vec<Style>>> = Vec::with_capacity(editor.lines.len());
    let last_visible = (editor.scroll_row + inner.height as usize).min(editor.lines.len());
    for (idx, line) in editor.lines.iter().enumerate() {
        if idx >= last_visible {
            break;
        }
        let styles = line_styles(line, &mut state);
        per_line_styles.push(if idx >= editor.scroll_row {
            Some(styles)
        } else {
            None
        });
    }

    let selection = if editor.mode == Mode::Visual {
        editor.selection_range()
    } else {
        None
    };

    let mut lines: Vec<Line> = Vec::new();
    for (idx, line) in editor
        .lines
        .iter()
        .enumerate()
        .skip(editor.scroll_row)
        .take(inner.height as usize)
    {
        let styles = per_line_styles
            .get(idx)
            .and_then(|s| s.clone())
            .unwrap_or_default();
        let num_style = if idx == editor.row {
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(DIM)
        };
        let mut spans = vec![Span::styled(
            format!("{:>w$} ", idx + 1, w = (gutter_w - 1) as usize),
            num_style,
        )];

        let chars: Vec<char> = line.chars().collect();
        let mut col = editor.scroll_col;
        let mut width_used = 0usize;
        while col < chars.len() && width_used < text_w {
            let c = chars[col];
            let cw = c.width().unwrap_or(1);
            let mut style = styles.get(col).copied().unwrap_or_default();
            if let Some(((sr, sc), (er, ec))) = selection {
                let in_sel = (idx > sr || (idx == sr && col >= sc))
                    && (idx < er || (idx == er && col <= ec));
                if in_sel {
                    style = style.bg(SEL_BG);
                }
            }
            if focused && idx == editor.row && col == editor.col && mode != Mode::Insert {
                style = style.add_modifier(Modifier::REVERSED);
            }
            spans.push(Span::styled(c.to_string(), style));
            width_used += cw;
            col += 1;
        }
        // Cursor sitting past the end of the line (normal mode on empty line,
        // or insert-mode at EOL is handled by the terminal cursor).
        if focused && idx == editor.row && mode != Mode::Insert && editor.col >= chars.len() {
            spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);

    if focused && mode == Mode::Insert && app.minibuffer.is_none() {
        let editor = &app.tabs[active_tab];
        let x = inner.x
            + gutter_w
            + editor
                .lines
                .get(editor.row)
                .map(|l| {
                    l.chars()
                        .skip(editor.scroll_col)
                        .take(editor.col.saturating_sub(editor.scroll_col))
                        .map(|c| c.width().unwrap_or(1) as u16)
                        .sum::<u16>()
                })
                .unwrap_or(0);
        let y = inner.y + (editor.row - editor.scroll_row) as u16;
        f.set_cursor_position(Position::new(
            x.min(inner.x + inner.width.saturating_sub(1)),
            y,
        ));
    }
}

// ---- results panel ----------------------------------------------------------

fn draw_results(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Results;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    let tab_idx = match app.results_tab {
        ResultsTab::Results => 0,
        ResultsTab::Messages => 1,
        ResultsTab::History => 2,
    };
    let titles = ["Results", "Messages", "History"]
        .iter()
        .map(|t| Line::from(format!(" {t} ")))
        .collect::<Vec<_>>();
    let tabs = Tabs::new(titles)
        .select(tab_idx)
        .style(Style::default().fg(DIM))
        .highlight_style(Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))
        .divider("│");
    f.render_widget(tabs, rows[0]);

    match app.results_tab {
        ResultsTab::Results => draw_grid(f, app, rows[1], focused),
        ResultsTab::Messages => draw_messages(f, app, rows[1]),
        ResultsTab::History => draw_history(f, app, rows[1], focused),
    }
}

fn draw_grid(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let Some(grid) = &mut app.grid else {
        let hint = Paragraph::new(Line::from(vec![Span::styled(
            "No results yet — run a query with Ctrl+Enter or F5",
            Style::default().fg(DIM),
        )]))
        .centered();
        f.render_widget(hint, area);
        return;
    };
    if area.height < 2 {
        return;
    }
    grid.view_h = area.height as usize - 1; // one line for the header
    grid.ensure_visible();

    // Advance col_off until the selected column fits in the viewport.
    let width = area.width as usize;
    loop {
        let mut used = 0usize;
        let mut fits = false;
        for c in grid.col_off..grid.rs.columns.len() {
            used += grid.widths[c] as usize + 1;
            if c == grid.sel_col {
                fits = used <= width;
                break;
            }
            if used > width {
                break;
            }
        }
        if fits || grid.col_off >= grid.sel_col {
            break;
        }
        grid.col_off += 1;
    }

    let visible_cols: Vec<usize> = {
        let mut v = Vec::new();
        let mut used = 0usize;
        for c in grid.col_off..grid.rs.columns.len() {
            let w = grid.widths[c] as usize + 1;
            if used + w > width && !v.is_empty() {
                break;
            }
            used += w;
            v.push(c);
        }
        v
    };

    let mut lines: Vec<Line> = Vec::new();
    // Header
    let mut spans = Vec::new();
    for &c in &visible_cols {
        let w = grid.widths[c] as usize;
        let name = clip_pad(&grid.rs.columns[c], w);
        let mut st = Style::default()
            .fg(ACCENT)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        if c == grid.sel_col && focused {
            st = st.bg(SEL_BG);
        }
        spans.push(Span::styled(name, st));
        spans.push(Span::raw(" "));
    }
    lines.push(Line::from(spans));

    for (ri, row) in grid
        .rs
        .rows
        .iter()
        .enumerate()
        .skip(grid.row_off)
        .take(grid.view_h)
    {
        let mut spans = Vec::new();
        for &c in &visible_cols {
            let w = grid.widths[c] as usize;
            let (text, mut style) = match row.get(c) {
                Some(Some(v)) => (clip_pad(v, w), Style::default()),
                Some(None) => (
                    clip_pad("NULL", w),
                    Style::default().fg(DIM).add_modifier(Modifier::ITALIC),
                ),
                None => (clip_pad("", w), Style::default()),
            };
            let selected = ri == grid.sel_row && c == grid.sel_col;
            if selected && focused {
                style = style.add_modifier(Modifier::REVERSED);
            } else if ri == grid.sel_row && focused {
                style = style.bg(Color::Rgb(35, 38, 46));
            }
            spans.push(Span::styled(text, style));
            spans.push(Span::raw(" "));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), area);
}

fn clip_pad(s: &str, w: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    // Render newlines/tabs inline so every row stays exactly one line tall.
    let text: String = s
        .chars()
        .map(|c| match c {
            '\n' | '\r' => '⏎',
            '\t' => '→',
            c => c,
        })
        .collect();

    let mut out = String::new();
    let mut used = 0usize;
    if text.as_str().width() > w {
        // Reserve the last column for the ellipsis so clipping is always visible.
        for c in text.chars() {
            let cw = c.width().unwrap_or(1);
            if used + cw > w.saturating_sub(1) {
                break;
            }
            out.push(c);
            used += cw;
        }
        out.push('…');
        used += 1;
    } else {
        out = text;
        used = out.as_str().width();
    }
    for _ in used..w {
        out.push(' ');
    }
    out
}

fn draw_messages(f: &mut Frame, app: &App, area: Rect) {
    let h = area.height as usize;
    // Build wrapped-ish lines: split multi-line messages.
    let mut all: Vec<Line> = Vec::new();
    for m in &app.messages {
        let color = match m.level {
            MessageLevel::Info => Color::Reset,
            MessageLevel::Notice => Color::Yellow,
            MessageLevel::Error => Color::Red,
        };
        for (i, part) in m.text.lines().enumerate() {
            if i == 0 {
                all.push(Line::from(vec![
                    Span::styled(format!("{} ", m.time), Style::default().fg(DIM)),
                    Span::styled(part.to_string(), Style::default().fg(color)),
                ]));
            } else {
                all.push(Line::from(vec![
                    Span::raw("         "),
                    Span::styled(part.to_string(), Style::default().fg(color)),
                ]));
            }
        }
    }
    let total = all.len();
    let end = total.saturating_sub(app.messages_scroll);
    let start = end.saturating_sub(h);
    let visible: Vec<Line> = all[start..end].to_vec();
    f.render_widget(Paragraph::new(visible), area);
}

fn draw_history(f: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    let (filtered, sel) = app.history_view();
    app.history_sel = sel;
    let mut lines: Vec<Line> = Vec::new();
    let h = area.height as usize;
    if !app.history_filter.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("filter: ", Style::default().fg(DIM)),
            Span::styled(app.history_filter.clone(), Style::default().fg(ACCENT)),
            Span::styled(
                "   (`/` to change, Enter load · r run · y copy)",
                Style::default().fg(DIM),
            ),
        ]));
    }
    let list_h = h.saturating_sub(lines.len());
    let scroll = sel.saturating_sub(list_h.saturating_sub(1) / 2).min(
        filtered.len().saturating_sub(list_h),
    );
    for (vi, &hi) in filtered.iter().enumerate().skip(scroll).take(list_h) {
        let e = &app.history[hi];
        let selected = vi == sel;
        let base = if selected && focused {
            Style::default().bg(SEL_BG)
        } else {
            Style::default()
        };
        let ok = if e.ok {
            Span::styled("✓", base.patch(Style::default().fg(Color::Green)))
        } else {
            Span::styled("✗", base.patch(Style::default().fg(Color::Red)))
        };
        // Flatten to one line so multi-line queries stay recognizable.
        let preview: String = e
            .sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(160)
            .collect();
        let dur = e
            .duration_ms
            .map(|ms| format!("{ms:>5}ms"))
            .unwrap_or_else(|| "      ".into());
        lines.push(Line::from(vec![
            Span::styled(
                e.timestamp.format("%m-%d %H:%M ").to_string(),
                base.patch(Style::default().fg(DIM)),
            ),
            ok,
            Span::styled(format!(" {dur} "), base.patch(Style::default().fg(DIM))),
            Span::styled(
                format!("{}·{} ", e.connection, e.database),
                base.patch(Style::default().fg(Color::Magenta)),
            ),
            Span::styled(preview, base),
        ]));
    }
    if filtered.is_empty() {
        lines.push(Line::from(Span::styled(
            "No history yet",
            Style::default().fg(DIM),
        )));
    }
    f.render_widget(Paragraph::new(lines), area);
}

// ---- status bar / minibuffer ------------------------------------------------

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    if let Some(mb) = &app.minibuffer {
        let masked = matches!(mb.kind, MiniKind::Password { .. });
        let shown: String = if masked {
            "•".repeat(mb.input.chars().count())
        } else {
            mb.input.clone()
        };
        let line = Line::from(vec![
            Span::styled(mb.prompt.clone(), Style::default().fg(ACCENT)),
            Span::raw(shown),
        ]);
        f.render_widget(Paragraph::new(line), area);
        let x = area.x
            + mb.prompt.chars().count() as u16
            + mb.cursor as u16;
        f.set_cursor_position(Position::new(x.min(area.x + area.width - 1), area.y));
        return;
    }

    let mut spans: Vec<Span> = Vec::new();
    // Show the editor's mode only while the editor has focus; otherwise name
    // the focused pane, so Ctrl+h/j/k/l always leaves a visible trace.
    let (label, color) = match app.focus {
        Focus::Editor if app.vim() => match app.tabs[app.active_tab].mode {
            Mode::Normal => (" NORMAL ", Color::Blue),
            Mode::Insert => (" INSERT ", Color::Green),
            Mode::Visual => (" VISUAL ", Color::Magenta),
        },
        Focus::Editor => (" EDITOR ", Color::Green),
        Focus::Tree => (" EXPLORER ", Color::Cyan),
        Focus::Results => (" RESULTS ", Color::Yellow),
    };
    spans.push(Span::styled(
        label,
        Style::default()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));
    match &app.active {
        Some((conn, db)) => {
            let connected = app
                .conn_states
                .get(&(conn.clone(), db.clone()))
                .map(|s| *s == ConnState::Connected)
                .unwrap_or(false);
            let dot = if connected {
                Span::styled("● ", Style::default().fg(Color::Green))
            } else {
                Span::styled("○ ", Style::default().fg(Color::Red))
            };
            spans.push(dot);
            spans.push(Span::styled(
                format!("{conn}/{db} "),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
        None => {
            spans.push(Span::styled("○ not connected ", Style::default().fg(DIM)));
        }
    }
    if app.txn_open {
        spans.push(Span::styled(
            "[TXN] ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ));
    }
    if app.busy_since.is_some() {
        spans.push(Span::styled(
            format!("{} running… ", SPINNER[app.spinner_idx]),
            Style::default().fg(Color::Yellow),
        ));
    } else if let Some((rows, dur)) = &app.last_stats {
        spans.push(Span::styled(
            format!("{} row(s) · {} ", rows, human_duration(*dur)),
            Style::default().fg(DIM),
        ));
    }
    if let Some((msg, is_err)) = &app.status {
        let style = if *is_err {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        spans.push(Span::styled("· ", Style::default().fg(DIM)));
        spans.push(Span::styled(msg.clone(), style));
    }
    let left: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let help = "?:help  ::cmd";
    let pad = (area.width as usize)
        .saturating_sub(left)
        .saturating_sub(help.chars().count());
    spans.push(Span::raw(" ".repeat(pad)));
    spans.push(Span::styled(help, Style::default().fg(DIM)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

// ---- overlays ---------------------------------------------------------------

fn draw_picker(f: &mut Frame, app: &App, sel: usize) {
    let h = (app.cfg.connections.len() as u16 + 4).clamp(5, 20);
    let area = centered_rect(f.area(), 50, h);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Connect (Enter to connect, Esc to close) ")
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = Vec::new();
    if app.cfg.connections.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("No connections. Edit {}", crate::config::config_path().display()),
            Style::default().fg(Color::Yellow),
        )));
    }
    for (i, c) in app
        .connections_by_recency()
        .into_iter()
        .filter_map(|idx| app.cfg.connections.get(idx))
        .enumerate()
    {
        let connected = app
            .conn_states
            .iter()
            .any(|((n, _), s)| *n == c.name && *s == ConnState::Connected);
        let dot = if connected {
            Span::styled("● ", Style::default().fg(Color::Green))
        } else {
            Span::styled("○ ", Style::default().fg(DIM))
        };
        let base = if i == sel {
            Style::default().bg(SEL_BG)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(" ", base),
            dot,
            Span::styled(
                format!("{}  ", c.name),
                base.patch(Style::default().add_modifier(Modifier::BOLD)),
            ),
            Span::styled(
                format!("{}@{}:{}/{}", c.user, c.host, c.port, c.dbname()),
                base.patch(Style::default().fg(DIM)),
            ),
        ]));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_help(f: &mut Frame) {
    let area = centered_rect(f.area(), 72, 30);
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help — press any key to close ")
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let entries: &[(&str, &str)] = &[
        ("Ctrl+Enter / F5", "Execute buffer (selection in visual mode)"),
        ("F9  /  :stmt", "Execute statement under cursor"),
        ("Ctrl+p", "Connection picker"),
        ("Ctrl+h/j/k/l", "Move between panes (left/down/up/right)"),
        ("Ctrl+e", "Toggle / focus explorer"),
        ("Ctrl+o", "Cycle focus editor → results → explorer"),
        ("Ctrl+t / Ctrl+w", "New / close SQL tab"),
        ("Tab / Shift+Tab", "Next / previous SQL tab"),
        ("Ctrl+g", "Query history"),
        ("Ctrl+c", "Cancel running query"),
        ("[ / ]", "Switch results tabs (in results pane)"),
        ("", ""),
        ("i a I A o O", "Enter insert mode"),
        ("Esc", "Back to normal mode"),
        ("h j k l  w b e  0 ^ $", "Vim motions"),
        ("gg / G, Ctrl+d/u/f/b", "Jump / scroll"),
        ("v", "Visual selection"),
        ("dd yy p x u Ctrl+r", "Delete / yank / paste / undo / redo"),
        ("/  n  N", "Search in editor / explorer"),
        ("r (explorer)", "Refresh node"),
        ("y (results)", "Copy cell — Y copies row"),
        ("", ""),
        (":q", "Quit"),
        (":run  :explain [analyze]", "Execute / explain current SQL"),
        (":begin :commit :rollback", "Transaction control"),
        (":export ~/out.csv", "Export results as CSV"),
        (":connect [name]  :disconnect", "Manage connections"),
        (":clear  :history  :help", "Utilities"),
    ];
    let lines: Vec<Line> = entries
        .iter()
        .map(|(k, v)| {
            Line::from(vec![
                Span::styled(
                    format!(" {k:<26}"),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw((*v).to_string()),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
