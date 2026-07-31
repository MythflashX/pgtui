use ratatui::style::{Color, Modifier, Style};

// Lexer state carried across lines (block comments and dollar-quoted strings span lines).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum LexState {
    #[default]
    Normal,
    /// PostgreSQL block comments nest; track depth.
    BlockComment(u32),
    /// Inside a dollar-quoted string; holds the full delimiter, e.g. "$fn$".
    Dollar(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    Plain,
    Keyword,
    String,
    Number,
    Comment,
    Ident, // "quoted identifier"
    Operator,
    Function,
    Type,
}

fn style_for(tok: Tok) -> Style {
    match tok {
        Tok::Plain => Style::default().fg(Color::Reset),
        Tok::Keyword => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        Tok::String => Style::default().fg(Color::Green),
        Tok::Number => Style::default().fg(Color::Yellow),
        Tok::Comment => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
        Tok::Ident => Style::default().fg(Color::Cyan),
        Tok::Operator => Style::default().fg(Color::LightBlue),
        Tok::Function => Style::default().fg(Color::Blue),
        Tok::Type => Style::default().fg(Color::LightCyan),
    }
}

const KEYWORDS: &[&str] = &[
    "select", "from", "where", "insert", "into", "values", "update", "set", "delete", "create",
    "table", "view", "index", "drop", "alter", "add", "column", "primary", "key", "foreign",
    "references", "unique", "not", "null", "default", "and", "or", "in", "is", "like", "ilike",
    "between", "exists", "case", "when", "then", "else", "end", "as", "on", "join", "inner",
    "left", "right", "full", "outer", "cross", "union", "all", "distinct", "group", "by",
    "having", "order", "asc", "desc", "limit", "offset", "begin", "commit", "rollback",
    "transaction", "explain", "analyze", "verbose", "vacuum", "grant", "revoke", "with",
    "recursive", "returning", "using", "cascade", "restrict", "if", "replace", "temp",
    "temporary", "materialized", "schema", "database", "sequence", "function", "procedure",
    "trigger", "type", "domain", "constraint", "check", "true", "false", "cast", "over",
    "partition", "window", "row", "rows", "range", "current", "preceding", "following",
    "unbounded", "lateral", "natural", "conflict", "do", "nothing", "excluded", "fetch",
    "first", "next", "only", "for", "of", "share", "nowait", "skip", "locked", "tablespace",
    "extension", "comment", "rename", "owner", "to", "copy", "truncate", "listen", "notify",
    "prepare", "execute", "deallocate", "declare", "cursor", "close", "grant", "any", "some",
    "array", "interval", "isnull", "notnull", "collate", "filter", "within", "ordinality",
    "generated", "always", "identity", "returns", "language", "immutable", "stable", "volatile",
];

const TYPES: &[&str] = &[
    "int", "integer", "smallint", "bigint", "serial", "bigserial", "smallserial", "numeric",
    "decimal", "real", "double", "precision", "money", "text", "varchar", "char", "character",
    "varying", "bytea", "timestamp", "timestamptz", "date", "time", "timetz", "boolean", "bool",
    "uuid", "json", "jsonb", "xml", "inet", "cidr", "macaddr", "point", "line", "polygon",
    "tsvector", "tsquery", "float", "float4", "float8", "int2", "int4", "int8", "oid",
];

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Compute a style per character of `line`, advancing `state` past the line.
pub fn line_styles(line: &str, state: &mut LexState) -> Vec<Style> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut toks: Vec<Tok> = vec![Tok::Plain; n];
    let mut i = 0;

    while i < n {
        match state.clone() {
            LexState::BlockComment(depth) => {
                let mut depth = depth;
                let start = i;
                while i < n {
                    if i + 1 < n && chars[i] == '*' && chars[i + 1] == '/' {
                        i += 2;
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    } else if i + 1 < n && chars[i] == '/' && chars[i + 1] == '*' {
                        i += 2;
                        depth += 1;
                    } else {
                        i += 1;
                    }
                }
                for t in &mut toks[start..i] {
                    *t = Tok::Comment;
                }
                *state = if depth == 0 {
                    LexState::Normal
                } else {
                    LexState::BlockComment(depth)
                };
            }
            LexState::Dollar(tag) => {
                let start = i;
                let tag_chars: Vec<char> = tag.chars().collect();
                let mut closed = false;
                while i < n {
                    if chars[i] == '$' && i + tag_chars.len() <= n
                        && chars[i..i + tag_chars.len()] == tag_chars[..]
                    {
                        i += tag_chars.len();
                        closed = true;
                        break;
                    }
                    i += 1;
                }
                for t in &mut toks[start..i] {
                    *t = Tok::String;
                }
                if closed {
                    *state = LexState::Normal;
                }
            }
            LexState::Normal => {
                let c = chars[i];
                if c == '-' && i + 1 < n && chars[i + 1] == '-' {
                    for t in &mut toks[i..n] {
                        *t = Tok::Comment;
                    }
                    i = n;
                } else if c == '/' && i + 1 < n && chars[i + 1] == '*' {
                    *state = LexState::BlockComment(1);
                    toks[i] = Tok::Comment;
                    toks[i + 1] = Tok::Comment;
                    i += 2;
                } else if c == '\'' || ((c == 'e' || c == 'E') && i + 1 < n && chars[i + 1] == '\'')
                {
                    let start = i;
                    if c != '\'' {
                        i += 1; // skip the E prefix
                    }
                    i += 1; // opening quote
                    while i < n {
                        if chars[i] == '\'' {
                            if i + 1 < n && chars[i + 1] == '\'' {
                                i += 2; // escaped quote
                            } else {
                                i += 1;
                                break;
                            }
                        } else if chars[i] == '\\' && (c == 'e' || c == 'E') {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    for t in &mut toks[start..i.min(n)] {
                        *t = Tok::String;
                    }
                } else if c == '"' {
                    let start = i;
                    i += 1;
                    while i < n {
                        if chars[i] == '"' {
                            i += 1;
                            break;
                        }
                        i += 1;
                    }
                    for t in &mut toks[start..i.min(n)] {
                        *t = Tok::Ident;
                    }
                } else if c == '$' {
                    // Dollar quote: $tag$ or $$ (tag is word chars only)
                    let start = i;
                    let mut j = i + 1;
                    while j < n && is_word_char(chars[j]) {
                        j += 1;
                    }
                    if j < n && chars[j] == '$' {
                        let tag: String = chars[start..=j].iter().collect();
                        for t in &mut toks[start..=j] {
                            *t = Tok::String;
                        }
                        i = j + 1;
                        *state = LexState::Dollar(tag);
                    } else {
                        i += 1;
                    }
                } else if c.is_ascii_digit() {
                    let start = i;
                    while i < n && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                        i += 1;
                    }
                    for t in &mut toks[start..i] {
                        *t = Tok::Number;
                    }
                } else if is_word_char(c) {
                    let start = i;
                    while i < n && is_word_char(chars[i]) {
                        i += 1;
                    }
                    let word: String = chars[start..i].iter().collect::<String>().to_lowercase();
                    let tok = if KEYWORDS.contains(&word.as_str()) {
                        Tok::Keyword
                    } else if TYPES.contains(&word.as_str()) {
                        Tok::Type
                    } else if i < n && chars[i] == '(' {
                        Tok::Function
                    } else {
                        Tok::Plain
                    };
                    for t in &mut toks[start..i] {
                        *t = tok;
                    }
                } else if "+-*/<>=~!@#%^&|`?".contains(c) {
                    toks[i] = Tok::Operator;
                    i += 1;
                } else {
                    i += 1;
                }
            }
        }
    }

    toks.into_iter().map(style_for).collect()
}
