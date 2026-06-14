//! PDO query observation — framework-agnostic (works in any PHP app).
//!
//! Observes `PDO::exec`, `PDO::query`, and `PDOStatement::execute`. Duration is
//! measured `begin`→`end`, so the frame is emitted in `end`. The `connection`
//! field is best-effort `""` here; Laravel's `QueryExecuted` enriches it later.

use std::cell::RefCell;
use std::time::Instant;

use serde_json::Value;

use crate::frame::{truncate, FIELD_CAP};
use crate::request::{self, Feature};
use crate::zend_util::{arg, caller_location};
use ext_php_rs::types::Zval;
use ext_php_rs::zend::ExecuteData;

/// Which PDO symbol fired (decides where the SQL and bindings come from).
#[derive(Clone, Copy)]
pub enum QueryKind {
    /// `PDO::exec(string $sql)` / `PDO::query(string $sql, ...)`.
    PdoSqlArg,
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

/// Emit the query frame with its measured duration.
pub fn on_end(ex: &ExecuteData, kind: QueryKind) {
    let time_ms = STARTS
        .with(|s| s.borrow_mut().pop())
        .map_or(0.0, |t| t.elapsed().as_secs_f64() * 1000.0);

    let (sql, bindings) = extract(ex, kind);
    let Some(sql) = sql else { return };
    let sql = truncate(&sql, FIELD_CAP);
    let (file, line) = caller_location(ex);

    request::emit(Feature::Queries, "query", move || {
        serde_json::json!({
            "sql": sql,
            "bindings": bindings,
            "time_ms": time_ms,
            "connection": "",
            "file": file,
            "line": line,
        })
    });
}

/// Extract `(sql, bindings)` for the given PDO symbol.
fn extract(ex: &ExecuteData, kind: QueryKind) -> (Option<String>, Vec<Value>) {
    match kind {
        QueryKind::PdoSqlArg => {
            let sql = arg(ex, 0).and_then(Zval::string);
            (sql, Vec::new())
        }
        QueryKind::StmtExecute => {
            // SQL lives on the statement object: $this->queryString.
            let sql = ex
                .This
                .object()
                .and_then(|obj| obj.get_property::<String>("queryString").ok());
            let bindings = arg(ex, 0)
                .and_then(Zval::array)
                .map(|ht| ht.iter().take(256).map(|(_, v)| binding_value(v)).collect())
                .unwrap_or_default();
            (sql, bindings)
        }
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
