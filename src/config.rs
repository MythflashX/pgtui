use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub name: String,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    /// Database to connect to initially. Defaults to the user name.
    #[serde(default)]
    pub dbname: Option<String>,
    /// Optional plaintext password. Prefer ~/.pgpass or the in-app prompt.
    #[serde(default)]
    pub password: Option<String>,
    /// Shell command printing the password on stdout, e.g. `pass show db/prod`.
    /// Runs in the connection task so a pinentry prompt cannot block the UI.
    #[serde(default)]
    pub password_command: Option<String>,
}

impl ConnectionConfig {
    pub fn dbname(&self) -> String {
        self.dbname.clone().unwrap_or_else(|| self.user.clone())
    }
}

fn default_host() -> String {
    "localhost".to_string()
}

fn default_port() -> u16 {
    5432
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Enable vim-style modal editing. When false the editor is always in insert mode.
    pub vim_mode: bool,
    /// Maximum number of rows kept in the results grid per query.
    pub row_limit: usize,
    /// Maximum number of history entries kept on disk.
    pub history_limit: usize,
    /// Enable mouse support (click to focus, wheel to scroll).
    pub mouse: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            vim_mode: true,
            row_limit: 5000,
            history_limit: 1000,
            mouse: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub settings: Settings,
    #[serde(rename = "connection")]
    pub connections: Vec<ConnectionConfig>,
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pgtui")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("pgtui")
}

const SAMPLE_CONFIG: &str = r#"# pgtui configuration
#
# [settings]
# vim_mode = true       # false = plain (non-modal) editing
# row_limit = 5000      # max rows kept in the results grid
# history_limit = 1000  # max query history entries
# mouse = true

[settings]
vim_mode = true

# Define one [[connection]] block per server.
#
# Passwords are resolved in this order:
#   1. `password`         plaintext in this file — not recommended
#   2. `password_command` shell command printing the password, e.g. a
#                         password manager: `pass show db/prod`
#   3. ~/.pgpass          standard PostgreSQL password file (chmod 600)
#   4. an in-app prompt   only if the server actually asks; the answer is
#                         cached in memory for the session and never written
#
# [[connection]]
# name = "local"
# host = "localhost"
# port = 5432
# user = "postgres"
# dbname = "postgres"

# [[connection]]
# name = "prod"
# host = "db.example.com"
# user = "app"
# dbname = "app_db"
# password_command = "pass show db/prod"
"#;

impl Config {
    pub fn load() -> (Self, Option<String>) {
        let path = config_path();
        if !path.exists() {
            let _ = std::fs::create_dir_all(config_dir());
            let _ = std::fs::write(&path, SAMPLE_CONFIG);
            return (
                Self::default(),
                Some(format!("Created sample config at {}", path.display())),
            );
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => (cfg, None),
                Err(e) => (Self::default(), Some(format!("Config parse error: {e}"))),
            },
            Err(e) => (Self::default(), Some(format!("Config read error: {e}"))),
        }
    }
}

/// Look up a password in the standard ~/.pgpass file.
/// Format: hostname:port:database:username:password, `*` matches anything.
pub fn lookup_pgpass(host: &str, port: u16, dbname: &str, user: &str) -> Option<String> {
    let path = match std::env::var("PGPASSFILE") {
        Ok(p) => PathBuf::from(p),
        Err(_) => dirs::home_dir()?.join(".pgpass"),
    };
    let text = std::fs::read_to_string(path).ok()?;
    let port_s = port.to_string();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields = split_pgpass_line(line);
        if fields.len() != 5 {
            continue;
        }
        let matches = |pat: &str, val: &str| pat == "*" || pat == val;
        if matches(&fields[0], host)
            && matches(&fields[1], &port_s)
            && matches(&fields[2], dbname)
            && matches(&fields[3], user)
        {
            return Some(fields[4].clone());
        }
    }
    None
}

/// Split a pgpass line on `:`, honoring `\:` and `\\` escapes.
fn split_pgpass_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            ':' => {
                fields.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    fields.push(cur);
    fields
}
