//! PDO query observation — framework-agnostic (works in any PHP app).
//!
//! Observes `PDO::exec`, `PDO::query`, and `PDOStatement::execute`. Duration is
//! measured `begin`→`end`, so the frame is emitted in `end`. The `connection`
//! field is best-effort `""` here; Laravel's `QueryExecuted` enriches it later.
//!
//! In addition to the parameterized `sql` + `bindings`, we emit `sql_full`: the
//! statement with bound values interpolated for display (never executed).

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use serde_json::Value;

use crate::frame::{truncate, FIELD_CAP};
use crate::request::{self, Feature};
use crate::zend_util::{arg, caller_location};
use ext_php_rs::types::{ZendHashTable, ZendObject, Zval};
use ext_php_rs::zend::ExecuteData;

/// Per-string-binding cap inside the interpolated SQL (keeps `sql_full` bounded
/// even when a value is large, e.g. a big JSON column).
const BINDING_STR_CAP: usize = 4 * 1024;

/// Which PDO symbol fired (decides where the SQL, bindings, and row count come from).
#[derive(Clone, Copy)]
pub enum QueryKind {
    /// `PDO::exec(string $sql)` — returns the affected-row count.
    PdoExec,
    /// `PDO::query(string $sql, ...)` — returns a `PDOStatement`.
    PdoQuery,
    /// `PDOStatement::execute(?array $params)` — SQL from `$this->queryString`.
    StmtExecute,
}

thread_local! {
    /// Start times, as a stack to tolerate nested calls. Reset each request.
    static STARTS: RefCell<Vec<Instant>> = const { RefCell::new(Vec::new()) };
}

/// Clear the timing stack. Called at RINIT so a bailout-leaked frame on a
/// previous request can't desynchronize timings on a reused worker.
pub fn reset() {
    STARTS.with(|s| s.borrow_mut().clear());
}

/// Record the call start.
pub fn on_begin() {
    STARTS.with(|s| s.borrow_mut().push(Instant::now()));
}

/// Emit the query frame with its measured duration and row count.
pub fn on_end(ex: &ExecuteData, kind: QueryKind, retval: Option<&Zval>) {
    let time_ms = STARTS
        .with(|s| s.borrow_mut().pop())
        .map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);

    let q = extract(ex, kind);
    let Some(sql) = q.sql else { return };
    let sql = truncate(&sql, FIELD_CAP);
    let sql_full = truncate(&q.sql_full.unwrap_or_default(), FIELD_CAP);
    let bindings = q.bindings;
    let row_count = row_count(ex, kind, retval);
    let (file, line) = caller_location(ex);

    request::emit(Feature::Queries, "query", move || {
        serde_json::json!({
            "sql": sql,
            "sql_full": sql_full,
            "bindings": bindings,
            "row_count": row_count,
            "time_ms": time_ms,
            "connection": "",
            "file": file,
            "line": line,
        })
    });
}

/// Best-effort affected/returned row count.
///
/// - `PDO::exec` returns the affected-row count directly.
/// - `PDO::query` / `PDOStatement::execute` → `rowCount()` on the statement:
///   exact for writes (INSERT/UPDATE/DELETE); for SELECTs it is driver-dependent
///   (accurate on buffered MySQL, often `0` on SQLite/Postgres). `None` when
///   unavailable — serialized as JSON `null`.
fn row_count(ex: &ExecuteData, kind: QueryKind, retval: Option<&Zval>) -> Option<i64> {
    match kind {
        QueryKind::PdoExec => retval.and_then(|z| z.dereference().long()),
        QueryKind::PdoQuery => retval
            .and_then(|z| z.dereference().object())
            .and_then(call_row_count),
        QueryKind::StmtExecute => ex.This.object().and_then(call_row_count),
    }
}

/// Call `$stmt->rowCount()`. `rowCount` is an internal method (not observed, can't
/// be overridden), so this does not re-enter the observer.
fn call_row_count(stmt: &ZendObject) -> Option<i64> {
    stmt.try_call_method("rowCount", vec![])
        .ok()
        .and_then(|z| z.long())
}

struct Extracted {
    sql: Option<String>,
    bindings: Vec<Value>,
    /// SQL with bound values interpolated for display. Never executed.
    sql_full: Option<String>,
}

/// Extract SQL, bindings, and the interpolated display SQL for the given symbol.
fn extract(ex: &ExecuteData, kind: QueryKind) -> Extracted {
    match kind {
        QueryKind::PdoExec | QueryKind::PdoQuery => {
            // exec()/query() already carry literal values — no placeholders.
            let sql = arg(ex, 0).and_then(Zval::string);
            Extracted {
                sql_full: sql.clone(),
                sql,
                bindings: Vec::new(),
            }
        }
        QueryKind::StmtExecute => {
            // SQL lives on the statement object: $this->queryString.
            let sql = ex
                .This
                .object()
                .and_then(|obj| obj.get_property::<String>("queryString").ok());
            let params = arg(ex, 0).and_then(Zval::array);
            let bindings = params
                .map(|ht| ht.iter().take(256).map(|(_, v)| binding_value(v)).collect())
                .unwrap_or_default();
            let sql_full = match (&sql, params) {
                (Some(s), Some(ht)) => Some(interpolate(s, ht)),
                (Some(s), None) => Some(s.clone()),
                _ => None,
            };
            Extracted {
                sql,
                bindings,
                sql_full,
            }
        }
    }
}

/// Substitute bound values into the prepared SQL for display. Handles positional
/// `?` and named `:name` placeholders, and skips `?`/`:` inside single-quoted
/// string literals. **Display-only** — the result is never sent to the database.
fn interpolate(sql: &str, params: &ZendHashTable) -> String {
    // Integer keys → positional (in order); string keys → named (strip leading ':').
    let mut positional: Vec<String> = Vec::new();
    let mut named: HashMap<String, String> = HashMap::new();
    for (k, v) in params.iter() {
        let key = k.to_string();
        if key.parse::<i64>().is_ok() {
            positional.push(sql_literal(v));
        } else {
            named.insert(key.trim_start_matches(':').to_owned(), sql_literal(v));
        }
    }

    substitute(sql, &positional, &named)
}

/// Pure placeholder substitution (no PHP types) — the testable core of
/// [`interpolate`]. Replaces positional `?` (in order) and named `:name`, while
/// leaving `?`/`:` inside single-quoted string literals untouched.
fn substitute(sql: &str, positional: &[String], named: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(sql.len() + 32);
    let mut chars = sql.chars().peekable();
    let mut pos = 0usize;
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\'' {
                // A doubled '' is an escaped quote, not the end of the literal.
                if chars.peek() == Some(&'\'') {
                    out.push(chars.next().unwrap());
                } else {
                    in_str = false;
                }
            }
            continue;
        }
        match c {
            '\'' => {
                in_str = true;
                out.push(c);
            }
            '?' => {
                if let Some(val) = positional.get(pos) {
                    out.push_str(val);
                    pos += 1;
                } else {
                    out.push('?');
                }
            }
            ':' => {
                let mut name = String::new();
                while let Some(&n) = chars.peek() {
                    if n.is_alphanumeric() || n == '_' {
                        name.push(n);
                        chars.next();
                    } else {
                        break;
                    }
                }
                match named.get(&name) {
                    Some(val) if !name.is_empty() => out.push_str(val),
                    _ => {
                        out.push(':');
                        out.push_str(&name);
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Render a bound value as a SQL literal for display (quoted/escaped, bounded).
fn sql_literal(z: &Zval) -> String {
    let z = z.dereference();
    if z.is_null() {
        "NULL".to_owned()
    } else if let Some(b) = z.bool() {
        // PDO binds bool as int.
        if b { "1" } else { "0" }.to_owned()
    } else if let Some(n) = z.long() {
        n.to_string()
    } else if let Some(f) = z.double() {
        f.to_string()
    } else if let Some(s) = z.str() {
        format!("'{}'", truncate(&s.replace('\'', "''"), BINDING_STR_CAP))
    } else {
        "'(binary)'".to_owned()
    }
}

/// Render a single bound parameter as a JSON-friendly value (bounded).
fn binding_value(z: &Zval) -> Value {
    let z = z.dereference();
    if z.is_null() {
        Value::Null
    } else if let Some(b) = z.bool() {
        Value::Bool(b)
    } else if let Some(n) = z.long() {
        Value::from(n)
    } else if let Some(f) = z.double() {
        Value::from(f)
    } else if let Some(s) = z.str() {
        Value::from(truncate(s, 1024))
    } else {
        Value::from("(binary)")
    }
}

#[cfg(test)]
mod tests {
    use super::substitute;
    use std::collections::HashMap;

    #[test]
    fn positional_in_order() {
        let got = substitute(
            r#"SELECT * FROM "p" WHERE id = ? AND name = ?"#,
            &["7".into(), "'Ada'".into()],
            &HashMap::new(),
        );
        assert_eq!(got, r#"SELECT * FROM "p" WHERE id = 7 AND name = 'Ada'"#);
    }

    #[test]
    fn named_placeholders() {
        let mut named = HashMap::new();
        named.insert("id".to_owned(), "42".to_owned());
        named.insert("active".to_owned(), "1".to_owned());
        let got = substitute("VALUES (:id, :active)", &[], &named);
        assert_eq!(got, "VALUES (42, 1)");
    }

    #[test]
    fn does_not_touch_question_mark_inside_string_literal() {
        // The `?` inside the literal must survive; only the real placeholder binds.
        let got = substitute(
            "WHERE note = 'why? really' AND id = ?",
            &["9".into()],
            &HashMap::new(),
        );
        assert_eq!(got, "WHERE note = 'why? really' AND id = 9");
    }

    #[test]
    fn leftover_placeholder_when_too_few_bindings() {
        let got = substitute("a = ? AND b = ?", &["1".into()], &HashMap::new());
        assert_eq!(got, "a = 1 AND b = ?");
    }

    #[test]
    fn escaped_quote_inside_literal() {
        let got = substitute(
            "name = 'O''Brien' AND id = ?",
            &["3".into()],
            &HashMap::new(),
        );
        assert_eq!(got, "name = 'O''Brien' AND id = 3");
    }

    #[test]
    fn unknown_named_left_intact() {
        let got = substitute("x = :missing", &[], &HashMap::new());
        assert_eq!(got, "x = :missing");
    }
}
