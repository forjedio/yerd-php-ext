//! Outgoing HTTP request observation via `curl_exec`.
//!
//! Emitted in `end` (we need the response — status, effective URL, total time).
//! `curl_exec` returns normally (no bailout), so `end` is safe. After it returns
//! we call PHP's `curl_getinfo($handle)` to read the result.
//!
//! This also captures **Guzzle** and PSR-18 clients that use the curl handler
//! (the common synchronous case, since they call `curl_exec` underneath).
//! Stream-based clients and `curl_multi_exec` (async/pooled) are not yet covered.
//!
//! Safety: `curl_getinfo` is an internal function (not observed, can't be
//! overridden), so this does not re-enter our observer. The PHP call happens
//! before `request::emit`, so no thread-local borrow is held across it.

use ext_php_rs::convert::IntoZvalDyn;
use ext_php_rs::types::Zval;
use ext_php_rs::zend::{ExecuteData, Function};

use crate::frame::{truncate, FIELD_CAP};
use crate::request::{self, Feature};
use crate::zend_util::{arg, caller_location};

/// Handle `curl_exec($handle)` completion: read `curl_getinfo` and emit `http`.
pub fn on_curl_exec_end(ex: &ExecuteData) {
    let Some(handle) = arg(ex, 0) else { return };

    // curl_getinfo($handle) → associative array of transfer info.
    let Some(getinfo) = Function::try_from_function("curl_getinfo") else {
        return;
    };
    let Ok(info) = getinfo.try_call(vec![handle as &dyn IntoZvalDyn]) else {
        return;
    };
    let Some(arr) = info.array() else { return };

    let url = arr
        .get("url")
        .and_then(Zval::str)
        .unwrap_or_default()
        .to_owned();
    if url.is_empty() {
        return; // nothing useful to report
    }
    let status = arr.get("http_code").and_then(Zval::long).unwrap_or(0);
    // curl reports total_time in seconds (float).
    let duration_ms = arr.get("total_time").and_then(Zval::double).unwrap_or(0.0) * 1000.0;
    // effective_method is present on newer curl; best-effort otherwise.
    let method = arr
        .get("effective_method")
        .and_then(Zval::str)
        .unwrap_or_default()
        .to_owned();
    let url = truncate(&url, FIELD_CAP);
    let (file, line) = caller_location(ex);

    request::emit(Feature::Http, "http", move || {
        serde_json::json!({
            "method": method,
            "url": url,
            "status": status,
            "duration_ms": duration_ms,
            "file": file,
            "line": line,
        })
    });
}
