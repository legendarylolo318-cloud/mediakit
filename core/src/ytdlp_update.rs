//! Self-update for the bundled yt-dlp binary: check GitHub's "latest
//! release" against the running version, download + checksum-verify the
//! platform binary, and atomically swap it into place with a rollback copy
//! kept alongside it.
//!
//! This only ever updates yt-dlp - ffmpeg/ffprobe stay pinned to whatever
//! `vendor.toml` shipped with this build, since yt-dlp is the one tool here
//! that goes stale on its own (sites change constantly; ffmpeg does not).

use crate::error::{CoreError, CoreResult};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const RELEASES_LATEST_URL: &str = "https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest";
const USER_AGENT: &str = concat!("MediaKit/", env!("CARGO_PKG_VERSION"));

fn asset_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp_linux"
    }
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "yt-dlp.exe"
    } else {
        "yt-dlp"
    }
}

/// What GitHub's "latest release" says about yt-dlp right now, resolved down
/// to this platform's specific asset and its published checksum.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseInfo {
    pub version: String,
    pub published_at: String,
    pub download_url: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateState {
    pub last_checked_unix: Option<u64>,
    pub latest_known_version: Option<String>,
    pub latest_known_published_at: Option<String>,
}

fn state_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join("ytdlp_update_state.json")
}

pub fn load_state(app_data_dir: &Path) -> UpdateState {
    std::fs::read(state_path(app_data_dir))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_state(app_data_dir: &Path, state: &UpdateState) {
    if let Ok(json) = serde_json::to_vec_pretty(state) {
        let _ = std::fs::write(state_path(app_data_dir), json);
    }
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    published_at: String,
    assets: Vec<GhAsset>,
}

/// Hit GitHub's API for yt-dlp's latest release, then resolve this
/// platform's binary asset and its published SHA-256 (from the release's
/// `SHA2-256SUMS` asset, never trusted from anywhere else).
pub fn fetch_latest_release_info() -> CoreResult<ReleaseInfo> {
    let release: GhRelease = ureq::get(RELEASES_LATEST_URL)
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| CoreError::Download(format!("checking yt-dlp releases failed: {e}")))?
        .body_mut()
        .read_json()
        .map_err(|e| CoreError::Download(format!("parsing GitHub release response: {e}")))?;

    let wanted = asset_name();
    let binary_asset = release
        .assets
        .iter()
        .find(|a| a.name == wanted)
        .ok_or_else(|| CoreError::Download(format!("no '{wanted}' asset in latest release")))?;

    let sums_asset = release
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case("SHA2-256SUMS"))
        .ok_or_else(|| CoreError::Download("no SHA2-256SUMS asset in latest release".into()))?;

    let sums_text = fetch_text(&sums_asset.browser_download_url)?;
    let sha256 = parse_sha256sums(&sums_text, wanted).ok_or_else(|| {
        CoreError::Download(format!("no checksum for '{wanted}' in SHA2-256SUMS"))
    })?;

    Ok(ReleaseInfo {
        version: release.tag_name,
        published_at: release.published_at,
        download_url: binary_asset.browser_download_url.clone(),
        sha256,
    })
}

fn fetch_text(url: &str) -> CoreResult<String> {
    let mut body = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| CoreError::Download(format!("downloading {url} failed: {e}")))?
        .into_body();
    let text = body
        .read_to_string()
        .map_err(|e| CoreError::Download(format!("reading {url} failed: {e}")))?;
    Ok(text)
}

/// Parse a `sha256sum`-style checksums file (`<hex>  <filename>` per line,
/// possibly `*filename` for binary mode) and return the hash for `filename`.
fn parse_sha256sums(text: &str, filename: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == filename).then(|| hash.to_lowercase())
    })
}

/// Download `release`'s binary to `dest`, verifying its SHA-256 against the
/// checksum GitHub's release published. Fails loudly (and does not leave a
/// half-written file at `dest`) on any mismatch.
pub fn download_and_verify(release: &ReleaseInfo, dest: &Path) -> CoreResult<()> {
    let tmp = dest.with_extension("part");

    let mut body = ureq::get(&release.download_url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| CoreError::Download(format!("downloading yt-dlp failed: {e}")))?
        .into_body();

    let mut bytes = Vec::new();
    body.as_reader()
        .read_to_end(&mut bytes)
        .map_err(|e| CoreError::Download(format!("reading yt-dlp download: {e}")))?;

    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex_encode(&hasher.finalize());
    if !actual.eq_ignore_ascii_case(&release.sha256) {
        return Err(CoreError::Download(format!(
            "checksum mismatch for yt-dlp {}: expected {}, got {actual}",
            release.version, release.sha256
        )));
    }

    std::fs::write(&tmp, &bytes).map_err(|source| CoreError::Io {
        path: tmp.clone(),
        source,
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&tmp)
            .map_err(|source| CoreError::Io {
                path: tmp.clone(),
                source,
            })?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tmp, perms).map_err(|source| CoreError::Io {
            path: tmp.clone(),
            source,
        })?;
    }

    std::fs::rename(&tmp, dest).map_err(|source| CoreError::Io {
        path: dest.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn previous_path(bin_dir: &Path) -> PathBuf {
    bin_dir.join(format!("{}.previous", binary_name()))
}

/// Download + verify the latest yt-dlp into `bin_dir`, keeping the
/// currently-installed binary around as `<name>.previous` so
/// [`rollback`] can restore it. Returns the new binary's path.
pub fn perform_update(bin_dir: &Path) -> CoreResult<PathBuf> {
    let release = fetch_latest_release_info()?;
    let current = bin_dir.join(binary_name());
    let new_file = bin_dir.join(format!("{}.new", binary_name()));

    download_and_verify(&release, &new_file)?;

    if current.is_file() {
        let previous = previous_path(bin_dir);
        let _ = std::fs::remove_file(&previous);
        std::fs::rename(&current, &previous).map_err(|source| CoreError::Io {
            path: current.clone(),
            source,
        })?;
    }
    std::fs::rename(&new_file, &current).map_err(|source| CoreError::Io {
        path: current.clone(),
        source,
    })?;

    let mut state = load_state(bin_dir);
    state.last_checked_unix = Some(now_unix());
    state.latest_known_version = Some(release.version);
    state.latest_known_published_at = Some(release.published_at);
    save_state(bin_dir, &state);

    Ok(current)
}

/// Restore the binary saved by the most recent [`perform_update`] call,
/// undoing it. Errors if there is nothing to roll back to.
pub fn rollback(bin_dir: &Path) -> CoreResult<PathBuf> {
    let previous = previous_path(bin_dir);
    if !previous.is_file() {
        return Err(CoreError::Download(
            "no previous yt-dlp version to roll back to".to_string(),
        ));
    }
    let current = bin_dir.join(binary_name());
    let _ = std::fs::remove_file(&current);
    std::fs::rename(&previous, &current).map_err(|source| CoreError::Io {
        path: current.clone(),
        source,
    })?;
    Ok(current)
}

/// Check whether a newer yt-dlp is available without downloading it,
/// recording the check in [`UpdateState`] regardless of the outcome (so
/// staleness warnings and "last checked" UI stay accurate even when the
/// running version is already current).
pub fn check_for_update(bin_dir: &Path, current_version: &str) -> CoreResult<bool> {
    let release = fetch_latest_release_info()?;
    let mut state = load_state(bin_dir);
    state.last_checked_unix = Some(now_unix());
    state.latest_known_version = Some(release.version.clone());
    state.latest_known_published_at = Some(release.published_at.clone());
    save_state(bin_dir, &state);
    Ok(release.version != current_version)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Days between an RFC 3339 timestamp (as GitHub returns, e.g.
/// `"2026-07-04T10:00:00Z"`) and now. `None` if the timestamp can't be
/// parsed. Implemented by hand (no date/time crate) since this is the only
/// place MediaKit needs calendar math.
pub fn days_since(rfc3339: &str) -> Option<i64> {
    let then_unix = parse_rfc3339_unix(rfc3339)?;
    let now = now_unix() as i64;
    Some((now - then_unix as i64) / 86_400)
}

fn parse_rfc3339_unix(s: &str) -> Option<u64> {
    let s = s.trim();
    let (date, time) = s.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;

    let time = time.trim_end_matches('Z');
    let time = time.split(['+', '-']).next().unwrap_or(time);
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts
        .next()
        .and_then(|s| s.split('.').next())
        .unwrap_or("0")
        .parse()
        .ok()?;

    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    if secs < 0 {
        None
    } else {
        Some(secs as u64)
    }
}

/// Howard Hinnant's `days_from_civil`: proleptic-Gregorian (year, month,
/// day) -> days since 1970-01-01, valid for the entire range MediaKit will
/// ever see (yt-dlp release dates).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sha256sums_file() {
        let text = "\
deadbeefcafebabe0000000000000000000000000000000000000000000000  yt-dlp_linux
1111111111111111111111111111111111111111111111111111111111111a  yt-dlp.exe
";
        assert_eq!(
            parse_sha256sums(text, "yt-dlp_linux"),
            Some("deadbeefcafebabe0000000000000000000000000000000000000000000000".to_string())
        );
        assert_eq!(
            parse_sha256sums(text, "yt-dlp.exe"),
            Some("1111111111111111111111111111111111111111111111111111111111111a".to_string())
        );
        assert_eq!(parse_sha256sums(text, "nonexistent"), None);
    }

    #[test]
    fn parses_sha256sums_with_binary_mode_marker() {
        let text = "abc123  *yt-dlp_linux\n";
        assert_eq!(
            parse_sha256sums(text, "yt-dlp_linux"),
            Some("abc123".to_string())
        );
    }

    #[test]
    fn days_from_civil_matches_known_epoch_offsets() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
    }

    #[test]
    fn parses_rfc3339_to_a_plausible_unix_timestamp() {
        let unix = parse_rfc3339_unix("2026-07-04T10:00:00Z").unwrap();
        assert!(unix > 1_700_000_000);
    }

    #[test]
    fn parses_rfc3339_with_fractional_seconds() {
        let unix = parse_rfc3339_unix("2026-07-04T10:00:00.123Z").unwrap();
        assert_eq!(unix, parse_rfc3339_unix("2026-07-04T10:00:00Z").unwrap());
    }

    #[test]
    fn staleness_is_positive_for_a_date_far_in_the_past() {
        let days = days_since("2020-01-01T00:00:00Z").unwrap();
        assert!(
            days > 365 * 5,
            "expected several years of staleness, got {days}"
        );
    }

    #[test]
    fn staleness_is_none_for_garbage_input() {
        assert_eq!(days_since("not-a-date"), None);
    }
}
