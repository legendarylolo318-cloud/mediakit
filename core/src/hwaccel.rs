//! Detects which hardware video encoders ffmpeg was built with, from the
//! `ffmpeg -encoders` list already parsed by [`crate::ffmpeg_env`]. Purely a
//! name-pattern classifier - no I/O, no GPU probing (ffmpeg only ever lists
//! encoders it was compiled against; actually using one against real
//! hardware is validated by trying it and falling back on failure, see
//! [`crate::engine`]).

use crate::command::VideoCodec;
use crate::ffmpeg_env::EncoderInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HwApi {
    Nvenc,
    Qsv,
    Vaapi,
    Amf,
    VideoToolbox,
}

impl HwApi {
    pub fn label(self) -> &'static str {
        match self {
            HwApi::Nvenc => "NVENC (NVIDIA)",
            HwApi::Qsv => "Quick Sync (Intel)",
            HwApi::Vaapi => "VAAPI (Linux)",
            HwApi::Amf => "AMF (AMD)",
            HwApi::VideoToolbox => "VideoToolbox (macOS)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardwareEncoder {
    pub api: HwApi,
    pub codec: VideoCodec,
    /// The literal ffmpeg encoder name, e.g. `"h264_nvenc"`.
    pub encoder_name: String,
}

/// Classify ffmpeg's encoder list into recognized hardware encoders. Names
/// follow ffmpeg's well-established `<codec>_<api>` convention.
pub fn detect_hardware_encoders(encoders: &[EncoderInfo]) -> Vec<HardwareEncoder> {
    encoders.iter().filter_map(|e| classify(&e.name)).collect()
}

fn classify(name: &str) -> Option<HardwareEncoder> {
    let (codec_part, api) = if let Some(c) = name.strip_suffix("_nvenc") {
        (c, HwApi::Nvenc)
    } else if let Some(c) = name.strip_suffix("_qsv") {
        (c, HwApi::Qsv)
    } else if let Some(c) = name.strip_suffix("_vaapi") {
        (c, HwApi::Vaapi)
    } else if let Some(c) = name.strip_suffix("_amf") {
        (c, HwApi::Amf)
    } else {
        let c = name.strip_suffix("_videotoolbox")?;
        (c, HwApi::VideoToolbox)
    };

    let codec = match codec_part {
        "h264" => VideoCodec::H264,
        "hevc" | "h265" => VideoCodec::H265,
        "av1" => VideoCodec::Av1,
        _ => return None,
    };

    Some(HardwareEncoder {
        api,
        codec,
        encoder_name: name.to_string(),
    })
}

/// Hardware encoders for one codec family, e.g. all the H.264 hardware
/// options to offer once the user has picked H.264 as the codec.
pub fn for_codec(encoders: &[HardwareEncoder], codec: VideoCodec) -> Vec<&HardwareEncoder> {
    encoders.iter().filter(|e| e.codec == codec).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ffmpeg_env::EncoderKind;

    fn encoder(name: &str) -> EncoderInfo {
        EncoderInfo {
            name: name.to_string(),
            description: String::new(),
            kind: EncoderKind::Video,
        }
    }

    #[test]
    fn detects_all_known_hw_apis() {
        let list = vec![
            encoder("libx264"),
            encoder("h264_nvenc"),
            encoder("hevc_nvenc"),
            encoder("h264_qsv"),
            encoder("h264_vaapi"),
            encoder("hevc_amf"),
            encoder("h264_videotoolbox"),
            encoder("av1_nvenc"),
            encoder("aac"),
        ];
        let hw = detect_hardware_encoders(&list);
        assert_eq!(hw.len(), 7);
        assert!(hw
            .iter()
            .any(|e| e.api == HwApi::Nvenc && e.codec == VideoCodec::H264));
        assert!(hw
            .iter()
            .any(|e| e.api == HwApi::Nvenc && e.codec == VideoCodec::H265));
        assert!(hw
            .iter()
            .any(|e| e.api == HwApi::Nvenc && e.codec == VideoCodec::Av1));
        assert!(hw
            .iter()
            .any(|e| e.api == HwApi::Qsv && e.codec == VideoCodec::H264));
        assert!(hw
            .iter()
            .any(|e| e.api == HwApi::Vaapi && e.codec == VideoCodec::H264));
        assert!(hw
            .iter()
            .any(|e| e.api == HwApi::Amf && e.codec == VideoCodec::H265));
        assert!(hw
            .iter()
            .any(|e| e.api == HwApi::VideoToolbox && e.codec == VideoCodec::H264));
    }

    #[test]
    fn ignores_software_and_unrelated_encoders() {
        let list = vec![encoder("libx264"), encoder("libvpx-vp9"), encoder("aac")];
        assert!(detect_hardware_encoders(&list).is_empty());
    }

    #[test]
    fn for_codec_filters_by_family() {
        let list = vec![
            encoder("h264_nvenc"),
            encoder("hevc_nvenc"),
            encoder("h264_vaapi"),
        ];
        let hw = detect_hardware_encoders(&list);
        let h264_only = for_codec(&hw, VideoCodec::H264);
        assert_eq!(h264_only.len(), 2);
        assert!(h264_only.iter().all(|e| e.codec == VideoCodec::H264));
    }
}
