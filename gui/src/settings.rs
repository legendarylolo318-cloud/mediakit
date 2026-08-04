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
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 6] = [
        ThemeChoice::System,
        ThemeChoice::Light,
        ThemeChoice::Dark,
        ThemeChoice::Nord,
        ThemeChoice::SolarizedDark,
        ThemeChoice::HighContrast,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
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
    /// Weekly background check for a newer yt-dlp release, downloading and
    /// installing it automatically if one's found. Off by default - a user
    /// has to opt in to MediaKit updating a bundled tool on its own.
    pub ytdlp_auto_update_enabled: bool,
    /// Set once the user has dismissed the Download tab's one-time notice
    /// about being responsible for the target site's ToS/copyright - so it
    /// doesn't nag on every launch, only before the tab is used for the
    /// first time.
    pub download_responsibility_acknowledged: bool,
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

        save_to_path(&path, &settings);
        let loaded = load_from_path(&path);
        assert_eq!(loaded, settings);
    }

    #[test]
    fn corrupt_file_falls_back_to_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.toml");
        std::fs::write(&path, "not valid toml {{{").unwrap();
        assert_eq!(load_from_path(&path), PersistedSettings::default());
    }
}
