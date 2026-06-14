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
| `query`  | `PDO::exec`, `PDO::query`, `PDOStatement::execute` (framework-agnostic) |
| `job` / `view` / `cache` / `log` | `Illuminate\Events\Dispatcher::dispatch` (event class → category) |
| `request`| assembled from SAPI globals at RSHUTDOWN |

Every observer body runs behind a panic firewall; all telemetry is best-effort and must
never break the user's application.

## Loading contract (IMPORTANT — coordinate with Yerd)

`ext-php-rs` produces a **regular PHP extension** (the modern `zend_observer` API works
from a normal extension's MINIT). It is therefore loaded with **`-d extension=<path>`**,
not `-d zend_extension=<path>`. Yerd must wire `extension=`.

The rest of the contract is unchanged: NDJSON loopback transport, the frame schema, the
`yerd_dump.state_path` INI directive, `state.json`, and the
`yerd-dump-<minor>-<os>-<arch>.so` artifact naming.

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
  observers/    per-category logic: dumps, queries, events
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
