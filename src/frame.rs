//! Frame construction and the serialized-line truncation budget.
//!
//! Wire format: one compact UTF-8 JSON object per line,
//! terminated by `\n`. Yerd drops any line longer than the cap, so truncation is
//! measured on the **encoded line** (post-JSON-escape), not on the raw fields —
//! escaping `value_html`/bindings expands the byte length.

use serde::Serialize;
use serde_json::Value;
use std::time::{SystemTime, UNIX_EPOCH};

/// Maximum serialized line length in bytes (incl. the trailing `\n`). Frames
/// larger than this are replaced by a compact "truncated" marker so a line is
/// never silently dropped by Yerd.
pub const MAX_LINE: usize = 256 * 1024;

/// Per-string-field soft cap applied by observers before building a frame, so
/// the common case never approaches [`MAX_LINE`]. Leaves headroom for escape
/// expansion and the surrounding JSON.
pub const FIELD_CAP: usize = 200 * 1024;

/// Truncate a string to at most `cap` bytes on a UTF-8 boundary, appending an
/// ellipsis marker when truncation occurs.
#[must_use]
pub fn truncate(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_owned();
    }
    let mut end = cap;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_owned();
    out.push_str("…[truncated]");
    out
}

/// Current wall-clock time as epoch milliseconds. Saturates to 0 if the clock is
/// before the epoch (never panics).
#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[derive(Serialize)]
struct Frame<'a> {
    category: &'a str,
    ts: u64,
    site: &'a str,
    request_id: &'a str,
    payload: &'a Value,
}

/// Build the newline-terminated JSON line for one frame.
///
/// If the encoded line exceeds [`MAX_LINE`], the payload is replaced with a
/// minimal marker so Yerd still receives a valid, in-budget line.
#[must_use]
pub fn build_line(category: &str, ts: u64, site: &str, request_id: &str, payload: Value) -> String {
    let frame = Frame {
        category,
        ts,
        site,
        request_id,
        payload: &payload,
    };
    let mut line = serde_json::to_string(&frame).unwrap_or_default();

    if line.len() + 1 > MAX_LINE {
        let marker = serde_json::json!({ "truncated": true });
        let fallback = Frame {
            category,
            ts,
            site,
            request_id,
            payload: &marker,
        };
        line = serde_json::to_string(&fallback).unwrap_or_default();
    }

    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("hello", 100), "hello");
    }

    #[test]
    fn truncate_long_string_marks() {
        let out = truncate(&"a".repeat(50), 10);
        assert!(out.starts_with(&"a".repeat(10)));
        assert!(out.ends_with("[truncated]"));
    }

    #[test]
    fn truncate_respects_utf8_boundary() {
        // "é" is two bytes; cap of 1 must back off to 0 rather than split it.
        let out = truncate("é", 1);
        assert!(out.ends_with("[truncated]"));
        assert!(out.starts_with("…[truncated]") || !out.starts_with('é'));
    }

    #[test]
    fn build_line_is_single_newline_terminated_json() {
        let line = build_line(
            "dump",
            123,
            "blog.test",
            "abc",
            serde_json::json!({"value_text": "hi"}),
        );
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["category"], "dump");
        assert_eq!(parsed["payload"]["value_text"], "hi");
    }

    #[test]
    fn build_line_oversized_payload_falls_back_to_marker() {
        // A payload whose ESCAPED form blows past MAX_LINE (quotes everywhere).
        let huge = "\"".repeat(MAX_LINE);
        let line = build_line(
            "dump",
            1,
            "s",
            "r",
            serde_json::json!({ "value_html": huge }),
        );
        assert!(line.len() <= MAX_LINE);
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(parsed["payload"]["truncated"], true);
    }
}
