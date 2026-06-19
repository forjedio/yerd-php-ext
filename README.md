# yerd-php-ext (`yerd-dump`)

A [Forjed](https://forjed.io) Project

A native PHP extension, written in Rust with [`ext-php-rs`](https://ext-php.rs), that
captures Laravel/PHP telemetry (dumps, queries, jobs, views, requests, logs, cache) and
streams it as newline-delimited JSON to **Yerd's** loopback dump server for display in
Yerd's GUI "Dumps" window. It is the open-source equivalent of Laravel Herd's proprietary
dump extension.

It is **consumed by Yerd**, not installed by end users: the Yerd daemon downloads the
matching artifact per installed PHP version and loads it into PHP-FPM. There is no
`composer`/`pecl` step.

See [`RELEASING.md`](RELEASING.md) for the release process.

## Also published here: `pcov`

For convenience this repo *additionally* builds and publishes the upstream
[`krakjoe/pcov`](https://github.com/krakjoe/pcov) code-coverage driver (pinned to
`v1.0.12`, PHP 8.2–8.5) so Yerd can download and load it the same way it loads `yerd-dump`.
pcov is **upstream C, not Rust** — it is fully isolated from the Rust crate: its own
workflow (`release-pcov.yml`), its own `pcov-v*` tags, and its own `pcov-manifest.json` /
`SHA256SUMS-pcov` (kept separate from `yerd-dump`'s `manifest.json` so the two never
collide on the consumer side). See [`RELEASING.md`](RELEASING.md#releasing-pcov-separate-isolated-pipeline).

## How it works

The extension registers a single `zend_observer` fcall observer (modern, cached per
function definition) plus RINIT/RSHUTDOWN hooks. It observes:

| Category | Observed symbol |
|----------|-----------------|
| `dump`   | `Symfony\Component\VarDumper\VarDumper::dump` (the chokepoint `dump()`/`dd()` funnel through) |
| `query`  | `PDO::{exec,query}`, `PDOStatement::{execute,bindValue,bindParam,fetchAll}` (framework-agnostic); emits `sql` + `bindings` + `sql_full` (interpolated) + `row_count` |
| `job` / `view` / `cache` / `log` | `Illuminate\Events\Dispatcher::dispatch` (event class → category) |
| `request`| assembled at RSHUTDOWN: `uri`/`method`/`ip` from `$_SERVER` (Yerd's proxy sets the real `REQUEST_URI` incl. query string), `status` from SAPI globals |
| `http`   | `curl_exec` (also covers Guzzle / PSR-18 clients that use the curl handler); reads `curl_getinfo` for url/status/time |

Every observer body runs behind a panic firewall; all telemetry is best-effort and must
never break the user's application.

## Loading contract (IMPORTANT — coordinate with Yerd)

`ext-php-rs` produces a **regular PHP extension** (the modern `zend_observer` API works
from a normal extension's MINIT). It is therefore loaded with **`-d extension=<path>`**,
not `-d zend_extension=<path>`. Yerd must wire `extension=`.

The rest of the contract is unchanged: NDJSON loopback transport, the frame schema, the
`yerd_dump.state_path` INI directive, `state.json`, and the
`yerd-dump-<minor>-<os>-<arch>.so` artifact naming.

### New `http` category (needs the Yerd side too)

Outgoing HTTP telemetry is a **new category** and a contract change Yerd must mirror:

- **`state.json`**: add `features.http` (bool). When absent/false the extension does not
  observe `curl_exec` — fully backward compatible.
- **Dump server / GUI**: add an `http` `DumpCategory` (+ counts + an "HTTP" tab).
- **Frame payload** (`category: "http"`): `{ method, url, status (int), duration_ms (float) }`
  — `url` is the effective URL incl. query string; `method` is best-effort (`effective_method`,
  empty on older curl).

Until Yerd ships these, set `features.http=false` (or omit it) and behaviour is unchanged.

### `query` payload gained `sql_full` and `row_count`

The `query` payload now includes (all additive / backward-compatible):
- **`sql_full`** — the statement with bound values interpolated for display
  (e.g. `… WHERE "id" = 7 AND name = 'Ada'`), alongside `sql` (parameterized) + `bindings`.
  Display-only (never executed): strings quoted/escaped, `NULL`/bool handled, and `?`/`:name`
  inside string literals left untouched. Yerd's GUI should show `sql_full` for the runnable query.
- **`row_count`** (int or `null`) — writes use the affected-row count (`PDO::exec` return
  value / `rowCount()`); **reads** are deferred and counted from the rows returned by
  `fetchAll` (so SELECTs report the real count, not the driver-dependent `rowCount()`).
  Reads fetched row-by-row or never fetched fall back to `rowCount()` / `null`.

**Laravel parameter binding:** Eloquent binds via `PDOStatement::bindValue()` and calls
`execute()` with no args, so the extension accumulates `bindValue`/`bindParam` per
statement (correlated by object handle) to populate `bindings` and `sql_full`. Deferred
read frames keep their execute-time `ts`, so ordering is preserved even though they're
emitted at `fetchAll` (or flushed at request end).

### `file` / `line` added to more categories

Call-site resolution now skips all `vendor/` frames and reports the user's **app** frame.
`file`/`line` are present on: **`dump`**, **`query`**, **`http`**, **`log`**, **`cache`**,
**`view`**, and **`job`**. Caveats: `job` events fire inside the queue worker during
execution, so their `file:line` is usually empty / not the dispatch site; `view`/`cache`
point at the app call that triggered them. `request` has no single call site (omitted).

## Build & test (local, macOS/Linux)

Requires Rust (stable), a PHP with dev headers + `php-config` on `PATH`, and `libclang`
(for bindgen). On macOS the simplest source of headers is Homebrew (`brew install php`).

```bash
# Build
cargo build                  # debug → target/debug/libyerd_dump.{so,dylib}
cargo build --release        # release (stripped, LTO)

# Lint + unit tests
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all

# End-to-end smoke (loads the .so into PHP, asserts frames on a sink)
cp target/debug/libyerd_dump.{so,dylib} /tmp/yerd_dump.so 2>/dev/null
EXT_SO=/tmp/yerd_dump.so PHP=$(which php) bash tests/integration/smoke.sh
```

### Manual one-shot

```bash
echo '{"enabled":true,"port":2304,"features":{"dumps":true,"queries":true}}' > /tmp/state.json
nc -l 2304 &   # or any loopback listener
php -d extension=$PWD/target/debug/libyerd_dump.so \
    -d yerd_dump.state_path=/tmp/state.json \
    tests/integration/fixtures/telemetry.php
```

## Repository layout

```
src/
  lib.rs        MINIT/RINIT/RSHUTDOWN + #[php_module]
  config.rs     INI directive + state.json
  observer.rs   the single fcall observer (symbol classification + dispatch)
  observers/    per-category logic: dumps, queries, events, http
  frame.rs      frame schema + serialized-line truncation
  transport.rs  non-blocking loopback TCP, connect-once-per-request
  request.rs    per-request state (thread-local) + emit
  render.rs     bounded, panic-safe Zval → text/HTML
  caller.rs     → see zend_util.rs (caller resolution)
  zend_util.rs  thin raw-Zend glue (arg/identity/caller)
  panic.rs      catch_unwind firewall
tests/integration/  TCP sink + fixture + smoke.sh
.github/workflows/  ci.yml, release.yml
```

## License

[MIT](LICENSE)
