//! Per-category observer logic. Each submodule turns an observed call into a
//! frame; all are entered only through [`crate::observer::YerdObserver`], which
//! has already applied the panic guard and the per-request feature gate.

pub mod dumps;
pub mod events;
pub mod http;
pub mod queries;
