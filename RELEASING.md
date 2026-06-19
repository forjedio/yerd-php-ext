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

# Releasing `pcov` (separate, isolated pipeline)

This repo also publishes the upstream [`krakjoe/pcov`](https://github.com/krakjoe/pcov)
code-coverage driver, built per (PHP minor × cell) the same way as `yerd-dump`, so Yerd
can download and load it identically. **pcov is upstream C, not Rust** — it is built with
the standard PHP extension toolchain (`phpize && ./configure --enable-pcov && make`) and is
fully additive: it does not touch the Rust crate, `Cargo.toml`, `src/`, or `release.yml`.

### Why it is a *separate* release

The Yerd consumer (`bin/yerdd/src/ext_install.rs`) matches manifest entries on
`(php, os, arch)` **only — there is no extension-name field.** If pcov files were listed in
`yerd-dump`'s `manifest.json`, Yerd could fetch a pcov `.so` when it wanted `yerd-dump`. So
pcov is completely isolated:

| | `yerd-dump` | `pcov` |
|---|---|---|
| Workflow | `release.yml` | `release-pcov.yml` |
| Tag trigger | `v*` | `pcov-v*` |
| Manifest | `manifest.json` | `pcov-manifest.json` |
| Checksums | `SHA256SUMS` | `SHA256SUMS-pcov` |
| Artifact name | `yerd-dump-<minor>-<os>-<arch>.so` | `pcov-<minor>-<os>-<arch>.so` |

The `pcov-manifest.json` uses the **identical schema** to `manifest.json` (same `version` /
`php_minors` / `files[{name,php,os,arch,sha256,size}]` shape). Yerd reads it from its own
release asset. `release.yml` and `manifest.json` are never modified.

### Pinned pcov version

pcov is pinned to **`v1.0.12`** (latest upstream tag, 2024-12-04) via the `PCOV_VERSION`
env in `release-pcov.yml`, fetched as a tagged tarball in CI (no submodule). Its source has
no upper-bound `PHP_VERSION_ID` gate, so it compiles across **8.2, 8.3, 8.4, 8.5** (8.5
takes the `>= 80400` branch; PECL/windows.php.net also ship 1.0.12 for 8.5). Every leg runs
a load + `pcov\start`/`pcov\collect` namespace smoke test, so an unsupported minor fails its
build and the count guard blocks the release. To adopt a newer pcov, bump `PCOV_VERSION` (in
both `release-pcov.yml` and `ci.yml`) and the tag.

### Cut a pcov release

```bash
git tag pcov-v1.0.12
git push origin pcov-v1.0.12
```

This runs the matrix (4 minors × 3 cells = `12` artifacts) and creates a GitHub Release
containing `pcov-<minor>-<os>-<arch>.so` for every cell, `SHA256SUMS-pcov`,
`pcov-manifest.json`, and build-provenance attestations. The publish job fails if any cell
is missing. The release version string is the tag (e.g. `pcov-v1.0.12`); re-tag with a
trailing `-N` (e.g. `pcov-v1.0.12-2`) to re-publish the same pcov upstream version.
```
