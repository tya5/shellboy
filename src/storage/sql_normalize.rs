//! Conservative SQL shape rendering: never emit identifiers or literal values.
use sha2::{Digest, Sha256};

const MAX_SQL: usize = 256;

pub(super) fn normalize(sql: &[u8]) -> String {
    fn shape(sql: &[u8]) -> Option<String> {
        let mut out = String::new();
        let mut i = 0;
        while i < sql.len() {
            let c = sql[i];
            if c.is_ascii_whitespace() {
                i += 1;
                continue;
            }
            let start = i;
            let token: String;
            if c == 39 {
                i += 1;
                loop {
                    if i >= sql.len() {
                        return None;
                    }
                    if sql[i] == 39 {
                        i += 1;
                        if i < sql.len() && sql[i] == 39 {
                            i += 1;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                token = "?".into();
            } else if c.is_ascii_alphabetic() || c == b'_' {
                i += 1;
                while i < sql.len() && (sql[i].is_ascii_alphanumeric() || sql[i] == b'_') {
                    i += 1;
                }
                let word = std::str::from_utf8(&sql[start..i])
                    .ok()?
                    .to_ascii_uppercase();
                token = match word.as_str() {
                    "SELECT" | "FROM" | "WHERE" | "AND" | "OR" | "NOT" | "NULL" | "IS" | "AS"
                    | "JOIN" | "ON" | "LEFT" | "INNER" | "OUTER" | "ORDER" | "BY" | "GROUP"
                    | "HAVING" | "LIMIT" | "OFFSET" | "INSERT" | "INTO" | "VALUES" | "UPDATE"
                    | "SET" | "DELETE" | "WITH" | "RECURSIVE" | "UNION" | "ALL" | "DISTINCT"
                    | "CASE" | "WHEN" | "THEN" | "ELSE" | "END" | "EXISTS" | "IN" | "LIKE"
                    | "ASC" | "DESC" | "RETURNING" => word,
                    _ => "?".into(),
                };
            } else if c.is_ascii_digit()
                || (c == b'.' && i + 1 < sql.len() && sql[i + 1].is_ascii_digit())
            {
                if c == b'0' && i + 1 < sql.len() && matches!(sql[i + 1], b'x' | b'X') {
                    i += 2;
                    while i < sql.len() && (sql[i].is_ascii_hexdigit() || sql[i] == b'_') {
                        i += 1;
                    }
                } else {
                    i += 1;
                    while i < sql.len()
                        && (sql[i].is_ascii_digit() || matches!(sql[i], b'.' | b'_'))
                    {
                        i += 1;
                    }
                    if i < sql.len() && matches!(sql[i], b'e' | b'E') {
                        i += 1;
                        if i < sql.len() && matches!(sql[i], b'+' | b'-') {
                            i += 1;
                        }
                        while i < sql.len() && (sql[i].is_ascii_digit() || sql[i] == b'_') {
                            i += 1;
                        }
                    }
                }
                token = "?".into();
            } else if c == b'?' {
                i += 1;
                while i < sql.len() && sql[i].is_ascii_digit() {
                    i += 1;
                }
                token = "?".into();
            } else if matches!(
                c,
                b'(' | b')'
                    | b','
                    | b'.'
                    | b'='
                    | b'<'
                    | b'>'
                    | b'+'
                    | b'-'
                    | b'*'
                    | b'/'
                    | b'%'
                    | b';'
                    | b'!'
                    | b'|'
                    | b'&'
                    | b'~'
            ) {
                // Comments and unsupported syntax fall back to a hash.
                if i + 1 < sql.len()
                    && ((c == b'-' && sql[i + 1] == b'-') || (c == b'/' && sql[i + 1] == b'*'))
                {
                    return None;
                }
                i += 1;
                token = (c as char).to_string();
            } else {
                return None;
            }
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&token);
            if out.len() > MAX_SQL {
                return None;
            }
        }
        Some(out)
    }
    shape(sql).unwrap_or_else(|| {
        use std::fmt::Write;
        let mut hash = String::from("sha256:");
        for byte in Sha256::digest(sql) {
            let _ = write!(hash, "{byte:02x}");
        }
        hash
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_values_and_hashes_unsupported_syntax() {
        let raw = b"SELECT 'private body', x'4142', 1.23e-45 FROM messages WHERE id=?123";
        let value = normalize(raw);
        assert!(value.starts_with("SELECT"));
        assert!(!value.contains("private"));
        assert!(!value.contains("4142"));
        assert!(!value.contains("123"));
        assert!(normalize(b"SELECT 1 -- private").starts_with("sha256:"));
        assert!(normalize(&[b'?'; 1000]).starts_with("sha256:"));
    }
}
