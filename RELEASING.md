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

| PHP minor | cell | runner |
|-----------|------|--------|
| 8.3 / 8.4 / 8.5 | macOS arm64 | `macos-14` |
| 8.3 / 8.4 / 8.5 | macOS x86_64 | `macos-15-intel` |
| 8.3 / 8.4 / 8.5 | linux x86_64 | `ubuntu-22.04` |
| 8.3 / 8.4 / 8.5 | linux aarch64 | `ubuntu-22.04-arm` |

`12` artifacts total (3 minors × 4 cells). The publish job fails if any cell is missing.

## `manifest.json` schema (source of truth — keep in sync with Yerd)

```jsonc
{
  "version": "v0.1.0",
  "php_minors": ["8.3", "8.4", "8.5"],
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
- **macOS x86_64 sunset.** `macos-15-intel` is the last hosted Intel image (~until Aug
  2027). After that, the macOS x86_64 cell needs a self-hosted Intel runner or
  cross-compilation.
- **Adding a PHP minor.** Add it to the `php` matrix list in `release.yml` (and the count
  check) and re-tag. Each minor needs its own build (`ZEND_MODULE_API_NO` is per-minor).
```
