//! Panic firewall.
//!
//! Observer bodies run inside every PHP request. A Rust panic unwinding across
//! the C ABI boundary is undefined behaviour and will crash the FPM worker.
//! Every observer entry point funnels through [`guard`], which catches and
//! swallows panics so telemetry can never break the user's application.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Once;

/// Install a no-op panic hook once, so a caught panic does not spew a backtrace
/// to the worker's stderr (which would end up in the user's FPM logs).
fn silence_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        std::panic::set_hook(Box::new(|_info| {
            // Intentionally silent: the panic is caught by `guard` and the
            // request continues. We do not want to pollute FPM logs.
        }));
    });
}

/// Run `f`, catching and discarding any panic.
///
/// This is the only sanctioned way to enter Rust logic from a C callback.
pub fn guard<F: FnOnce()>(f: F) {
    silence_panics();
    // `AssertUnwindSafe` is justified: we discard all state on panic and never
    // observe partially-mutated values afterwards (thread-local request state is
    // rebuilt next request; a poisoned frame is simply dropped).
    let _ = catch_unwind(AssertUnwindSafe(f));
}

#[cfg(test)]
mod tests {
    use super::guard;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn guard_swallows_panic_and_continues() {
        static REACHED: AtomicBool = AtomicBool::new(false);
        guard(|| panic!("boom inside observer"));
        // If the panic escaped, the process would have aborted before this line.
        guard(|| REACHED.store(true, Ordering::SeqCst));
        assert!(REACHED.load(Ordering::SeqCst));
    }
}
