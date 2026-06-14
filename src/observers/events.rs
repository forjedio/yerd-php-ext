//! Laravel signal observation via `Illuminate\Events\Dispatcher::dispatch`.
//!
//! One observation point funnels jobs, cache, logs, and views. The event is the
//! first argument: usually an event **object** (class name → category), or a
//! **string** name (e.g. `"composing: home"` for views). Emitted in `begin`
//! (the event is fully formed by then; `dispatch` returns normally).
//!
//! Each frame carries the app `file:line` that triggered it (e.g. the
//! `Cache::get`/`Log::info`/`view()` call site), resolved by walking past
//! `vendor/`. Note: `job` events fire inside the queue worker during execution,
//! so their `file:line` is typically empty / not the dispatch site.
//!
//! Property extraction is best-effort and bounded: a missing/odd property yields
//! an empty value rather than an error, so non-standard Laravel versions degrade
//! gracefully.

use std::collections::HashMap;

use serde_json::Value;

use crate::frame::{truncate, FIELD_CAP};
use crate::request::{self, Feature};
use crate::zend_util::{arg, caller_location};
use ext_php_rs::types::ZendObject;
use ext_php_rs::zend::ExecuteData;

/// Handle a `Dispatcher::dispatch($event, ...)` call.
pub fn on_dispatch(ex: &ExecuteData) {
    let Some(z) = arg(ex, 0) else {
        return;
    };
    let z = z.dereference();
    let (file, line) = caller_location(ex);

    if let Some(obj) = z.object() {
        let Ok(class) = obj.get_class_name() else {
            return;
        };
        dispatch_object(class.as_str(), obj, file, line);
    } else if let Some(name) = z.str() {
        dispatch_string(name, file, line);
    }
}

fn dispatch_object(class: &str, obj: &ZendObject, file: Option<String>, line: u32) {
    match class {
        "Illuminate\\Queue\\Events\\JobProcessing" => emit_job(obj, "processing", file, line),
        "Illuminate\\Queue\\Events\\JobProcessed" => emit_job(obj, "processed", file, line),
        "Illuminate\\Queue\\Events\\JobFailed" => emit_job(obj, "failed", file, line),

        "Illuminate\\Cache\\Events\\CacheHit" => emit_cache(obj, "hit", file, line),
        "Illuminate\\Cache\\Events\\CacheMissed" => emit_cache(obj, "missed", file, line),
        "Illuminate\\Cache\\Events\\KeyWritten" => emit_cache(obj, "written", file, line),
        "Illuminate\\Cache\\Events\\KeyForgotten" => emit_cache(obj, "forgotten", file, line),

        "Illuminate\\Log\\Events\\MessageLogged" => emit_log(obj, file, line),

        _ => {}
    }
}

/// String events: views are dispatched as `"composing: <name>"` / `"creating: <name>"`.
fn dispatch_string(name: &str, file: Option<String>, line: u32) {
    for prefix in ["composing: ", "creating: "] {
        if let Some(view) = name.strip_prefix(prefix) {
            let view = view.to_owned();
            request::emit(
                Feature::Views,
                "view",
                move || serde_json::json!({ "name": view, "path": "", "data_keys": [], "file": file, "line": line }),
            );
            return;
        }
    }
}

fn emit_job(obj: &ZendObject, status: &str, file: Option<String>, line: u32) {
    let connection = prop_str(obj, "connectionName").unwrap_or_default();
    let queue = prop_str(obj, "queue").unwrap_or_default();
    let name = job_name(obj);
    let status = status.to_owned();
    request::emit(Feature::Jobs, "job", move || {
        serde_json::json!({
            "name": name,
            "connection": connection,
            "queue": queue,
            "status": status,
            "file": file,
            "line": line,
        })
    });
}

fn emit_cache(obj: &ZendObject, event: &str, file: Option<String>, line: u32) {
    let key = prop_str(obj, "key").unwrap_or_default();
    let store = prop_str(obj, "storeName").unwrap_or_default();
    let event = event.to_owned();
    request::emit(
        Feature::Cache,
        "cache",
        move || serde_json::json!({ "event": event, "key": key, "store": store, "file": file, "line": line }),
    );
}

fn emit_log(obj: &ZendObject, file: Option<String>, line: u32) {
    let level = prop_str(obj, "level").unwrap_or_default();
    let message = prop_str(obj, "message")
        .map(|m| truncate(&m, FIELD_CAP))
        .unwrap_or_default();
    // Read context as an OWNED map: `get_property::<&Zval>` would borrow a value
    // that may point into a temporary the engine drops (use-after-free). An owned
    // `HashMap<String, String>` copies the values out before that temporary dies.
    // Non-string-coercible contexts simply yield an empty map (best-effort).
    let context = obj
        .get_property::<HashMap<String, String>>("context")
        .ok()
        .map(|map| {
            let mut out = serde_json::Map::new();
            for (k, v) in map.into_iter().take(64) {
                out.insert(k, Value::from(truncate(&v, 4096)));
            }
            Value::Object(out)
        })
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    request::emit(
        Feature::Logs,
        "log",
        move || serde_json::json!({ "level": level, "message": message, "context": context, "file": file, "line": line }),
    );
}

/// Best-effort job display name: the `Job::resolveName()`-style info isn't
/// cheaply reachable, so fall back to the queued job's class if present.
fn job_name(obj: &ZendObject) -> String {
    prop_str(obj, "job")
        .or_else(|| obj.get_class_name().ok())
        .unwrap_or_default()
}

fn prop_str(obj: &ZendObject, name: &str) -> Option<String> {
    obj.get_property::<String>(name)
        .ok()
        .filter(|s| !s.is_empty())
}
