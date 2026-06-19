# Releasing

Artifacts are built and published by `.github/workflows/release.yml` on a `v*` tag.

## Cut a release

```bash
git tag v0.1.0
git push origin v0.1.0
```

This runs the matrix (PHP minor × cell), verifies each artifact, and creates a GitHub
Release containing:

- `yerd-dump-<minor>-<os>-<arch>.so` for every cell,
- `SHA256SUMS`,
- `manifest.json` (the download contract Yerd verifies),
- build-provenance attestations.

## Build matrix

PHP minors: **8.2, 8.3, 8.4, 8.5**.

> Floor is 8.2: `ext-php-rs` 0.15.15 requires PHP ≥ 8.1, and `zend_observer` only observes
> internal functions (PDO queries) from 8.2+. PHP 8.0/8.1 are also EOL.

| cell | runner |
|------|--------|
| macOS arm64 | `macos-14` |
| linux x86_64 | `ubuntu-22.04` |
| linux aarch64 | `ubuntu-22.04-arm` |

`12` artifacts total (4 minors × 3 cells). Yerd is Apple-Silicon-only, so there is no
Intel macOS cell. The publish job fails if any cell is missing.

## `manifest.json` schema (source of truth — keep in sync with Yerd)

```jsonc
{
  "version": "v0.1.0",
  "php_minors": ["8.2", "8.3", "8.4", "8.5"],
  "files": [
    { "name": "yerd-dump-8.3-linux-x86_64.so",
      "php": "8.3", "os": "linux", "arch": "x86_64",
      "sha256": "…", "size": 123456 }
  ]
}
```

## ABI / build-id

A PHP extension's compatibility is fixed by `ZEND_MODULE_API_NO` (stable within a minor) +
NTS + non-debug. The build-id guard in CI asserts the build PHP is **NTS** and
**non-debug**; that, plus building per minor, is what makes each artifact load into Yerd's
PHP of the same minor.

The same-runner build-id is *not* self-compared (that is tautological — `ext-php-rs`
derives ZTS/debug from the PHP it built against). For a stronger guarantee, set the repo/CI
variable `EXPECTED_PHP_EXTENSION_BUILD` to the exact `PHP Extension Build` string that
Yerd's static-php.dev PHP reports for that minor; the guard then asserts equality.

## Known follow-ups (coordinate with Yerd)

- **glibc floor.** Linux cells currently build on `ubuntu-22.04` (glibc 2.35). If Yerd
  supports end-user hosts older than glibc 2.35, switch the Linux cells to build inside an
  old-glibc container (e.g. `debian:bullseye`, glibc 2.31) and validate that
  `shivammathur/setup-php` works in-container or install PHP headers directly there.
- **Adding a PHP minor.** Add it to the `php` matrix list in `release.yml` (the `minors`
  count check derives from it) and re-tag. Each minor needs its own build
  (`ZEND_MODULE_API_NO` is per-minor).

---

# `pcov` (rides the same `v*` release)

This repo *also* publishes the upstream [`krakjoe/pcov`](https://github.com/krakjoe/pcov)
code-coverage driver, so Yerd can download and load it the same way it loads `yerd-dump`.
**pcov is upstream C, not Rust** — the `build-pcov` job in `release.yml` builds it per
(PHP minor × cell) with the standard PHP extension toolchain (`phpize && ./configure
--enable-pcov && make`); no Rust, bindgen, or libclang.

pcov is built and attached to the **same `v*` release** as `yerd-dump` (not a separate
`pcov-v*` tag). This is deliberate:

- Yerd's `yerd-dump` consumer fetches `…/releases/latest/download/manifest.json`. A
  separate pcov release would become the repo's **"latest"** and that URL would 404
  (a pcov release has no `manifest.json`). One release stream keeps "latest" correct.
- The `build-pcov` job is **required** — `publish` needs it, so if pcov fails to build the
  entire release is blocked (all-or-nothing; every release carries both extensions).

### Two manifests, never merged

The Yerd consumer (`bin/yerdd/src/ext_install.rs`) matches manifest entries on
`(php, os, arch)` **only — there is no extension-name field.** So pcov gets its **own**
manifest; its files are never listed in `yerd-dump`'s `manifest.json`:

| | `yerd-dump` | `pcov` |
|---|---|---|
| Build job | `build` (Rust) | `build-pcov` (upstream C) |
| Manifest | `manifest.json` | `pcov-manifest.json` |
| Checksums | `SHA256SUMS` | `SHA256SUMS-pcov` |
| Artifact name | `yerd-dump-<minor>-<os>-<arch>.so` | `pcov-<minor>-<os>-<arch>.so` |

Both manifests share the **identical schema** (`version` / `php_minors` /
`files[{name,php,os,arch,sha256,size}]`) and are attached to the same release. Yerd's
`yerd-dump` downloader reads `manifest.json`; Yerd's pcov downloader reads
`pcov-manifest.json` from `…/releases/latest/download/`. `manifest.json` content is
unchanged.

### Pinned pcov version

pcov is pinned to **`v1.0.12`** (latest upstream tag, 2024-12-04) via the `PCOV_VERSION`
env in `release.yml`, fetched as a tagged tarball in CI (no submodule). Its source has no
upper-bound `PHP_VERSION_ID` gate, so it compiles across **8.2, 8.3, 8.4, 8.5** (8.5 takes
the `>= 80400` branch; PECL/windows.php.net also ship 1.0.12 for 8.5). Every leg runs a
load + `pcov\start`/`pcov\collect` namespace smoke test, so an unsupported minor fails its
build and the count guard blocks the release. To adopt a newer pcov, bump `PCOV_VERSION` in
both `release.yml` and `ci.yml`. (The `pcov-manifest.json` `version` field is the `v*` tag,
not the pcov upstream version — yerd matches on file `php`/`os`/`arch`, not `version`.)

> **macOS build note.** On the macOS cell pcov needs the Homebrew `pcre2` keg's headers on
> `CPPFLAGS` (PHP's `php_pcre.h` `#include "pcre2.h"`); the job installs `pcre2` and sets the
> flag. Linux ships those headers inline.

### Cutting a release (both extensions)

Nothing changes from the `yerd-dump` flow above — a single `v*` tag now produces both. The
release contains, in addition to the `yerd-dump` assets: `pcov-<minor>-<os>-<arch>.so` for
every cell, `SHA256SUMS-pcov`, and `pcov-manifest.json`. `12` pcov artifacts (4 × 3); the
publish job fails if any cell is missing.
