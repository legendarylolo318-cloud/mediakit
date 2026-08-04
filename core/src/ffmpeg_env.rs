//! Locating, caching, and (optionally) downloading the ffmpeg/ffprobe binaries.
//!
//! Detection order is fixed by product requirement: app data dir first (so a
//! previously auto-downloaded build always wins), then next to the running
//! executable (a "portable" bundle), then finally `$PATH`.

use crate::error::{CoreError, CoreResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

pub const APP_QUALIFIER: &str = "";
pub const APP_ORG: &str = "";
pub const APP_NAME: &str = "mediakit";

/// Resolve the per-OS application data directory (created if missing).
pub fn app_data_dir() -> CoreResult<PathBuf> {
    let dirs = directories::ProjectDirs::from(APP_QUALIFIER, APP_ORG, APP_NAME)
        .ok_or(CoreError::NoAppDataDir)?;
    let dir = dirs.data_dir().to_path_buf();
    std::fs::create_dir_all(&dir).map_err(|source| CoreError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

fn binary_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Search `$PATH` for an executable named `name` (platform-appropriate suffix
/// already applied by the caller). Only used by `slim` builds; `bundled`
/// builds never consult `PATH` (see [`locate_binary`]).
#[cfg(not(feature = "bundled"))]
fn find_in_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .parent()
        .map(|p| p.to_path_buf())
}

/// Locate a single binary (`ffmpeg` or `ffprobe`) using the fixed detection
/// order: app data dir, then next to the executable, then `$PATH`.
///
/// `bundled` builds never fall through to `$PATH` - the vendored binaries
/// extracted into the app data dir (see [`crate::vendor::ensure_extracted`])
/// are supposed to always be found at the first tier, so a user is never
/// required to have anything on `PATH`. `slim` builds keep the full chain,
/// since they have nothing vendored to rely on.
pub fn locate_binary(base_name: &str, app_data_dir: &Path) -> Option<PathBuf> {
    let name = binary_name(base_name);

    let in_app_data = app_data_dir.join(&name);
    if is_executable_file(&in_app_data) {
        return Some(in_app_data);
    }

    if let Some(dir) = exe_dir() {
        let candidate = dir.join(&name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    #[cfg(feature = "bundled")]
    {
        None
    }
    #[cfg(not(feature = "bundled"))]
    {
        find_in_path(&name)
    }
}

/// Locate both `ffmpeg` and `ffprobe`. Both must be found for this to succeed.
pub fn locate_ffmpeg_and_ffprobe(app_data_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let ffmpeg = locate_binary("ffmpeg", app_data_dir)?;
    let ffprobe = locate_binary("ffprobe", app_data_dir)?;
    Some((ffmpeg, ffprobe))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncoderKind {
    Video,
    Audio,
    Subtitle,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderInfo {
    pub name: String,
    pub description: String,
    pub kind: EncoderKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FfmpegEnv {
    pub ffmpeg_path: PathBuf,
    pub ffprobe_path: PathBuf,
    pub version: String,
    pub encoders: Vec<EncoderInfo>,
    /// `None` in `slim` builds when yt-dlp isn't found anywhere (PATH
    /// included) - the Download tab is simply unavailable in that case.
    pub ytdlp_path: Option<PathBuf>,
    pub ytdlp_version: Option<String>,
}

/// `ffmpeg -version`, returning just the first line's version token (e.g. `"6.1.1"`).
pub fn probe_version(ffmpeg: &Path) -> CoreResult<String> {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-version");
    crate::sys::no_console_window(&mut cmd);
    let output = cmd.output().map_err(|source| CoreError::Spawn {
        binary: ffmpeg.to_path_buf(),
        source,
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_version_line(&stdout).unwrap_or_else(|| "unknown".to_string()))
}

fn parse_version_line(stdout: &str) -> Option<String> {
    let first_line = stdout.lines().next()?;
    // e.g. "ffmpeg version 6.1.1-full_build-www.gyan.dev Copyright (c) 2000-2023 ..."
    let after = first_line.strip_prefix("ffmpeg version ")?;
    after.split_whitespace().next().map(|s| s.to_string())
}

/// `ffmpeg -encoders`, parsed into a structured list.
pub fn probe_encoders(ffmpeg: &Path) -> CoreResult<Vec<EncoderInfo>> {
    let mut cmd = Command::new(ffmpeg);
    cmd.arg("-hide_banner").arg("-encoders");
    crate::sys::no_console_window(&mut cmd);
    let output = cmd.output().map_err(|source| CoreError::Spawn {
        binary: ffmpeg.to_path_buf(),
        source,
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_encoders_output(&stdout))
}

fn parse_encoders_output(stdout: &str) -> Vec<EncoderInfo> {
    // Lines look like: " V..... libx264              libx264 H.264 / AVC / MPEG-4 AVC ..."
    // The flags column's first character indicates the media kind: V/A/S.
    let mut encoders = Vec::new();
    let mut past_header = false;
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        if !past_header {
            if trimmed.starts_with("------") {
                past_header = true;
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(3, char::is_whitespace);
        let flags = match parts.next() {
            Some(f) => f,
            None => continue,
        };
        let name = match parts.next() {
            Some(n) => n,
            None => continue,
        };
        let description = parts.next().unwrap_or("").trim().to_string();

        let kind = match flags.chars().next() {
            Some('V') => EncoderKind::Video,
            Some('A') => EncoderKind::Audio,
            Some('S') => EncoderKind::Subtitle,
            _ => EncoderKind::Unknown,
        };

        encoders.push(EncoderInfo {
            name: name.to_string(),
            description,
            kind,
        });
    }
    encoders
}

#[derive(Debug, Serialize, Deserialize)]
struct FfmpegEnvCache {
    ffmpeg_path: PathBuf,
    ffprobe_path: PathBuf,
    ffmpeg_mtime_unix: u64,
    env: FfmpegEnvCacheable,
}

#[derive(Debug, Serialize, Deserialize)]
struct FfmpegEnvCacheable {
    version: String,
    encoders: Vec<EncoderInfo>,
}

fn cache_file_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("ffmpeg_env_cache.json")
}

fn mtime_unix(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Resolve ffmpeg/ffprobe/yt-dlp paths: in `bundled` builds, ensure the
/// vendored binaries are extracted (a fast no-op after the first run) and
/// use those directly; in `slim` builds, fall back to the
/// app-data/exe-dir/PATH chain for ffmpeg/ffprobe, and a PATH-only search
/// for yt-dlp (which was never part of the original ffmpeg detection story).
fn resolve_tool_paths(
    app_data_dir: &Path,
) -> CoreResult<(PathBuf, PathBuf, Option<PathBuf>, Option<String>)> {
    if let Some(vendored) = crate::vendor::ensure_extracted(app_data_dir)? {
        // yt-dlp can be self-updated in place (see `ytdlp_update`), which
        // leaves the on-disk binary newer than `vendor_manifest.json`'s
        // pinned version - so its version is always probed fresh rather
        // than trusted from the manifest. ffmpeg/ffprobe are never
        // self-updated, so their manifest version is trustworthy and
        // skipping a probe there keeps startup fast.
        let ytdlp_version = probe_ytdlp_version(&vendored.ytdlp).unwrap_or(vendored.ytdlp_version);
        return Ok((
            vendored.ffmpeg,
            vendored.ffprobe,
            Some(vendored.ytdlp),
            Some(ytdlp_version),
        ));
    }
    let (ffmpeg_path, ffprobe_path) =
        locate_ffmpeg_and_ffprobe(app_data_dir).ok_or(CoreError::FfmpegNotFound)?;
    let ytdlp_path = locate_binary("yt-dlp", app_data_dir);
    let ytdlp_version = ytdlp_path
        .as_deref()
        .and_then(|p| probe_ytdlp_version(p).ok());
    Ok((ffmpeg_path, ffprobe_path, ytdlp_path, ytdlp_version))
}

/// `yt-dlp --version`, which prints just the bare version string.
pub fn probe_ytdlp_version(ytdlp: &Path) -> CoreResult<String> {
    let mut cmd = Command::new(ytdlp);
    cmd.arg("--version");
    crate::sys::no_console_window(&mut cmd);
    let output = cmd.output().map_err(|source| CoreError::Spawn {
        binary: ytdlp.to_path_buf(),
        source,
    })?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// User-supplied binary path overrides (Settings -> Tools), taking priority
/// over whatever was auto-detected/bundled.
#[derive(Debug, Clone, Default)]
pub struct ToolOverrides {
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    pub ytdlp: Option<PathBuf>,
}

impl FfmpegEnv {
    /// Detect ffmpeg/ffprobe, then load version + encoder info from a disk
    /// cache if it's still valid (keyed on binary path + mtime), otherwise
    /// probe fresh and refresh the cache.
    pub fn detect_cached(app_data_dir: &Path) -> CoreResult<FfmpegEnv> {
        Self::detect_cached_with_overrides(app_data_dir, &ToolOverrides::default())
    }

    /// Same as [`Self::detect_cached`], but any path set in `overrides`
    /// wins over auto-detection/bundling for that specific tool.
    pub fn detect_cached_with_overrides(
        app_data_dir: &Path,
        overrides: &ToolOverrides,
    ) -> CoreResult<FfmpegEnv> {
        let (mut ffmpeg_path, mut ffprobe_path, mut ytdlp_path, mut ytdlp_version) =
            resolve_tool_paths(app_data_dir)?;

        if let Some(p) = &overrides.ffmpeg {
            ffmpeg_path = p.clone();
        }
        if let Some(p) = &overrides.ffprobe {
            ffprobe_path = p.clone();
        }
        if let Some(p) = &overrides.ytdlp {
            ytdlp_path = Some(p.clone());
            ytdlp_version = probe_ytdlp_version(p).ok();
        }

        let current_mtime = mtime_unix(&ffmpeg_path);

        let cache_path = cache_file_path(app_data_dir);
        if let Ok(bytes) = std::fs::read(&cache_path) {
            if let Ok(cached) = serde_json::from_slice::<FfmpegEnvCache>(&bytes) {
                if cached.ffmpeg_path == ffmpeg_path
                    && cached.ffprobe_path == ffprobe_path
                    && cached.ffmpeg_mtime_unix == current_mtime
                {
                    return Ok(FfmpegEnv {
                        ffmpeg_path,
                        ffprobe_path,
                        version: cached.env.version,
                        encoders: cached.env.encoders,
                        ytdlp_path,
                        ytdlp_version,
                    });
                }
            }
        }

        let version = probe_version(&ffmpeg_path)?;
        let encoders = probe_encoders(&ffmpeg_path)?;

        let cache = FfmpegEnvCache {
            ffmpeg_path: ffmpeg_path.clone(),
            ffprobe_path: ffprobe_path.clone(),
            ffmpeg_mtime_unix: current_mtime,
            env: FfmpegEnvCacheable {
                version: version.clone(),
                encoders: encoders.clone(),
            },
        };
        if let Ok(json) = serde_json::to_vec_pretty(&cache) {
            let _ = std::fs::write(&cache_path, json);
        }

        Ok(FfmpegEnv {
            ffmpeg_path,
            ffprobe_path,
            version,
            encoders,
            ytdlp_path,
            ytdlp_version,
        })
    }
}

/// Progress events emitted while downloading a static ffmpeg build.
pub use ffmpeg_sidecar::download::FfmpegDownloadProgressEvent as DownloadProgress;

/// Download a static ffmpeg build into `app_data_dir` (creating it if
/// necessary). Used when detection fails and the user opts in to an
/// automatic download rather than browsing for a binary manually.
pub fn download_into(app_data_dir: &Path, progress: impl Fn(DownloadProgress)) -> CoreResult<()> {
    std::fs::create_dir_all(app_data_dir).map_err(|source| CoreError::Io {
        path: app_data_dir.to_path_buf(),
        source,
    })?;

    let url = ffmpeg_sidecar::download::ffmpeg_download_url()
        .map_err(|e| CoreError::Download(e.to_string()))?;
    let archive_path = ffmpeg_sidecar::download::download_ffmpeg_package_with_progress(
        url,
        app_data_dir,
        &progress,
    )
    .map_err(|e| CoreError::Download(e.to_string()))?;

    progress(DownloadProgress::UnpackingArchive);
    ffmpeg_sidecar::download::unpack_ffmpeg(&archive_path, app_data_dir)
        .map_err(|e| CoreError::Download(e.to_string()))?;
    progress(DownloadProgress::Done);

    if locate_ffmpeg_and_ffprobe(app_data_dir).is_none() {
        return Err(CoreError::Download(
            "ffmpeg was downloaded but binaries were not found afterwards".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_line() {
        let stdout = "ffmpeg version 6.1.1-full_build-www.gyan.dev Copyright (c) 2000-2023 the FFmpeg developers\nbuilt with gcc...";
        assert_eq!(
            parse_version_line(stdout).as_deref(),
            Some("6.1.1-full_build-www.gyan.dev")
        );
    }

    #[test]
    fn parses_version_line_simple() {
        let stdout = "ffmpeg version 7.0 Copyright (c) 2000-2024 the FFmpeg developers";
        assert_eq!(parse_version_line(stdout).as_deref(), Some("7.0"));
    }

    #[test]
    fn returns_none_for_garbage_version_output() {
        assert_eq!(parse_version_line("not ffmpeg output"), None);
    }

    #[test]
    fn parses_encoders_list() {
        let stdout = "\
Encoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 ------
 V..... libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10
 V....S h264_nvenc            NVIDIA NVENC H.264 encoder (codec h264)
 A..... aac                  AAC (Advanced Audio Coding)
 S..... srt                  SubRip subtitle
";
        let encoders = parse_encoders_output(stdout);
        assert_eq!(encoders.len(), 4);
        assert_eq!(encoders[0].name, "libx264");
        assert_eq!(encoders[0].kind, EncoderKind::Video);
        assert!(encoders[0].description.contains("H.264"));

        assert_eq!(encoders[1].name, "h264_nvenc");
        assert_eq!(encoders[1].kind, EncoderKind::Video);

        assert_eq!(encoders[2].name, "aac");
        assert_eq!(encoders[2].kind, EncoderKind::Audio);

        assert_eq!(encoders[3].name, "srt");
        assert_eq!(encoders[3].kind, EncoderKind::Subtitle);
    }

    #[test]
    fn locate_binary_prefers_app_data_dir_over_path() {
        let tmp = tempfile::tempdir().unwrap();
        let app_data = tmp.path().join("app_data");
        std::fs::create_dir_all(&app_data).unwrap();
        let fake_ffmpeg = app_data.join(binary_name("ffmpeg"));
        std::fs::write(&fake_ffmpeg, b"fake").unwrap();

        let found = locate_binary("ffmpeg", &app_data).expect("should find binary");
        assert_eq!(found, fake_ffmpeg);
    }

    #[test]
    fn locate_binary_returns_none_when_missing_everywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let app_data = tmp.path().join("empty");
        std::fs::create_dir_all(&app_data).unwrap();
        assert!(
            locate_binary("definitely_not_a_real_mediakit_test_binary_xyz", &app_data).is_none()
        );
    }
}
