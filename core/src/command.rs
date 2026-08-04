//! Strongly-typed encode settings and the ffmpeg argument builder.
//!
//! [`build_args`] is the single source of truth for the exact command that
//! will run — it backs both the actual subprocess invocation and the "copy
//! command" live preview in the GUI, so the two can never drift apart.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Container {
    #[default]
    Mp4,
    WebM,
    Mkv,
    Gif,
    WebP,
    Apng,
    Mp3,
    Opus,
    Flac,
    Wav,
    Png,
    Jpg,
    Bmp,
    Ico,
    Avif,
}

impl Container {
    pub fn extension(self) -> &'static str {
        match self {
            Container::Mp4 => "mp4",
            Container::WebM => "webm",
            Container::Mkv => "mkv",
            Container::Gif => "gif",
            Container::WebP => "webp",
            Container::Apng => "png",
            Container::Mp3 => "mp3",
            Container::Opus => "opus",
            Container::Flac => "flac",
            Container::Wav => "wav",
            Container::Png => "png",
            Container::Jpg => "jpg",
            Container::Bmp => "bmp",
            Container::Ico => "ico",
            Container::Avif => "avif",
        }
    }

    /// Only containers that can be a two-pass null-output target (video
    /// containers with an unambiguous muxer name) need an explicit `-f`;
    /// everything else is left to ffmpeg's extension-based auto-detection.
    fn muxer_name(self) -> Option<&'static str> {
        match self {
            Container::Mp4 => Some("mp4"),
            Container::WebM => Some("webm"),
            Container::Mkv => Some("matroska"),
            Container::Apng => Some("apng"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
    Vp9,
    Av1,
    /// Stream copy (`-c:v copy`): no re-encode, used by mute/strip-metadata.
    Copy,
    Gif,
    Apng,
    Webp,
    Png,
    Mjpeg,
    Bmp,
    Avif,
}

impl VideoCodec {
    fn encoder_name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "libx264",
            VideoCodec::H265 => "libx265",
            VideoCodec::Vp9 => "libvpx-vp9",
            VideoCodec::Av1 => "libaom-av1",
            VideoCodec::Copy => "copy",
            VideoCodec::Gif => "gif",
            VideoCodec::Apng => "apng",
            VideoCodec::Webp => "libwebp",
            VideoCodec::Png => "png",
            VideoCodec::Mjpeg => "mjpeg",
            VideoCodec::Bmp => "bmp",
            VideoCodec::Avif => "libaom-av1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    Opus,
    Mp3,
    Flac,
    Pcm,
    Copy,
}

impl AudioCodec {
    fn encoder_name(self) -> &'static str {
        match self {
            AudioCodec::Aac => "aac",
            AudioCodec::Opus => "libopus",
            AudioCodec::Mp3 => "libmp3lame",
            AudioCodec::Flac => "flac",
            AudioCodec::Pcm => "pcm_s16le",
            AudioCodec::Copy => "copy",
        }
    }
}

/// Which pass of a (potentially two-pass) encode this invocation is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodePass {
    /// A normal, single-pass encode (CRF mode, stream copy, etc).
    Single,
    /// First pass of a two-pass bitrate-targeted encode: no audio, output
    /// discarded to the platform's null device.
    First { passlog_prefix: PathBuf },
    /// Second pass of a two-pass bitrate-targeted encode: full output.
    Second { passlog_prefix: PathBuf },
}

#[derive(Debug, Clone)]
pub struct VideoSettings {
    pub codec: VideoCodec,
    /// Target bitrate for bitrate-controlled (typically two-pass) encodes.
    pub bitrate_kbps: Option<u64>,
    /// Constant-quality mode; mutually exclusive with `bitrate_kbps` in
    /// practice, but nothing stops both being set for advanced users.
    pub crf: Option<u32>,
    pub preset: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    pub pixel_format: Option<String>,
    /// Overrides `codec`'s default software encoder name (e.g. "libx264")
    /// with a hardware encoder (e.g. "h264_nvenc"), while `codec` itself
    /// keeps describing the codec *family* for container/pixel-format
    /// purposes. `None` means software encode, the default for reliability.
    pub hardware_encoder_override: Option<String>,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            codec: VideoCodec::H264,
            bitrate_kbps: None,
            crf: Some(23),
            preset: Some("medium".to_string()),
            width: None,
            height: None,
            fps: None,
            pixel_format: None,
            hardware_encoder_override: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AudioSettings {
    pub codec: AudioCodec,
    pub bitrate_kbps: Option<u64>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u32>,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            codec: AudioCodec::Aac,
            bitrate_kbps: Some(128),
            sample_rate_hz: None,
            channels: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Trim {
    pub start_seconds: Option<f64>,
    pub end_seconds: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct EncodeSettings {
    pub input: PathBuf,
    pub output: PathBuf,
    pub container: Container,
    /// `None` means no video stream in the output (`-vn`).
    pub video: Option<VideoSettings>,
    /// `None` means no audio stream in the output (`-an`).
    pub audio: Option<AudioSettings>,
    pub trim: Trim,
    pub overwrite: bool,
    /// `-framerate` input option, for image-sequence inputs like
    /// `frame%04d.png` (Images -> GIF/APNG/WebP presets).
    pub input_framerate: Option<f64>,
    /// `-loop 1` input option: treat a single still image as an infinitely
    /// repeating input stream, cut down to a fixed length via
    /// `input_duration_limit_seconds` (Image -> GIF preset). No-op for real
    /// video/multi-frame inputs.
    pub loop_input: bool,
    /// `-t` *input* option (before `-i`, not the output-side `-to`/`-t`
    /// `trim` uses): how many seconds to read from an infinitely-looping
    /// `loop_input` source. Bounding an infinite input at the demuxer level
    /// like this is the reliable way to do it - an output-side `-to` on a
    /// `-loop 1` source left some real ffmpeg builds building a palette
    /// (`split`+`palettegen`+`paletteuse`) over the input indefinitely
    /// instead of stopping, observed hanging against a real static ffmpeg
    /// build during testing.
    pub input_duration_limit_seconds: Option<f64>,
    /// Extra `-vf` filtergraph clauses, comma-joined with the width/height
    /// scale filter (if any) into a single `-vf` argument. Used by presets
    /// needing custom filters: palette-based GIFs, rotate/flip, reverse,
    /// speed change.
    pub video_filters: Vec<String>,
    /// Extra `-af` filtergraph clauses, comma-joined into a single `-af`
    /// argument. Used by reverse (`areverse`) and speed change (`atempo`).
    pub audio_filters: Vec<String>,
    /// Loop the animated output forever (`-loop 0` for gif/webp, `-plays 0`
    /// for apng). No-op for non-animated containers.
    pub loop_forever: bool,
    /// Appended verbatim at the end of the argument list; the manual escape
    /// hatch for the "Advanced" panel's raw ffmpeg args field.
    pub extra_args: Vec<String>,
}

fn null_output() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

/// Build the full ffmpeg argument list (everything after the binary path)
/// for one pass of an encode. This is the single source of truth used both
/// to actually run ffmpeg and to render the "copy command" preview.
pub fn build_args(settings: &EncodeSettings, pass: &EncodePass) -> Vec<String> {
    let mut args = Vec::new();

    if settings.overwrite {
        args.push("-y".to_string());
    } else {
        args.push("-n".to_string());
    }

    if let Some(start) = settings.trim.start_seconds {
        args.push("-ss".to_string());
        args.push(format!("{start}"));
    }

    if let Some(framerate) = settings.input_framerate {
        args.push("-framerate".to_string());
        args.push(format!("{framerate}"));
    }

    if settings.loop_input {
        args.push("-loop".to_string());
        args.push("1".to_string());
    }

    if let Some(limit) = settings.input_duration_limit_seconds {
        args.push("-t".to_string());
        args.push(format!("{limit}"));
    }

    args.push("-i".to_string());
    args.push(settings.input.to_string_lossy().into_owned());

    if let Some(end) = settings.trim.end_seconds {
        args.push("-to".to_string());
        args.push(format!("{end}"));
    }

    let is_first_pass = matches!(pass, EncodePass::First { .. });

    match &settings.video {
        None => args.push("-vn".to_string()),
        Some(video) => {
            args.push("-c:v".to_string());
            args.push(
                video
                    .hardware_encoder_override
                    .clone()
                    .unwrap_or_else(|| video.codec.encoder_name().to_string()),
            );

            if let Some(bitrate) = video.bitrate_kbps {
                args.push("-b:v".to_string());
                args.push(format!("{bitrate}k"));
            } else if let Some(crf) = video.crf {
                args.push("-crf".to_string());
                args.push(crf.to_string());
            }

            if let Some(preset) = &video.preset {
                args.push("-preset".to_string());
                args.push(preset.clone());
            }

            if let Some(fps) = video.fps {
                args.push("-r".to_string());
                args.push(format!("{fps}"));
            }

            let mut video_filter_parts = Vec::new();
            if let (Some(w), Some(h)) = (video.width, video.height) {
                video_filter_parts.push(format!("scale={w}:{h}"));
            }
            video_filter_parts.extend(settings.video_filters.iter().cloned());
            if !video_filter_parts.is_empty() {
                args.push("-vf".to_string());
                args.push(video_filter_parts.join(","));
            }

            if let Some(pix_fmt) = &video.pixel_format {
                args.push("-pix_fmt".to_string());
                args.push(pix_fmt.clone());
            }
        }
    }

    match pass {
        EncodePass::Single => {}
        EncodePass::First { passlog_prefix } | EncodePass::Second { passlog_prefix } => {
            let pass_num = if is_first_pass { "1" } else { "2" };
            args.push("-pass".to_string());
            args.push(pass_num.to_string());
            args.push("-passlogfile".to_string());
            args.push(passlog_prefix.to_string_lossy().into_owned());
        }
    }

    if is_first_pass {
        // First pass never needs (or should emit) an audio stream.
        args.push("-an".to_string());
    } else {
        match &settings.audio {
            None => args.push("-an".to_string()),
            Some(audio) => {
                args.push("-c:a".to_string());
                args.push(audio.codec.encoder_name().to_string());

                if audio.codec != AudioCodec::Copy {
                    if let Some(bitrate) = audio.bitrate_kbps {
                        args.push("-b:a".to_string());
                        args.push(format!("{bitrate}k"));
                    }
                    if let Some(sample_rate) = audio.sample_rate_hz {
                        args.push("-ar".to_string());
                        args.push(sample_rate.to_string());
                    }
                    if let Some(channels) = audio.channels {
                        args.push("-ac".to_string());
                        args.push(channels.to_string());
                    }
                }
            }
        }

        if !settings.audio_filters.is_empty() {
            args.push("-af".to_string());
            args.push(settings.audio_filters.join(","));
        }
    }

    if settings.loop_forever && !is_first_pass {
        match settings.container {
            Container::Gif | Container::WebP => {
                args.push("-loop".to_string());
                args.push("0".to_string());
            }
            Container::Apng => {
                args.push("-plays".to_string());
                args.push("0".to_string());
            }
            _ => {}
        }
    }

    if let Some(muxer) = settings.container.muxer_name() {
        args.push("-f".to_string());
        args.push(muxer.to_string());
    }

    args.extend(settings.extra_args.iter().cloned());

    if is_first_pass {
        args.push(null_output().to_string());
    } else {
        args.push(settings.output.to_string_lossy().into_owned());
    }

    args
}

/// Render a command's args as a shell-quoted, copy-pastable string for the
/// "copy command" button, prefixed with the ffmpeg binary itself.
pub fn preview_command_string(ffmpeg_bin: &Path, args: &[String]) -> String {
    let mut parts = vec![shell_quote(&ffmpeg_bin.to_string_lossy())];
    parts.extend(args.iter().map(|a| shell_quote(a)));
    parts.join(" ")
}

fn shell_quote(s: &str) -> String {
    let needs_quoting = s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || "\"'\\$`|&;<>()[]{}*?!~".contains(c));
    if !needs_quoting {
        return s.to_string();
    }
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_settings() -> EncodeSettings {
        EncodeSettings {
            input: PathBuf::from("input.mp4"),
            output: PathBuf::from("output.mp4"),
            container: Container::Mp4,
            video: Some(VideoSettings::default()),
            audio: Some(AudioSettings::default()),
            trim: Trim::default(),
            overwrite: true,
            ..Default::default()
        }
    }

    #[test]
    fn single_pass_crf_encode_has_expected_flags() {
        let settings = base_settings();
        let args = build_args(&settings, &EncodePass::Single);

        assert!(args.contains(&"-y".to_string()));
        assert!(args.contains(&"-i".to_string()));
        assert!(args.contains(&"input.mp4".to_string()));
        assert!(args.contains(&"-c:v".to_string()));
        assert!(args.contains(&"libx264".to_string()));
        assert!(args.contains(&"-crf".to_string()));
        assert!(args.contains(&"23".to_string()));
        assert!(args.contains(&"-c:a".to_string()));
        assert!(args.contains(&"aac".to_string()));
        assert_eq!(args.last(), Some(&"output.mp4".to_string()));
        // CRF mode must not emit a -pass flag.
        assert!(!args.contains(&"-pass".to_string()));
    }

    #[test]
    fn two_pass_first_pass_has_no_audio_and_targets_null_output() {
        let mut settings = base_settings();
        settings.video = Some(VideoSettings {
            bitrate_kbps: Some(2500),
            crf: None,
            ..Default::default()
        });
        let pass = EncodePass::First {
            passlog_prefix: PathBuf::from("/tmp/mediakit_pass"),
        };
        let args = build_args(&settings, &pass);

        assert!(args.contains(&"-an".to_string()));
        assert!(args.contains(&"-pass".to_string()));
        let pass_idx = args.iter().position(|a| a == "-pass").unwrap();
        assert_eq!(args[pass_idx + 1], "1");
        assert!(args.contains(&"-b:v".to_string()));
        assert!(args.contains(&"2500k".to_string()));
        assert_eq!(args.last(), Some(&null_output().to_string()));
        // No -crf when a bitrate is set.
        assert!(!args.contains(&"-crf".to_string()));
    }

    #[test]
    fn two_pass_second_pass_includes_audio_and_real_output() {
        let mut settings = base_settings();
        settings.video = Some(VideoSettings {
            bitrate_kbps: Some(2500),
            crf: None,
            ..Default::default()
        });
        let pass = EncodePass::Second {
            passlog_prefix: PathBuf::from("/tmp/mediakit_pass"),
        };
        let args = build_args(&settings, &pass);

        assert!(args.contains(&"-c:a".to_string()));
        let pass_idx = args.iter().position(|a| a == "-pass").unwrap();
        assert_eq!(args[pass_idx + 1], "2");
        assert_eq!(args.last(), Some(&"output.mp4".to_string()));
    }

    #[test]
    fn mute_sets_no_audio_flag() {
        let mut settings = base_settings();
        settings.audio = None;
        let args = build_args(&settings, &EncodePass::Single);
        assert!(args.contains(&"-an".to_string()));
        assert!(!args.contains(&"-c:a".to_string()));
    }

    #[test]
    fn no_video_sets_vn_flag() {
        let mut settings = base_settings();
        settings.video = None;
        let args = build_args(&settings, &EncodePass::Single);
        assert!(args.contains(&"-vn".to_string()));
    }

    #[test]
    fn trim_adds_seek_and_end_flags() {
        let mut settings = base_settings();
        settings.trim = Trim {
            start_seconds: Some(1.5),
            end_seconds: Some(10.0),
        };
        let args = build_args(&settings, &EncodePass::Single);
        assert!(args.contains(&"-ss".to_string()));
        assert!(args.contains(&"1.5".to_string()));
        assert!(args.contains(&"-to".to_string()));
        assert!(args.contains(&"10".to_string()));
    }

    #[test]
    fn loop_input_emits_loop_flag_before_the_input() {
        let mut settings = base_settings();
        settings.loop_input = true;
        let args = build_args(&settings, &EncodePass::Single);

        let loop_idx = args.iter().position(|a| a == "-loop").unwrap();
        assert_eq!(args[loop_idx + 1], "1");
        let input_idx = args.iter().position(|a| a == "-i").unwrap();
        assert!(loop_idx < input_idx);
    }

    #[test]
    fn input_duration_limit_emits_t_flag_before_the_input() {
        let mut settings = base_settings();
        settings.loop_input = true;
        settings.input_duration_limit_seconds = Some(2.0);
        let args = build_args(&settings, &EncodePass::Single);

        let input_idx = args.iter().position(|a| a == "-i").unwrap();
        // The input-side `-t` (bounding an infinite `-loop 1` source at the
        // demuxer level) must come before `-i`, not after it - that's the
        // whole point versus the output-side `-to` `trim` uses.
        let t_idx = args
            .iter()
            .enumerate()
            .take(input_idx)
            .position(|(_, a)| a == "-t")
            .unwrap();
        assert_eq!(args[t_idx + 1], "2");
        assert!(t_idx < input_idx);
    }

    #[test]
    fn extra_args_are_appended_before_output() {
        let mut settings = base_settings();
        settings.extra_args = vec!["-map_metadata".to_string(), "-1".to_string()];
        let args = build_args(&settings, &EncodePass::Single);
        let extra_idx = args.iter().position(|a| a == "-map_metadata").unwrap();
        let output_idx = args.iter().position(|a| a == "output.mp4").unwrap();
        assert!(extra_idx < output_idx);
    }

    #[test]
    fn preview_command_quotes_paths_with_spaces() {
        let mut settings = base_settings();
        settings.input = PathBuf::from("/tmp/my video.mp4");
        let args = build_args(&settings, &EncodePass::Single);
        let preview = preview_command_string(Path::new("/usr/bin/ffmpeg"), &args);
        assert!(preview.contains("\"/tmp/my video.mp4\""));
        assert!(preview.starts_with("/usr/bin/ffmpeg"));
    }

    #[test]
    fn resolution_emits_scale_filter() {
        let mut settings = base_settings();
        settings.video = Some(VideoSettings {
            width: Some(1280),
            height: Some(720),
            ..Default::default()
        });
        let args = build_args(&settings, &EncodePass::Single);
        assert!(args.contains(&"scale=1280:720".to_string()));
    }
}
