//! One-click preset builders. Each function turns user-facing preset choices
//! into the strongly-typed [`EncodeSettings`] (and, for target-size presets,
//! a full [`JobSpec`]) that [`crate::command::build_args`] and
//! [`crate::engine`] already know how to run.

use crate::bitrate::{self, DEFAULT_MAX_RETRIES};
use crate::command::{
    AudioCodec, AudioSettings, Container, EncodePass, EncodeSettings, VideoCodec, VideoSettings,
};
use crate::job::{JobId, JobSpec, TargetSizePolicy};
use std::path::PathBuf;

fn copy_video() -> VideoSettings {
    VideoSettings {
        codec: VideoCodec::Copy,
        bitrate_kbps: None,
        crf: None,
        preset: None,
        width: None,
        height: None,
        fps: None,
        pixel_format: None,
        hardware_encoder_override: None,
    }
}

fn copy_audio() -> AudioSettings {
    AudioSettings {
        codec: AudioCodec::Copy,
        bitrate_kbps: None,
        sample_rate_hz: None,
        channels: None,
    }
}

fn reencode_video(crf: u32) -> VideoSettings {
    VideoSettings {
        codec: VideoCodec::H264,
        bitrate_kbps: None,
        crf: Some(crf),
        preset: Some("medium".to_string()),
        width: None,
        height: None,
        fps: None,
        pixel_format: None,
        hardware_encoder_override: None,
    }
}

// --- Target size (data-driven presets - see `crate::size_presets` - plus
// --- "Custom target size") -------------------------------------------------

#[derive(Debug, Clone)]
pub struct TargetSizeRequest {
    pub input: PathBuf,
    pub output: PathBuf,
    pub container: Container,
    pub video_codec: VideoCodec,
    pub target_bytes: u64,
    pub duration_seconds: f64,
    pub audio_bitrate_kbps: u64,
    /// Fraction of `target_bytes` actually targeted, leaving headroom for
    /// container/muxing overhead - see
    /// [`crate::size_presets::SizePresetsConfig::safety_margin_fraction`].
    /// Callers not going through a size preset (nothing in this codebase
    /// does, but `Default` needs *a* value) get
    /// [`bitrate::DEFAULT_SAFETY_MARGIN`].
    pub safety_margin: f64,
}

/// Build a two-pass, target-size job with automatic overshoot retry. Backs
/// every target-size preset and "Custom target size".
pub fn build_target_size_job(id: JobId, req: &TargetSizeRequest) -> JobSpec {
    let bitrate_result = bitrate::compute_target_bitrate(&bitrate::TargetSizeParams {
        target_bytes: req.target_bytes,
        duration_seconds: req.duration_seconds,
        audio_bitrate_kbps: req.audio_bitrate_kbps,
        safety_margin: req.safety_margin,
    });

    let passlog_prefix = req.output.with_extension("passlog");

    let settings = EncodeSettings {
        input: req.input.clone(),
        output: req.output.clone(),
        container: req.container,
        video: Some(VideoSettings {
            codec: req.video_codec,
            bitrate_kbps: Some(bitrate_result.video_bitrate_kbps),
            crf: None,
            preset: Some("medium".to_string()),
            ..Default::default()
        }),
        audio: Some(AudioSettings {
            codec: AudioCodec::Aac,
            bitrate_kbps: Some(req.audio_bitrate_kbps),
            ..Default::default()
        }),
        overwrite: true,
        ..Default::default()
    };

    JobSpec {
        id,
        settings,
        passes: vec![
            EncodePass::First {
                passlog_prefix: passlog_prefix.clone(),
            },
            EncodePass::Second { passlog_prefix },
        ],
        total_duration_seconds: req.duration_seconds,
        target_size: Some(TargetSizePolicy {
            target_bytes: req.target_bytes,
            safety_margin: req.safety_margin,
            max_retries: DEFAULT_MAX_RETRIES,
        }),
    }
}

/// `true` if the initial bitrate computation hit the sane-minimum floor,
/// meaning the caller should offer to downscale resolution/fps instead of
/// (or in addition to) just encoding at a barely-watchable bitrate.
pub fn target_size_should_suggest_downscale(req: &TargetSizeRequest) -> bool {
    bitrate::compute_target_bitrate(&bitrate::TargetSizeParams {
        target_bytes: req.target_bytes,
        duration_seconds: req.duration_seconds,
        audio_bitrate_kbps: req.audio_bitrate_kbps,
        safety_margin: req.safety_margin,
    })
    .hit_floor
}

// --- Video -> GIF (high quality) -------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct GifOptions {
    pub fps: u32,
    pub width: u32,
    pub dither: bool,
}

/// High-quality video -> GIF via the standard palettegen/paletteuse
/// technique, fused into one ffmpeg invocation with `split` so we don't need
/// a genuinely separate two-pass process (a temporary palette PNG plus a
/// second command) to get the same result.
pub fn video_to_gif(input: PathBuf, output: PathBuf, opts: GifOptions) -> EncodeSettings {
    let dither_mode = if opts.dither { "bayer" } else { "none" };
    let filter = format!(
        "fps={fps},scale={width}:-1:flags=lanczos,split[s0][s1];[s0]palettegen=stats_mode=diff[p];[s1][p]paletteuse=dither={dither_mode}",
        fps = opts.fps,
        width = opts.width,
    );

    EncodeSettings {
        input,
        output,
        container: Container::Gif,
        video: Some(VideoSettings {
            codec: VideoCodec::Gif,
            crf: None,
            preset: None,
            ..Default::default()
        }),
        audio: None,
        overwrite: true,
        video_filters: vec![filter],
        loop_forever: true,
        ..Default::default()
    }
}

// --- Single image -> GIF (a short, fixed-length loop of one picture) ------

#[derive(Debug, Clone, Copy)]
pub struct ImageToGifOptions {
    /// How long the resulting GIF plays for - the whole point of this
    /// preset is turning one static picture into a short loopable clip.
    pub duration_seconds: f64,
    pub fps: u32,
    pub width: u32,
}

impl Default for ImageToGifOptions {
    fn default() -> Self {
        Self {
            duration_seconds: 2.0,
            fps: 10,
            width: 480,
        }
    }
}

/// A single PNG/JPEG/etc turned into a short, looping GIF: `-loop 1` treats
/// the still image as an infinite input stream, which `trim.end_seconds`
/// then cuts down to `opts.duration_seconds`. Reuses the same
/// palettegen/paletteuse technique as [`video_to_gif`] for quality, even
/// though a single flat-color-count source frame makes less difference
/// there than for real video.
pub fn image_to_gif(input: PathBuf, output: PathBuf, opts: ImageToGifOptions) -> EncodeSettings {
    let filter = format!(
        "fps={fps},scale={width}:-1:flags=lanczos,split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse",
        fps = opts.fps,
        width = opts.width,
    );

    EncodeSettings {
        input,
        output,
        container: Container::Gif,
        video: Some(VideoSettings {
            codec: VideoCodec::Gif,
            crf: None,
            preset: None,
            ..Default::default()
        }),
        audio: None,
        loop_input: true,
        // An *input*-side duration limit, not `trim.end_seconds` (which
        // would emit an output-side `-to`) - bounding a `-loop 1` infinite
        // image source at the demuxer level is the reliable way to do
        // this; an output-side cutoff left some real ffmpeg builds trying
        // to build the palette (`split`+`palettegen`+`paletteuse`) over the
        // input indefinitely instead of stopping.
        input_duration_limit_seconds: Some(opts.duration_seconds),
        overwrite: true,
        video_filters: vec![filter],
        loop_forever: true,
        ..Default::default()
    }
}

// --- Images -> GIF / APNG / WebP -------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct ImageSequenceOptions {
    pub fps: u32,
    pub loop_forever: bool,
}

/// `input_pattern` is a printf-style ffmpeg image sequence pattern, e.g.
/// `frame%04d.png`.
pub fn images_to_gif(
    input_pattern: PathBuf,
    output: PathBuf,
    opts: ImageSequenceOptions,
) -> EncodeSettings {
    EncodeSettings {
        input: input_pattern,
        output,
        container: Container::Gif,
        video: Some(VideoSettings {
            codec: VideoCodec::Gif,
            crf: None,
            preset: None,
            ..Default::default()
        }),
        audio: None,
        overwrite: true,
        input_framerate: Some(opts.fps as f64),
        loop_forever: opts.loop_forever,
        ..Default::default()
    }
}

pub fn images_to_apng(
    input_pattern: PathBuf,
    output: PathBuf,
    opts: ImageSequenceOptions,
) -> EncodeSettings {
    EncodeSettings {
        input: input_pattern,
        output,
        container: Container::Apng,
        video: Some(VideoSettings {
            codec: VideoCodec::Apng,
            crf: None,
            preset: None,
            ..Default::default()
        }),
        audio: None,
        overwrite: true,
        input_framerate: Some(opts.fps as f64),
        loop_forever: opts.loop_forever,
        ..Default::default()
    }
}

pub fn images_to_webp(
    input_pattern: PathBuf,
    output: PathBuf,
    opts: ImageSequenceOptions,
) -> EncodeSettings {
    EncodeSettings {
        input: input_pattern,
        output,
        container: Container::WebP,
        video: Some(VideoSettings {
            codec: VideoCodec::Webp,
            crf: None,
            preset: None,
            ..Default::default()
        }),
        audio: None,
        overwrite: true,
        input_framerate: Some(opts.fps as f64),
        loop_forever: opts.loop_forever,
        ..Default::default()
    }
}

// --- GIF -> MP4/WebM (shrinks massively) -----------------------------------

pub fn gif_to_video(
    input: PathBuf,
    output: PathBuf,
    container: Container,
    codec: VideoCodec,
) -> EncodeSettings {
    EncodeSettings {
        input,
        output,
        container,
        video: Some(VideoSettings {
            codec,
            crf: Some(23),
            preset: Some("medium".to_string()),
            pixel_format: Some("yuv420p".to_string()),
            ..Default::default()
        }),
        audio: None,
        overwrite: true,
        ..Default::default()
    }
}

// --- Extract audio -----------------------------------------------------

pub fn extract_audio(
    input: PathBuf,
    output: PathBuf,
    codec: AudioCodec,
    bitrate_kbps: Option<u64>,
) -> EncodeSettings {
    let container = match codec {
        AudioCodec::Mp3 => Container::Mp3,
        AudioCodec::Opus => Container::Opus,
        AudioCodec::Flac => Container::Flac,
        AudioCodec::Pcm => Container::Wav,
        AudioCodec::Aac | AudioCodec::Copy => Container::Mp4,
    };

    EncodeSettings {
        input,
        output,
        container,
        video: None,
        audio: Some(AudioSettings {
            codec,
            bitrate_kbps,
            ..Default::default()
        }),
        overwrite: true,
        ..Default::default()
    }
}

// --- Convert image ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpg,
    Webp,
    Avif,
    Bmp,
    Ico,
}

/// Maps a 0(worst)-100(best) quality slider to mjpeg's `-q:v` scale, which
/// runs the opposite direction (2 = best, 31 = worst).
fn jpeg_qscale_from_quality(quality: u8) -> u32 {
    let quality = quality.min(100) as f64;
    (31.0 - (quality / 100.0) * 29.0).round().clamp(2.0, 31.0) as u32
}

/// Maps a 0(worst)-100(best) quality slider to libaom-av1's CRF scale, which
/// also runs the opposite direction (0 = best, 63 = worst).
fn avif_crf_from_quality(quality: u8) -> u32 {
    let quality = quality.min(100) as f64;
    (63.0 - (quality / 100.0) * 63.0).round().clamp(0.0, 63.0) as u32
}

pub fn convert_image(
    input: PathBuf,
    output: PathBuf,
    format: ImageFormat,
    quality: Option<u8>,
) -> EncodeSettings {
    let (container, codec) = match format {
        ImageFormat::Png => (Container::Png, VideoCodec::Png),
        ImageFormat::Jpg => (Container::Jpg, VideoCodec::Mjpeg),
        ImageFormat::Webp => (Container::WebP, VideoCodec::Webp),
        ImageFormat::Avif => (Container::Avif, VideoCodec::Avif),
        ImageFormat::Bmp => (Container::Bmp, VideoCodec::Bmp),
        ImageFormat::Ico => (Container::Ico, VideoCodec::Bmp),
    };

    let mut video = VideoSettings {
        codec,
        crf: None,
        preset: None,
        ..Default::default()
    };
    // The image2 muxer otherwise expects a numbered sequence pattern and
    // warns (functioning, but noisily) for a single still frame; -update 1
    // tells it explicitly that this is one image being overwritten in place.
    let mut extra_args = vec!["-update".to_string(), "1".to_string()];

    if let Some(q) = quality {
        match format {
            ImageFormat::Jpg => {
                extra_args.push("-q:v".to_string());
                extra_args.push(jpeg_qscale_from_quality(q).to_string());
            }
            ImageFormat::Webp => {
                extra_args.push("-quality".to_string());
                extra_args.push(q.to_string());
            }
            ImageFormat::Avif => {
                video.crf = Some(avif_crf_from_quality(q));
            }
            ImageFormat::Png | ImageFormat::Bmp | ImageFormat::Ico => {}
        }
    }

    EncodeSettings {
        input,
        output,
        container,
        video: Some(video),
        audio: None,
        overwrite: true,
        extra_args,
        ..Default::default()
    }
}

// --- Mute / strip metadata ----------------------------------------------

pub fn mute(input: PathBuf, output: PathBuf, container: Container) -> EncodeSettings {
    EncodeSettings {
        input,
        output,
        container,
        video: Some(copy_video()),
        audio: None,
        overwrite: true,
        ..Default::default()
    }
}

pub fn strip_metadata(input: PathBuf, output: PathBuf, container: Container) -> EncodeSettings {
    EncodeSettings {
        input,
        output,
        container,
        video: Some(copy_video()),
        audio: Some(copy_audio()),
        overwrite: true,
        extra_args: vec!["-map_metadata".to_string(), "-1".to_string()],
        ..Default::default()
    }
}

// --- Rotate / flip -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rotation {
    None,
    Cw90,
    Ccw90,
    Rotate180,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlipMode {
    None,
    Horizontal,
    Vertical,
    Both,
}

pub fn rotate_flip(
    input: PathBuf,
    output: PathBuf,
    container: Container,
    rotation: Rotation,
    flip: FlipMode,
) -> EncodeSettings {
    let mut filters = Vec::new();
    match rotation {
        Rotation::Cw90 => filters.push("transpose=1".to_string()),
        Rotation::Ccw90 => filters.push("transpose=2".to_string()),
        Rotation::Rotate180 => filters.push("transpose=1,transpose=1".to_string()),
        Rotation::None => {}
    }
    match flip {
        FlipMode::Horizontal => filters.push("hflip".to_string()),
        FlipMode::Vertical => filters.push("vflip".to_string()),
        FlipMode::Both => {
            filters.push("hflip".to_string());
            filters.push("vflip".to_string());
        }
        FlipMode::None => {}
    }

    EncodeSettings {
        input,
        output,
        container,
        video: Some(reencode_video(18)),
        audio: Some(copy_audio()),
        overwrite: true,
        video_filters: filters,
        ..Default::default()
    }
}

// --- Reverse ---------------------------------------------------------------

pub fn reverse(input: PathBuf, output: PathBuf, container: Container) -> EncodeSettings {
    EncodeSettings {
        input,
        output,
        container,
        video: Some(reencode_video(18)),
        audio: Some(AudioSettings::default()),
        overwrite: true,
        video_filters: vec!["reverse".to_string()],
        audio_filters: vec!["areverse".to_string()],
        ..Default::default()
    }
}

// --- Speed up / slow down --------------------------------------------------

/// ffmpeg's `atempo` filter only accepts a [0.5, 2.0] range per instance;
/// chain multiple instances to reach more extreme speed factors.
fn atempo_chain(factor: f64) -> Vec<String> {
    let mut factor = if factor > 0.0 { factor } else { 1.0 };
    let mut filters = Vec::new();
    while factor > 2.0 {
        filters.push("atempo=2".to_string());
        factor /= 2.0;
    }
    while factor < 0.5 {
        filters.push("atempo=0.5".to_string());
        factor /= 0.5;
    }
    filters.push(format!("atempo={factor:.6}"));
    filters
}

pub fn speed_change(
    input: PathBuf,
    output: PathBuf,
    container: Container,
    factor: f64,
) -> EncodeSettings {
    let safe_factor = if factor > 0.0 { factor } else { 1.0 };
    let video_filter = format!("setpts={:.6}*PTS", 1.0 / safe_factor);

    EncodeSettings {
        input,
        output,
        container,
        video: Some(reencode_video(18)),
        audio: Some(AudioSettings::default()),
        overwrite: true,
        video_filters: vec![video_filter],
        audio_filters: atempo_chain(safe_factor),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitrate::DEFAULT_SAFETY_MARGIN;
    use crate::command::build_args;

    #[test]
    fn target_size_job_computes_two_pass_bitrate_and_policy() {
        let req = TargetSizeRequest {
            input: PathBuf::from("in.mp4"),
            output: PathBuf::from("out.mp4"),
            container: Container::Mp4,
            video_codec: VideoCodec::H264,
            target_bytes: 10_485_760,
            duration_seconds: 60.0,
            audio_bitrate_kbps: 128,
            safety_margin: DEFAULT_SAFETY_MARGIN,
        };
        let spec = build_target_size_job(1, &req);

        assert_eq!(spec.passes.len(), 2);
        assert!(matches!(spec.passes[0], EncodePass::First { .. }));
        assert!(matches!(spec.passes[1], EncodePass::Second { .. }));

        let policy = spec.target_size.expect("target size policy");
        assert_eq!(policy.target_bytes, 10_485_760);
        assert_eq!(policy.max_retries, DEFAULT_MAX_RETRIES);

        let video_bitrate = spec.settings.video.as_ref().unwrap().bitrate_kbps.unwrap();
        assert!(video_bitrate > 0);

        let pass2_args = build_args(&spec.settings, &spec.passes[1]);
        assert!(pass2_args.contains(&"-c:a".to_string()));
        assert!(pass2_args.contains(&"aac".to_string()));
    }

    #[test]
    fn target_size_flags_absurdly_low_bitrate_for_downscale_suggestion() {
        let req = TargetSizeRequest {
            input: PathBuf::from("in.mp4"),
            output: PathBuf::from("out.mp4"),
            container: Container::Mp4,
            video_codec: VideoCodec::H264,
            target_bytes: 500_000, // tiny
            duration_seconds: 600.0,
            audio_bitrate_kbps: 128,
            safety_margin: DEFAULT_SAFETY_MARGIN,
        };
        assert!(target_size_should_suggest_downscale(&req));
    }

    #[test]
    fn video_to_gif_uses_palettegen_paletteuse_filter_and_loops() {
        let settings = video_to_gif(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.gif"),
            GifOptions {
                fps: 15,
                width: 480,
                dither: true,
            },
        );
        let args = build_args(&settings, &EncodePass::Single);
        let vf_idx = args.iter().position(|a| a == "-vf").unwrap();
        let filter = &args[vf_idx + 1];
        assert!(filter.contains("fps=15"));
        assert!(filter.contains("scale=480:-1"));
        assert!(filter.contains("palettegen"));
        assert!(filter.contains("paletteuse"));
        assert!(filter.contains("dither=bayer"));
        assert!(args.contains(&"-loop".to_string()));
        assert!(args.contains(&"-an".to_string()));
    }

    #[test]
    fn image_to_gif_loops_the_input_and_trims_to_the_requested_duration() {
        let settings = image_to_gif(
            PathBuf::from("in.png"),
            PathBuf::from("out.gif"),
            ImageToGifOptions {
                duration_seconds: 2.0,
                fps: 10,
                width: 480,
            },
        );
        let args = build_args(&settings, &EncodePass::Single);

        // `-loop 1` (loop the still image as input) must come before `-i`.
        let input_loop_idx = args.iter().position(|a| a == "-loop").unwrap();
        assert_eq!(args[input_loop_idx + 1], "1");
        let input_idx = args.iter().position(|a| a == "-i").unwrap();
        assert!(input_loop_idx < input_idx);

        // `-t 2` (an *input*-side duration limit, not the output-side `-to`
        // `trim` would use) also comes before `-i` - bounding the infinite
        // `-loop 1` source at the demuxer level, not after the fact.
        let t_idx = args
            .iter()
            .enumerate()
            .take(input_idx)
            .position(|(_, a)| a == "-t")
            .unwrap();
        assert_eq!(args[t_idx + 1], "2");
        assert!(t_idx < input_idx);
        assert!(!args.contains(&"-to".to_string()));

        // `-loop 0` (loop the *output* GIF animation forever) is separate
        // from the input-side `-loop 1` above.
        let output_loop_idx = args
            .iter()
            .enumerate()
            .skip(input_idx)
            .position(|(_, a)| a == "-loop")
            .map(|offset| offset + input_idx)
            .unwrap();
        assert_eq!(args[output_loop_idx + 1], "0");

        let vf_idx = args.iter().position(|a| a == "-vf").unwrap();
        let filter = &args[vf_idx + 1];
        assert!(filter.contains("fps=10"));
        assert!(filter.contains("palettegen"));
        assert!(filter.contains("paletteuse"));
        assert!(args.contains(&"-an".to_string()));
    }

    #[test]
    fn video_to_gif_no_dither_uses_dither_none() {
        let settings = video_to_gif(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.gif"),
            GifOptions {
                fps: 10,
                width: 320,
                dither: false,
            },
        );
        let args = build_args(&settings, &EncodePass::Single);
        let vf_idx = args.iter().position(|a| a == "-vf").unwrap();
        assert!(args[vf_idx + 1].contains("dither=none"));
    }

    #[test]
    fn images_to_gif_sets_input_framerate_and_loop() {
        let settings = images_to_gif(
            PathBuf::from("frame%04d.png"),
            PathBuf::from("out.gif"),
            ImageSequenceOptions {
                fps: 24,
                loop_forever: true,
            },
        );
        let args = build_args(&settings, &EncodePass::Single);
        let fr_idx = args.iter().position(|a| a == "-framerate").unwrap();
        assert_eq!(args[fr_idx + 1], "24");
        // -framerate must precede -i.
        let i_idx = args.iter().position(|a| a == "-i").unwrap();
        assert!(fr_idx < i_idx);
        assert!(args.contains(&"-loop".to_string()));
    }

    #[test]
    fn images_to_apng_uses_plays_flag_for_loop() {
        let settings = images_to_apng(
            PathBuf::from("frame%04d.png"),
            PathBuf::from("out.png"),
            ImageSequenceOptions {
                fps: 12,
                loop_forever: true,
            },
        );
        let args = build_args(&settings, &EncodePass::Single);
        assert!(args.contains(&"-plays".to_string()));
        assert!(!args.contains(&"-loop".to_string()));
    }

    #[test]
    fn images_to_webp_builds_animated_webp() {
        let settings = images_to_webp(
            PathBuf::from("frame%04d.png"),
            PathBuf::from("out.webp"),
            ImageSequenceOptions {
                fps: 20,
                loop_forever: false,
            },
        );
        let args = build_args(&settings, &EncodePass::Single);
        assert!(args.contains(&"libwebp".to_string()));
        assert!(!args.contains(&"-loop".to_string()));
    }

    #[test]
    fn gif_to_video_forces_yuv420p_for_compatibility() {
        let settings = gif_to_video(
            PathBuf::from("in.gif"),
            PathBuf::from("out.mp4"),
            Container::Mp4,
            VideoCodec::H264,
        );
        let args = build_args(&settings, &EncodePass::Single);
        assert!(args.contains(&"yuv420p".to_string()));
        assert!(args.contains(&"-an".to_string()));
    }

    #[test]
    fn extract_audio_picks_matching_container_per_codec() {
        let mp3 = extract_audio(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.mp3"),
            AudioCodec::Mp3,
            Some(192),
        );
        assert_eq!(mp3.container, Container::Mp3);
        assert_eq!(mp3.container.extension(), "mp3");

        let flac = extract_audio(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.flac"),
            AudioCodec::Flac,
            None,
        );
        assert_eq!(flac.container, Container::Flac);

        let args = build_args(&mp3, &EncodePass::Single);
        assert!(args.contains(&"-vn".to_string()));
        assert!(args.contains(&"libmp3lame".to_string()));
    }

    #[test]
    fn convert_image_jpg_quality_maps_to_inverted_qscale() {
        assert_eq!(jpeg_qscale_from_quality(100), 2);
        assert_eq!(jpeg_qscale_from_quality(0), 31);

        let settings = convert_image(
            PathBuf::from("in.png"),
            PathBuf::from("out.jpg"),
            ImageFormat::Jpg,
            Some(90),
        );
        let args = build_args(&settings, &EncodePass::Single);
        assert!(args.contains(&"mjpeg".to_string()));
        let q_idx = args.iter().position(|a| a == "-q:v").unwrap();
        assert_eq!(args[q_idx + 1], jpeg_qscale_from_quality(90).to_string());
    }

    #[test]
    fn convert_image_avif_quality_maps_to_inverted_crf() {
        assert_eq!(avif_crf_from_quality(100), 0);
        assert_eq!(avif_crf_from_quality(0), 63);

        let settings = convert_image(
            PathBuf::from("in.png"),
            PathBuf::from("out.avif"),
            ImageFormat::Avif,
            Some(80),
        );
        assert_eq!(settings.video.unwrap().crf, Some(avif_crf_from_quality(80)));
    }

    #[test]
    fn convert_image_webp_quality_uses_quality_flag() {
        let settings = convert_image(
            PathBuf::from("in.png"),
            PathBuf::from("out.webp"),
            ImageFormat::Webp,
            Some(75),
        );
        let args = build_args(&settings, &EncodePass::Single);
        let q_idx = args.iter().position(|a| a == "-quality").unwrap();
        assert_eq!(args[q_idx + 1], "75");
    }

    #[test]
    fn mute_strips_audio_and_copies_video() {
        let settings = mute(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.mp4"),
            Container::Mp4,
        );
        let args = build_args(&settings, &EncodePass::Single);
        assert!(args.contains(&"-c:v".to_string()));
        assert!(args.contains(&"copy".to_string()));
        assert!(args.contains(&"-an".to_string()));
        // Stream copy must never carry re-encode flags.
        assert!(!args.contains(&"-crf".to_string()));
        assert!(!args.contains(&"-preset".to_string()));
    }

    #[test]
    fn strip_metadata_copies_streams_and_drops_metadata() {
        let settings = strip_metadata(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.mp4"),
            Container::Mp4,
        );
        let args = build_args(&settings, &EncodePass::Single);
        assert_eq!(
            args.iter().filter(|a| a.as_str() == "copy").count(),
            2,
            "both video and audio should be stream-copied"
        );
        assert!(args.contains(&"-map_metadata".to_string()));
        assert!(args.contains(&"-1".to_string()));
    }

    #[test]
    fn rotate_flip_emits_transpose_and_flip_filters() {
        let settings = rotate_flip(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.mp4"),
            Container::Mp4,
            Rotation::Cw90,
            FlipMode::Horizontal,
        );
        let args = build_args(&settings, &EncodePass::Single);
        let vf_idx = args.iter().position(|a| a == "-vf").unwrap();
        assert_eq!(args[vf_idx + 1], "transpose=1,hflip");
    }

    #[test]
    fn rotate_flip_none_none_produces_no_filter() {
        let settings = rotate_flip(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.mp4"),
            Container::Mp4,
            Rotation::None,
            FlipMode::None,
        );
        let args = build_args(&settings, &EncodePass::Single);
        assert!(!args.contains(&"-vf".to_string()));
    }

    #[test]
    fn reverse_emits_reverse_and_areverse_filters() {
        let settings = reverse(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.mp4"),
            Container::Mp4,
        );
        let args = build_args(&settings, &EncodePass::Single);
        let vf_idx = args.iter().position(|a| a == "-vf").unwrap();
        assert_eq!(args[vf_idx + 1], "reverse");
        let af_idx = args.iter().position(|a| a == "-af").unwrap();
        assert_eq!(args[af_idx + 1], "areverse");
    }

    #[test]
    fn atempo_chain_within_range_is_a_single_filter() {
        assert_eq!(atempo_chain(1.5), vec!["atempo=1.500000".to_string()]);
    }

    #[test]
    fn atempo_chain_splits_extreme_speedup() {
        let chain = atempo_chain(4.0);
        assert_eq!(
            chain,
            vec!["atempo=2".to_string(), "atempo=2.000000".to_string()]
        );
    }

    #[test]
    fn atempo_chain_splits_extreme_slowdown() {
        let chain = atempo_chain(0.25);
        assert_eq!(
            chain,
            vec!["atempo=0.5".to_string(), "atempo=0.500000".to_string()]
        );
    }

    #[test]
    fn speed_change_sets_setpts_and_atempo() {
        let settings = speed_change(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.mp4"),
            Container::Mp4,
            2.0,
        );
        let args = build_args(&settings, &EncodePass::Single);
        let vf_idx = args.iter().position(|a| a == "-vf").unwrap();
        assert_eq!(args[vf_idx + 1], "setpts=0.500000*PTS");
        let af_idx = args.iter().position(|a| a == "-af").unwrap();
        assert_eq!(args[af_idx + 1], "atempo=2.000000");
    }

    #[test]
    fn speed_change_rejects_non_positive_factor_by_defaulting_to_1x() {
        let settings = speed_change(
            PathBuf::from("in.mp4"),
            PathBuf::from("out.mp4"),
            Container::Mp4,
            0.0,
        );
        let args = build_args(&settings, &EncodePass::Single);
        let vf_idx = args.iter().position(|a| a == "-vf").unwrap();
        assert_eq!(args[vf_idx + 1], "setpts=1.000000*PTS");
    }
}
