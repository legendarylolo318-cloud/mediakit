//! Output location/naming: same folder as source vs. a custom folder, a
//! `{name}_converted.{ext}`-style filename template, and overwrite
//! protection (auto-suffixing `(1)`, `(2)`, ... when the target exists).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputLocation {
    SameAsSource,
    Custom(PathBuf),
}

#[derive(Debug, Clone)]
pub struct OutputSettings {
    pub location: OutputLocation,
    pub filename_template: String,
    pub overwrite_existing: bool,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            location: OutputLocation::SameAsSource,
            filename_template: "{name}_converted.{ext}".to_string(),
            overwrite_existing: false,
        }
    }
}

impl OutputSettings {
    fn render_filename(&self, input: &Path, ext: &str) -> String {
        let name = input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "output".to_string());
        self.filename_template
            .replace("{name}", &name)
            .replace("{ext}", ext)
    }

    /// Resolve the final output path for `input`, auto-suffixing with
    /// ` (1)`, ` (2)`, ... if `overwrite_existing` is false and the target
    /// already exists.
    pub fn resolve(&self, input: &Path, ext: &str) -> PathBuf {
        let dir = match &self.location {
            OutputLocation::SameAsSource => input
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".")),
            OutputLocation::Custom(dir) => dir.clone(),
        };

        let filename = self.render_filename(input, ext);
        let candidate = dir.join(&filename);

        if self.overwrite_existing || !candidate.exists() {
            return candidate;
        }

        let stem = Path::new(&filename)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| filename.clone());
        let candidate_ext = Path::new(&filename)
            .extension()
            .map(|e| e.to_string_lossy().into_owned());

        for n in 1..10_000 {
            let numbered = match &candidate_ext {
                Some(e) => format!("{stem} ({n}).{e}"),
                None => format!("{stem} ({n})"),
            };
            let path = dir.join(numbered);
            if !path.exists() {
                return path;
            }
        }
        candidate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_as_source_places_output_next_to_input() {
        let settings = OutputSettings {
            location: OutputLocation::SameAsSource,
            filename_template: "{name}_converted.{ext}".to_string(),
            overwrite_existing: true,
        };
        let out = settings.resolve(Path::new("/videos/clip.mov"), "mp4");
        assert_eq!(out, PathBuf::from("/videos/clip_converted.mp4"));
    }

    #[test]
    fn custom_folder_overrides_directory() {
        let settings = OutputSettings {
            location: OutputLocation::Custom(PathBuf::from("/exports")),
            filename_template: "{name}.{ext}".to_string(),
            overwrite_existing: true,
        };
        let out = settings.resolve(Path::new("/videos/clip.mov"), "gif");
        assert_eq!(out, PathBuf::from("/exports/clip.gif"));
    }

    #[test]
    fn overwrite_protection_appends_numbered_suffix() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("clip.mov");
        std::fs::write(&input, b"x").unwrap();
        let existing = tmp.path().join("clip_converted.mp4");
        std::fs::write(&existing, b"x").unwrap();

        let settings = OutputSettings {
            location: OutputLocation::SameAsSource,
            filename_template: "{name}_converted.{ext}".to_string(),
            overwrite_existing: false,
        };
        let out = settings.resolve(&input, "mp4");
        assert_eq!(out, tmp.path().join("clip_converted (1).mp4"));
    }

    #[test]
    fn overwrite_enabled_reuses_existing_name() {
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("clip.mov");
        std::fs::write(&input, b"x").unwrap();
        let existing = tmp.path().join("clip_converted.mp4");
        std::fs::write(&existing, b"x").unwrap();

        let settings = OutputSettings {
            location: OutputLocation::SameAsSource,
            filename_template: "{name}_converted.{ext}".to_string(),
            overwrite_existing: true,
        };
        let out = settings.resolve(&input, "mp4");
        assert_eq!(out, existing);
    }
}
