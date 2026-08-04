//! Headless CLI mode:
//! `mediakit --preset target-size --size-preset discord-free input.mp4 -o out.mp4`.
//! Uses the exact same `mediakit_core` engine as the GUI, just driven from
//! argv instead of clicks, with progress printed to stdout. Run with
//! `--list-size-presets` to see the available `--size-preset` ids.

use crate::output::{OutputLocation, OutputSettings};
use clap::Parser;
use mediakit_core::command::{AudioCodec, Container, VideoCodec};
use mediakit_core::engine::{EngineEvent, JobEngine};
use mediakit_core::ffmpeg_env::{self, FfmpegEnv};
use mediakit_core::job::JobSpec;
use mediakit_core::presets::{self, GifOptions, TargetSizeRequest};
use mediakit_core::probe;
use mediakit_core::size_presets::{self, SizePresetsConfig};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "mediakit",
    about = "MediaKit: batch media conversion (headless mode)"
)]
pub struct CliArgs {
    /// Input file(s) to convert. Not required when using
    /// `--list-size-presets`.
    pub inputs: Vec<PathBuf>,

    /// Preset to apply. Not required when using `--list-size-presets`.
    #[arg(long, value_enum)]
    pub preset: Option<CliPreset>,

    /// Output file (single input) or output directory (multiple inputs).
    /// Defaults to alongside each input, using `{name}_converted.{ext}`.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Target size in MiB, for `target-size` when `--size-preset` isn't
    /// given (ignored by other presets).
    #[arg(long, default_value_t = 25)]
    pub target_mib: u64,

    /// Named entry from `presets.toml` to target (see `--list-size-presets`
    /// for the available ids), for `target-size`. Takes priority over
    /// `--target-mib` when given. Presets are data, not code - MediaKit
    /// ships a few (e.g. Discord's tiers) but doesn't hardcode any platform
    /// by name here, so this always reflects whatever presets.toml
    /// currently has, including a user's own edits.
    #[arg(long)]
    pub size_preset: Option<String>,

    /// Print the available `--size-preset` ids and their byte limits, then
    /// exit without converting anything.
    #[arg(long, default_value_t = false)]
    pub list_size_presets: bool,

    /// Audio bitrate in kbps.
    #[arg(long, default_value_t = 128)]
    pub audio_bitrate_kbps: u64,

    /// Loop duration in seconds, for `image-to-gif` (ignored by other
    /// presets).
    #[arg(long, default_value_t = 2.0)]
    pub image_gif_seconds: f64,

    /// Overwrite existing output files instead of auto-numbering.
    #[arg(long, default_value_t = false)]
    pub overwrite: bool,
}

#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum CliPreset {
    #[value(name = "target-size")]
    TargetSize,
    #[value(name = "video-to-gif")]
    VideoToGif,
    #[value(name = "image-to-gif")]
    ImageToGif,
    #[value(name = "gif-to-mp4")]
    GifToMp4,
    #[value(name = "gif-to-webm")]
    GifToWebm,
    #[value(name = "extract-audio-mp3")]
    ExtractAudioMp3,
    #[value(name = "extract-audio-opus")]
    ExtractAudioOpus,
    #[value(name = "extract-audio-flac")]
    ExtractAudioFlac,
    #[value(name = "extract-audio-wav")]
    ExtractAudioWav,
    #[value(name = "mute")]
    Mute,
    #[value(name = "strip-metadata")]
    StripMetadata,
}

impl CliPreset {
    fn output_extension(self) -> &'static str {
        match self {
            CliPreset::TargetSize | CliPreset::Mute | CliPreset::StripMetadata => "mp4",
            CliPreset::VideoToGif | CliPreset::ImageToGif => "gif",
            CliPreset::GifToMp4 => "mp4",
            CliPreset::GifToWebm => "webm",
            CliPreset::ExtractAudioMp3 => "mp3",
            CliPreset::ExtractAudioOpus => "opus",
            CliPreset::ExtractAudioFlac => "flac",
            CliPreset::ExtractAudioWav => "wav",
        }
    }
}

/// Runs headless conversion for every input, printing progress to stdout.
/// Returns the process exit code (0 if every job succeeded).
pub fn run(args: CliArgs) -> i32 {
    let size_presets = size_presets::config_dir()
        .map(|dir| size_presets::load_or_seed(&dir))
        .unwrap_or_else(SizePresetsConfig::defaults);

    if args.list_size_presets {
        print_size_presets(&size_presets);
        return 0;
    }

    let Some(preset) = args.preset else {
        eprintln!("error: --preset is required (or use --list-size-presets)");
        return 1;
    };
    if args.inputs.is_empty() {
        eprintln!("error: at least one input file is required");
        return 1;
    }

    let ffmpeg_env = match locate_ffmpeg() {
        Ok(env) => env,
        Err(msg) => {
            eprintln!("error: {msg}");
            return 1;
        }
    };

    if args.output.is_some() && args.inputs.len() > 1 {
        if let Some(out) = &args.output {
            if out.extension().is_some() && !out.is_dir() {
                eprintln!("error: -o/--output must be a directory when converting multiple inputs");
                return 1;
            }
        }
    }

    let output_settings = build_output_settings(&args);

    let (engine, rx) = JobEngine::new(ffmpeg_env.ffmpeg_path.clone(), 1);
    let mut had_failure = false;

    for (idx, input) in args.inputs.iter().enumerate() {
        let duration = probe::probe(&ffmpeg_env.ffprobe_path, input)
            .map(|info| info.duration_seconds)
            .unwrap_or(0.0);

        let output = output_settings.resolve(input, preset.output_extension());
        let (settings, passes, target_size) = match build_settings(
            preset,
            input.clone(),
            output.clone(),
            duration,
            &args,
            &size_presets,
        ) {
            Ok(built) => built,
            Err(msg) => {
                eprintln!("error: {msg}");
                had_failure = true;
                continue;
            }
        };

        println!(
            "[{}/{}] {} -> {}",
            idx + 1,
            args.inputs.len(),
            input.display(),
            output.display()
        );

        let job_id = idx as u64 + 1;
        let target_limit_bytes = target_size.as_ref().map(|t| t.target_bytes);
        engine.submit(JobSpec {
            id: job_id,
            settings,
            passes,
            total_duration_seconds: duration,
            target_size,
        });

        if !wait_for_job(&rx, job_id) {
            had_failure = true;
        } else if let Some(limit_bytes) = target_limit_bytes {
            print_size_check(&output, limit_bytes);
        }
    }

    drop(engine);
    if had_failure {
        1
    } else {
        0
    }
}

fn wait_for_job(rx: &std::sync::mpsc::Receiver<EngineEvent>, job_id: u64) -> bool {
    loop {
        match rx.recv_timeout(Duration::from_secs(3600)) {
            Ok(EngineEvent::Progress { id, info }) if id == job_id => {
                if let Some(percent) = info.percent {
                    print!("\r  {percent:.0}%");
                    use std::io::Write;
                    let _ = std::io::stdout().flush();
                }
            }
            Ok(EngineEvent::Retrying { id, attempt, .. }) if id == job_id => {
                println!("\n  output too large, retrying (attempt {attempt})");
            }
            Ok(EngineEvent::Done { id, .. }) if id == job_id => {
                println!("\r  done            ");
                return true;
            }
            Ok(EngineEvent::Failed { id, error, .. }) if id == job_id => {
                println!("\r  failed: {error}");
                return false;
            }
            Ok(EngineEvent::Cancelled { id }) if id == job_id => {
                println!("\r  cancelled");
                return false;
            }
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

fn print_size_presets(config: &SizePresetsConfig) {
    println!("Available --size-preset ids (from presets.toml):");
    for preset in &config.presets {
        let mib = preset.limit_bytes as f64 / size_presets::MIB as f64;
        println!(
            "  {:<24} {:>10.1} MiB  {}",
            preset.id, mib, preset.display_name
        );
    }
    println!(
        "Safety margin: {:.0}% (edit presets.toml or Settings -> Presets in the GUI to change)",
        config.safety_margin_percent
    );
}

/// After a size-targeted job finishes, "done" alone doesn't say whether it
/// actually landed under the cap - print a real pass/fail with byte counts.
fn print_size_check(output: &std::path::Path, limit_bytes: u64) {
    let Ok(metadata) = std::fs::metadata(output) else {
        return;
    };
    let check = size_presets::check_output_size(metadata.len(), limit_bytes);
    let actual_mib = check.actual_bytes as f64 / size_presets::MIB as f64;
    let limit_mib = check.limit_bytes as f64 / size_presets::MIB as f64;
    if check.passed {
        println!("  size check: PASS ({actual_mib:.2} MiB <= {limit_mib:.2} MiB limit)");
    } else {
        println!("  size check: FAIL ({actual_mib:.2} MiB > {limit_mib:.2} MiB limit)");
    }
}

fn locate_ffmpeg() -> Result<FfmpegEnv, String> {
    let app_data_dir = ffmpeg_env::app_data_dir()
        .map_err(|e| format!("could not resolve app data directory: {e}"))?;
    FfmpegEnv::detect_cached(&app_data_dir).map_err(|_| {
        "ffmpeg/ffprobe not found (checked app data dir, next to executable, and PATH). \
         Install ffmpeg or run the GUI once to download it."
            .to_string()
    })
}

fn build_output_settings(args: &CliArgs) -> OutputSettings {
    let mut settings = OutputSettings {
        overwrite_existing: args.overwrite,
        ..Default::default()
    };
    if let Some(out) = &args.output {
        if args.inputs.len() == 1 && out.extension().is_some() {
            // Single explicit output file: honor it exactly via a template
            // that ignores the input name entirely.
            settings.location = OutputLocation::Custom(
                out.parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from(".")),
            );
            settings.filename_template = out
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "{name}_converted.{ext}".to_string());
        } else {
            settings.location = OutputLocation::Custom(out.clone());
        }
    }
    settings
}

type BuiltSettings = (
    mediakit_core::command::EncodeSettings,
    Vec<mediakit_core::command::EncodePass>,
    Option<mediakit_core::job::TargetSizePolicy>,
);

fn build_settings(
    preset: CliPreset,
    input: PathBuf,
    output: PathBuf,
    duration: f64,
    args: &CliArgs,
    size_presets: &SizePresetsConfig,
) -> Result<BuiltSettings, String> {
    use mediakit_core::command::EncodePass;

    match preset {
        CliPreset::TargetSize => {
            let target_bytes = match &args.size_preset {
                Some(id) => size_presets
                    .find(id)
                    .map(|p| p.limit_bytes)
                    .ok_or_else(|| {
                        let available = size_presets
                            .presets
                            .iter()
                            .map(|p| p.id.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!(
                            "unknown --size-preset '{id}' (available: {available}; \
                             see --list-size-presets)"
                        )
                    })?,
                None => args.target_mib * size_presets::MIB,
            };
            let spec = presets::build_target_size_job(
                0,
                &TargetSizeRequest {
                    input,
                    output,
                    container: Container::Mp4,
                    video_codec: VideoCodec::H264,
                    target_bytes,
                    duration_seconds: duration,
                    audio_bitrate_kbps: args.audio_bitrate_kbps,
                    safety_margin: size_presets.safety_margin_fraction(),
                },
            );
            Ok((spec.settings, spec.passes, spec.target_size))
        }
        CliPreset::VideoToGif => {
            let settings = presets::video_to_gif(
                input,
                output,
                GifOptions {
                    fps: 15,
                    width: 480,
                    dither: true,
                },
            );
            Ok((settings, vec![EncodePass::Single], None))
        }
        CliPreset::ImageToGif => {
            let settings = presets::image_to_gif(
                input,
                output,
                presets::ImageToGifOptions {
                    duration_seconds: args.image_gif_seconds,
                    fps: 10,
                    width: 480,
                },
            );
            Ok((settings, vec![EncodePass::Single], None))
        }
        CliPreset::GifToMp4 => {
            let settings = presets::gif_to_video(input, output, Container::Mp4, VideoCodec::H264);
            Ok((settings, vec![EncodePass::Single], None))
        }
        CliPreset::GifToWebm => {
            let settings = presets::gif_to_video(input, output, Container::WebM, VideoCodec::Vp9);
            Ok((settings, vec![EncodePass::Single], None))
        }
        CliPreset::ExtractAudioMp3 => {
            let settings = presets::extract_audio(
                input,
                output,
                AudioCodec::Mp3,
                Some(args.audio_bitrate_kbps),
            );
            Ok((settings, vec![EncodePass::Single], None))
        }
        CliPreset::ExtractAudioOpus => {
            let settings = presets::extract_audio(
                input,
                output,
                AudioCodec::Opus,
                Some(args.audio_bitrate_kbps),
            );
            Ok((settings, vec![EncodePass::Single], None))
        }
        CliPreset::ExtractAudioFlac => {
            let settings = presets::extract_audio(input, output, AudioCodec::Flac, None);
            Ok((settings, vec![EncodePass::Single], None))
        }
        CliPreset::ExtractAudioWav => {
            let settings = presets::extract_audio(input, output, AudioCodec::Pcm, None);
            Ok((settings, vec![EncodePass::Single], None))
        }
        CliPreset::Mute => {
            let settings = presets::mute(input, output, Container::Mp4);
            Ok((settings, vec![EncodePass::Single], None))
        }
        CliPreset::StripMetadata => {
            let settings = presets::strip_metadata(input, output, Container::Mp4);
            Ok((settings, vec![EncodePass::Single], None))
        }
    }
}
