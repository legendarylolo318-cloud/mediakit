//! Strongly-typed `ffprobe -show_format -show_streams` metadata extraction.

use crate::error::{CoreError, CoreResult};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct MediaInfo {
    pub path: PathBuf,
    pub file_size_bytes: u64,
    pub duration_seconds: f64,
    pub container_format: String,
    pub overall_bitrate_bps: Option<u64>,
    pub video: Option<VideoStreamInfo>,
    pub audio: Option<AudioStreamInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoStreamInfo {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub bitrate_bps: Option<u64>,
    pub pixel_format: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioStreamInfo {
    pub codec: String,
    pub channels: u32,
    pub sample_rate_hz: Option<u32>,
    pub bitrate_bps: Option<u64>,
}

/// Probe a media file with ffprobe and parse the result into [`MediaInfo`].
pub fn probe(ffprobe_bin: &Path, file: &Path) -> CoreResult<MediaInfo> {
    let mut cmd = Command::new(ffprobe_bin);
    cmd.args([
        "-v",
        "quiet",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ])
    .arg(file);
    crate::sys::no_console_window(&mut cmd);

    let output = cmd.output().map_err(|source| CoreError::Spawn {
        binary: ffprobe_bin.to_path_buf(),
        source,
    })?;

    if !output.status.success() {
        return Err(CoreError::ProbeFailed {
            path: file.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    let raw: RawProbe =
        serde_json::from_slice(&output.stdout).map_err(|source| CoreError::ProbeParse {
            path: file.to_path_buf(),
            source,
        })?;

    Ok(raw.into_media_info(file))
}

// --- raw ffprobe JSON shape -------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawProbe {
    format: RawFormat,
    #[serde(default)]
    streams: Vec<RawStream>,
}

#[derive(Debug, Deserialize)]
struct RawFormat {
    #[serde(default)]
    format_name: String,
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    bit_rate: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawStream {
    codec_type: String,
    #[serde(default)]
    codec_name: String,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    r_frame_rate: Option<String>,
    #[serde(default)]
    avg_frame_rate: Option<String>,
    #[serde(default)]
    bit_rate: Option<String>,
    #[serde(default)]
    channels: Option<u32>,
    #[serde(default)]
    sample_rate: Option<String>,
    #[serde(default)]
    pix_fmt: Option<String>,
}

impl RawProbe {
    fn into_media_info(self, file: &Path) -> MediaInfo {
        let duration_seconds = self
            .format
            .duration
            .as_deref()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        let file_size_bytes = self
            .format
            .size
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or_else(|| std::fs::metadata(file).map(|m| m.len()).unwrap_or(0));

        let overall_bitrate_bps = self.format.bit_rate.as_deref().and_then(|s| s.parse().ok());

        let video = self
            .streams
            .iter()
            .find(|s| s.codec_type == "video")
            .map(|s| VideoStreamInfo {
                codec: s.codec_name.clone(),
                width: s.width.unwrap_or(0),
                height: s.height.unwrap_or(0),
                fps: parse_frame_rate(s.avg_frame_rate.as_deref())
                    .or_else(|| parse_frame_rate(s.r_frame_rate.as_deref()))
                    .unwrap_or(0.0),
                bitrate_bps: s.bit_rate.as_deref().and_then(|b| b.parse().ok()),
                pixel_format: s.pix_fmt.clone(),
            });

        let audio = self
            .streams
            .iter()
            .find(|s| s.codec_type == "audio")
            .map(|s| AudioStreamInfo {
                codec: s.codec_name.clone(),
                channels: s.channels.unwrap_or(0),
                sample_rate_hz: s.sample_rate.as_deref().and_then(|r| r.parse().ok()),
                bitrate_bps: s.bit_rate.as_deref().and_then(|b| b.parse().ok()),
            });

        MediaInfo {
            path: file.to_path_buf(),
            file_size_bytes,
            duration_seconds,
            container_format: self.format.format_name,
            overall_bitrate_bps,
            video,
            audio,
        }
    }
}

/// Parses ffprobe's `"30000/1001"`-style rational frame rate strings.
fn parse_frame_rate(raw: Option<&str>) -> Option<f64> {
    let raw = raw?;
    let mut parts = raw.split('/');
    let num: f64 = parts.next()?.parse().ok()?;
    let den: f64 = parts.next()?.parse().ok()?;
    if den == 0.0 {
        None
    } else {
        Some(num / den)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> &'static str {
        r#"
        {
          "streams": [
            {
              "codec_type": "video",
              "codec_name": "h264",
              "width": 1920,
              "height": 1080,
              "r_frame_rate": "30000/1001",
              "avg_frame_rate": "30000/1001",
              "bit_rate": "5000000",
              "pix_fmt": "yuv420p"
            },
            {
              "codec_type": "audio",
              "codec_name": "aac",
              "channels": 2,
              "sample_rate": "48000",
              "bit_rate": "128000"
            }
          ],
          "format": {
            "format_name": "mov,mp4,m4a,3gp,3g2,mj2",
            "duration": "12.345000",
            "size": "9876543",
            "bit_rate": "6400000"
          }
        }
        "#
    }

    #[test]
    fn parses_full_probe_output() {
        let raw: RawProbe = serde_json::from_str(sample_json()).unwrap();
        let info = raw.into_media_info(Path::new("/tmp/example.mp4"));

        assert_eq!(info.duration_seconds, 12.345);
        assert_eq!(info.file_size_bytes, 9_876_543);
        assert_eq!(info.overall_bitrate_bps, Some(6_400_000));
        assert_eq!(info.container_format, "mov,mp4,m4a,3gp,3g2,mj2");

        let video = info.video.expect("video stream");
        assert_eq!(video.codec, "h264");
        assert_eq!(video.width, 1920);
        assert_eq!(video.height, 1080);
        assert!((video.fps - 29.97).abs() < 0.01);
        assert_eq!(video.bitrate_bps, Some(5_000_000));
        assert_eq!(video.pixel_format.as_deref(), Some("yuv420p"));

        let audio = info.audio.expect("audio stream");
        assert_eq!(audio.codec, "aac");
        assert_eq!(audio.channels, 2);
        assert_eq!(audio.sample_rate_hz, Some(48_000));
        assert_eq!(audio.bitrate_bps, Some(128_000));
    }

    #[test]
    fn handles_audio_only_file() {
        let json = r#"
        {
          "streams": [
            {
              "codec_type": "audio",
              "codec_name": "mp3",
              "channels": 2,
              "sample_rate": "44100"
            }
          ],
          "format": {
            "format_name": "mp3",
            "duration": "180.0",
            "size": "2000000"
          }
        }
        "#;
        let raw: RawProbe = serde_json::from_str(json).unwrap();
        let info = raw.into_media_info(Path::new("/tmp/example.mp3"));

        assert!(info.video.is_none());
        assert!(info.audio.is_some());
        assert_eq!(info.overall_bitrate_bps, None);
    }

    #[test]
    fn parse_frame_rate_handles_zero_denominator() {
        assert_eq!(parse_frame_rate(Some("0/0")), None);
        assert_eq!(parse_frame_rate(Some("25/1")), Some(25.0));
        assert_eq!(parse_frame_rate(None), None);
    }
}
