# Changelog

All notable changes to MediaKit are documented here. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/); versions match the
tags on the [Releases](../../releases) page.

## [0.1.1] - 2026-08-04

### Fixed
- CI's smoke test invoked a `--preset` that no longer exists (`discord-10mb`);
  it now uses the real CLI surface (`--preset target-size --size-preset
  discord-free`), matching the README and a new test that keeps the two from
  drifting apart again (`gui/src/cli.rs`).
- The bundled Linux ffmpeg was pinned to a johnvansickle.com URL that looks
  version-pinned but isn't - it's a rolling "latest" URL that johnvansickle
  overwrites in place on every upstream release, so the checksum recorded in
  `core/vendor.toml` eventually drifted out from under it and every bundled
  build started failing with a checksum mismatch. Switched to BtbN's
  `linux64-gpl` autobuild (the same immutable, per-tag release family already
  used for Windows), pinned to the same ffmpeg 8.1.2 build on both platforms.
- `core/build.rs` now caches downloaded vendor binaries under
  `$CARGO_HOME/mediakit-vendor/` (keyed on name + version + checksum) instead
  of a per-`OUT_DIR` cache, so a single `cargo test --all-targets` no longer
  re-downloads the same ~100MB archive once per target, and CI caches that
  directory keyed on `core/vendor.toml`'s own hash.
- Added a `cargo metadata --locked` job as the first CI step, so a dependency
  edit that wasn't followed by a committed `Cargo.lock` update fails in
  seconds with a clear message instead of ~20 minutes into the full test
  matrix.
- Process-tree adoption failures (e.g. `AssignProcessToJobObject` denied by a
  restrictive parent job on some CI runners) now log loudly (`tracing::error!`)
  instead of a quiet warning, and can be forced on demand via
  `MEDIAKIT_DISABLE_JOB_OBJECTS=1` for testing the no-op fallback path
  deliberately. A new test confirms cancellation still completes promptly
  (via a direct `Child::kill()`) rather than hanging when this fallback is
  active.
- Documented the narrow, unaddressed race where a grandchild process spawned
  between `spawn()` and `adopt()` can escape tree-kill (`core/src/procgroup.rs`).
- Fixed the placeholder `repository` URL in `Cargo.toml`.

### Added
- `workflow_dispatch` trigger for the release job, with a `version` input, so
  the full build → package → checksum → publish → verify pipeline can be run
  and debugged on demand (published as a draft release) without pushing a
  tag for every attempt.
- SHA-256 checksums generated for every release artifact and uploaded
  alongside it.
- A `verify-release` CI job that downloads the just-published artifacts and
  runs `--version` on each, so a broken release fails the workflow instead of
  sitting on the Releases page.
- A Windows test (`core/src/procgroup.rs`) that reproduces the real spawn
  call order and confirms `CREATE_NEW_PROCESS_GROUP` actually survives
  `ffmpeg-sidecar`'s own `CREATE_NO_WINDOW` flag set (`creation_flags` is a
  plain set, not an OR - call order matters).

### Changed
- README's binary-size note no longer names a specific ffmpeg version inline
  (it drifted out of sync with `core/vendor.toml`); it now points at
  `core/vendor.toml` as the single source of truth.
- Tightened the GPL/license aggregation language in the README and
  `THIRD_PARTY_LICENSES/README.md`: MediaKit invokes ffmpeg only as a
  subprocess and never links it, the GPL build is redistributed unmodified
  under its own terms, and the source link for the exact pinned build is
  verified to resolve.

## [0.1.0] - 2026-08-03

Initial release.
