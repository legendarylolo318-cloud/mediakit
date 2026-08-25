# Changelog

All notable changes to MediaKit are documented here. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/en/1.0.0/); versions match the
tags on the [Releases](../../releases) page.

## [Unreleased]

### Added
- **YouTube PO-token support**: a new `mediakit_core::pot_provider` module
  manages a local [bgutil-ytdlp-pot-provider](https://github.com/Brainicism/bgutil-ytdlp-pot-provider)
  HTTP server to work around YouTube's "Sign in to confirm you're not a bot"
  403s. It starts lazily on 127.0.0.1:4416 (configurable, falls back to a
  free port if taken) the first time a YouTube/`youtu.be` URL is queued,
  reuses an already-listening server instead of spawning a second one, polls
  the port for readiness (20 × 500ms), logs to the app data dir, and is
  killed on app shutdown (via `procgroup`, so it can't leak an orphan on
  Windows either). Since upstream publishes no prebuilt standalone server
  binary (only a tiny yt-dlp plugin zip and a Docker image - verified by
  hand, not assumed), the pinned plugin and source tarball are downloaded
  and checksum-verified into the app data dir, and the server is compiled
  once with the user's own system Node.js/npm (≥ 22, per the pinned
  version's own `package.json`) rather than mediakit's build. Missing/too-old
  Node degrades gracefully to a "failed" status with a clear reason, not a
  hard failure - YouTube downloads still work, just without the extra
  mitigation. Script mode (`getpot_bgutil_script`) was investigated as a way
  to avoid the server build entirely; verified against the real plugin
  (`yt-dlp -v` with plugin dirs pointed at a fresh extraction) that it still
  needs the exact same compiled output, so HTTP mode (also upstream's own
  recommendation) is what's used. yt-dlp invocations for YouTube URLs now
  pass `--plugin-dirs`/`--no-plugin-dirs` (pointed at a mediakit-private
  directory, never the user's own yt-dlp plugin folders/config/PATH),
  `--extractor-args "youtube:player_client=mweb"`,
  `--extractor-args "youtubepot-bgutilhttp:base_url=http://127.0.0.1:<port>"`,
  and `--js-runtimes node` - all four verified against the real bundled
  yt-dlp 2026.07.04 binary's `--help` output and a live dry run, not assumed
  from documentation. A status indicator (stopped/starting/ready/failed)
  sits next to the ffmpeg status in the top panel, and a Settings toggle
  (default on) plus port field control it (`core/src/pot_provider.rs`,
  `core/src/downloader.rs`, `gui/src/app.rs`, `gui/src/settings.rs`).
  Verified end-to-end against a real YouTube video (real `npm ci`/`tsc`
  build, real server, real PO token retrieved and used by a real merged
  download) both standalone and through the fixed `queue_download` path
  below.
- **Convert All**: a queue-wide conversion action, bounded by the existing
  configurable concurrency limit (default `min(4, cpu cores)`, already
  non-blocking via `JobEngine`'s worker pool). The button now disables
  itself when the queue is empty or a batch is already running, tracks the
  in-flight batch's job ids to show live overall progress in the status bar,
  and reports a "n succeeded / n failed" summary once every job in the batch
  reaches a terminal state - per-file failures no longer need a separate
  mechanism since the engine already isolates them from the rest of the
  batch, and Cancel All already kills in-flight ffmpeg process trees
  (`gui/src/app.rs`).
- A Pink theme, wired into the same `ThemeChoice` enum/switcher and
  persisted setting as the existing themes - no colors hardcoded at call
  sites. Dark-first palette (a near-black plum base, hot-pink accent);
  every text/background pairing checked against WCAG AA by hand (body text
  on any background layer is ~13-16:1, the accent as link text is ~5.9:1,
  and the hover/selection shades the shared `themed_visuals` helper derives
  from it stay at 6.7-8.4:1) (`gui/src/settings.rs`, `gui/src/app.rs`).
- H.264 MP4 output now gets `-pix_fmt yuv420p` (when a pixel format wasn't
  already set explicitly) and `-movflags +faststart` forced by default -
  these are what keep files playable in Discord/QuickTime/browsers instead
  of just "plays locally on my machine". Opt out via the Advanced panel's
  "Skip compatibility defaults" checkbox or `--no-compat-defaults` on the
  CLI (`core/src/command.rs`, `gui/src/encode_config.rs`, `gui/src/cli.rs`).

### Fixed
- The PO-token server never actually protected the download that started
  it: `queue_download` called `PotProviderManager::ensure_started` (which
  kicks off setup on a background thread and returns immediately) and then
  read `pot_manager.port()` in the same call, which is essentially always
  `None` since setup takes several seconds minimum - so the very YouTube
  download that triggered the server's first-ever start always got
  submitted with empty `extra_args`, silently falling back to the
  no-PO-token behavior it was meant to fix. Fixed by deferring the actual
  `engine.submit()` for a YouTube URL to a background thread that starts
  the server and *waits* for it (up to 90s) before submitting, via a new
  `DownloadEngine::handle()`/`DownloadSubmitHandle` that can be handed to a
  thread the GUI doesn't own - never blocks the UI thread either way
  (`core/src/download_engine.rs`, `gui/src/app.rs`). Reproduced the race
  and confirmed the fix with a real cold-start download against a real
  YouTube video.
- The pinned BtbN ffmpeg autobuild tag (`autobuild-2026-08-03-14-02`) had
  been pruned upstream, exactly as `core/vendor.toml`'s own comments warn
  can happen, breaking every `bundled`-feature build with a 404. Re-pinned
  to a current autobuild tag at the same ffmpeg 8.1.2 version, with both
  platforms' checksums re-verified by hand against BtbN's own
  `checksums.sha256` (`core/vendor.toml`).

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
