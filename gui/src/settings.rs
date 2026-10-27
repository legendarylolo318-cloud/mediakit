//! Persisted app settings (window size, theme, output preferences), stored
//! as TOML under the per-OS config directory via the `directories` crate.
//! Loaded once at startup, saved on clean exit.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct WindowState {
    pub width: f32,
    pub height: f32,
}

impl Default for WindowState {
    fn default() -> Self {
        Self {
            width: 1100.0,
            height: 650.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ThemeChoice {
    #[default]
    System,
    Light,
    Dark,
    Nord,
    SolarizedDark,
    HighContrast,
    Pink,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 7] = [
        ThemeChoice::System,
        ThemeChoice::Light,
        ThemeChoice::Dark,
        ThemeChoice::Nord,
        ThemeChoice::SolarizedDark,
        ThemeChoice::HighContrast,
        ThemeChoice::Pink,
    ];
}

// `serde(default)` on the whole struct: a settings file from an older
// version that lacks any field (not just the `Option` ones) must still load
// rather than silently resetting everything to defaults.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct PersistedSettings {
    pub window: WindowState,
    pub theme: ThemeChoice,
    pub concurrency: Option<usize>,
    pub custom_output_folder: Option<PathBuf>,
    pub filename_template: Option<String>,
    pub overwrite_existing: Option<bool>,
    /// Where the Download tab saves finished downloads. `None` means "not
    /// chosen yet" - the GUI falls back to the OS Downloads folder.
    pub download_output_dir: Option<PathBuf>,
    /// User-supplied overrides for the Settings -> Tools page, taking
    /// priority over bundled/detected binaries. `None` means "use whatever
    /// was auto-detected."
    pub override_ffmpeg_path: Option<PathBuf>,
    pub override_ffprobe_path: Option<PathBuf>,
    pub override_ytdlp_path: Option<PathBuf>,
    /// Background check (at most once per 24 h, at launch) for a newer
    /// yt-dlp release, installing it automatically if one's found. `None`
    /// means "not set yet" and is treated as on: YouTube breaks every few
    /// weeks, so a stale yt-dlp is the most common reason downloads fail.
    pub ytdlp_auto_update_enabled: Option<bool>,
    /// Set once the user has dismissed the Download tab's one-time notice
    /// about being responsible for the target site's ToS/copyright - so it
    /// doesn't nag on every launch, only before the tab is used for the
    /// first time.
    pub download_responsibility_acknowledged: bool,

    /// Whether the YouTube PO-token server (see
    /// `mediakit_core::pot_provider`) is allowed to start at all. `None`
    /// means "not set yet" - defaults to enabled, unlike
    /// `ytdlp_auto_update_enabled`, which also defaults to on.
    pub pot_provider_enabled: Option<bool>,
    /// Port the PO-token server listens on. `None` means "use the default"
    /// (`mediakit_core::pot_provider::DEFAULT_PORT`).
    pub pot_provider_port: Option<u16>,
    /// Whether the Convert tab's Advanced section was left open.
    pub advanced_open: Option<bool>,
    /// Offer to download video links copied to the clipboard. Off by default.
    pub clipboard_watch_enabled: Option<bool>,
    /// System notification when a job or batch finishes. `None` = on.
    pub notifications_enabled: Option<bool>,
    /// Extra hosts (comma/space/newline separated) the clipboard watcher
    /// should also offer, on top of the built-in list.
    pub clipboard_extra_hosts: Option<String>,
}

fn settings_path() -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "mediakit")?;
    Some(dirs.config_dir().join("settings.toml"))
}

fn load_from_path(path: &Path) -> PersistedSettings {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_to_path(path: &Path, settings: &PersistedSettings) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = toml::to_string_pretty(settings) {
        let _ = std::fs::write(path, text);
    }
}

pub fn load() -> PersistedSettings {
    match settings_path() {
        Some(path) => load_from_path(&path),
        None => PersistedSettings::default(),
    }
}

pub fn save(settings: &PersistedSettings) {
    if let Some(path) = settings_path() {
        save_to_path(&path, settings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("does_not_exist.toml");
        assert_eq!(load_from_path(&path), PersistedSettings::default());
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("settings.toml");

        let mut settings = PersistedSettings::default();
        settings.window.width = 1440.0;
        settings.window.height = 900.0;
        settings.theme = ThemeChoice::Dark;
        settings.concurrency = Some(3);
        settings.custom_output_folder = Some(PathBuf::from("/exports"));
        settings.filename_template = Some("{name}.out.{ext}".to_string());
        settings.overwrite_existing = Some(true);
        settings.pot_provider_enabled = Some(false);
        settings.pot_provider_port = Some(9999);

        save_to_path(&path, &settings);
        let loaded = load_from_path(&path);
        assert_eq!(loaded, settings);
    }

    #[test]
    fn v0_1_2_settings_still_load() {
        // What v0.1.2's `save` wrote (`None` fields are omitted by TOML).
        let v012 = r#"
ytdlp_auto_update_enabled = false
download_responsibility_acknowledged = true
theme = "Pink"
concurrency = 2
download_output_dir = "/home/me/Downloads"
pot_provider_enabled = true

[window]
width = 1280.0
height = 720.0
"#;
        let loaded: PersistedSettings = toml::from_str(v012).unwrap();
        assert_eq!(loaded.theme, ThemeChoice::Pink);
        assert_eq!(loaded.window.width, 1280.0);
        assert_eq!(loaded.ytdlp_auto_update_enabled, Some(false));
        assert!(loaded.download_responsibility_acknowledged);
        assert_eq!(loaded.advanced_open, None);
        assert_eq!(loaded.clipboard_watch_enabled, None);
    }

    #[test]
    fn older_files_missing_non_optional_fields_keep_what_they_have() {
        let loaded: PersistedSettings = toml::from_str("theme = \"Nord\"\n").unwrap();
        assert_eq!(loaded.theme, ThemeChoice::Nord);
        assert_eq!(loaded.window, WindowState::default());
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        std::fs::write(&path, "not valid toml {{{").unwrap();
        assert_eq!(load_from_path(&path), PersistedSettings::default());
    }
}
