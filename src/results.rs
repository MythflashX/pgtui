use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Default)]
pub struct ResultSet {
    pub columns: Vec<String>,
    /// Cell values; None represents SQL NULL.
    pub rows: Vec<Vec<Option<String>>>,
    /// Total rows reported by the server (may exceed rows.len() when truncated).
    pub total_rows: u64,
    pub truncated: bool,
}

/// Scroll/selection state over a ResultSet.
#[derive(Debug, Clone)]
pub struct Grid {
    pub rs: ResultSet,
    pub sel_row: usize,
    pub sel_col: usize,
    pub row_off: usize,
    pub col_off: usize,
    pub widths: Vec<u16>,
    /// Viewport height in data rows, updated by the renderer.
    pub view_h: usize,
}

impl Grid {
    pub fn new(rs: ResultSet) -> Self {
        let widths = compute_widths(&rs);
        Self {
            rs,
            sel_row: 0,
            sel_col: 0,
            row_off: 0,
            col_off: 0,
            widths,
            view_h: 10,
        }
    }

    pub fn cell(&self, row: usize, col: usize) -> Option<&Option<String>> {
        self.rs.rows.get(row).and_then(|r| r.get(col))
    }

    pub fn selected_cell_text(&self) -> Option<String> {
        self.cell(self.sel_row, self.sel_col)
            .map(|c| c.clone().unwrap_or_else(|| "NULL".into()))
    }

    pub fn selected_row_tsv(&self) -> Option<String> {
        self.rs.rows.get(self.sel_row).map(|r| {
            r.iter()
                .map(|c| c.clone().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\t")
        })
    }

    pub fn move_sel(&mut self, dr: i32, dc: i32) {
        let nrows = self.rs.rows.len();
        let ncols = self.rs.columns.len();
        if nrows == 0 || ncols == 0 {
            return;
        }
        self.sel_row = add_clamped(self.sel_row, dr, nrows - 1);
        self.sel_col = add_clamped(self.sel_col, dc, ncols - 1);
        self.ensure_visible();
    }

    pub fn goto_top(&mut self) {
        self.sel_row = 0;
        self.ensure_visible();
    }

    pub fn goto_bottom(&mut self) {
        self.sel_row = self.rs.rows.len().saturating_sub(1);
        self.ensure_visible();
    }

    pub fn first_col(&mut self) {
        self.sel_col = 0;
        self.ensure_visible();
    }

    pub fn last_col(&mut self) {
        self.sel_col = self.rs.columns.len().saturating_sub(1);
        self.ensure_visible();
    }

    pub fn half_page(&mut self, down: bool) {
        let half = (self.view_h / 2).max(1) as i32;
        self.move_sel(if down { half } else { -half }, 0);
    }

    pub fn ensure_visible(&mut self) {
        let h = self.view_h.max(1);
        if self.sel_row < self.row_off {
            self.row_off = self.sel_row;
        }
        if self.sel_row >= self.row_off + h {
            self.row_off = self.sel_row + 1 - h;
        }
        if self.sel_col < self.col_off {
            self.col_off = self.sel_col;
        }
        // Horizontal visibility is resolved by the renderer, which knows the
        // pane width; it may advance col_off further so the selection fits.
    }

    pub fn to_csv(&self) -> String {
        let mut out = String::new();
        let esc = |s: &str| -> String {
            if s.contains(',') || s.contains('"') || s.contains('\n') {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        };
        out.push_str(
            &self
                .rs
                .columns
                .iter()
                .map(|c| esc(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
        for row in &self.rs.rows {
            out.push_str(
                &row.iter()
                    .map(|c| c.as_deref().map(esc).unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(","),
            );
            out.push('\n');
        }
        out
    }
}

fn add_clamped(v: usize, d: i32, max: usize) -> usize {
    if d < 0 {
        v.saturating_sub((-d) as usize)
    } else {
        (v + d as usize).min(max)
    }
}

fn compute_widths(rs: &ResultSet) -> Vec<u16> {
    let mut widths: Vec<u16> = rs
        .columns
        .iter()
        .map(|c| c.width().clamp(3, 40) as u16)
        .collect();
    for row in rs.rows.iter().take(200) {
        for (i, cell) in row.iter().enumerate() {
            if i >= widths.len() {
                break;
            }
            let w = cell
                .as_deref()
                .map(|s| s.width())
                .unwrap_or(4) // "NULL"
                .clamp(3, 40) as u16;
            widths[i] = widths[i].max(w);
        }
    }
    widths
}
