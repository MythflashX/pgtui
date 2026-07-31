use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::config::data_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub sql: String,
    pub connection: String,
    pub database: String,
    pub timestamp: DateTime<Local>,
    pub duration_ms: Option<u64>,
    pub ok: bool,
    pub rows: Option<u64>,
}

fn history_path() -> PathBuf {
    data_dir().join("history.jsonl")
}

pub fn load() -> Vec<HistoryEntry> {
    let Ok(text) = std::fs::read_to_string(history_path()) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn append(entry: &HistoryEntry) {
    let _ = std::fs::create_dir_all(data_dir());
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(history_path())
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Rewrite the history file keeping only the most recent `limit` entries.
pub fn compact(entries: &[HistoryEntry], limit: usize) {
    if entries.len() <= limit {
        return;
    }
    let keep = &entries[entries.len() - limit..];
    let mut out = String::new();
    for e in keep {
        if let Ok(line) = serde_json::to_string(e) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    let _ = std::fs::write(history_path(), out);
}
