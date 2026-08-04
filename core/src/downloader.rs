//! yt-dlp integration: metadata fetching, download argument construction,
//! and progress parsing. MediaKit only ever shells out to yt-dlp - it does
//! not implement or work around any site-specific extraction or DRM itself.

use crate::error::{CoreError, CoreResult};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

// --- Metadata ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Metadata {
    Video(VideoMetadata),
    Playlist(PlaylistMetadata),
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoMetadata {
    pub title: String,
    pub uploader: Option<String>,
    pub duration_seconds: Option<f64>,
    pub thumbnail_url: Option<String>,
    pub webpage_url: String,
    pub formats: Vec<FormatInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormatInfo {
    pub format_id: String,
    pub ext: Option<String>,
    pub resolution: Option<String>,
    pub fps: Option<f64>,
    pub vcodec: Option<String>,
    pub acodec: Option<String>,
    pub filesize_bytes: Option<u64>,
    pub format_note: Option<String>,
}

impl FormatInfo {
    fn is_none_codec(codec: &Option<String>) -> bool {
        codec.as_deref().is_none_or(|c| c == "none")
    }

    pub fn has_video(&self) -> bool {
        !Self::is_none_codec(&self.vcodec)
    }

    pub fn has_audio(&self) -> bool {
        !Self::is_none_codec(&self.acodec)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistMetadata {
    pub title: String,
    pub entries: Vec<PlaylistEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistEntry {
    pub id: String,
    pub title: String,
    pub url: String,
    pub duration_seconds: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct RawMetadata {
    #[serde(rename = "_type", default)]
    type_: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    uploader: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    webpage_url: Option<String>,
    #[serde(default)]
    formats: Vec<RawFormat>,
    #[serde(default)]
    entries: Vec<RawPlaylistEntry>,
}

#[derive(Debug, Deserialize)]
struct RawFormat {
    format_id: String,
    #[serde(default)]
    ext: Option<String>,
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    fps: Option<f64>,
    #[serde(default)]
    vcodec: Option<String>,
    #[serde(default)]
    acodec: Option<String>,
    #[serde(default)]
    filesize: Option<u64>,
    #[serde(default)]
    filesize_approx: Option<u64>,
    #[serde(default)]
    format_note: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawPlaylistEntry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
}

/// Fetch metadata for `url` via `yt-dlp -J`. Works for both a single
/// video/URL and a playlist/channel URL - `--flat-playlist` keeps playlist
/// listing fast (title/id/url per entry only) without fully resolving every
/// entry's formats, which single-video URLs are unaffected by.
pub fn fetch_metadata(
    ytdlp: &Path,
    url: &str,
    cookies: Option<&CookieSource>,
) -> CoreResult<Metadata> {
    let mut cmd = Command::new(ytdlp);
    cmd.args(["-J", "--flat-playlist", "--no-warnings"]);
    if let Some(cookies) = cookies {
        cookies.apply(&mut cmd);
    }
    cmd.arg(url);
    crate::sys::no_console_window(&mut cmd);

    let output = cmd.output().map_err(|source| CoreError::Spawn {
        binary: ytdlp.to_path_buf(),
        source,
    })?;

    if !output.status.success() {
        return Err(CoreError::Other(anyhow::anyhow!(
            "yt-dlp failed to fetch metadata for {url}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let raw: RawMetadata =
        serde_json::from_slice(&output.stdout).map_err(|source| CoreError::ProbeParse {
            path: PathBuf::from(url),
            source,
        })?;

    if raw.type_.as_deref() == Some("playlist") {
        Ok(Metadata::Playlist(PlaylistMetadata {
            title: raw.title.unwrap_or_else(|| url.to_string()),
            entries: raw
                .entries
                .into_iter()
                .filter_map(|e| {
                    Some(PlaylistEntry {
                        id: e.id?,
                        title: e.title.unwrap_or_else(|| "(untitled)".to_string()),
                        url: e.url?,
                        duration_seconds: e.duration,
                    })
                })
                .collect(),
        }))
    } else {
        Ok(Metadata::Video(VideoMetadata {
            title: raw.title.unwrap_or_else(|| url.to_string()),
            uploader: raw.uploader,
            duration_seconds: raw.duration,
            thumbnail_url: raw.thumbnail,
            webpage_url: raw.webpage_url.unwrap_or_else(|| url.to_string()),
            formats: raw
                .formats
                .into_iter()
                .map(|f| FormatInfo {
                    format_id: f.format_id,
                    ext: f.ext,
                    resolution: f.resolution,
                    fps: f.fps,
                    vcodec: f.vcodec,
                    acodec: f.acodec,
                    filesize_bytes: f.filesize.or(f.filesize_approx),
                    format_note: f.format_note,
                })
                .collect(),
        }))
    }
}

// --- Download options ---------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum FormatSelection {
    Best,
    BestUpTo1080p,
    BestUpTo720p,
    AudioOnlyMp3,
    AudioOnlyBest,
    Custom(String),
}

impl FormatSelection {
    fn selector(&self) -> Option<&str> {
        match self {
            FormatSelection::Best => Some("bestvideo*+bestaudio/best"),
            FormatSelection::BestUpTo1080p => {
                Some("bestvideo[height<=1080]+bestaudio/best[height<=1080]")
            }
            FormatSelection::BestUpTo720p => {
                Some("bestvideo[height<=720]+bestaudio/best[height<=720]")
            }
            // Audio-only extraction is driven by `-x`/`--audio-format`
            // rather than `-f`, so there's no format selector to pass here.
            FormatSelection::AudioOnlyMp3 | FormatSelection::AudioOnlyBest => None,
            FormatSelection::Custom(s) => Some(s.as_str()),
        }
    }
}

/// A browser-cookies source (`--cookies-from-browser`) or an exported
/// `cookies.txt` file (`--cookies`), for content that requires being logged
/// in. MediaKit never stores or logs cookie values itself.
#[derive(Debug, Clone, PartialEq)]
pub enum CookieSource {
    Browser {
        browser: String,
        profile: Option<String>,
    },
    File(PathBuf),
}

impl CookieSource {
    fn apply(&self, cmd: &mut Command) {
        match self {
            CookieSource::Browser { browser, profile } => {
                let spec = match profile {
                    Some(p) => format!("{browser}:{p}"),
                    None => browser.clone(),
                };
                cmd.arg("--cookies-from-browser").arg(spec);
            }
            CookieSource::File(path) => {
                cmd.arg("--cookies").arg(path);
            }
        }
    }

    /// The equivalent CLI args, with the value never included - for
    /// building a *displayable* command preview without leaking which
    /// browser profile or file path a user has configured (the flag name
    /// alone isn't sensitive, but the value is worth keeping out of a
    /// screen anyone might glance at or a log that gets shared).
    fn redacted_args(&self) -> Vec<String> {
        match self {
            CookieSource::Browser { .. } => {
                vec![
                    "--cookies-from-browser".to_string(),
                    "<redacted>".to_string(),
                ]
            }
            CookieSource::File(_) => vec!["--cookies".to_string(), "<redacted>".to_string()],
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleOptions {
    pub languages: Vec<String>,
    pub auto_subs: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadOptions {
    pub embed_thumbnail: bool,
    pub embed_metadata: bool,
    pub subtitles: Option<SubtitleOptions>,
    pub sponsorblock_remove: bool,
    pub rate_limit_kbps: Option<u64>,
    pub concurrent_fragments: Option<u32>,
    pub cookies: Option<CookieSource>,
    /// yt-dlp output filename template, e.g. `"%(title)s.%(ext)s"`.
    pub output_template: String,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            embed_thumbnail: false,
            embed_metadata: false,
            subtitles: None,
            sponsorblock_remove: false,
            rate_limit_kbps: None,
            concurrent_fragments: None,
            cookies: None,
            output_template: "%(title)s.%(ext)s".to_string(),
        }
    }
}

/// The `--progress-template` yt-dlp is always invoked with; [`parse_progress_line`]
/// is the matching parser. Keeping these next to each other means they can
/// never drift apart.
pub const PROGRESS_TEMPLATE: &str =
    "%(progress._percent_str)s|%(progress._speed_str)s|%(progress._eta_str)s|%(progress.status)s";

/// A distinctive line prefix used to extract the final output path (after
/// merging/remuxing/post-processing) from yt-dlp's mixed stdout stream -
/// see [`parse_final_path_line`]. Needed for chaining a download straight
/// into a conversion job.
const FINAL_PATH_MARKER: &str = "MEDIAKIT_FINAL_PATH:";

/// Build the full yt-dlp argument list (everything after the binary path)
/// for a download. `ffmpeg_dir` is passed via `--ffmpeg-location` so yt-dlp
/// uses MediaKit's own bundled ffmpeg for merging/remuxing/audio-extraction
/// rather than requiring one on `PATH`.
pub fn build_download_args(
    url: &str,
    format: &FormatSelection,
    ffmpeg_dir: &Path,
    options: &DownloadOptions,
) -> Vec<String> {
    let mut args = vec![
        "--newline".to_string(),
        "--no-warnings".to_string(),
        "--ffmpeg-location".to_string(),
        ffmpeg_dir.to_string_lossy().into_owned(),
        "--progress-template".to_string(),
        PROGRESS_TEMPLATE.to_string(),
        "--print".to_string(),
        format!("after_move:{FINAL_PATH_MARKER}%(filepath)s"),
    ];

    match format {
        FormatSelection::AudioOnlyMp3 => {
            args.push("-x".to_string());
            args.push("--audio-format".to_string());
            args.push("mp3".to_string());
        }
        FormatSelection::AudioOnlyBest => {
            args.push("-x".to_string());
        }
        _ => {
            if let Some(selector) = format.selector() {
                args.push("-f".to_string());
                args.push(selector.to_string());
            }
        }
    }

    if options.embed_thumbnail {
        args.push("--embed-thumbnail".to_string());
    }
    if options.embed_metadata {
        args.push("--embed-metadata".to_string());
    }
    if let Some(subs) = &options.subtitles {
        args.push("--write-subs".to_string());
        if subs.auto_subs {
            args.push("--write-auto-subs".to_string());
        }
        if !subs.languages.is_empty() {
            args.push("--sub-langs".to_string());
            args.push(subs.languages.join(","));
        }
        args.push("--embed-subs".to_string());
    }
    if options.sponsorblock_remove {
        args.push("--sponsorblock-remove".to_string());
        args.push("all".to_string());
    }
    if let Some(rate) = options.rate_limit_kbps {
        args.push("--limit-rate".to_string());
        args.push(format!("{rate}K"));
    }
    if let Some(n) = options.concurrent_fragments {
        args.push("--concurrent-fragments".to_string());
        args.push(n.to_string());
    }
    if let Some(cookies) = &options.cookies {
        match cookies {
            CookieSource::Browser { browser, profile } => {
                let spec = match profile {
                    Some(p) => format!("{browser}:{p}"),
                    None => browser.clone(),
                };
                args.push("--cookies-from-browser".to_string());
                args.push(spec);
            }
            CookieSource::File(path) => {
                args.push("--cookies".to_string());
                args.push(path.to_string_lossy().into_owned());
            }
        }
    }

    args.push("-o".to_string());
    args.push(options.output_template.clone());
    args.push(url.to_string());
    args
}

/// Same as [`build_download_args`], but with any cookie value replaced by
/// `<redacted>` - for the copy-command preview and anything else shown on
/// screen or written to a log.
pub fn build_download_args_redacted(
    url: &str,
    format: &FormatSelection,
    ffmpeg_dir: &Path,
    options: &DownloadOptions,
) -> Vec<String> {
    if options.cookies.is_none() {
        return build_download_args(url, format, ffmpeg_dir, options);
    }
    let mut redacted_options = options.clone();
    let cookie_marker = redacted_options.cookies.take();
    let mut args = build_download_args(url, format, ffmpeg_dir, &redacted_options);
    if let Some(cookies) = cookie_marker {
        args.push("#".to_string());
        args.extend(cookies.redacted_args());
    }
    args
}

// --- Progress parsing ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStatus {
    Downloading,
    Finished,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DownloadProgress {
    pub percent: Option<f64>,
    /// Human-readable speed string as yt-dlp formats it (e.g. `"18.23MiB/s"`);
    /// `None` when yt-dlp reports it as unknown.
    pub speed: Option<String>,
    /// Human-readable ETA string (e.g. `"00:03"`); `None` when unknown.
    pub eta: Option<String>,
    pub status: DownloadStatus,
}

/// Parse one line of output from a yt-dlp invocation using [`PROGRESS_TEMPLATE`].
pub fn parse_progress_line(line: &str) -> Option<DownloadProgress> {
    let parts: Vec<&str> = line.trim().splitn(4, '|').collect();
    if parts.len() != 4 {
        return None;
    }

    let percent = parts[0].trim().trim_end_matches('%').parse::<f64>().ok();
    let speed = parts[1].trim();
    let eta = parts[2].trim();
    let status = match parts[3].trim() {
        "downloading" => DownloadStatus::Downloading,
        "finished" => DownloadStatus::Finished,
        _ => DownloadStatus::Other,
    };

    Some(DownloadProgress {
        percent,
        speed: (speed != "Unknown B/s" && !speed.is_empty()).then(|| speed.to_string()),
        eta: (eta != "Unknown" && eta != "NA" && !eta.is_empty()).then(|| eta.to_string()),
        status,
    })
}

/// Extract the final output path from a line of yt-dlp output, if it's the
/// `after_move` marker line added by [`build_download_args`].
pub fn parse_final_path_line(line: &str) -> Option<PathBuf> {
    line.trim()
        .strip_prefix(FINAL_PATH_MARKER)
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_video_metadata() {
        let json = r#"{
            "_type": "video",
            "title": "Big Buck Bunny",
            "uploader": "Blender",
            "duration": 635.0,
            "thumbnail": "https://example.com/thumb.jpg",
            "webpage_url": "https://example.com/watch?v=x",
            "formats": [
                {"format_id": "18", "ext": "mp4", "resolution": "640x360", "vcodec": "avc1.42001E", "acodec": "mp4a.40.2", "filesize": 12345},
                {"format_id": "sb1", "ext": "mhtml", "vcodec": "none", "acodec": "none"}
            ]
        }"#;
        let raw: RawMetadata = serde_json::from_str(json).unwrap();
        assert_eq!(raw.type_.as_deref(), Some("video"));
        assert_eq!(raw.formats.len(), 2);
    }

    #[test]
    fn format_info_detects_video_only_and_audio_only() {
        let video_only = FormatInfo {
            format_id: "137".to_string(),
            ext: Some("mp4".to_string()),
            resolution: Some("1920x1080".to_string()),
            fps: Some(30.0),
            vcodec: Some("avc1".to_string()),
            acodec: Some("none".to_string()),
            filesize_bytes: None,
            format_note: None,
        };
        assert!(video_only.has_video());
        assert!(!video_only.has_audio());

        let audio_only = FormatInfo {
            acodec: Some("mp4a.40.2".to_string()),
            vcodec: Some("none".to_string()),
            ..video_only.clone()
        };
        assert!(!audio_only.has_video());
        assert!(audio_only.has_audio());
    }

    #[test]
    fn format_selection_selectors_match_expectations() {
        assert_eq!(
            FormatSelection::Best.selector(),
            Some("bestvideo*+bestaudio/best")
        );
        assert!(FormatSelection::BestUpTo1080p
            .selector()
            .unwrap()
            .contains("1080"));
        assert!(FormatSelection::BestUpTo720p
            .selector()
            .unwrap()
            .contains("720"));
        assert_eq!(FormatSelection::AudioOnlyMp3.selector(), None);
        assert_eq!(
            FormatSelection::Custom("bv+ba".to_string()).selector(),
            Some("bv+ba")
        );
    }

    #[test]
    fn build_args_includes_ffmpeg_location_and_progress_template() {
        let args = build_download_args(
            "https://example.com/watch?v=x",
            &FormatSelection::Best,
            Path::new("/opt/mediakit/bin"),
            &DownloadOptions::default(),
        );
        assert!(args.contains(&"--ffmpeg-location".to_string()));
        assert!(args.contains(&"/opt/mediakit/bin".to_string()));
        assert!(args.contains(&"--progress-template".to_string()));
        assert!(args.contains(&PROGRESS_TEMPLATE.to_string()));
        assert_eq!(
            args.last(),
            Some(&"https://example.com/watch?v=x".to_string())
        );
    }

    #[test]
    fn build_args_audio_only_mp3_uses_extract_audio_flags() {
        let args = build_download_args(
            "url",
            &FormatSelection::AudioOnlyMp3,
            Path::new("/bin"),
            &DownloadOptions::default(),
        );
        assert!(args.contains(&"-x".to_string()));
        assert!(args.contains(&"--audio-format".to_string()));
        assert!(args.contains(&"mp3".to_string()));
        assert!(!args.contains(&"-f".to_string()));
    }

    #[test]
    fn build_args_includes_subtitle_and_sponsorblock_options() {
        let options = DownloadOptions {
            subtitles: Some(SubtitleOptions {
                languages: vec!["en".to_string(), "de".to_string()],
                auto_subs: true,
            }),
            sponsorblock_remove: true,
            rate_limit_kbps: Some(500),
            concurrent_fragments: Some(4),
            ..Default::default()
        };
        let args = build_download_args("url", &FormatSelection::Best, Path::new("/bin"), &options);
        assert!(args.contains(&"--write-auto-subs".to_string()));
        assert!(args.contains(&"en,de".to_string()));
        assert!(args.contains(&"--sponsorblock-remove".to_string()));
        assert!(args.contains(&"--limit-rate".to_string()));
        assert!(args.contains(&"500K".to_string()));
        assert!(args.contains(&"--concurrent-fragments".to_string()));
        assert!(args.contains(&"4".to_string()));
    }

    #[test]
    fn cookie_args_are_redacted_in_preview_but_present_in_real_args() {
        let options = DownloadOptions {
            cookies: Some(CookieSource::Browser {
                browser: "firefox".to_string(),
                profile: Some("default-release".to_string()),
            }),
            ..Default::default()
        };
        let real = build_download_args("url", &FormatSelection::Best, Path::new("/bin"), &options);
        assert!(real.contains(&"firefox:default-release".to_string()));

        let redacted = build_download_args_redacted(
            "url",
            &FormatSelection::Best,
            Path::new("/bin"),
            &options,
        );
        assert!(!redacted.contains(&"firefox:default-release".to_string()));
        assert!(redacted.contains(&"<redacted>".to_string()));
    }

    #[test]
    fn parses_real_progress_lines() {
        let p = parse_progress_line(" 35.1%|  18.23MiB/s|00:00|downloading").unwrap();
        assert_eq!(p.percent, Some(35.1));
        assert_eq!(p.speed.as_deref(), Some("18.23MiB/s"));
        assert_eq!(p.eta.as_deref(), Some("00:00"));
        assert_eq!(p.status, DownloadStatus::Downloading);

        let unknown = parse_progress_line(" 35.1%| Unknown B/s|Unknown|downloading").unwrap();
        assert_eq!(unknown.speed, None);
        assert_eq!(unknown.eta, None);

        let finished = parse_progress_line("100.0%|15.74MiB/s|NA|finished").unwrap();
        assert_eq!(finished.status, DownloadStatus::Finished);
        assert_eq!(finished.eta, None);
    }

    #[test]
    fn parse_progress_line_rejects_malformed_input() {
        assert!(parse_progress_line("not a progress line").is_none());
        assert!(parse_progress_line("").is_none());
    }

    #[test]
    fn build_args_includes_final_path_print_marker() {
        let args = build_download_args(
            "url",
            &FormatSelection::Best,
            Path::new("/bin"),
            &DownloadOptions::default(),
        );
        assert!(args.contains(&"--print".to_string()));
        assert!(args
            .iter()
            .any(|a| a.starts_with("after_move:MEDIAKIT_FINAL_PATH:")));
    }

    #[test]
    fn parses_final_path_marker_line() {
        let path = parse_final_path_line("MEDIAKIT_FINAL_PATH:/tmp/video.mp4").unwrap();
        assert_eq!(path, PathBuf::from("/tmp/video.mp4"));

        assert!(parse_final_path_line("some other yt-dlp log line").is_none());
    }
}
