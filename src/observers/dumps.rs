//! `dump()` / `dd()` observation.
//!
//! Emitted in **`begin`**: `dd()` terminates via `zend_bailout`, and the engine
//! does NOT invoke `fcall_end` on a bailout, so an end-only strategy would drop
//! the most common dump. The dumped value is fully available from the arguments
//! at `begin`. We never touch the return value or suppress output, so the user's
//! visible dump is preserved.

use crate::frame::{truncate, FIELD_CAP};
use crate::render::render;
use crate::request::{self, Feature};
use crate::zend_util::{arg, caller_location, num_args};
use ext_php_rs::zend::ExecuteData;

/// Handle a `dump`/`dd`/`ddd`/`dumpe` call: one frame per argument.
pub fn on_dump(ex: &ExecuteData) {
    let (file, line) = caller_location(ex);
    let n = num_args(ex);
    for i in 0..n {
        let Some(z) = arg(ex, i) else { continue };
        let (html, text) = render(z);
        let html = truncate(&html, FIELD_CAP);
        let text = truncate(&text, FIELD_CAP);
        let file = file.clone();
        request::emit(Feature::Dumps, "dump", move || {
            serde_json::json!({
                "value_html": html,
                "value_text": text,
                "file": file,
                "line": line,
            })
        });
    }
}
