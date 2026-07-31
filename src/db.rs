use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio_postgres::{AsyncMessage, CancelToken, NoTls, SimpleQueryMessage};

use crate::config::ConnectionConfig;
use crate::results::ResultSet;

/// Identifies one live client: (connection name, database name).
pub type ConnKey = (String, String);

#[derive(Debug, Clone)]
pub enum Purpose {
    /// A user-initiated query from the editor (or history re-run).
    User { sql: String },
    ListDatabases,
    ListSchemas { db: String },
    ListRelations { db: String, schema: String },
}

#[derive(Debug)]
pub enum DbRequest {
    Query { sql: String, purpose: Purpose },
}

#[derive(Debug)]
pub struct QueryOutput {
    pub result_sets: Vec<ResultSet>,
    /// One entry per completed command: rows affected.
    pub commands: Vec<u64>,
}

#[derive(Debug)]
pub enum DbEvent {
    Connected {
        key: ConnKey,
    },
    ConnectFailed {
        key: ConnKey,
        error: String,
    },
    Closed {
        key: ConnKey,
        error: Option<String>,
    },
    Notice {
        key: ConnKey,
        severity: String,
        message: String,
    },
    QueryDone {
        key: ConnKey,
        purpose: Purpose,
        outcome: Result<QueryOutput, String>,
        elapsed: Duration,
    },
}

#[derive(Clone)]
pub struct DbHandle {
    tx: UnboundedSender<DbRequest>,
    cancel: Arc<Mutex<Option<CancelToken>>>,
}

impl DbHandle {
    pub fn send(&self, req: DbRequest) -> bool {
        self.tx.send(req).is_ok()
    }

    /// Ask the server to cancel whatever this client is running.
    pub fn cancel_running(&self) {
        let token = self.cancel.lock().ok().and_then(|g| g.clone());
        if let Some(token) = token {
            tokio::spawn(async move {
                let _ = token.cancel_query(NoTls).await;
            });
        }
    }
}

/// Spawn a background task owning one PostgreSQL client.
/// Requests queue on the channel until the connection is established.
pub fn spawn_connection(
    key: ConnKey,
    cfg: ConnectionConfig,
    dbname: String,
    password: Option<String>,
    row_limit: usize,
    events: UnboundedSender<DbEvent>,
) -> DbHandle {
    let (tx, mut rx) = mpsc::unbounded_channel::<DbRequest>();
    let cancel: Arc<Mutex<Option<CancelToken>>> = Arc::new(Mutex::new(None));
    let cancel_slot = cancel.clone();

    tokio::spawn(async move {
        // Resolve `password_command` here rather than on the UI thread: it may
        // spawn a pinentry/GPG prompt that takes seconds.
        let password = match password {
            Some(p) => Some(p),
            None => match &cfg.password_command {
                Some(cmd) => match run_password_command(cmd).await {
                    Ok(p) => Some(p),
                    Err(e) => {
                        let _ = events.send(DbEvent::ConnectFailed {
                            key,
                            error: format!("password_command failed: {e}"),
                        });
                        return;
                    }
                },
                None => None,
            },
        };

        let mut pg = tokio_postgres::Config::new();
        pg.host(&cfg.host)
            .port(cfg.port)
            .user(&cfg.user)
            .dbname(&dbname)
            .application_name("pgtui")
            .connect_timeout(Duration::from_secs(8));
        if let Some(p) = &password {
            pg.password(p);
        }

        let (client, mut connection) = match pg.connect(NoTls).await {
            Ok(pair) => pair,
            Err(e) => {
                let _ = events.send(DbEvent::ConnectFailed {
                    key,
                    error: format_connect_error(&e),
                });
                return;
            }
        };

        if let Ok(mut guard) = cancel_slot.lock() {
            *guard = Some(client.cancel_token());
        }

        // Drive the connection and forward notices (RAISE NOTICE etc.).
        let notice_events = events.clone();
        let notice_key = key.clone();
        tokio::spawn(async move {
            let mut stream =
                futures::stream::poll_fn(move |cx| connection.poll_message(cx));
            let mut close_error = None;
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(AsyncMessage::Notice(n)) => {
                        let _ = notice_events.send(DbEvent::Notice {
                            key: notice_key.clone(),
                            severity: n.severity().to_string(),
                            message: n.message().to_string(),
                        });
                    }
                    Ok(_) => {}
                    Err(e) => {
                        close_error = Some(e.to_string());
                        break;
                    }
                }
            }
            let _ = notice_events.send(DbEvent::Closed {
                key: notice_key,
                error: close_error,
            });
        });

        let _ = events.send(DbEvent::Connected { key: key.clone() });

        while let Some(req) = rx.recv().await {
            match req {
                DbRequest::Query { sql, purpose } => {
                    let start = Instant::now();
                    let outcome = run_query(&client, &sql, row_limit).await;
                    let elapsed = start.elapsed();
                    let _ = events.send(DbEvent::QueryDone {
                        key: key.clone(),
                        purpose,
                        outcome,
                        elapsed,
                    });
                }
            }
        }
    });

    DbHandle { tx, cancel }
}

/// Run a statement batch, keeping at most `row_limit` rows per result set in
/// memory. The stream is drained either way so row counts stay accurate.
async fn run_query(
    client: &tokio_postgres::Client,
    sql: &str,
    row_limit: usize,
) -> Result<QueryOutput, String> {
    let stream = client
        .simple_query_raw(sql)
        .await
        .map_err(|e| format_pg_error(&e))?;
    futures::pin_mut!(stream);

    let mut result_sets: Vec<ResultSet> = Vec::new();
    let mut commands: Vec<u64> = Vec::new();
    let mut cur: Option<ResultSet> = None;

    while let Some(msg) = stream.next().await {
        match msg.map_err(|e| format_pg_error(&e))? {
            SimpleQueryMessage::RowDescription(cols) => {
                if let Some(rs) = cur.take() {
                    result_sets.push(rs);
                }
                cur = Some(ResultSet {
                    columns: cols.iter().map(|c| c.name().to_string()).collect(),
                    ..Default::default()
                });
            }
            SimpleQueryMessage::Row(row) => {
                let rs = cur.get_or_insert_with(|| ResultSet {
                    columns: row.columns().iter().map(|c| c.name().to_string()).collect(),
                    ..Default::default()
                });
                rs.total_rows += 1;
                if rs.rows.len() < row_limit {
                    let vals = (0..row.len())
                        .map(|i| row.get(i).map(|s| s.to_string()))
                        .collect();
                    rs.rows.push(vals);
                } else {
                    rs.truncated = true;
                }
            }
            SimpleQueryMessage::CommandComplete(n) => {
                commands.push(n);
                if let Some(mut rs) = cur.take() {
                    if rs.total_rows == 0 {
                        rs.total_rows = n;
                    }
                    result_sets.push(rs);
                }
            }
            _ => {}
        }
    }
    if let Some(rs) = cur.take() {
        result_sets.push(rs);
    }
    Ok(QueryOutput {
        result_sets,
        commands,
    })
}

fn format_pg_error(e: &tokio_postgres::Error) -> String {
    if let Some(db) = e.as_db_error() {
        let mut out = format!("{}: {}", db.severity(), db.message());
        if let Some(detail) = db.detail() {
            out.push_str(&format!("\nDETAIL: {detail}"));
        }
        if let Some(hint) = db.hint() {
            out.push_str(&format!("\nHINT: {hint}"));
        }
        if let Some(tokio_postgres::error::ErrorPosition::Original(p)) = db.position() {
            out.push_str(&format!("\nPOSITION: {p}"));
        }
        out
    } else {
        e.to_string()
    }
}

async fn run_password_command(cmd: &str) -> Result<String, String> {
    let out = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "exited with {}: {}",
            out.status,
            err.trim().lines().next().unwrap_or("")
        ));
    }
    let pw = String::from_utf8_lossy(&out.stdout);
    let pw = pw.lines().next().unwrap_or("").to_string();
    if pw.is_empty() {
        Err("produced no output".into())
    } else {
        Ok(pw)
    }
}

/// Connect-time errors are often wrapped ("invalid configuration" ->
/// "password missing"), so include the whole source chain.
fn format_connect_error(e: &tokio_postgres::Error) -> String {
    if e.as_db_error().is_some() {
        return format_pg_error(e);
    }
    let mut out = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}

/// True when the server demanded a password we did not supply.
pub fn is_password_missing(error: &str) -> bool {
    error.contains("password missing")
}

/// True when the password we supplied was rejected.
pub fn is_password_rejected(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("password authentication failed")
        || lower.contains("authentication failed")
        || lower.contains("no password supplied")
}

// ---- catalog queries --------------------------------------------------------

pub const LIST_DATABASES_SQL: &str =
    "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate ORDER BY 1";

pub const LIST_SCHEMAS_SQL: &str = "SELECT nspname FROM pg_namespace \
     WHERE nspname NOT LIKE 'pg\\_%' AND nspname <> 'information_schema' ORDER BY 1";

pub fn list_relations_sql(schema: &str) -> String {
    format!(
        "SELECT c.relname, c.relkind::text FROM pg_class c \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = '{}' AND c.relkind IN ('r','p','v','m','f') ORDER BY 1",
        schema.replace('\'', "''")
    )
}
