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
    extra_args: &[String],
) -> CoreResult<Metadata> {
    let mut cmd = Command::new(ytdlp);
    cmd.args(["-J", "--flat-playlist", "--no-warnings"]);
    if let Some(cookies) = cookies {
        cookies.apply(&mut cmd);
    }
    cmd.args(extra_args);
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

// --- Error classification ------------------------------------------------

/// What went wrong, as far as yt-dlp's stderr lets us tell. Lets the UI show
/// one plain sentence and a button that actually fixes (or retries) the
/// problem instead of dumping a traceback on the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YtdlpErrorKind {
    /// "Sign in to confirm you're not a bot" - YouTube wants a logged-in
    /// session or a PO token.
    BotCheck,
    /// HTTP 403 from the media server (stale extractor, expired URL, ...).
    Http403,
    /// No JS runtime available, or the n-challenge / signature solve failed.
    JsRuntime,
    /// Age-restricted or members/login-only content.
    LoginRequired,
    /// Private, deleted or otherwise unavailable video.
    Unavailable,
    UnsupportedUrl,
    /// The site is refusing requests from this IP/region (TikTok says so
    /// outright: "Your IP address is blocked from accessing this post").
    IpBlocked,
    /// A TikTok photo/slideshow post, which yt-dlp can't download as video.
    TikTokPhoto,
    Network,
    Unknown,
}

/// Classify yt-dlp's stderr (or an error string embedding it). Order
/// matters: the more specific signatures are checked first, because e.g. a
/// bot-check message also contains words found in the login-required one.
pub fn classify_ytdlp_error(stderr: &str) -> YtdlpErrorKind {
    let s = stderr.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| s.contains(n));

    if has(&[
        "confirm you\u{2019}re not a bot",
        "confirm you're not a bot",
    ]) {
        YtdlpErrorKind::BotCheck
    } else if has(&[
        "n challenge",
        "n-challenge",
        "nsig extraction failed",
        "signature extraction failed",
        "javascript runtime",
        "js runtime",
        "no supported javascript",
        "only images are available",
    ]) || (s.contains("[youtube]") && s.contains("requested format is not available"))
    {
        // "Requested format is not available" on YouTube is almost always
        // the signature/n-challenge step failing (so only storyboards are
        // left); on other sites it's a real format-selection problem.
        YtdlpErrorKind::JsRuntime
    } else if has(&["http error 403", "403: forbidden", "403 forbidden"]) {
        YtdlpErrorKind::Http403
    } else if has(&[
        "age-restricted",
        "age restricted",
        "confirm your age",
        "sign in to confirm your age",
        "login required",
        "this video is only available for registered users",
        "members-only",
        "join this channel",
    ]) {
        YtdlpErrorKind::LoginRequired
    } else if has(&[
        "private video",
        "video unavailable",
        "this video has been removed",
        "video is not available",
        "has been terminated",
        "this video is no longer available",
        "video does not exist",
        "playlist does not exist",
        "channel does not exist",
    ]) {
        YtdlpErrorKind::Unavailable
    } else if has(&["ip address is blocked", "your ip is blocked"]) {
        YtdlpErrorKind::IpBlocked
    } else if s.contains("unsupported url") && s.contains("tiktok.com/") && s.contains("/photo/") {
        YtdlpErrorKind::TikTokPhoto
    } else if s.contains("unsupported url: https://www.tiktok.com/?")
        || s.trim_end()
            .ends_with("unsupported url: https://www.tiktok.com/")
    {
        // A dead vm./vt.tiktok.com share link redirects to the home page.
        YtdlpErrorKind::Unavailable
    } else if has(&["unsupported url", "is not a valid url"]) {
        YtdlpErrorKind::UnsupportedUrl
    } else if has(&[
        // Not a bare "unable to download": yt-dlp prefixes HTTP 404/429/5xx
        // and plenty of non-network failures with it too.
        "urlopen error",
        "name or service not known",
        "temporary failure in name resolution",
        "connection reset",
        "connection refused",
        "timed out",
        "network is unreachable",
        "getaddrinfo failed",
        "failed to resolve",
        "transporterror",
        "remote end closed connection",
        "connection aborted",
    ]) {
        YtdlpErrorKind::Network
    } else {
        YtdlpErrorKind::Unknown
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

/// Download only part of a video (yt-dlp `--download-sections`). Times
/// are seconds from the start; `end_seconds: None` means "to the end".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipSection {
    pub start_seconds: f64,
    pub end_seconds: Option<f64>,
}

impl ClipSection {
    /// The `--download-sections` value, e.g. `*83.5-105` or `*83.5-inf`.
    pub fn spec(&self) -> String {
        let fmt = |t: f64| {
            let s = format!("{t:.3}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        };
        let end = self
            .end_seconds
            .map(fmt)
            .unwrap_or_else(|| "inf".to_string());
        format!("*{}-{end}", fmt(self.start_seconds))
    }

    /// Short human label used in the file name, e.g. `1m23s-1m45s`.
    pub fn label(&self) -> String {
        let fmt = |t: f64| {
            let t = t.round() as u64;
            let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
            if h > 0 {
                format!("{h}h{m:02}m{s:02}s")
            } else if m > 0 {
                format!("{m}m{s:02}s")
            } else {
                format!("{s}s")
            }
        };
        match self.end_seconds {
            Some(end) => format!("{}-{}", fmt(self.start_seconds), fmt(end)),
            None => format!("{}-end", fmt(self.start_seconds)),
        }
    }
}

/// Parse a user-typed timestamp: `90`, `90.5`, `1:30`, `01:02:03.5`.
/// Empty input is `None`; so is anything malformed or negative.
pub fn parse_timestamp(text: &str) -> Option<f64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let parts: Vec<&str> = text.split(':').collect();
    if parts.len() > 3 {
        return None;
    }
    let mut total = 0.0;
    for (i, part) in parts.iter().enumerate() {
        let last = i == parts.len() - 1;
        let value: f64 = if last {
            part.parse().ok()?
        } else {
            part.parse::<u32>().ok()? as f64
        };
        if !value.is_finite() || value < 0.0 || (i > 0 && value >= 60.0) {
            return None;
        }
        total = total * 60.0 + value;
    }
    Some(total)
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
    /// Only download this part of the video. Cuts are made exact with
    /// `--force-keyframes-at-cuts` (re-encodes around the cut points).
    pub section: Option<ClipSection>,
    /// yt-dlp output filename template, e.g. `"%(title)s.%(ext)s"`.
    pub output_template: String,
    /// Extra yt-dlp arguments MediaKit itself injects rather than the user
    /// (currently: [`crate::pot_provider::youtube_extra_args`] for YouTube
    /// URLs). Inserted right after the fixed base args in
    /// [`build_download_args`], before format/quality flags, so it never
    /// disturbs the merge/format logic there.
    pub extra_args: Vec<String>,
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
            section: None,
            output_template: "%(title)s.%(ext)s".to_string(),
            extra_args: Vec::new(),
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
    args.extend(options.extra_args.iter().cloned());

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

    let mut output_template = options.output_template.clone();
    if let Some(section) = &options.section {
        args.push("--download-sections".to_string());
        args.push(section.spec());
        args.push("--force-keyframes-at-cuts".to_string());
        // Tag the file name so a clip never overwrites the full video
        // (or another clip of it).
        let tag = format!(" (clip {})", section.label());
        output_template = match output_template.strip_suffix(".%(ext)s") {
            Some(stem) => format!("{stem}{tag}.%(ext)s"),
            None => format!("{output_template}{tag}"),
        };
    }

    args.push("-o".to_string());
    args.push(output_template);
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
    fn parses_timestamps() {
        assert_eq!(parse_timestamp("90"), Some(90.0));
        assert_eq!(parse_timestamp(" 1:30 "), Some(90.0));
        assert_eq!(parse_timestamp("1:02:03.5"), Some(3723.5));
        assert_eq!(parse_timestamp("0:05.25"), Some(5.25));
        for bad in [
            "", "abc", "1:60", "-5", "1::2", "1:2:3:4", "1.5:00", "nan", "inf",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad}");
        }
    }

    #[test]
    fn clip_section_spec_and_label() {
        let clip = ClipSection {
            start_seconds: 83.5,
            end_seconds: Some(105.0),
        };
        assert_eq!(clip.spec(), "*83.5-105");
        assert_eq!(clip.label(), "1m24s-1m45s");
        let open = ClipSection {
            start_seconds: 3725.0,
            end_seconds: None,
        };
        assert_eq!(open.spec(), "*3725-inf");
        assert_eq!(open.label(), "1h02m05s-end");
    }

    #[test]
    fn clip_section_adds_flags_and_tags_the_filename() {
        let options = DownloadOptions {
            section: Some(ClipSection {
                start_seconds: 10.0,
                end_seconds: Some(20.0),
            }),
            output_template: "/out/%(title)s.%(ext)s".into(),
            ..Default::default()
        };
        let args = build_download_args("url", &FormatSelection::Best, Path::new("/bin"), &options);
        let i = args
            .iter()
            .position(|a| a == "--download-sections")
            .unwrap();
        assert_eq!(args[i + 1], "*10-20");
        assert!(args.contains(&"--force-keyframes-at-cuts".to_string()));
        let o = args.iter().position(|a| a == "-o").unwrap();
        assert_eq!(args[o + 1], "/out/%(title)s (clip 10s-20s).%(ext)s");
        assert_eq!(args.last().unwrap(), "url");

        let plain = build_download_args(
            "url",
            &FormatSelection::Best,
            Path::new("/bin"),
            &DownloadOptions::default(),
        );
        assert!(!plain.contains(&"--download-sections".to_string()));
    }

    #[test]
    fn classifies_real_looking_ytdlp_errors() {
        use YtdlpErrorKind::*;
        let cases = [
            (
                "ERROR: [youtube] dQw4w9WgXcQ: Sign in to confirm you\u{2019}re not a bot. Use --cookies-from-browser",
                BotCheck,
            ),
            (
                "WARNING: [youtube] n challenge solving failed: Some formats may be missing. Ensure you have a supported JavaScript runtime",
                JsRuntime,
            ),
            (
                "ERROR: unable to download video data: HTTP Error 403: Forbidden",
                Http403,
            ),
            (
                "ERROR: [youtube] abc: Sign in to confirm your age. This video may be inappropriate for some users.",
                LoginRequired,
            ),
            ("ERROR: [youtube] abc: Private video. Sign in if you've been granted access", Unavailable),
            ("ERROR: [youtube] abc: Video unavailable. This video has been removed by the uploader", Unavailable),
            ("ERROR: Unsupported URL: https://example.com/foo", UnsupportedUrl),
            (
                "ERROR: [generic] Unable to download webpage: <urlopen error [Errno -2] Name or service not known>",
                Network,
            ),
            (
                "ERROR: [youtube] abc: Requested format is not available. Use --list-formats for a list of available formats",
                JsRuntime,
            ),
            (
                "ERROR: [vimeo] 123: Requested format is not available. Use --list-formats",
                Unknown,
            ),
            // A local path problem is not an unavailable video...
            ("ERROR: cookies file /home/me/cookies.txt does not exist", Unknown),
            // ...and an HTTP 404 is not a network failure.
            ("ERROR: Unable to download webpage: HTTP Error 404: Not Found", Unknown),
            (
                "ERROR: Unable to download webpage: <urlopen error [Errno 110] Connection timed out>",
                Network,
            ),
            (
                "ERROR: [youtube] abc: Unable to download API page: HTTP Error 403: Forbidden",
                Http403,
            ),
            (
                "ERROR: [TikTok] 7300000000000000000: Your IP address is blocked from accessing this post",
                IpBlocked,
            ),
            (
                "ERROR: Unsupported URL: https://www.tiktok.com/@someone/photo/7300000000000000000",
                TikTokPhoto,
            ),
            (
                "WARNING: [generic] Falling back on generic information extractor\nERROR: Unsupported URL: https://www.tiktok.com/?_r=1",
                Unavailable,
            ),
            ("something entirely unexpected", Unknown),
            ("", Unknown),
        ];
        for (stderr, expected) in cases {
            assert_eq!(classify_ytdlp_error(stderr), expected, "{stderr}");
        }
    }

    #[test]
    fn extra_args_are_inserted_before_format_flags() {
        let options = DownloadOptions {
            extra_args: vec!["--js-runtimes".to_string(), "node".to_string()],
            ..Default::default()
        };
        let args = build_download_args("url", &FormatSelection::Best, Path::new("/bin"), &options);
        let extra_idx = args.iter().position(|a| a == "--js-runtimes").unwrap();
        let format_idx = args.iter().position(|a| a == "-f").unwrap();
        assert!(extra_idx < format_idx);
        assert_eq!(args[extra_idx + 1], "node");
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
