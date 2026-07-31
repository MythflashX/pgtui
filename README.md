```
██████╗  ██████╗████████╗██╗   ██╗██╗
██╔══██╗██╔════╝╚══██╔══╝██║   ██║██║
██████╔╝██║  ███╗  ██║   ██║   ██║██║
██╔═══╝ ██║   ██║  ██║   ██║   ██║██║
██║     ╚██████╔╝  ██║   ╚██████╔╝██║
╚═╝      ╚═════╝   ╚═╝    ╚═════╝ ╚═╝
```

A lightweight, keyboard-driven PostgreSQL client for the terminal — the query
tool from pgAdmin, without the rest of pgAdmin.

Starts in ~15 ms, idles at ~6 MB, ships as a single 2 MB binary. Vim motions
are first-class; nothing requires the mouse.

```
┌ Explorer ──────────────┐  query1 │ query2
│ ● local                │┌ SQL — query1 ─────────────────────────────────┐
│   🗄 app_db             ││ 1 SELECT id, username, email                  │
│     ◇ public           ││ 2 FROM users                                  │
│       ▾ tables (12)    ││ 3 WHERE email IS NOT NULL;                    │
│           ▤ users      │└───────────────────────────────────────────────┘
│           ▤ orders     │┌───────────────────────────────────────────────┐
│       ▸ views (3)      ││  Results │ Messages │ History                 │
│   🗄 analytics         ││id  username email                             │
│                        ││1   alice    alice@test.com                    │
│                        ││2   bob      bob@test.com                      │
└────────────────────────┘└───────────────────────────────────────────────┘
 NORMAL  ● local/app_db  2 row(s) · 14 ms                      ?:help  ::cmd
```

## Install

```sh
cargo build --release
install -Dm755 target/release/pgtui ~/.local/bin/pgtui
```

## Configure

On first run pgtui writes a commented sample to
`~/.config/pgtui/config.toml`. Add one `[[connection]]` block per server:

```toml
[settings]
vim_mode = true       # false = plain, non-modal editing
row_limit = 5000      # max rows kept in the results grid
history_limit = 1000  # max query history entries
mouse = true

[[connection]]
name = "local"
host = "localhost"
port = 5432
user = "postgres"
dbname = "postgres"

[[connection]]
name = "prod"
host = "db.example.com"
user = "app"
dbname = "app_db"
password_command = "pass show db/prod"
```

Run `pgtui` to start, or `pgtui prod` to connect on launch.

### Passwords

pgtui never writes a password to disk. It resolves one in this order:

1. `password` — plaintext in the config file. Not recommended.
2. `password_command` — any shell command that prints the password on stdout,
   so you can use `pass`, `gpg`, `secret-tool`, the 1Password CLI, and so on.
   It runs off the UI thread, so a GPG pinentry prompt won't freeze the app.
3. `~/.pgpass` — the standard PostgreSQL password file, with `*` wildcards.
4. An in-app prompt — only if the server actually asks for a password. The
   answer is cached in memory for the session and never persisted.

Servers using `trust` or `peer` auth connect with no prompt at all.

## Keys

Press `?` anywhere for the full list.

| Key | Action |
| --- | --- |
| `Ctrl+Enter` / `F5` | Execute the buffer, or the visual selection |
| `F9` / `:stmt` | Execute just the statement under the cursor |
| `Ctrl+h` `Ctrl+j` `Ctrl+k` `Ctrl+l` | Move between panes: left, down, up, right |
| `Ctrl+p` | Connection picker (most recently used first) |
| `Ctrl+e` | Toggle / focus the explorer |
| `Ctrl+o` | Cycle focus: editor → results → explorer |
| `Ctrl+t` / `Ctrl+w` | New / close SQL tab |
| `Tab` / `Shift+Tab` | Next / previous SQL tab |
| `Ctrl+g` | Query history |
| `Ctrl+c` | Cancel the running query |
| `Ctrl+q` | Quit |

`Ctrl+Enter` needs a terminal that supports the kitty keyboard protocol
(kitty, WezTerm, foot, Ghostty, recent Alacritty). `F5` works everywhere.

Panes are laid out with the explorer full-height on the left and the editor
above the results on the right, so `Ctrl+h`/`Ctrl+j`/`Ctrl+k`/`Ctrl+l` move the
way they look. Moving left onto a hidden explorer reveals it. Movement stops at
the edges instead of wrapping.

```
        +-----------+-----------+
        |           |  Editor   |     Ctrl+h ← → Ctrl+l
        | Explorer  +-----------+     Ctrl+k ↑ ↓ Ctrl+j
        |           |  Results  |
        +-----------+-----------+
```

### Editor

Modal by default, close to Neovim where it matters:

- `i a I A o O` insert, `Esc` back to normal, `v` visual
- `h j k l`, `w b e`, `0 ^ $`, `gg` `G`
- `Ctrl+d`/`Ctrl+u` half page, `Ctrl+f`/`Ctrl+b` full page
- `dd yy p P x D u Ctrl+r`
- `/pattern`, then `n` / `N`

Yanked text is sent to the system clipboard via OSC 52, so it works over SSH.
Set `vim_mode = false` for a plain always-insert editor.

### Explorer

`j k` move, `l`/`Enter` expand (opening a table drops a `SELECT` template into
the editor), `h` collapse or jump to parent, `r` refresh, `/` search with
`n`/`N` — handy when a server has a few dozen databases.

Selecting a database makes it the active one for queries.

### Results

`h j k l` move by cell, `gg`/`G` first/last row, `0`/`$` first/last column,
`Ctrl+d`/`Ctrl+u` half page, `y` copy the cell, `Y` copy the row as TSV.
`[` and `]` switch between Results, Messages, and History.

In History: `Enter` loads a query into the editor, `r` re-runs it, `y` copies
it, `/` filters. History persists across sessions in
`~/.local/share/pgtui/history.jsonl`.

### Commands

`:` opens the command palette.

| Command | Action |
| --- | --- |
| `:q` | Quit |
| `:run` / `:stmt` | Execute buffer / statement under cursor |
| `:explain [analyze]` | EXPLAIN the statement under the cursor |
| `:begin` `:commit` `:rollback` | Transaction control (`[TXN]` shows in the status bar) |
| `:export ~/out.csv` | Write the current results to CSV |
| `:connect [name]` `:disconnect` `:reconnect` | Manage connections |
| `:new` `:close` | SQL tabs |
| `:clear` `:history` `:help` | Utilities |

## Notes on behavior

- **Multiple statements** in one buffer run as a batch; the last result set
  with columns is shown in the grid, and every statement's row count lands in
  Messages. Use `F9` to run just one.
- **Large results** stream: rows past `row_limit` are counted but not kept, so
  `SELECT * FROM huge_table` reports the true row count without exhausting
  memory. A note appears in Messages when a result was truncated.
- **Errors** show severity, detail, hint, and position in Messages.
- **`RAISE NOTICE`** output arrives asynchronously in Messages.
- **Dropped connections** are re-established on the next query automatically.
- Each database gets its own connection, opened lazily and kept warm.

## Deliberately out of scope

No user or role management, backups, monitoring, extension management,
property editors, visual query builders, ER diagrams, or dashboards. This is a
SQL client.

## Built with

[ratatui](https://ratatui.rs) · [crossterm](https://github.com/crossterm-rs/crossterm)
· [tokio](https://tokio.rs) · [tokio-postgres](https://github.com/sfackler/rust-postgres)

Syntax highlighting is a small purpose-built SQL lexer (~200 lines) rather than
syntect or tree-sitter — it handles nested block comments, dollar-quoted
bodies, and `E''` escapes, and costs nothing at startup.
