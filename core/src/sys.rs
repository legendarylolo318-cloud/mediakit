//! Small cross-platform process-spawning helpers.

use std::process::Command;

/// Suppress the console window flash on Windows when spawning a child
/// process from a GUI (`windows_subsystem = "windows"`) app. No-op on other
/// platforms. `ffmpeg-sidecar`'s `FfmpegCommand` already does this
/// internally; this covers our own raw `std::process::Command` calls
/// (ffprobe, `ffmpeg -version`/`-encoders`, yt-dlp).
pub fn no_console_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
