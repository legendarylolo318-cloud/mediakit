//! Vendored ffmpeg/ffprobe/yt-dlp binaries: compressed and embedded into
//! the binary at build time (see `build.rs` and `vendor.toml`) when the
//! `bundled` feature is active, extracted to the app data directory on
//! first run - or whenever the embedded version differs from what's
//! recorded in `manifest.json` - and never touched again after that.
//!
//! `slim` builds (`--no-default-features`) compile this module's embedded
//! data out entirely; [`ensure_extracted`] becomes a no-op returning
//! `Ok(None)`, and callers fall back to PATH/manual detection instead.

#[cfg(feature = "bundled")]
use crate::error::CoreError;
use crate::error::CoreResult;
#[cfg(feature = "bundled")]
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(feature = "bundled")]
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/vendor_generated.rs"));
    pub static FFMPEG_ZST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ffmpeg.zst"));
    pub static FFPROBE_ZST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ffprobe.zst"));
    pub static YTDLP_ZST: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/ytdlp.zst"));
}

#[cfg(feature = "bundled")]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct VendorManifestFile {
    ffmpeg_version: String,
    ffmpeg_sha256: String,
    ytdlp_version: String,
    ytdlp_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendoredPaths {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub ytdlp: PathBuf,
    pub ffmpeg_version: String,
    pub ytdlp_version: String,
}

/// License identifiers for the currently-embedded vendored binaries (e.g.
/// `"GPL-3.0-or-later"` for ffmpeg, `"Unlicense"` for yt-dlp) - used by the
/// in-app Licenses dialog. `None` in `slim` builds, where nothing is
/// embedded.
#[cfg(feature = "bundled")]
pub fn embedded_licenses() -> Option<(&'static str, &'static str)> {
    Some((embedded::FFMPEG_LICENSE, embedded::YTDLP_LICENSE))
}

#[cfg(not(feature = "bundled"))]
pub fn embedded_licenses() -> Option<(&'static str, &'static str)> {
    None
}

#[cfg(feature = "bundled")]
fn binary_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

#[cfg(feature = "bundled")]
fn manifest_path(bin_dir: &Path) -> PathBuf {
    bin_dir.join("vendor_manifest.json")
}

#[cfg(feature = "bundled")]
fn wanted_manifest() -> VendorManifestFile {
    VendorManifestFile {
        ffmpeg_version: embedded::FFMPEG_VERSION.to_string(),
        ffmpeg_sha256: embedded::FFMPEG_SHA256.to_string(),
        ytdlp_version: embedded::YTDLP_VERSION.to_string(),
        ytdlp_sha256: embedded::YTDLP_SHA256.to_string(),
    }
}

/// Ensure the vendored binaries are present and current in `bin_dir`
/// (creating it if necessary), re-extracting only if the embedded version
/// differs from `vendor_manifest.json`. Fast no-op on every launch after
/// the first (or after an update ships a newer pinned version).
///
/// Returns `Ok(None)` in `slim` builds, where there is nothing embedded.
#[cfg(feature = "bundled")]
pub fn ensure_extracted(bin_dir: &Path) -> CoreResult<Option<VendoredPaths>> {
    std::fs::create_dir_all(bin_dir).map_err(|source| CoreError::Io {
        path: bin_dir.to_path_buf(),
        source,
    })?;

    let wanted = wanted_manifest();
    let manifest_file = manifest_path(bin_dir);
    let existing: Option<VendorManifestFile> = std::fs::read(&manifest_file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());

    let ffmpeg_path = bin_dir.join(binary_name("ffmpeg"));
    let ffprobe_path = bin_dir.join(binary_name("ffprobe"));
    let ytdlp_path = bin_dir.join(binary_name("yt-dlp"));

    let up_to_date = existing.as_ref() == Some(&wanted)
        && ffmpeg_path.is_file()
        && ffprobe_path.is_file()
        && ytdlp_path.is_file();

    if !up_to_date {
        extract_one(embedded::FFMPEG_ZST, &ffmpeg_path)?;
        extract_one(embedded::FFPROBE_ZST, &ffprobe_path)?;
        extract_one(embedded::YTDLP_ZST, &ytdlp_path)?;

        let json = serde_json::to_vec_pretty(&wanted)
            .map_err(|e| CoreError::Other(anyhow::Error::new(e)))?;
        std::fs::write(&manifest_file, json).map_err(|source| CoreError::Io {
            path: manifest_file,
            source,
        })?;
    }

    Ok(Some(VendoredPaths {
        ffmpeg: ffmpeg_path,
        ffprobe: ffprobe_path,
        ytdlp: ytdlp_path,
        ffmpeg_version: wanted.ffmpeg_version,
        ytdlp_version: wanted.ytdlp_version,
    }))
}

/// Force a fresh extraction regardless of whether `vendor_manifest.json`
/// already matches - for the Settings -> Tools "Re-extract" button, in case
/// a user has deleted/corrupted an extracted binary by hand.
#[cfg(feature = "bundled")]
pub fn force_reextract(bin_dir: &Path) -> CoreResult<Option<VendoredPaths>> {
    let _ = std::fs::remove_file(manifest_path(bin_dir));
    ensure_extracted(bin_dir)
}

#[cfg(not(feature = "bundled"))]
pub fn force_reextract(_bin_dir: &Path) -> CoreResult<Option<VendoredPaths>> {
    Ok(None)
}

#[cfg(feature = "bundled")]
fn extract_one(compressed: &[u8], dest: &Path) -> CoreResult<()> {
    let data = zstd::decode_all(compressed).map_err(|e| CoreError::Other(anyhow::Error::new(e)))?;
    std::fs::write(dest, &data).map_err(|source| CoreError::Io {
        path: dest.to_path_buf(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(dest)
            .map_err(|source| CoreError::Io {
                path: dest.to_path_buf(),
                source,
            })?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(dest, perms).map_err(|source| CoreError::Io {
            path: dest.to_path_buf(),
            source,
        })?;
    }

    Ok(())
}

#[cfg(not(feature = "bundled"))]
pub fn ensure_extracted(_bin_dir: &Path) -> CoreResult<Option<VendoredPaths>> {
    Ok(None)
}

#[cfg(all(test, feature = "bundled"))]
mod tests {
    use super::*;

    #[test]
    fn extracts_binaries_and_writes_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");

        let paths = ensure_extracted(&bin_dir).unwrap().expect("bundled build");
        assert!(paths.ffmpeg.is_file());
        assert!(paths.ffprobe.is_file());
        assert!(paths.ytdlp.is_file());
        assert!(manifest_path(&bin_dir).is_file());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&paths.ffmpeg)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn second_call_is_a_fast_noop_reusing_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");

        let first = ensure_extracted(&bin_dir).unwrap().unwrap();
        let mtime_before = std::fs::metadata(&first.ffmpeg)
            .unwrap()
            .modified()
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(10));
        let second = ensure_extracted(&bin_dir).unwrap().unwrap();
        let mtime_after = std::fs::metadata(&second.ffmpeg)
            .unwrap()
            .modified()
            .unwrap();

        assert_eq!(
            mtime_before, mtime_after,
            "should not re-extract when already current"
        );
    }

    #[test]
    fn re_extracts_when_manifest_is_stale() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();

        let stale = VendorManifestFile {
            ffmpeg_version: "0.0.0-stale".to_string(),
            ffmpeg_sha256: "deadbeef".to_string(),
            ytdlp_version: "0.0.0-stale".to_string(),
            ytdlp_sha256: "deadbeef".to_string(),
        };
        std::fs::write(
            manifest_path(&bin_dir),
            serde_json::to_vec_pretty(&stale).unwrap(),
        )
        .unwrap();

        let paths = ensure_extracted(&bin_dir).unwrap().unwrap();
        assert_ne!(paths.ffmpeg_version, "0.0.0-stale");
    }

    #[test]
    fn force_reextract_rewrites_even_when_already_current() {
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");

        let first = ensure_extracted(&bin_dir).unwrap().unwrap();
        // Corrupt the extracted binary by hand, simulating what the
        // "Re-extract" button in Settings -> Tools is for.
        std::fs::write(&first.ffmpeg, b"corrupted").unwrap();
        assert_eq!(std::fs::metadata(&first.ffmpeg).unwrap().len(), 9);

        let restored = force_reextract(&bin_dir).unwrap().unwrap();
        assert!(std::fs::metadata(&restored.ffmpeg).unwrap().len() > 1000);
    }
}
