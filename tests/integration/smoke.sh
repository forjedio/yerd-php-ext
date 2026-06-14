#!/usr/bin/env bash
# Integration smoke test: load the built extension into a real PHP, drive the
# fixture, and assert the expected frames arrive on a loopback sink. Also checks
# the negative paths (server down, disabled state) degrade silently.
#
# Usage:
#   EXT_SO=/path/to/yerd_dump.so PHP=/path/to/php tests/integration/smoke.sh
#
# Env:
#   EXT_SO  path to the built extension (required)
#   PHP     php binary (default: php)
#   PORT    loopback port (default: 2304)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PHP="${PHP:-php}"
PORT="${PORT:-2304}"
EXT_SO="${EXT_SO:?set EXT_SO to the built extension path}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

STATE="$WORK/state.json"
FRAMES="$WORK/frames.log"
FIXTURE="$HERE/fixtures/telemetry.php"

# Opcache on, to exercise the internal-observer path that matters for FPM.
PHP_FLAGS=(-d "extension=$EXT_SO" -d "yerd_dump.state_path=$STATE" -d "opcache.enable_cli=1")

fail() { echo "SMOKE FAIL: $*" >&2; exit 1; }

echo "==> [1/4] extension loads"
$PHP -d "extension=$EXT_SO" -m | grep -qi 'yerd' || fail "extension not listed by -m"

echo "==> [2/4] INI directive registered and -d value visible"
got="$($PHP "${PHP_FLAGS[@]}" -r 'echo ini_get("yerd_dump.state_path");')"
[ "$got" = "$STATE" ] || fail "state_path not visible (got '$got')"

echo "==> [3/4] all categories stream to the sink"
cat > "$STATE" <<JSON
{"enabled":true,"port":$PORT,"features":{"dumps":true,"queries":true,"jobs":true,"views":true,"requests":true,"logs":true,"cache":true}}
JSON
# sink.py args: <port> <out-file> <inactivity-timeout-seconds>
python3 "$HERE/sink.py" "$PORT" "$FRAMES" 6 &
SINK=$!
# Wait until the sink is actually listening (avoids a fixed-sleep race on cold CI).
for _ in $(seq 1 50); do
  if (exec 3<>"/dev/tcp/127.0.0.1/$PORT") 2>/dev/null; then exec 3>&- 3<&-; break; fi
  sleep 0.1
done
$PHP "${PHP_FLAGS[@]}" "$FIXTURE" >/dev/null 2>&1 || fail "fixture run errored"
wait "$SINK" 2>/dev/null || true

python3 - "$FRAMES" <<'PY' || exit 1
import json, sys
cats = {}
for line in open(sys.argv[1]):
    line = line.strip()
    if not line:
        continue
    d = json.loads(line)
    cats[d["category"]] = cats.get(d["category"], 0) + 1
    # Contract: every frame has the required envelope keys.
    for k in ("category", "ts", "site", "request_id", "payload"):
        assert k in d, f"frame missing key {k}: {d}"
print("  captured:", cats)
expected = {"dump", "query", "job", "cache", "log", "view", "request"}
missing = expected - set(cats)
if missing:
    print("SMOKE FAIL: missing categories:", missing, file=sys.stderr)
    sys.exit(1)
# Spot-check a couple of payloads.
PY

echo "==> [4/4] negative paths degrade silently (no sink running)"
# Server down: app must still complete quickly and cleanly.
$PHP "${PHP_FLAGS[@]}" "$FIXTURE" >/dev/null 2>&1 || fail "server-down broke the app"
# Disabled state.
echo '{"enabled":false,"port":'"$PORT"',"features":{}}' > "$STATE"
$PHP "${PHP_FLAGS[@]}" -r 'echo "ok";' >/dev/null 2>&1 || fail "disabled state broke the app"
# Garbage state.
echo 'not-json{{{' > "$STATE"
$PHP "${PHP_FLAGS[@]}" -r 'echo "ok";' >/dev/null 2>&1 || fail "garbage state broke the app"

echo "SMOKE PASS"
