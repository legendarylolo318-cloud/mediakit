//! Target-size presets (Discord's tiers today, anything else tomorrow) as
//! data, not code. Platform upload caps change over time - Discord alone
//! has moved its free-tier limit three times - so nothing in MediaKit's
//! source hardcodes a byte count for any of them. See `presets.toml` for
//! the shipped defaults; a user's own copy in their config dir always wins
//! and can be hand-edited or managed via Settings -> Presets.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The presets shipped with MediaKit itself, seeded into a user's config
/// dir the first time [`load_or_seed`] runs there.
pub const DEFAULT_PRESETS_TOML: &str = include_str!("../presets.toml");

/// MiB (1024^2), not decimal MB (1000^2): matches what file managers, `du`,
/// and most OSes report, which is what a user actually compares an output
/// file against.
pub const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SizePreset {
    pub id: String,
    pub display_name: String,
    pub limit_bytes: u64,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SizePresetsConfig {
    /// Fraction of `limit_bytes` actually targeted, leaving headroom for
    /// container/muxing overhead and bitrate-rounding so real output lands
    /// under the cap rather than skating right against it. A setting, not
    /// a constant - how much margin is "enough" is a judgment call users
    /// may reasonably want to tune.
    #[serde(default = "default_safety_margin_percent")]
    pub safety_margin_percent: f32,
    #[serde(default)]
    pub presets: Vec<SizePreset>,
}

fn default_safety_margin_percent() -> f32 {
    crate::bitrate::DEFAULT_SAFETY_MARGIN as f32 * 100.0
}

impl SizePresetsConfig {
    /// The presets MediaKit ships with, parsed from the bundled
    /// `presets.toml`. Panics only if that bundled file itself is
    /// malformed, which would be a build-time bug, not a runtime one.
    pub fn defaults() -> Self {
        toml::from_str(DEFAULT_PRESETS_TOML).expect("bundled presets.toml must parse")
    }

    pub fn safety_margin_fraction(&self) -> f64 {
        (self.safety_margin_percent as f64 / 100.0).clamp(0.5, 1.0)
    }

    pub fn find(&self, id: &str) -> Option<&SizePreset> {
        self.presets.iter().find(|p| p.id == id)
    }
}

pub fn presets_path(config_dir: &Path) -> PathBuf {
    config_dir.join("presets.toml")
}

/// Resolve the per-OS config directory presets.toml lives in (distinct from
/// the app-data dir the vendored ffmpeg/ffprobe/yt-dlp binaries extract
/// into) - `None` only if the OS/env gives us nowhere sane to put it.
pub fn config_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from(
        crate::ffmpeg_env::APP_QUALIFIER,
        crate::ffmpeg_env::APP_ORG,
        crate::ffmpeg_env::APP_NAME,
    )
    .map(|d| d.config_dir().to_path_buf())
}

/// Load the user's `presets.toml`, seeding it with the shipped defaults on
/// first run so there's a real, hand-editable file from the start. Falls
/// back to in-memory defaults (never crashes the app) if the file is
/// missing, fails to parse, or - after a user has deleted every entry -
/// ends up with an empty preset list, since the rest of MediaKit assumes
/// there's always at least one preset to fall back to.
pub fn load_or_seed(config_dir: &Path) -> SizePresetsConfig {
    let path = presets_path(config_dir);
    if !path.is_file() {
        let _ = std::fs::create_dir_all(config_dir);
        let _ = std::fs::write(&path, DEFAULT_PRESETS_TOML);
    }
    let loaded = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| toml::from_str::<SizePresetsConfig>(&s).ok());
    match loaded {
        Some(config) if !config.presets.is_empty() => config,
        _ => SizePresetsConfig::defaults(),
    }
}

pub fn save(config_dir: &Path, config: &SizePresetsConfig) -> std::io::Result<()> {
    let path = presets_path(config_dir);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(config).expect("SizePresetsConfig always serializes");
    std::fs::write(path, text)
}

pub fn restore_defaults(config_dir: &Path) -> std::io::Result<SizePresetsConfig> {
    let defaults = SizePresetsConfig::defaults();
    save(config_dir, &defaults)?;
    Ok(defaults)
}

/// Pass/fail check for a size-targeted encode's *real* output, run after
/// every such job finishes - "done" alone isn't enough information; the
/// user needs to know whether it actually landed under the cap.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizeCheckResult {
    pub actual_bytes: u64,
    pub limit_bytes: u64,
    pub passed: bool,
}

pub fn check_output_size(actual_bytes: u64, limit_bytes: u64) -> SizeCheckResult {
    SizeCheckResult {
        actual_bytes,
        limit_bytes,
        passed: actual_bytes <= limit_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_presets_toml_parses_and_is_nonempty() {
        let config = SizePresetsConfig::defaults();
        assert!(!config.presets.is_empty());
        for preset in &config.presets {
            assert!(!preset.id.is_empty());
            assert!(preset.limit_bytes > 0);
        }
    }

    #[test]
    fn safety_margin_fraction_clamps_to_sane_range() {
        let mut config = SizePresetsConfig::defaults();
        config.safety_margin_percent = 10.0;
        assert_eq!(config.safety_margin_fraction(), 0.5);
        config.safety_margin_percent = 500.0;
        assert_eq!(config.safety_margin_fraction(), 1.0);
        config.safety_margin_percent = 97.0;
        assert!((config.safety_margin_fraction() - 0.97).abs() < 1e-9);
    }

    #[test]
    fn find_looks_up_by_id() {
        let config = SizePresetsConfig::defaults();
        assert!(config.find("discord-free").is_some());
        assert!(config.find("does-not-exist").is_none());
    }

    #[test]
    fn load_or_seed_writes_defaults_on_first_run() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("config");
        assert!(!presets_path(&dir).is_file());

        let loaded = load_or_seed(&dir);
        assert!(presets_path(&dir).is_file());
        assert_eq!(loaded, SizePresetsConfig::defaults());
    }

    #[test]
    fn load_or_seed_reads_back_user_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("config");
        let mut config = load_or_seed(&dir);
        config.presets[0].display_name = "Renamed".to_string();
        save(&dir, &config).unwrap();

        let reloaded = load_or_seed(&dir);
        assert_eq!(reloaded.presets[0].display_name, "Renamed");
    }

    #[test]
    fn load_or_seed_falls_back_to_defaults_on_corrupt_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("config");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(presets_path(&dir), "not valid toml {{{").unwrap();

        let loaded = load_or_seed(&dir);
        assert_eq!(loaded, SizePresetsConfig::defaults());
    }

    #[test]
    fn load_or_seed_falls_back_to_defaults_when_presets_emptied() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("config");
        let empty = SizePresetsConfig {
            safety_margin_percent: 97.0,
            presets: Vec::new(),
        };
        save(&dir, &empty).unwrap();

        let loaded = load_or_seed(&dir);
        assert!(!loaded.presets.is_empty());
    }

    #[test]
    fn restore_defaults_overwrites_user_edits() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("config");
        let mut config = load_or_seed(&dir);
        config.presets.clear();
        save(&dir, &config).unwrap();

        let restored = restore_defaults(&dir).unwrap();
        assert_eq!(restored, SizePresetsConfig::defaults());
        assert_eq!(load_or_seed(&dir), SizePresetsConfig::defaults());
    }

    #[test]
    fn check_output_size_reports_pass_and_fail() {
        let ok = check_output_size(9_000_000, 10_485_760);
        assert!(ok.passed);
        let over = check_output_size(11_000_000, 10_485_760);
        assert!(!over.passed);
    }
}
