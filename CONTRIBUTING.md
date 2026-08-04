# Contributing to MediaKit

Thanks for taking a look. A few things to know before sending a PR.

## Project layout

- `core/` — GUI-free library (`mediakit-core`). ffmpeg/yt-dlp command building, ffprobe parsing, the job/download engines, target-size bitrate math, presets, the vendored-binary build pipeline (`vendor.rs`/`build.rs`/`vendor.toml`) and yt-dlp self-update (`ytdlp_update.rs`). No `egui`/`eframe` dependency, ever — this crate must stay usable headlessly and be fully unit-testable without a display.
- `gui/` — the `mediakit` binary. The egui frontend (`src/app.rs` and friends) plus the headless CLI mode (`src/cli.rs`). Business logic belongs in `core`; this crate should mostly be "wire core up to widgets."

`core` has a `bundled` (default) and `slim` (`--no-default-features`) Cargo feature - see the README's "Slim builds" section. Anything that assumes a binary is reachable via a fixed path must be gated correctly for both (`#[cfg(feature = "bundled")]` / `#[cfg(not(feature = "bundled"))]`), and both should be exercised - `cargo build --workspace` alone only checks the default (`bundled`) feature set.

## Getting set up

```sh
cargo build --workspace
cargo test --workspace
cargo build --workspace --no-default-features   # also check the slim feature set
```

Some tests in `core` spawn a real `ffmpeg`/`ffprobe`/`yt-dlp` to validate command construction and the job/download engines end-to-end against actual encoder/downloader output, rather than just asserting on argument strings. They skip (print a message, don't fail) if the binary isn't found on `PATH` - note that in a default (`bundled`) build, `PATH` is never consulted at all (see `ffmpeg_env::locate_binary`), so those particular tests only get real coverage under `--no-default-features` with the binary installed, or via the CI smoke test that runs an actual release binary. Tests that hit real network endpoints (yt-dlp metadata fetches, the self-update checker) are gated behind `MEDIAKIT_TEST_NETWORK=1` and skipped by default, so `cargo test` never depends on network access - set that env var if you want to exercise them.

## Before opening a PR

```sh
cargo fmt
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

All three must be clean; CI enforces this on `ubuntu-latest` and `windows-latest`.

## Code style / conventions

- Strong typing over stringly-typed config — encode settings are enums and structs (see `core::command`), not raw strings passed around. New presets or advanced-panel options should follow that pattern.
- No stringly-typed ffmpeg args unless it's the deliberate raw-args escape hatch (`EncodeSettings::extra_args`).
- `core::command::build_args` is the single source of truth for the exact ffmpeg invocation — it backs both the real subprocess call and the GUI's "copy command" preview. If you add a new setting, make sure it flows through there rather than being applied in two places that can drift.
- Errors carry real ffmpeg stderr where relevant (see `CoreError`) — never collapse a failure down to a bare "something went wrong."
- New preset builders (`core::presets`) should get unit tests asserting on the generated argument list, following the existing style in `core/src/presets.rs`. If practical, sanity-check the exact command against a real ffmpeg invocation once by hand (as the existing presets were) even if that check doesn't ship as an automated test.
- Keep `core` and `gui` decoupled: if you find yourself wanting to import `egui` in `core`, the logic probably belongs in `gui` instead (or the abstraction needs rethinking).

## Reporting bugs

Include your OS, ffmpeg version (`ffmpeg -version`), the preset/settings used, and — if a job failed — the per-job log (visible via "Show log" in the app, or printed to stdout in CLI mode).
