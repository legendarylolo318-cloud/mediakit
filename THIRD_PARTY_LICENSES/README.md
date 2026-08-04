# Third-party licenses

MediaKit's own source code is MIT-licensed (see the top-level `LICENSE`
file). It bundles pre-built binaries of two other projects so users never
have to install anything themselves; those binaries carry their own
licenses, reproduced here.

## ffmpeg / ffprobe — GPL-3.0-or-later

MediaKit bundles a GPL build of ffmpeg for both platforms (via the
[BtbN FFmpeg-Builds] `win64-gpl` and `linux64-gpl` autobuilds), because it
includes `libx264`/`libx265` for high-quality H.264/H.265 encoding. Those
codec libraries are themselves GPL-licensed, which makes the combined
ffmpeg/ffprobe binaries GPL-3.0-or-later as a whole.

MediaKit invokes ffmpeg/ffprobe **only** as a separate subprocess, launched
the same way a shell would launch any other program on the system - it
never links against libav*/libx264 code, statically or dynamically, and
shares no process memory or address space with it. That keeps MediaKit's
own source outside the GPL's definition of a derivative work, so it stays
under its MIT license; bundling the unmodified GPL binary alongside it is
mere aggregation, not a combined work. The GPL build itself is redistributed
here unmodified and under its own terms (GPL-3.0-or-later), satisfying the
GPL's source-availability requirement via the pinned build's own upstream
source, not a MediaKit-hosted mirror:

- ffmpeg source: <https://ffmpeg.org/download.html> (or the exact commit
  used by the pinned BtbN autobuild in `core/vendor.toml`)
- x264 source: <https://code.videolan.org/videolan/x264>
- Full license text: [`ffmpeg-GPL-3.0.txt`](./ffmpeg-GPL-3.0.txt),
  [`x264-GPL-2.0.txt`](./x264-GPL-2.0.txt)

The exact pinned build (version, download URL, SHA-256) is recorded in
[`core/vendor.toml`](../core/vendor.toml), so anyone can verify exactly
which ffmpeg build a given MediaKit release contains, and download that
exact same build directly from BtbN's release page.

## yt-dlp — Unlicense (public domain)

MediaKit bundles a pre-built [yt-dlp] binary for the optional Download tab.
yt-dlp is released into the public domain under the Unlicense, which places
no restrictions or attribution requirements on MediaKit; the license text is
included here for completeness. Source: <https://github.com/yt-dlp/yt-dlp>.

Full license text: [`yt-dlp-Unlicense.txt`](./yt-dlp-Unlicense.txt)

## Slim builds

Distro packagers building with `--no-default-features` (the `slim` build)
don't bundle any of these binaries at all - MediaKit just looks for
`ffmpeg`/`ffprobe`/`yt-dlp` on `PATH` or lets the user point at their own
install, same as any other program that shells out to them.

[BtbN FFmpeg-Builds]: https://github.com/BtbN/FFmpeg-Builds
[yt-dlp]: https://github.com/yt-dlp/yt-dlp
