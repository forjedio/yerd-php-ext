//! `yerd-dump` — a native PHP extension (Rust + ext-php-rs) that captures
//! Laravel/PHP telemetry and streams it as newline-delimited JSON to Yerd's
//! loopback dump server.
//!
//! Safety posture: `unsafe` is permitted (Zend C ABI), but every observer body
//! is wrapped in a panic firewall ([`panic::guard`]) and all telemetry is
//! best-effort — it must never break the user's application.
#![cfg_attr(windows, feature(abi_vectorcall))]

mod config;
mod frame;
mod observer;
mod observers;
mod panic;
mod render;
mod request;
mod transport;
mod zend_util;

use ext_php_rs::prelude::*;

/// MINIT (module startup), chained ahead of ext-php-rs's own startup by the
/// `#[php(startup = ...)]` attribute. Registers the INI directive and caches the
/// resolved `state.json` path (PHP_INI_SYSTEM is immutable after startup).
pub fn minit(_ty: i32, mod_num: i32) -> i32 {
    panic::guard(|| {
        config::register_ini(mod_num);
        config::cache_state_path();
    });
    0
}

/// RINIT: arm per-request state (reads `state.json`); cheap no-op when disabled.
extern "C" fn rinit(_ty: i32, _mod_num: i32) -> i32 {
    panic::guard(|| {
        observers::queries::reset();
        request::on_rinit();
    });
    0
}

/// RSHUTDOWN: flush any deferred query frames, emit the request summary, and tear
/// down the request (closes socket).
extern "C" fn rshutdown(_ty: i32, _mod_num: i32) -> i32 {
    panic::guard(|| {
        observers::queries::flush();
        request::on_rshutdown();
    });
    0
}

#[php_module]
#[php(startup = "minit")]
pub fn get_module(module: ModuleBuilder) -> ModuleBuilder {
    module
        .request_startup_function(rinit)
        .request_shutdown_function(rshutdown)
        .fcall_observer(observer::YerdObserver::new)
}
