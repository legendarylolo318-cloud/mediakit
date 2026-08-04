# MediaKit

A native, cross-platform desktop app for converting and shrinking video, audio, GIFs, and images — the local alternative to ezgif, 8mb.video, and CloudConvert. No browser, no upload, no telemetry.

Built with Rust + [egui](https://github.com/emilk/egui) for the GUI, and drives `ffmpeg`/`ffprobe`/`yt-dlp` as subprocesses for all media work. The default download bundles all three, so **nothing needs to be installed separately** — download one file, run it.

<!--
  Screenshot placeholder: drop images in docs/ and reference them here, e.g.
  ![Batch queue with a Discord preset applied](docs/screenshot-queue.png)
-->


## Features

**One-click presets** (the whole point of this app)
- Data-driven target-size presets — Discord's Free/Nitro Basic/Nitro/legacy tiers ship as defaults, fully user-editable (add/rename/reorder/delete, "restore defaults") via Settings → Presets or by hand-editing `presets.toml`; see [Target-size presets](#target-size-presets-presetstoml) below
- Custom target size (in MiB)
- Video → GIF (high quality palette-based encode, configurable fps/width/dither)
- Image → GIF (turns one still picture into a short, fixed-length looping GIF)
- Images → animated GIF / APNG / WebP
- GIF → MP4/WebM (shrinks massively)
- Extract audio → MP3 / Opus / FLAC / WAV
- Convert image → PNG / JPG / WebP / AVIF / BMP / ICO, with a quality slider
- Mute, strip metadata, rotate/flip, reverse, speed up/slow down

**Download tab** (optional, powered by bundled `yt-dlp`)
- Paste one or many URLs (single videos, playlists, or channels) and fetch metadata — title, uploader, duration, thumbnail, format table
- One-click format picker (Best / Best ≤1080p / Best ≤720p / audio-only MP3 / audio-only best) or a raw format-selector field
- Playlist/channel support with per-item checkboxes and select-all/none
- Options: embed thumbnail/metadata, subtitles (with auto-subs), SponsorBlock removal, rate limiting, concurrent fragments, custom filename template
- Login-required content: cookies from a browser profile or a `cookies.txt` file — MediaKit never stores credentials and redacts cookie values from any displayed/copied command
- **Chaining**: "Download → Discord-size clip", "Download → GIF", "Download → MP3" queue as one entry; the intermediate download is cleaned up automatically once the conversion finishes
- Self-update: check for and install newer `yt-dlp` releases from Settings → Tools (checksum-verified, with one-click rollback), plus an optional weekly auto-check

MediaKit only ever shells out to `yt-dlp` — it doesn't implement site-specific extraction itself, and it will not attempt to work around DRM. **You're responsible for complying with the terms of service and copyright law of whatever site or content you download from.**

**Batch queue**
- Drag & drop or file-picker input, multi-file selection
- Per-item status (queued / running / done / failed / cancelled), reorder, remove, clear
- Real percentage + ETA progress per job, parsed from ffmpeg's machine-readable progress stream
- Configurable concurrency; cancel individual jobs or the whole queue
- Size-targeted jobs get a real pass/fail badge with the actual output byte count once they finish, not just "done"

**Advanced panel**
- Manual container/codec (H.264/H.265/VP9/AV1), CRF, encoder speed preset, resolution (with aspect-ratio lock), fps, audio codec/bitrate/sample rate
- Trim start/end, crop
- Raw ffmpeg args escape hatch
- Live preview of the exact ffmpeg command that will run, with a "copy command" button

**Hardware acceleration**
- Auto-detects NVENC, Quick Sync, VAAPI, AMF, and VideoToolbox encoders your ffmpeg build supports
- Off by default (software x264 for reliability); opt in per job, with automatic fallback to software if the hardware encode fails

**Everything else**
- ffmpeg/ffprobe/yt-dlp bundled and extracted automatically on first launch — no install, no PATH setup, no admin/root
- Six themes (System/Light/Dark plus Nord, Solarized Dark, and High Contrast), roomier rounded buttons
- Settings, window state, and target-size presets persisted per-OS
- Per-job ffmpeg/yt-dlp logs, viewable in-app, for debugging failures
- Headless CLI mode for scripting: `mediakit --preset target-size --size-preset discord-free input.mp4 -o out.mp4`

## Install

### Prebuilt binaries

Grab the latest release for your OS from the [Releases](../../releases) page:
- **Windows**: download and run `mediakit-windows-x86_64.zip` → `mediakit.exe`. Portable, no installer, no admin rights needed.
- **Linux**: download `mediakit-linux-x86_64.tar.gz` and extract, or use the `.AppImage` if one was published for that release.

That's it — the default build bundles its own `ffmpeg`, `ffprobe`, and `yt-dlp`, checksum-verified at build time and extracted to your user app-data directory on first launch. Nothing else to install. CI runs a smoke test on every build that converts a real clip inside a container with no ffmpeg present at all, to keep that claim honest.

If you'd rather use your own ffmpeg/yt-dlp install (or a Linux distro package maintainer needs a build that doesn't vendor binaries at all), see [Slim builds](#slim-builds---no-default-features) below.

### From source

Requires the Rust stable toolchain ([rustup.rs](https://rustup.rs)).

```sh
git clone <this repo>
cd mediakit
cargo build --release
./target/release/mediakit          # Linux
# .\target\release\mediakit.exe    # Windows
```

## Build instructions

```sh
cargo build --release          # build both crates (bundled, the default)
cargo build --release --no-default-features   # slim build - see below
cargo test --workspace         # run unit + integration tests (some tests spawn real ffmpeg and skip gracefully if it isn't on PATH)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt
```

The workspace has two crates:
- `core` — GUI-free library: ffmpeg/yt-dlp command building, ffprobe parsing, the job/download engines, target-size math, the vendored-binary build pipeline. Fully unit-testable, no UI dependency.
- `gui` — the `mediakit` binary: the egui frontend, plus the headless CLI mode.

Cross-compiling for Windows from Linux/macOS works with the `x86_64-pc-windows-msvc` or `x86_64-pc-windows-gnu` target once installed via `rustup target add`; CI builds natively on `windows-latest` (see `.github/workflows/ci.yml`), which runs both the `bundled` and `slim` variants on both platforms, plus a container-based smoke test of the actual release binary.

### Slim builds (`--no-default-features`)

Distro packagers (AUR, Nixpkgs, etc.) who must not vendor pre-built binaries can build with `--no-default-features`. Slim builds never download or embed anything at build time and never touch a bundled binary at runtime — they fall back entirely to the same app-data-dir → next-to-executable → `PATH` detection order the original version of MediaKit used, and expect the user (or the distro package's dependencies) to provide `ffmpeg`/`ffprobe`/`yt-dlp` themselves.

### Binary size

The default (`bundled`) release binary is roughly **~100 MB** on Linux (it embeds a full static ffmpeg build plus ffprobe and yt-dlp, zstd-compressed). Windows is in a similar ballpark. `slim` builds are a few MB, since they contain none of that. The exact ffmpeg/yt-dlp versions bundled (and their sizes) depend entirely on the pins in [`core/vendor.toml`](core/vendor.toml) at the time of a given release, not on anything in this README — check that file for what's actually in a given build; CI prints the actual built size for every release artifact.

## Target-size presets (`presets.toml`)

Target-size limits (Discord's tiers, and anywhere else you add) are **data, not code** — nothing in MediaKit's source hardcodes a byte count. They live in `presets.toml`:
- The defaults MediaKit ships with are seeded into your config directory the first time you run it (or the first time the CLI needs them).
- Edit that file directly, or use **Settings → Presets** in the app to add/rename/reorder/delete presets and tune the safety margin, with a "Restore defaults" button.
- Sizes are computed in **MiB (1024²)**, not decimal MB, matching what file managers and `du` report.
- After every size-targeted encode, MediaKit checks the real output size against the preset's limit and shows a pass/fail badge with the actual byte count — not just "done".

Shipped defaults (Discord's caps as verified **2026-08-03** — check Discord's own current limits and edit `presets.toml` if they've changed since):

| Preset | Limit |
|---|---|
| Discord — Free | 10 MB |
| Discord — Nitro Basic | 50 MB |
| Discord — Nitro | 500 MB |
| Discord — Legacy / safe | 8 MB |

## CLI mode

Passing any arguments switches MediaKit into headless mode — same engine, no window:

```sh
mediakit --preset target-size --size-preset discord-free input.mp4 -o out.mp4
mediakit --preset target-size --target-mib 25 input.mp4 -o out.mp4   # custom size, no preset
mediakit --list-size-presets                                         # show available --size-preset ids
mediakit --preset video-to-gif clip.mp4
mediakit --preset image-to-gif photo.png --image-gif-seconds 2
mediakit --preset extract-audio-mp3 movie.mkv -o audio.mp3
mediakit --help   # full list of presets and flags
```

The Download tab is GUI-only for now; the CLI covers local file conversion.

## FAQ

**Does this upload my files anywhere?**
No. Everything runs locally via local `ffmpeg`/`yt-dlp` subprocesses. The only network access MediaKit ever makes is: fetching a URL you explicitly paste into the Download tab, an optional yt-dlp self-update check, and (only in `slim` builds without a bundled ffmpeg) an opt-in one-time ffmpeg download.

**Do I need to install ffmpeg or yt-dlp myself?**
No, not with the default build — both are bundled and extracted automatically. See [Slim builds](#slim-builds---no-default-features) if you specifically want MediaKit to use your own install instead.

**Is it legal to bundle ffmpeg? Isn't ffmpeg GPL?**
MediaKit's own source stays MIT-licensed — it only ever invokes ffmpeg as a separate subprocess, the same way a shell would launch any other program, and never links against its code (statically or dynamically) or shares process memory with it, so MediaKit isn't a derivative work under the GPL. The bundled ffmpeg build itself is GPL-3.0-or-later (it includes libx264/libx265) and is redistributed here **unmodified**, under its own terms — bundling it alongside an MIT-licensed app is mere aggregation, not a combined work. Full compliance materials are in [`THIRD_PARTY_LICENSES/`](THIRD_PARTY_LICENSES/): license texts, a working source link back to the exact pinned build, and the exact pinned build/version/checksum in `core/vendor.toml`. Same info is in the in-app Licenses dialog. yt-dlp is public domain (Unlicense).

**What am I responsible for when using the Download tab?**
Complying with the terms of service and copyright law of whatever site/content you're downloading from — MediaKit is a thin wrapper around yt-dlp and doesn't implement or work around any site's DRM or access controls.

**Why does a target-size preset sometimes need more than one pass?**
Bitrate control isn't exact — encoders can overshoot the target, especially on hard-to-compress content. MediaKit does a real two-pass encode and, if the result is still over budget, automatically retries with a reduced bitrate (up to 3 times), logging each attempt, then shows a real pass/fail badge against the preset's byte limit once it's done.

**My hardware encoder failed / produced a black frame. What happened?**
Hardware encoding is opt-in and can fail for reasons unrelated to your settings (missing codec support, driver issues, VRAM limits). MediaKit catches encode failures and automatically retries once in software before reporting an error — check "Show log" on the job for the real ffmpeg output either way.

**Where does MediaKit store settings, presets, and the vendored binaries?**
The per-OS app data / config directory (via the `directories` crate) — e.g. `~/.local/share/mediakit` (vendored binaries, extraction manifest) and `~/.config/mediakit` (settings, `presets.toml`) on Linux, `%APPDATA%\mediakit` on Windows.

**Can I pass raw ffmpeg flags it doesn't have a control for?**
Yes — the Advanced panel has a "Custom ffmpeg args" field appended verbatim to the generated command, and the command preview shows exactly what will run before you convert.

## License

MediaKit's own source is **MIT** — see [LICENSE](LICENSE).

The default build bundles pre-built third-party binaries under their own licenses: **ffmpeg/ffprobe (GPL-3.0-or-later)** and **yt-dlp (Unlicense/public domain)**. MediaKit invokes both only as subprocesses — it never links against their code — so it isn't a derivative work, and the GPL binary is redistributed unmodified under its own terms (mere aggregation, not a combined work). Full texts, source links back to the exact pinned build, and the exact pinned versions/checksums (`core/vendor.toml`) are in [`THIRD_PARTY_LICENSES/`](THIRD_PARTY_LICENSES/) and the in-app Licenses dialog. `slim` builds (`--no-default-features`) bundle none of these.

See [CHANGELOG.md](CHANGELOG.md) for release history.
