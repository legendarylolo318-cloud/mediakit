# Third-party licenses

MediaKit's own source code is MIT-licensed (see the top-level `LICENSE`
file). It bundles pre-built binaries of two other projects so users never
have to install anything themselves; those binaries carry their own
licenses, reproduced here.

## ffmpeg / ffprobe — GPL-3.0-or-later

MediaKit bundles a GPL build of ffmpeg (via the [BtbN FFmpeg-Builds]
Windows build and the [johnvansickle.com] Linux static build), because it
includes `libx264`/`libx265` for high-quality H.264/H.265 encoding. Those
codec libraries are themselves GPL-licensed, which makes the combined
ffmpeg/ffprobe binaries GPL-3.0-or-later as a whole.

MediaKit only ever invokes ffmpeg/ffprobe as a separate subprocess - it
never links against libav*/libx264 code - so MediaKit's own source is not a
derivative work of ffmpeg and stays under its MIT license. The GPL binaries
themselves are distributed here under their own terms, satisfying the GPL's
source-availability requirement:

- ffmpeg source: <https://ffmpeg.org/download.html> (or the exact commit
  used by the pinned BtbN/johnvansickle build in `core/vendor.toml`)
- x264 source: <https://code.videolan.org/videolan/x264>
- Full license text: [`ffmpeg-GPL-3.0.txt`](./ffmpeg-GPL-3.0.txt),
  [`x264-GPL-2.0.txt`](./x264-GPL-2.0.txt)

The exact pinned build (version, download URL, SHA-256) is recorded in
[`core/vendor.toml`](../core/vendor.toml), so anyone can verify exactly
which ffmpeg build a given MediaKit release contains.

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
[johnvansickle.com]: https://johnvansickle.com/ffmpeg/
[yt-dlp]: https://github.com/yt-dlp/yt-dlp
