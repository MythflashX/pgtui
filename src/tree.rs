/// Explorer tree: servers -> databases -> schemas -> tables/views.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    Server { conn: String },
    Database { conn: String, db: String },
    Schema { conn: String, db: String, schema: String },
    TablesFolder { conn: String, db: String, schema: String },
    ViewsFolder { conn: String, db: String, schema: String },
    Table { conn: String, db: String, schema: String, name: String },
    View { conn: String, db: String, schema: String, name: String },
}

#[derive(Debug, Clone)]
pub struct Node {
    pub label: String,
    pub kind: NodeKind,
    pub expanded: bool,
    /// Children have been fetched (folders/leaves may be trivially loaded).
    pub loaded: bool,
    pub children: Vec<Node>,
}

impl Node {
    fn new(label: String, kind: NodeKind) -> Self {
        Self {
            label,
            kind,
            expanded: false,
            loaded: false,
            children: Vec::new(),
        }
    }

    fn leaf(label: String, kind: NodeKind) -> Self {
        Self {
            label,
            kind,
            expanded: false,
            loaded: true,
            children: Vec::new(),
        }
    }

    pub fn is_leaf(&self) -> bool {
        matches!(self.kind, NodeKind::Table { .. } | NodeKind::View { .. })
    }
}

#[derive(Debug, Default)]
pub struct Tree {
    pub roots: Vec<Node>,
    /// Index into the flattened visible list.
    pub selected: usize,
    pub scroll: usize,
    pub view_h: usize,
    /// Last `/` pattern, reused by n/N.
    pub search: Option<String>,
}

/// A row of the flattened tree for rendering/navigation.
pub struct FlatRow {
    pub path: Vec<usize>,
    pub depth: usize,
    pub label: String,
    pub kind: NodeKind,
    pub expanded: bool,
    pub is_leaf: bool,
    pub loaded: bool,
}

impl Tree {
    pub fn new(connection_names: &[String]) -> Self {
        let roots = connection_names
            .iter()
            .map(|n| Node::new(n.clone(), NodeKind::Server { conn: n.clone() }))
            .collect();
        Self {
            roots,
            selected: 0,
            scroll: 0,
            view_h: 10,
            search: None,
        }
    }

    pub fn flatten(&self) -> Vec<FlatRow> {
        let mut out = Vec::new();
        fn walk(nodes: &[Node], path: &mut Vec<usize>, depth: usize, out: &mut Vec<FlatRow>) {
            for (i, node) in nodes.iter().enumerate() {
                path.push(i);
                out.push(FlatRow {
                    path: path.clone(),
                    depth,
                    label: node.label.clone(),
                    kind: node.kind.clone(),
                    expanded: node.expanded,
                    is_leaf: node.is_leaf(),
                    loaded: node.loaded,
                });
                if node.expanded {
                    walk(&node.children, path, depth + 1, out);
                }
                path.pop();
            }
        }
        let mut path = Vec::new();
        walk(&self.roots, &mut path, 0, &mut out);
        out
    }

    pub fn node_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        let (&first, rest) = path.split_first()?;
        let mut node = self.roots.get_mut(first)?;
        for &i in rest {
            node = node.children.get_mut(i)?;
        }
        Some(node)
    }

    pub fn selected_row(&self) -> Option<FlatRow> {
        let flat = self.flatten();
        flat.into_iter().nth(self.selected)
    }

    pub fn move_sel(&mut self, delta: i32) {
        let count = self.flatten().len();
        if count == 0 {
            return;
        }
        let max = count - 1;
        self.selected = if delta < 0 {
            self.selected.saturating_sub((-delta) as usize)
        } else {
            (self.selected + delta as usize).min(max)
        };
        self.ensure_visible();
    }

    pub fn goto_top(&mut self) {
        self.selected = 0;
        self.ensure_visible();
    }

    pub fn goto_bottom(&mut self) {
        self.selected = self.flatten().len().saturating_sub(1);
        self.ensure_visible();
    }

    pub fn ensure_visible(&mut self) {
        let h = self.view_h.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + h {
            self.scroll = self.selected + 1 - h;
        }
    }

    /// After expanding a node near the bottom of the pane, scroll so its
    /// children come into view instead of staying just off-screen.
    pub fn reveal_children(&mut self) {
        let h = self.view_h.max(1);
        let total = self.flatten().len();
        if self.selected + 1 >= self.scroll + h {
            let max_scroll = total.saturating_sub(h);
            self.scroll = self.selected.saturating_sub(h / 4).min(max_scroll);
        }
    }

    /// Jump to the next node whose label contains `search`, wrapping around.
    pub fn search_jump(&mut self, forward: bool) {
        let Some(pat) = self.search.clone() else { return };
        let pat = pat.to_lowercase();
        if pat.is_empty() {
            return;
        }
        let flat = self.flatten();
        let n = flat.len();
        if n == 0 {
            return;
        }
        for step in 1..=n {
            let i = if forward {
                (self.selected + step) % n
            } else {
                (self.selected + n - (step % n)) % n
            };
            if flat[i].label.to_lowercase().contains(&pat) {
                self.selected = i;
                self.ensure_visible();
                return;
            }
        }
    }

    /// Collapse the selected node, or move to its parent if already collapsed.
    pub fn collapse_or_parent(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        let collapsed = {
            let node = self.node_mut(&row.path);
            match node {
                Some(n) if n.expanded => {
                    n.expanded = false;
                    true
                }
                _ => false,
            }
        };
        if !collapsed && row.path.len() > 1 {
            let parent_path = &row.path[..row.path.len() - 1];
            let flat = self.flatten();
            if let Some(idx) = flat.iter().position(|r| r.path == parent_path) {
                self.selected = idx;
                self.ensure_visible();
            }
        }
    }

    // ---- population helpers -------------------------------------------------

    fn find_server_mut(&mut self, conn: &str) -> Option<&mut Node> {
        self.roots
            .iter_mut()
            .find(|n| matches!(&n.kind, NodeKind::Server { conn: c } if c == conn))
    }

    pub fn set_databases(&mut self, conn: &str, names: Vec<String>) {
        if let Some(server) = self.find_server_mut(conn) {
            server.loaded = true;
            server.expanded = true;
            server.children = names
                .into_iter()
                .map(|db| {
                    Node::new(
                        db.clone(),
                        NodeKind::Database {
                            conn: conn.to_string(),
                            db,
                        },
                    )
                })
                .collect();
        }
    }

    pub fn set_schemas(&mut self, conn: &str, db: &str, names: Vec<String>) {
        let Some(server) = self.find_server_mut(conn) else {
            return;
        };
        if let Some(dbnode) = server
            .children
            .iter_mut()
            .find(|n| matches!(&n.kind, NodeKind::Database { db: d, .. } if d == db))
        {
            dbnode.loaded = true;
            dbnode.expanded = true;
            dbnode.children = names
                .into_iter()
                .map(|schema| {
                    Node::new(
                        schema.clone(),
                        NodeKind::Schema {
                            conn: conn.to_string(),
                            db: db.to_string(),
                            schema,
                        },
                    )
                })
                .collect();
        }
    }

    pub fn set_relations(
        &mut self,
        conn: &str,
        db: &str,
        schema: &str,
        tables: Vec<String>,
        views: Vec<String>,
    ) {
        let Some(server) = self.find_server_mut(conn) else {
            return;
        };
        let Some(dbnode) = server
            .children
            .iter_mut()
            .find(|n| matches!(&n.kind, NodeKind::Database { db: d, .. } if d == db))
        else {
            return;
        };
        let Some(snode) = dbnode
            .children
            .iter_mut()
            .find(|n| matches!(&n.kind, NodeKind::Schema { schema: s, .. } if s == schema))
        else {
            return;
        };
        snode.loaded = true;
        snode.expanded = true;
        let mk = |conn: &str, db: &str, schema: &str| {
            (conn.to_string(), db.to_string(), schema.to_string())
        };
        let (c, d, s) = mk(conn, db, schema);
        let mut tfolder = Node::new(
            format!("tables ({})", tables.len()),
            NodeKind::TablesFolder {
                conn: c.clone(),
                db: d.clone(),
                schema: s.clone(),
            },
        );
        tfolder.loaded = true;
        tfolder.children = tables
            .into_iter()
            .map(|t| {
                Node::leaf(
                    t.clone(),
                    NodeKind::Table {
                        conn: c.clone(),
                        db: d.clone(),
                        schema: s.clone(),
                        name: t,
                    },
                )
            })
            .collect();
        let mut vfolder = Node::new(
            format!("views ({})", views.len()),
            NodeKind::ViewsFolder {
                conn: c.clone(),
                db: d.clone(),
                schema: s.clone(),
            },
        );
        vfolder.loaded = true;
        vfolder.children = views
            .into_iter()
            .map(|v| {
                Node::leaf(
                    v.clone(),
                    NodeKind::View {
                        conn: c.clone(),
                        db: d.clone(),
                        schema: s.clone(),
                        name: v,
                    },
                )
            })
            .collect();
        snode.children = vec![tfolder, vfolder];
    }
}
