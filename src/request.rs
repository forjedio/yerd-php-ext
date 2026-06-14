//! Per-request state and the emit entry point.
//!
//! FPM is non-thread-safe (one request per worker thread at a time), so all
//! mutable per-request state lives in a `thread_local!` — no locks, and the
//! observer singleton itself stays trivially `Send + Sync`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

use serde_json::Value;

use crate::config::{Features, State};
use crate::frame;
use crate::transport::Conn;

/// Feature categories that gate individual observers.
#[derive(Clone, Copy)]
pub enum Feature {
    Dumps,
    Queries,
    Jobs,
    Views,
    Requests,
    Logs,
    Cache,
}

impl Features {
    #[must_use]
    fn allows(&self, f: Feature) -> bool {
        match f {
            Feature::Dumps => self.dumps,
            Feature::Queries => self.queries,
            Feature::Jobs => self.jobs,
            Feature::Views => self.views,
            Feature::Requests => self.requests,
            Feature::Logs => self.logs,
            Feature::Cache => self.cache,
        }
    }
}

struct RequestCtx {
    request_id: String,
    site: String,
    state: State,
    conn: Conn,
    started: Instant,
}

thread_local! {
    /// `Some` for an active, enabled request; `None` on the disabled fast path.
    static CTX: RefCell<Option<RequestCtx>> = const { RefCell::new(None) };
}

/// Generate a request id: a per-process random seed + a monotonic counter,
/// hex-encoded to 32 chars. Not a security token (it only groups GUI rows), so
/// this never blocks and never fails.
fn new_request_id() -> String {
    static SEED: OnceLock<u64> = OnceLock::new();
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let seed = *SEED.get_or_init(|| {
        let mut buf = [0u8; 8];
        // Best-effort randomness; fall back to a fixed value if unavailable.
        let _ = getrandom::getrandom(&mut buf);
        u64::from_le_bytes(buf)
    });
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{seed:016x}{n:016x}")
}

/// Best-effort `$_SERVER[HTTP_HOST | SERVER_NAME]` for the `site` field.
fn read_site() -> String {
    use ext_php_rs::zend::ProcessGlobals;
    let globals = ProcessGlobals::get();
    let Some(server) = globals.http_server_vars() else {
        return String::new();
    };
    for key in ["HTTP_HOST", "SERVER_NAME"] {
        if let Some(v) = server.get(key).and_then(ext_php_rs::types::Zval::str) {
            if !v.is_empty() {
                return v.to_owned();
            }
        }
    }
    String::new()
}

/// RINIT: load `state.json`; if enabled, arm the request. Cheap no-op when off.
pub fn on_rinit() {
    let ctx = State::load().map(|state| RequestCtx {
        request_id: new_request_id(),
        site: read_site(),
        state,
        conn: Conn::Idle,
        started: Instant::now(),
    });
    CTX.with(|c| *c.borrow_mut() = ctx);
}

/// RSHUTDOWN: emit the request summary (if enabled), then tear down the request
/// (drops the socket).
pub fn on_rshutdown() {
    emit_request_summary();
    CTX.with(|c| *c.borrow_mut() = None);
}

/// Whether telemetry is active and `feature` is enabled for this request.
/// Cheap; used by observers to bail before doing any rendering work.
#[must_use]
pub fn active(feature: Feature) -> bool {
    CTX.with(|c| {
        c.borrow()
            .as_ref()
            .is_some_and(|ctx| ctx.state.features.allows(feature))
    })
}

/// Build a frame line (via `make_payload`) and send it, but only if `feature` is
/// enabled. `make_payload` is invoked lazily so disabled categories cost nothing.
///
/// INVARIANT: `make_payload` must not call back into PHP (e.g. lazy `__toString`
/// / `get_property` on a magic object) — it runs while this `RefCell` is held, so
/// a re-entrant observer would double-borrow. All observers build the payload
/// from already-owned data captured *before* calling `emit`. (A violation would
/// only panic and be swallowed by the guard, never UB — but don't rely on that.)
pub fn emit<F>(feature: Feature, category: &str, make_payload: F)
where
    F: FnOnce() -> Value,
{
    CTX.with(|c| {
        let mut guard = c.borrow_mut();
        let Some(ctx) = guard.as_mut() else { return };
        if !ctx.state.features.allows(feature) {
            return;
        }
        let payload = make_payload();
        let line = frame::build_line(
            category,
            frame::now_ms(),
            &ctx.site,
            &ctx.request_id,
            payload,
        );
        ctx.conn.send(ctx.state.port, &line);
    });
}

/// Assemble and send the `request` summary frame from SAPI globals.
fn emit_request_summary() {
    use ext_php_rs::zend::{ProcessGlobals, SapiGlobals};

    CTX.with(|c| {
        let mut guard = c.borrow_mut();
        let Some(ctx) = guard.as_mut() else { return };
        if !ctx.state.features.allows(Feature::Requests) {
            return;
        }

        let sapi = SapiGlobals::get();
        let info = sapi.request_info();
        let method = info.request_method().unwrap_or("").to_owned();
        let uri = info.request_uri().unwrap_or("").to_owned();
        // Status via the SAPI globals (C-level), NOT the userland
        // `http_response_code()` — reliable under fpm-fcgi at RSHUTDOWN.
        let status = sapi.sapi_headers().http_response_code;
        drop(sapi);

        let ip = {
            let pg = ProcessGlobals::get();
            pg.http_server_vars()
                .and_then(|s| s.get("REMOTE_ADDR"))
                .and_then(ext_php_rs::types::Zval::str)
                .unwrap_or("")
                .to_owned()
        };

        let duration_ms = ctx.started.elapsed().as_secs_f64() * 1000.0;
        let payload = serde_json::json!({
            "method": method,
            "uri": uri,
            "status": status,
            "duration_ms": duration_ms,
            "ip": ip,
        });
        let line = frame::build_line(
            "request",
            frame::now_ms(),
            &ctx.site,
            &ctx.request_id,
            payload,
        );
        ctx.conn.send(ctx.state.port, &line);
    });
}
