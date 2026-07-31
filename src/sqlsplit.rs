// Splitting a buffer into individual SQL statements.
//
// The lexer skips over the constructs where a `;` is not a separator:
// line and (nesting) block comments, single-quoted strings including the
// `''` and `E'\''` escapes, quoted identifiers, and dollar-quoted bodies.

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Byte ranges of each non-empty statement, excluding the trailing `;`.
pub fn statement_ranges(sql: &str) -> Vec<(usize, usize)> {
    let b = sql.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < n {
        match b[i] {
            b'-' if i + 1 < n && b[i + 1] == b'-' => {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                let mut depth = 1;
                i += 2;
                while i < n && depth > 0 {
                    if i + 1 < n && b[i] == b'*' && b[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                    } else if i + 1 < n && b[i] == b'/' && b[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'\'' => {
                // E'...' strings honor backslash escapes; plain ones do not.
                let escaped = i > 0
                    && (b[i - 1] == b'e' || b[i - 1] == b'E')
                    && (i == 1 || !is_word_byte(b[i - 2]));
                i += 1;
                while i < n {
                    if b[i] == b'\\' && escaped {
                        i += 2;
                    } else if b[i] == b'\'' {
                        if i + 1 < n && b[i + 1] == b'\'' {
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'"' => {
                i += 1;
                while i < n {
                    if b[i] == b'"' {
                        if i + 1 < n && b[i + 1] == b'"' {
                            i += 2;
                        } else {
                            i += 1;
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'$' => {
                let tag_start = i;
                let mut j = i + 1;
                while j < n && is_word_byte(b[j]) {
                    j += 1;
                }
                if j < n && b[j] == b'$' {
                    let tag = &sql[tag_start..=j];
                    i = j + 1;
                    match sql[i..].find(tag) {
                        Some(pos) => i += pos + tag.len(),
                        None => i = n,
                    }
                } else {
                    i += 1;
                }
            }
            b';' => {
                out.push((start, i));
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < n {
        out.push((start, n));
    }
    out.retain(|(s, e)| !sql[*s..*e].trim().is_empty());
    out
}

/// The statement containing `offset`, or the nearest preceding one when the
/// cursor sits on the separator or trailing whitespace.
pub fn statement_at(sql: &str, offset: usize) -> Option<String> {
    let ranges = statement_ranges(sql);
    if ranges.is_empty() {
        return None;
    }
    let hit = ranges
        .iter()
        .find(|(s, e)| offset >= *s && offset <= *e)
        .or_else(|| ranges.iter().rev().find(|(s, _)| *s <= offset))
        .or_else(|| ranges.first())?;
    let text = sql[hit.0..hit.1].trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

/// Leading keyword of each statement, uppercased — used to follow BEGIN /
/// COMMIT / ROLLBACK across a batch.
pub fn leading_keywords(sql: &str) -> Vec<String> {
    statement_ranges(sql)
        .into_iter()
        .filter_map(|(s, e)| {
            sql[s..e]
                .split_whitespace()
                .next()
                .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_uppercase())
        })
        .filter(|w| !w.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stmts(sql: &str) -> Vec<String> {
        statement_ranges(sql)
            .into_iter()
            .map(|(s, e)| sql[s..e].trim().to_string())
            .collect()
    }

    #[test]
    fn splits_plain_statements() {
        assert_eq!(stmts("SELECT 1; SELECT 2;"), vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn ignores_semicolons_in_strings() {
        assert_eq!(stmts("SELECT ';'; SELECT 2"), vec!["SELECT ';'", "SELECT 2"]);
        assert_eq!(stmts("SELECT 'it''s; ok'"), vec!["SELECT 'it''s; ok'"]);
        assert_eq!(stmts(r"SELECT E'a\'; b'"), vec![r"SELECT E'a\'; b'"]);
    }

    #[test]
    fn ignores_semicolons_in_comments() {
        assert_eq!(stmts("SELECT 1 -- ;nope\n; SELECT 2"), vec!["SELECT 1 -- ;nope", "SELECT 2"]);
        assert_eq!(stmts("/* a ; /* b ; */ c */ SELECT 1"), vec!["/* a ; /* b ; */ c */ SELECT 1"]);
    }

    #[test]
    fn ignores_semicolons_in_dollar_quotes() {
        let sql = "CREATE FUNCTION f() RETURNS int AS $fn$ BEGIN RETURN 1; END $fn$ LANGUAGE plpgsql; SELECT f()";
        assert_eq!(stmts(sql).len(), 2);
    }

    #[test]
    fn ignores_semicolons_in_quoted_idents() {
        assert_eq!(stmts(r#"SELECT "a;b" FROM t"#), vec![r#"SELECT "a;b" FROM t"#]);
    }

    #[test]
    fn skips_empty_statements() {
        assert_eq!(stmts(";;\n\n; SELECT 1 ;;"), vec!["SELECT 1"]);
    }

    #[test]
    fn finds_statement_under_cursor() {
        let sql = "SELECT 1;\nSELECT 2;\nSELECT 3;";
        assert_eq!(statement_at(sql, 0).unwrap(), "SELECT 1");
        assert_eq!(statement_at(sql, 12).unwrap(), "SELECT 2");
        assert_eq!(statement_at(sql, sql.len()).unwrap(), "SELECT 3");
    }

    #[test]
    fn tracks_transaction_keywords() {
        assert_eq!(
            leading_keywords("BEGIN; UPDATE t SET a=1; COMMIT;"),
            vec!["BEGIN", "UPDATE", "COMMIT"]
        );
    }
}
