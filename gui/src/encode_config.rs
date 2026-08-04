//! GUI-side encode configuration: what the preset picker and the Advanced
//! panel both edit. [`EncodeConfig::build`] turns it into the core's
//! strongly-typed [`EncodeSettings`] plus whatever [`EncodePass`]es and
//! [`TargetSizePolicy`] are needed, which is also what powers the "exact
//! command that will run" preview.

use mediakit_core::command::{
    AudioCodec, AudioSettings, Container, EncodePass, EncodeSettings, Trim, VideoCodec,
    VideoSettings,
};
use mediakit_core::job::TargetSizePolicy;
use mediakit_core::presets::{self, FlipMode, ImageFormat, Rotation};
use mediakit_core::size_presets::{self, SizePreset};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Video,
    TargetSize,
    VideoToGif,
    ImageToGif,
    GifToVideo,
    ExtractAudio,
    ConvertImage,
    Mute,
    StripMetadata,
    RotateFlip,
    Reverse,
    SpeedChange,
}

impl Mode {
    pub const ALL: [Mode; 12] = [
        Mode::Video,
        Mode::TargetSize,
        Mode::VideoToGif,
        Mode::ImageToGif,
        Mode::GifToVideo,
        Mode::ExtractAudio,
        Mode::ConvertImage,
        Mode::Mute,
        Mode::StripMetadata,
        Mode::RotateFlip,
        Mode::Reverse,
        Mode::SpeedChange,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Mode::Video => "Convert video",
            Mode::TargetSize => "Target file size",
            Mode::VideoToGif => "Video -> GIF",
            Mode::ImageToGif => "Image -> GIF",
            Mode::GifToVideo => "GIF -> video",
            Mode::ExtractAudio => "Extract audio",
            Mode::ConvertImage => "Convert image",
            Mode::Mute => "Mute",
            Mode::StripMetadata => "Strip metadata",
            Mode::RotateFlip => "Rotate / flip",
            Mode::Reverse => "Reverse",
            Mode::SpeedChange => "Speed up / slow down",
        }
    }
}

/// Which target-size option is selected. `Preset` carries a *snapshot* of
/// the preset (display name + `target_bytes`) taken at selection time, not
/// just its id, so that editing `presets.toml` mid-session never
/// retroactively changes the size (or the label shown) for a job the user
/// already configured or queued.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetSizeChoice {
    Preset {
        id: String,
        display_name: String,
        target_bytes: u64,
    },
    Custom,
}

impl TargetSizeChoice {
    pub fn from_preset(preset: &SizePreset) -> Self {
        TargetSizeChoice::Preset {
            id: preset.id.clone(),
            display_name: preset.display_name.clone(),
            target_bytes: preset.limit_bytes,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            TargetSizeChoice::Preset { display_name, .. } => display_name,
            TargetSizeChoice::Custom => "Custom target size\u{2026}",
        }
    }
}

/// Everything the Advanced panel can edit, independent of which [`Mode`] is
/// active (some fields are only used by some modes).
#[derive(Debug, Clone)]
pub struct EncodeConfig {
    pub mode: Mode,

    // Container/codec (Mode::Video)
    pub container: Container,
    pub video_codec: VideoCodec,
    pub crf: u32,
    pub speed_preset: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub keep_aspect: bool,
    pub fps: Option<f64>,
    pub audio_codec: AudioCodec,
    pub audio_bitrate_kbps: u64,
    pub audio_sample_rate_hz: Option<u32>,

    // Trim (all video-bearing modes)
    pub trim_start_seconds: Option<f64>,
    pub trim_end_seconds: Option<f64>,
    pub crop: Option<(u32, u32, u32, u32)>,

    // Target size
    pub target_size_choice: TargetSizeChoice,
    pub custom_target_mib: u64,
    /// Snapshotted from `SizePresetsConfig::safety_margin_fraction` at the
    /// time a preset (or "Custom") was selected - see `TargetSizeChoice`'s
    /// doc comment for why this is a snapshot rather than a live lookup.
    pub target_size_safety_margin: f64,

    // Video -> GIF
    pub gif_fps: u32,
    pub gif_width: u32,
    pub gif_dither: bool,

    // Image -> GIF
    pub image_gif_duration_seconds: f64,
    pub image_gif_fps: u32,
    pub image_gif_width: u32,

    // GIF -> video
    pub gif_to_video_container: Container,
    pub gif_to_video_codec: VideoCodec,

    // Extract audio
    pub extract_audio_codec: AudioCodec,
    pub extract_audio_bitrate_kbps: u64,

    // Convert image
    pub image_format: ImageFormat,
    pub image_quality: u8,

    // Rotate / flip
    pub rotation: Rotation,
    pub flip: FlipMode,

    // Speed change
    pub speed_factor: f64,

    // Escape hatch: split on whitespace and appended verbatim.
    pub custom_args: String,

    pub hardware_encoder: Option<String>,
}

impl Default for EncodeConfig {
    fn default() -> Self {
        Self {
            mode: Mode::Video,
            container: Container::Mp4,
            video_codec: VideoCodec::H264,
            crf: 23,
            speed_preset: "medium".to_string(),
            width: None,
            height: None,
            keep_aspect: true,
            fps: None,
            audio_codec: AudioCodec::Aac,
            audio_bitrate_kbps: 128,
            audio_sample_rate_hz: None,
            trim_start_seconds: None,
            trim_end_seconds: None,
            crop: None,
            // A real default preset gets applied by whoever constructs this
            // in a context that has a loaded `SizePresetsConfig` (see
            // `MediaKitApp::new` and `cli::run`) - `Default` alone can't
            // know what presets.toml contains.
            target_size_choice: TargetSizeChoice::Custom,
            custom_target_mib: 25,
            target_size_safety_margin: mediakit_core::bitrate::DEFAULT_SAFETY_MARGIN,
            gif_fps: 15,
            gif_width: 480,
            gif_dither: true,
            image_gif_duration_seconds: 2.0,
            image_gif_fps: 10,
            image_gif_width: 480,
            gif_to_video_container: Container::Mp4,
            gif_to_video_codec: VideoCodec::H264,
            extract_audio_codec: AudioCodec::Mp3,
            extract_audio_bitrate_kbps: 192,
            image_format: ImageFormat::Png,
            image_quality: 85,
            rotation: Rotation::None,
            flip: FlipMode::None,
            speed_factor: 1.0,
            custom_args: String::new(),
            hardware_encoder: None,
        }
    }
}

pub struct BuiltJob {
    pub settings: EncodeSettings,
    pub passes: Vec<EncodePass>,
    pub target_size: Option<TargetSizePolicy>,
}

impl EncodeConfig {
    fn trim(&self) -> Trim {
        Trim {
            start_seconds: self.trim_start_seconds,
            end_seconds: self.trim_end_seconds,
        }
    }

    fn crop_filter(&self) -> Option<String> {
        self.crop
            .map(|(x, y, w, h)| format!("crop={w}:{h}:{x}:{y}"))
    }

    fn video_codec_for_hw(&self) -> VideoCodec {
        // Hardware encoder selection is surfaced as an encoder *name*
        // override (e.g. "h264_nvenc") layered on top of the chosen codec
        // family; the codec enum still picks the container-appropriate
        // software fallback used if hw encode fails (see gui::app).
        self.video_codec
    }

    /// Build the settings for the currently selected mode. `input`/`output`
    /// are the resolved paths for one queue item; `duration_seconds` comes
    /// from that item's probed metadata (0.0 if unknown).
    pub fn build(&self, input: PathBuf, output: PathBuf, duration_seconds: f64) -> BuiltJob {
        let extra_args: Vec<String> = self
            .custom_args
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        match self.mode {
            Mode::Video => {
                let mut video = VideoSettings {
                    codec: self.video_codec_for_hw(),
                    bitrate_kbps: None,
                    crf: Some(self.crf),
                    preset: Some(self.speed_preset.clone()),
                    width: if self.keep_aspect { None } else { self.width },
                    height: if self.keep_aspect { None } else { self.height },
                    fps: self.fps,
                    pixel_format: None,
                    hardware_encoder_override: self.hardware_encoder.clone(),
                };
                let mut video_filters = Vec::new();
                if self.keep_aspect {
                    if let Some(w) = self.width {
                        video_filters.push(format!("scale={w}:-2"));
                    } else if let Some(h) = self.height {
                        video_filters.push(format!("scale=-2:{h}"));
                    }
                } else {
                    // width/height already set on `video`, which build_args
                    // turns into its own scale filter; don't double up.
                    video.width = self.width;
                    video.height = self.height;
                }
                if let Some(crop) = self.crop_filter() {
                    video_filters.push(crop);
                }

                let settings = EncodeSettings {
                    input,
                    output,
                    container: self.container,
                    video: Some(video),
                    audio: Some(AudioSettings {
                        codec: self.audio_codec,
                        bitrate_kbps: Some(self.audio_bitrate_kbps),
                        sample_rate_hz: self.audio_sample_rate_hz,
                        channels: None,
                    }),
                    trim: self.trim(),
                    overwrite: true,
                    video_filters,
                    extra_args,
                    ..Default::default()
                };
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }

            Mode::TargetSize => {
                let target_bytes = match &self.target_size_choice {
                    TargetSizeChoice::Preset { target_bytes, .. } => *target_bytes,
                    TargetSizeChoice::Custom => self.custom_target_mib * size_presets::MIB,
                };
                let spec = presets::build_target_size_job(
                    0,
                    &presets::TargetSizeRequest {
                        input,
                        output,
                        container: self.container,
                        video_codec: self.video_codec_for_hw(),
                        target_bytes,
                        duration_seconds,
                        audio_bitrate_kbps: self.audio_bitrate_kbps,
                        safety_margin: self.target_size_safety_margin,
                    },
                );
                let mut settings = spec.settings;
                settings.trim = self.trim();
                settings.extra_args = extra_args;
                if let Some(video) = settings.video.as_mut() {
                    video.hardware_encoder_override = self.hardware_encoder.clone();
                }
                BuiltJob {
                    settings,
                    passes: spec.passes,
                    target_size: spec.target_size,
                }
            }

            Mode::VideoToGif => {
                let mut settings = presets::video_to_gif(
                    input,
                    output,
                    presets::GifOptions {
                        fps: self.gif_fps,
                        width: self.gif_width,
                        dither: self.gif_dither,
                    },
                );
                settings.trim = self.trim();
                settings.extra_args = extra_args;
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }

            Mode::ImageToGif => {
                // No generic trim applied here (unlike the other modes):
                // `presets::image_to_gif` already sets `trim.end_seconds`
                // to the requested duration, which is the whole point of
                // this preset - a generic trim would just clobber it back
                // to "no cutoff" for a `-loop 1` input that's infinite.
                let mut settings = presets::image_to_gif(
                    input,
                    output,
                    presets::ImageToGifOptions {
                        duration_seconds: self.image_gif_duration_seconds,
                        fps: self.image_gif_fps,
                        width: self.image_gif_width,
                    },
                );
                settings.extra_args = extra_args;
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }

            Mode::GifToVideo => {
                let mut settings = presets::gif_to_video(
                    input,
                    output,
                    self.gif_to_video_container,
                    self.gif_to_video_codec,
                );
                settings.extra_args = extra_args;
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }

            Mode::ExtractAudio => {
                let mut settings = presets::extract_audio(
                    input,
                    output,
                    self.extract_audio_codec,
                    Some(self.extract_audio_bitrate_kbps),
                );
                settings.trim = self.trim();
                settings.extra_args = extra_args;
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }

            Mode::ConvertImage => {
                let mut settings = presets::convert_image(
                    input,
                    output,
                    self.image_format,
                    Some(self.image_quality),
                );
                settings.extra_args.extend(extra_args);
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }

            Mode::Mute => {
                let mut settings = presets::mute(input, output, self.container);
                settings.trim = self.trim();
                settings.extra_args = extra_args;
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }

            Mode::StripMetadata => {
                let mut settings = presets::strip_metadata(input, output, self.container);
                settings.extra_args.extend(extra_args);
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }

            Mode::RotateFlip => {
                let mut settings =
                    presets::rotate_flip(input, output, self.container, self.rotation, self.flip);
                settings.trim = self.trim();
                settings.extra_args = extra_args;
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }

            Mode::Reverse => {
                let mut settings = presets::reverse(input, output, self.container);
                settings.extra_args = extra_args;
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }

            Mode::SpeedChange => {
                let mut settings =
                    presets::speed_change(input, output, self.container, self.speed_factor);
                settings.extra_args = extra_args;
                BuiltJob {
                    settings,
                    passes: vec![EncodePass::Single],
                    target_size: None,
                }
            }
        }
    }

    /// Apply a one-click target-size preset, overwriting only the fields
    /// that preset cares about so other Advanced-panel tweaks the user made
    /// survive.
    pub fn apply_size_preset(&mut self, preset: &SizePreset, safety_margin: f64) {
        self.mode = Mode::TargetSize;
        self.target_size_choice = TargetSizeChoice::from_preset(preset);
        self.target_size_safety_margin = safety_margin;
        self.container = Container::Mp4;
        self.video_codec = VideoCodec::H264;
    }

    pub fn output_extension(&self) -> &'static str {
        match self.mode {
            Mode::Video | Mode::TargetSize => self.container.extension(),
            Mode::VideoToGif | Mode::ImageToGif => Container::Gif.extension(),
            Mode::GifToVideo => self.gif_to_video_container.extension(),
            Mode::ExtractAudio => match self.extract_audio_codec {
                AudioCodec::Mp3 => "mp3",
                AudioCodec::Opus => "opus",
                AudioCodec::Flac => "flac",
                AudioCodec::Pcm => "wav",
                AudioCodec::Aac | AudioCodec::Copy => "m4a",
            },
            Mode::ConvertImage => match self.image_format {
                ImageFormat::Png => "png",
                ImageFormat::Jpg => "jpg",
                ImageFormat::Webp => "webp",
                ImageFormat::Avif => "avif",
                ImageFormat::Bmp => "bmp",
                ImageFormat::Ico => "ico",
            },
            Mode::Mute
            | Mode::StripMetadata
            | Mode::RotateFlip
            | Mode::Reverse
            | Mode::SpeedChange => self.container.extension(),
        }
    }
}
