//! Configuration: the `yerd_dump.state_path` INI directive and `state.json`.
//!
//! Per the Yerd integration contract the extension reads no
//! environment variables. The INI directive is registered in MINIT and supplies
//! the absolute path to a `state.json` written by Yerd; that file carries the
//! on/off switch, the loopback port, and per-feature flags.

use std::sync::OnceLock;

use ext_php_rs::flags::IniEntryPermission;
use ext_php_rs::zend::{ExecutorGlobals, IniEntryDef};
use serde::Deserialize;

/// The INI directive name. Registered by THIS extension in MINIT; Yerd supplies
/// the value via `php-fpm -d yerd_dump.state_path=/abs/path/state.json`.
pub const INI_STATE_PATH: &str = "yerd_dump.state_path";

/// Cached, resolved state-file path. `PHP_INI_SYSTEM` is immutable after
/// startup, so we read it exactly once in MINIT (avoids a per-request HashMap
/// allocation). `None` => directive unset/empty => the extension no-ops.
static STATE_PATH: OnceLock<Option<String>> = OnceLock::new();

/// Register the INI directive. Call from MINIT (the `#[php(startup = ...)]` fn).
pub fn register_ini(module_number: i32) {
    let entries = vec![IniEntryDef::new(
        INI_STATE_PATH.to_owned(),
        String::new(), // default empty => disabled until Yerd points it at a file
        &IniEntryPermission::System,
    )];
    IniEntryDef::register(entries, module_number);
}

/// Read and cache the directive value. Call from MINIT *after* `register_ini`.
pub fn cache_state_path() {
    let value = ExecutorGlobals::get()
        .ini_values()
        .get(INI_STATE_PATH)
        .cloned()
        .flatten()
        .filter(|s| !s.is_empty());
    let _ = STATE_PATH.set(value);
}

/// The cached state-file path, if configured.
#[must_use]
pub fn state_path() -> Option<&'static str> {
    STATE_PATH.get().and_then(|o| o.as_deref())
}

/// Per-feature on/off flags. Absent keys default to off —
/// a malformed/partial `features` object conservatively enables nothing.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(default)]
pub struct Features {
    pub dumps: bool,
    pub queries: bool,
    pub jobs: bool,
    pub views: bool,
    pub requests: bool,
    pub logs: bool,
    pub cache: bool,
    /// Outgoing HTTP client calls (curl/Guzzle). New category — Yerd must add
    /// the `http` DumpCategory + GUI tab and set `features.http` in state.json.
    pub http: bool,
}

/// The `state.json` document Yerd writes.
#[derive(Debug, Clone, Deserialize)]
pub struct State {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub features: Features,
}

impl State {
    /// Load and parse `state.json`. Returns `None` (disabled fast-path) if the
    /// path is unset, the file is missing/unreadable/garbage, `enabled` is
    /// false, or `port` is zero. Never panics, never blocks meaningfully (one
    /// stat + read of an OS-page-cached file).
    #[must_use]
    pub fn load() -> Option<State> {
        let path = state_path()?;
        let bytes = std::fs::read(path).ok()?;
        Self::from_slice(&bytes)
    }

    /// Pure parse + gate (no IO) — the testable core of [`State::load`].
    #[must_use]
    pub fn from_slice(bytes: &[u8]) -> Option<State> {
        let state: State = serde_json::from_slice(bytes).ok()?;
        if !state.enabled || state.port == 0 {
            return None;
        }
        Some(state)
    }
}

#[cfg(test)]
mod tests {
    use super::State;

    #[test]
    fn enabled_state_parses() {
        let s = State::from_slice(
            br#"{"enabled":true,"port":2304,"features":{"dumps":true,"queries":true}}"#,
        )
        .expect("should parse");
        assert_eq!(s.port, 2304);
        assert!(s.features.dumps);
        assert!(s.features.queries);
        assert!(!s.features.jobs); // absent → false
    }

    #[test]
    fn disabled_state_is_none() {
        assert!(State::from_slice(br#"{"enabled":false,"port":2304}"#).is_none());
    }

    #[test]
    fn zero_port_is_none() {
        assert!(State::from_slice(br#"{"enabled":true,"port":0}"#).is_none());
    }

    #[test]
    fn garbage_is_none() {
        assert!(State::from_slice(b"not json {{{").is_none());
        assert!(State::from_slice(b"").is_none());
    }

    #[test]
    fn partial_features_default_off() {
        let s = State::from_slice(br#"{"enabled":true,"port":1,"features":{"logs":true}}"#)
            .expect("should parse");
        assert!(s.features.logs);
        assert!(!s.features.dumps);
        assert!(!s.features.cache);
    }
}
