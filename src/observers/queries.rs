//! PDO query observation — framework-agnostic (works in any PHP app).
//!
//! Correlates the calls PHP/Laravel make on a single prepared statement so the
//! emitted frame has real values and a real row count:
//!
//! * `PDOStatement::bindValue` / `bindParam` — Laravel binds parameters this way
//!   and then calls `execute()` with **no** args, so we accumulate the bound
//!   values per statement (keyed by object handle) for interpolation.
//! * `PDO::exec` / `PDO::query` / `PDOStatement::execute` — capture SQL, bindings,
//!   timing, caller `file:line`, and an interpolated `sql_full` (display-only).
//!   Writes emit immediately with the affected-row count; **reads are deferred**
//!   (their row count isn't known until the rows are fetched).
//! * `PDOStatement::fetchAll` — counts the returned rows and emits the deferred
//!   read frame. Reads never fetched (or fetched row-by-row) flush at RSHUTDOWN.
//!
//! Frames keep their execute-time `ts`, so deferred emission preserves ordering.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Instant;

use serde_json::Value;

use crate::frame::{now_ms, truncate, FIELD_CAP};
use crate::request::{self, Feature};
use crate::zend_util::{arg, caller_location, object_handle};
use ext_php_rs::types::{ZendHashTable, ZendObject, Zval};
use ext_php_rs::zend::ExecuteData;

/// Per-string-binding cap inside the interpolated SQL (keeps `sql_full` bounded).
const BINDING_STR_CAP: usize = 4 * 1024;

/// Which observed PDO symbol fired.
#[derive(Clone, Copy)]
pub enum QueryKind {
    /// `PDO::exec(string $sql)` — write; returns affected-row count.
    PdoExec,
    /// `PDO::query(string $sql, ...)` — returns a `PDOStatement` (usually a read).
    PdoQuery,
    /// `PDOStatement::execute(?array $params)` — SQL from `$this->queryString`.
    StmtExecute,
    /// `PDOStatement::bindValue` / `bindParam` — accumulate a bound value.
    BindValue,
    /// `PDOStatement::fetchAll` — count returned rows for a deferred read.
    FetchAll,
}

enum BindKey {
    Positional,
    Named(String),
}

struct Bind {
    key: BindKey,
    /// Display literal for interpolation (quoted/escaped).
    lit: String,
    /// JSON value for the `bindings` array.
    json: Value,
}

/// A read whose row count isn't known until its rows are fetched.
struct Pending {
    ts: u64,
    sql: String,
    sql_full: String,
    bindings: Vec<Value>,
    time_ms: f64,
    file: Option<String>,
    line: u32,
    /// Used if the read is flushed without a `fetchAll` (e.g. row-by-row fetch).
    fallback_rows: Option<i64>,
}

thread_local! {
    /// Start times of in-flight exec/query/execute calls (stack for nesting).
    static STARTS: RefCell<Vec<Instant>> = const { RefCell::new(Vec::new()) };
    /// Accumulated binds per statement handle, awaiting `execute`.
    static BINDS: RefCell<HashMap<u32, Vec<Bind>>> = RefCell::new(HashMap::new());
    /// Deferred reads per statement handle, awaiting `fetchAll`/flush.
    static PENDING: RefCell<HashMap<u32, Pending>> = RefCell::new(HashMap::new());
}

/// Clear all per-request state at RINIT (handles are reused across requests).
pub fn reset() {
    STARTS.with(|s| s.borrow_mut().clear());
    BINDS.with(|b| b.borrow_mut().clear());
    PENDING.with(|p| p.borrow_mut().clear());
}

/// Record an exec/query/execute start.
pub fn on_begin() {
    STARTS.with(|s| s.borrow_mut().push(Instant::now()));
}

/// Accumulate a `bindValue($param, $value)` / `bindParam` for the statement.
pub fn on_bind(ex: &ExecuteData) {
    let Some(stmt) = ex.This.object() else { return };
    let handle = object_handle(stmt);
    let Some(param) = arg(ex, 0) else { return };
    let Some(value) = arg(ex, 1) else { return };

    let key = if param.long().is_some() {
        BindKey::Positional
    } else if let Some(s) = param.str() {
        BindKey::Named(s.trim_start_matches(':').to_owned())
    } else {
        BindKey::Positional
    };
    let bind = Bind {
        key,
        lit: sql_literal(value),
        json: binding_value(value),
    };
    BINDS.with(|b| b.borrow_mut().entry(handle).or_default().push(bind));
}

/// Emit (writes) or defer (reads) the query frame.
pub fn on_end(ex: &ExecuteData, kind: QueryKind, retval: Option<&Zval>) {
    // bindValue/fetchAll never pushed a start (only exec/query/execute do), so
    // they must not pop the timing stack.
    match kind {
        QueryKind::BindValue => return,
        QueryKind::FetchAll => {
            on_fetchall(ex, retval);
            return;
        }
        _ => {}
    }

    let time_ms = STARTS
        .with(|s| s.borrow_mut().pop())
        .map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);
    let (file, line) = caller_location(ex);
    let ts = now_ms();

    match kind {
        QueryKind::PdoExec => {
            // Write with no statement object — emit immediately, count = retval.
            let Some(sql) = arg(ex, 0).and_then(Zval::string) else {
                return;
            };
            let rows = retval.and_then(|z| z.dereference().long());
            emit_query(ts, &sql, &sql, &[], rows, time_ms, file, line);
        }
        QueryKind::PdoQuery => {
            // Returns a statement; usually a read → defer to its fetch.
            let Some(sql) = arg(ex, 0).and_then(Zval::string) else {
                return;
            };
            let stmt = retval.and_then(|z| z.dereference().object());
            defer_or_emit(stmt, ts, sql, Vec::new(), time_ms, file, line);
        }
        QueryKind::StmtExecute => {
            let Some(stmt) = ex.This.object() else { return };
            let handle = object_handle(stmt);
            // Prefer execute([$params]); otherwise use accumulated bindValue()s.
            // Always clear the statement's accumulated binds.
            let taken = BINDS
                .with(|b| b.borrow_mut().remove(&handle))
                .unwrap_or_default();
            let binds = match arg(ex, 0).and_then(Zval::array) {
                Some(ht) => binds_from_array(ht),
                None => taken,
            };
            let Some(sql) = stmt.get_property::<String>("queryString").ok() else {
                return;
            };
            let (positional, named, bindings) = build(&binds);
            let sql_full = substitute(&sql, &positional, &named);

            if is_read(&sql) {
                let fallback = call_row_count(stmt);
                PENDING.with(|p| {
                    p.borrow_mut().insert(
                        handle,
                        Pending {
                            ts,
                            sql,
                            sql_full,
                            bindings,
                            time_ms,
                            file,
                            line,
                            fallback_rows: fallback,
                        },
                    );
                });
            } else {
                let rows = call_row_count(stmt);
                emit_query(ts, &sql, &sql_full, &bindings, rows, time_ms, file, line);
            }
        }
        // BindValue/FetchAll returned early above.
        QueryKind::BindValue | QueryKind::FetchAll => {}
    }
}

/// `fetchAll` completed: emit the deferred read with the real returned-row count.
fn on_fetchall(ex: &ExecuteData, retval: Option<&Zval>) {
    let Some(stmt) = ex.This.object() else { return };
    let handle = object_handle(stmt);
    let Some(p) = PENDING.with(|pp| pp.borrow_mut().remove(&handle)) else {
        return;
    };
    #[allow(clippy::cast_possible_wrap)]
    let rows = retval
        .and_then(|z| z.dereference().array())
        .map(|a| a.len() as i64)
        .or(p.fallback_rows);
    emit_query(
        p.ts,
        &p.sql,
        &p.sql_full,
        &p.bindings,
        rows,
        p.time_ms,
        p.file,
        p.line,
    );
}

/// Emit any reads still pending at RSHUTDOWN (never fetched / row-by-row fetch).
pub fn flush() {
    let pending: Vec<Pending> = PENDING.with(|p| p.borrow_mut().drain().map(|(_, v)| v).collect());
    for p in pending {
        emit_query(
            p.ts,
            &p.sql,
            &p.sql_full,
            &p.bindings,
            p.fallback_rows,
            p.time_ms,
            p.file,
            p.line,
        );
    }
}

/// Store a read as pending (keyed by its statement), or emit now if no statement.
fn defer_or_emit(
    stmt: Option<&ZendObject>,
    ts: u64,
    sql: String,
    bindings: Vec<Value>,
    time_ms: f64,
    file: Option<String>,
    line: u32,
) {
    let sql_full = sql.clone();
    match stmt {
        Some(s) => {
            let fallback = call_row_count(s);
            PENDING.with(|p| {
                p.borrow_mut().insert(
                    object_handle(s),
                    Pending {
                        ts,
                        sql,
                        sql_full,
                        bindings,
                        time_ms,
                        file,
                        line,
                        fallback_rows: fallback,
                    },
                );
            });
        }
        None => emit_query(ts, &sql, &sql_full, &bindings, None, time_ms, file, line),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_query(
    ts: u64,
    sql: &str,
    sql_full: &str,
    bindings: &[Value],
    row_count: Option<i64>,
    time_ms: f64,
    file: Option<String>,
    line: u32,
) {
    let sql = truncate(sql, FIELD_CAP);
    let sql_full = truncate(sql_full, FIELD_CAP);
    let bindings = bindings.to_vec();
    request::emit_at(Feature::Queries, "query", ts, move || {
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

/// Call `$stmt->rowCount()` (internal method — not observed, can't re-enter).
fn call_row_count(stmt: &ZendObject) -> Option<i64> {
    stmt.try_call_method("rowCount", vec![])
        .ok()
        .and_then(|z| z.long())
}

/// Whether a statement is a read (its row count must come from the fetch, not
/// `rowCount()`, which only reports affected rows).
fn is_read(sql: &str) -> bool {
    let kw = sql.split_whitespace().next().unwrap_or("");
    matches!(
        kw.to_ascii_uppercase().as_str(),
        "SELECT" | "SHOW" | "WITH" | "PRAGMA" | "EXPLAIN" | "DESCRIBE" | "DESC"
    )
}

/// Build positional/named literal maps + the JSON bindings array from binds.
fn build(binds: &[Bind]) -> (Vec<String>, HashMap<String, String>, Vec<Value>) {
    let mut positional = Vec::new();
    let mut named = HashMap::new();
    let mut json = Vec::new();
    for b in binds {
        json.push(b.json.clone());
        match &b.key {
            BindKey::Positional => positional.push(b.lit.clone()),
            BindKey::Named(n) => {
                named.insert(n.clone(), b.lit.clone());
            }
        }
    }
    (positional, named, json)
}

/// Convert an `execute([$params])` array into binds (in array order).
fn binds_from_array(ht: &ZendHashTable) -> Vec<Bind> {
    ht.iter()
        .take(512)
        .map(|(k, v)| {
            let key = k.to_string();
            let bk = if key.parse::<i64>().is_ok() {
                BindKey::Positional
            } else {
                BindKey::Named(key.trim_start_matches(':').to_owned())
            };
            Bind {
                key: bk,
                lit: sql_literal(v),
                json: binding_value(v),
            }
        })
        .collect()
}

/// Pure placeholder substitution (no PHP types) — the testable core. Replaces
/// positional `?` (in order) and named `:name`, leaving `?`/`:` inside
/// single-quoted string literals untouched.
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

    #[test]
    fn reads_are_detected() {
        for s in [
            "select 1",
            "  SELECT *",
            "WITH x AS (..)",
            "pragma foo",
            "explain select",
        ] {
            assert!(super::is_read(s), "{s} should be a read");
        }
        for s in ["insert into t", "UPDATE t", "delete from t", "create table"] {
            assert!(!super::is_read(s), "{s} should be a write");
        }
    }
}
