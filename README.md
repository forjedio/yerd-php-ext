# yerd-php-ext (`yerd-dump`)

A native PHP extension, written in Rust with [`ext-php-rs`](https://ext-php.rs), that
captures Laravel/PHP telemetry (dumps, queries, jobs, views, requests, logs, cache) and
streams it as newline-delimited JSON to **Yerd's** loopback dump server for display in
Yerd's GUI "Dumps" window. It is the open-source equivalent of Laravel Herd's proprietary
dump extension.

It is **consumed by Yerd**, not installed by end users: the Yerd daemon downloads the
matching artifact per installed PHP version and loads it into PHP-FPM. There is no
`composer`/`pecl` step.

See [`RELEASING.md`](RELEASING.md) for the release process.

## How it works

The extension registers a single `zend_observer` fcall observer (modern, cached per
function definition) plus RINIT/RSHUTDOWN hooks. It observes:

| Category | Observed symbol |
|----------|-----------------|
| `dump`   | `Symfony\Component\VarDumper\VarDumper::dump` (the chokepoint `dump()`/`dd()` funnel through) |
| `query`  | `PDO::exec`, `PDO::query`, `PDOStatement::execute` (framework-agnostic); emits `sql` + `bindings` + `sql_full` (interpolated) + `row_count` |
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
- **`row_count`** (int or `null`) — affected/returned rows. Exact for writes
  (`PDO::exec` return value; `rowCount()` for INSERT/UPDATE/DELETE). For SELECTs it is
  driver-dependent (accurate on buffered MySQL, often `0` on SQLite/Postgres, since rows
  returned aren't known until fetch); `null` when unavailable.

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
