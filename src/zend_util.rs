//! Thin, panic-safe glue over the Zend C ABI.
//!
//! `ext-php-rs` does not pass an `FcallInfo` to the observer `begin`/`end`
//! callbacks (only to `should_observe`), and it does not expose the calling
//! frame's current line. This module reconstructs the small amount of raw
//! information we need directly from `ExecuteData`, mirroring how
//! `ext-php-rs`'s own `observer.rs` reads the engine structures.
//!
//! Every function here is `unsafe`-internally but exposes a safe surface; all
//! pointer dereferences are null-checked.

use ext_php_rs::ffi;
use ext_php_rs::zend::ExecuteData;

/// Copy a `zend_string` into an owned `String` (lossy UTF-8), or `None`.
unsafe fn zstr_to_string(zs: *mut ffi::zend_string) -> Option<String> {
    if zs.is_null() {
        return None;
    }
    let len = (*zs).len;
    let ptr = (*zs).val.as_ptr().cast::<u8>();
    if ptr.is_null() {
        return None;
    }
    let slice = std::slice::from_raw_parts(ptr, len);
    Some(String::from_utf8_lossy(slice).into_owned())
}

/// The `(class_name, function_name)` of the frame `ex` is currently executing.
///
/// `class_name` is `None` for plain functions. Used in `begin`/`end` to classify
/// which observed symbol fired (the same identity `should_observe` matched on).
#[must_use]
pub fn fn_identity(ex: &ExecuteData) -> (Option<String>, Option<String>) {
    unsafe {
        let func = ex.func;
        if func.is_null() {
            return (None, None);
        }
        let common = &(*func).common;
        let function_name = zstr_to_string(common.function_name);
        let class_name = if common.scope.is_null() {
            None
        } else {
            zstr_to_string((*common.scope).name)
        };
        (class_name, function_name)
    }
}

/// Number of arguments passed to the currently-executing call.
#[must_use]
pub fn num_args(ex: &ExecuteData) -> u32 {
    // SAFETY: all members of the `u2` union are the same width; `num_args` is the
    // valid interpretation inside a call frame (mirrors `parser_object`).
    unsafe { ex.This.u2.num_args }
}

/// Borrow argument `n` (0-indexed) of the current call, if present.
///
/// Returns a shared reference; we only ever read arguments.
#[must_use]
pub fn arg(ex: &ExecuteData, n: u32) -> Option<&ext_php_rs::types::Zval> {
    if n >= num_args(ex) {
        return None;
    }
    // SAFETY: `n` is bounds-checked above; the returned reference cannot outlive
    // `ex` (tied to its lifetime).
    unsafe { ex.zend_call_arg(n as usize).map(|z| &*z) }
}

/// Whether a source path is dependency code (Composer `vendor/`). We skip these
/// when resolving the call site so we report the user's app frame
/// (e.g. `app/.../Foo.php:36`) rather than the framework internals that wrap the
/// observed call — the dump/query/cache/log/HTTP helper lives in `vendor/`.
fn is_internal_path(path: &str) -> bool {
    path.contains("/vendor/")
}

/// Resolve the originating application `file:line` for the observed call.
///
/// Walks `prev_execute_data` outward and returns the first **userland**
/// (non-internal) frame whose file is not a framework-internal path. If every
/// userland frame is framework-internal, falls back to the nearest userland
/// frame. Best-effort: returns `(None, 0)` if nothing suitable is found.
#[must_use]
pub fn caller_location(ex: &ExecuteData) -> (Option<String>, u32) {
    let mut fallback: Option<(String, u32)> = None;
    unsafe {
        let mut cur = ex.prev_execute_data;
        while !cur.is_null() {
            let func = (*cur).func;
            if !func.is_null() {
                let common = &(*func).common;
                #[allow(clippy::cast_possible_truncation)]
                let is_internal = common.type_ == ffi::ZEND_INTERNAL_FUNCTION as u8;
                if !is_internal {
                    let op_array = &(*func).op_array;
                    if let Some(file) = zstr_to_string(op_array.filename) {
                        let line = if (*cur).opline.is_null() {
                            op_array.line_start
                        } else {
                            (*(*cur).opline).lineno
                        };
                        if !is_internal_path(&file) {
                            return (Some(file), line);
                        }
                        fallback.get_or_insert((file, line));
                    }
                }
            }
            cur = (*cur).prev_execute_data;
        }
    }
    match fallback {
        Some((f, l)) => (Some(f), l),
        None => (None, 0),
    }
}
