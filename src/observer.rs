//! The single fcall observer. `should_observe` filters by **symbol identity
//! only** (cached per function definition by the engine), and the per-request
//! `enabled`/feature gate is applied later in `begin`/`end` — so toggling a
//! feature via `state.json` takes effect without an FPM restart.

use crate::observers::{dumps, events, queries};
use crate::panic::guard;
use crate::request::{self, Feature};
use crate::zend_util::fn_identity;
use ext_php_rs::prelude::*;
use ext_php_rs::types::Zval;
use ext_php_rs::zend::ExecuteData;

/// The observed symbols.
#[derive(Clone, Copy)]
enum Sym {
    Dump,
    Query(queries::QueryKind),
    Dispatch,
}

/// Classify a `(class, function)` pair into an observed symbol, by identity only.
///
/// Dumps are observed at `VarDumper::dump` rather than the global `dump()`/`dd()`
/// helpers: it is the single chokepoint both helpers funnel through, and it takes
/// the value as a **positional** `mixed $var` argument. The global helpers are
/// variadic, and PHP relocates variadic args (`zend_copy_extra_args`) before the
/// observer's `begin` fires, leaving the original slots `UNDEF` — so observing
/// them directly cannot read the dumped value. `dd()` funnels through
/// `VarDumper::dump` too, so it is still captured (in `begin`, before its `exit`).
fn classify(class: Option<&str>, func: Option<&str>) -> Option<Sym> {
    match (class, func?) {
        (Some("Symfony\\Component\\VarDumper\\VarDumper"), "dump") => Some(Sym::Dump),
        (Some("PDO"), "exec" | "query") => Some(Sym::Query(queries::QueryKind::PdoSqlArg)),
        (Some("PDOStatement"), "execute") => Some(Sym::Query(queries::QueryKind::StmtExecute)),
        (Some("Illuminate\\Events\\Dispatcher"), "dispatch") => Some(Sym::Dispatch),
        _ => None,
    }
}

/// Stateless singleton; all per-request state lives in thread-locals.
#[derive(Default)]
pub struct YerdObserver;

impl YerdObserver {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl FcallObserver for YerdObserver {
    fn should_observe(&self, info: &FcallInfo) -> bool {
        // Identity only — result is cached by PHP for the process lifetime.
        classify(info.class_name, info.function_name).is_some()
    }

    fn begin(&self, ex: &ExecuteData) {
        guard(|| {
            let (class, func) = fn_identity(ex);
            let Some(sym) = classify(class.as_deref(), func.as_deref()) else {
                return;
            };
            match sym {
                Sym::Dump => {
                    if request::active(Feature::Dumps) {
                        dumps::on_dump(ex);
                    }
                }
                Sym::Query(_) => {
                    if request::active(Feature::Queries) {
                        queries::on_begin();
                    }
                }
                Sym::Dispatch => {
                    // Cheap gate: any of the dispatcher-backed categories on.
                    if request::active(Feature::Jobs)
                        || request::active(Feature::Cache)
                        || request::active(Feature::Logs)
                        || request::active(Feature::Views)
                    {
                        events::on_dispatch(ex);
                    }
                }
            }
        });
    }

    fn end(&self, ex: &ExecuteData, _retval: Option<&Zval>) {
        guard(|| {
            let (class, func) = fn_identity(ex);
            if let Some(Sym::Query(kind)) = classify(class.as_deref(), func.as_deref()) {
                if request::active(Feature::Queries) {
                    queries::on_end(ex, kind);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_symbols() {
        assert!(matches!(
            classify(
                Some("Symfony\\Component\\VarDumper\\VarDumper"),
                Some("dump")
            ),
            Some(Sym::Dump)
        ));
        assert!(matches!(
            classify(Some("PDO"), Some("query")),
            Some(Sym::Query(_))
        ));
        assert!(matches!(
            classify(Some("PDOStatement"), Some("execute")),
            Some(Sym::Query(_))
        ));
        assert!(matches!(
            classify(Some("Illuminate\\Events\\Dispatcher"), Some("dispatch")),
            Some(Sym::Dispatch)
        ));
    }

    #[test]
    fn ignores_unrelated_symbols() {
        assert!(classify(None, Some("array_map")).is_none());
        assert!(classify(Some("PDO"), Some("beginTransaction")).is_none());
        assert!(classify(None, None).is_none());
    }
}
