//! Downloads, verifies, and embeds the vendored ffmpeg/ffprobe/yt-dlp
//! binaries described in `vendor.toml` (only when the `bundled` feature is
//! active - `slim` builds skip this entirely and rely on PATH/manual
//! detection at runtime instead).
//!
//! Downloads are cached under `$CARGO_HOME/mediakit-vendor/`, keyed by
//! `<name>-<version>-<sha256 prefix>`, rather than in `OUT_DIR` - `OUT_DIR`
//! is per profile/target and gets wiped constantly, which is what used to
//! make a single `cargo test --all-targets` invocation (lib, test binary,
//! and any other target, each with their own `OUT_DIR`) redownload the same
//! archive multiple times. Keying on version *and* checksum means a
//! `vendor.toml` update to a new pinned release never collides with - or
//! reuses - a stale cache entry from the old one.
//!
//! Set `MEDIAKIT_VENDOR_DIR` to a directory of pre-downloaded,
//! correctly-named archives to build fully offline. Set
//! `MEDIAKIT_FFMPEG_URL` / `MEDIAKIT_YTDLP_URL` to fetch from a mirror
//! instead of the pinned upstream URL when a release tag has been pruned -
//! the downloaded bytes still have to match the pinned SHA-256, so this only
//! ever substitutes *where* the exact same file comes from, never *what* it
//! is.

use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
struct VendorManifest {
    ffmpeg: HashMap<String, VendorEntry>,
    ytdlp: HashMap<String, VendorEntry>,
}

#[derive(Debug, Deserialize)]
struct VendorEntry {
    version: String,
    url: String,
    sha256: String,
    archive_kind: String,
    #[serde(default)]
    ffmpeg_path_in_archive: Option<String>,
    #[serde(default)]
    ffprobe_path_in_archive: Option<String>,
    #[serde(default)]
    license: Option<String>,
    // Record-keeping only (see the module doc on `vendor.toml` for why these
    // matter): not read here, but `#[serde(default)]` so old-shaped entries
    // during a transition don't fail to parse.
    #[serde(default)]
    #[allow(dead_code)]
    release_tag: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    date_pinned: Option<String>,
}

fn main() {
    println!("cargo:rerun-if-changed=vendor.toml");
    println!("cargo:rerun-if-env-changed=MEDIAKIT_VENDOR_DIR");
    println!("cargo:rerun-if-env-changed=MEDIAKIT_FFMPEG_URL");
    println!("cargo:rerun-if-env-changed=MEDIAKIT_YTDLP_URL");
    println!("cargo:rerun-if-env-changed=MEDIAKIT_VENDOR_VERBOSE");
    println!("cargo:rerun-if-env-changed=CARGO_HOME");

    let bundled = std::env::var("CARGO_FEATURE_BUNDLED").is_ok();
    if !bundled {
        // Slim build: nothing to download or embed. `core::vendor`'s
        // `#[cfg(feature = "bundled")]` module is compiled out entirely, so
        // it never tries to `include_bytes!` the files we'd otherwise
        // produce here.
        return;
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").expect("CARGO_CFG_TARGET_OS not set");
    let target_arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").expect("CARGO_CFG_TARGET_ARCH not set");
    let platform_key = match (target_os.as_str(), target_arch.as_str()) {
        ("linux", "x86_64") => "linux-x86_64",
        ("windows", "x86_64") => "windows-x86_64",
        _ => {
            println!(
                "cargo:warning=mediakit-core: no vendored binaries pinned for {target_os}-{target_arch}; \
                 building without bundling. Runtime will fall back to PATH/manual detection."
            );
            return;
        }
    };

    let manifest_dir =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"));
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR not set"));
    let cache_root = vendor_cache_root();
    fs::create_dir_all(&cache_root)
        .unwrap_or_else(|e| panic!("create vendor cache dir {}: {e}", cache_root.display()));

    let vendor_toml_path = manifest_dir.join("vendor.toml");
    let vendor_toml_text = fs::read_to_string(&vendor_toml_path).expect("read core/vendor.toml");
    let vendor: VendorManifest = toml::from_str(&vendor_toml_text).expect("parse core/vendor.toml");

    let ffmpeg_entry = vendor
        .ffmpeg
        .get(platform_key)
        .unwrap_or_else(|| panic!("vendor.toml has no [ffmpeg.{platform_key}] entry"));
    let ytdlp_entry = vendor
        .ytdlp
        .get(platform_key)
        .unwrap_or_else(|| panic!("vendor.toml has no [ytdlp.{platform_key}] entry"));

    let ffmpeg_url =
        std::env::var("MEDIAKIT_FFMPEG_URL").unwrap_or_else(|_| ffmpeg_entry.url.clone());
    let ytdlp_url = std::env::var("MEDIAKIT_YTDLP_URL").unwrap_or_else(|_| ytdlp_entry.url.clone());

    let ffmpeg_archive = fetch_and_verify("ffmpeg", ffmpeg_entry, &ffmpeg_url, &cache_root);
    let ytdlp_raw = fetch_and_verify("yt-dlp", ytdlp_entry, &ytdlp_url, &cache_root);

    let (ffmpeg_bin, ffprobe_bin) = extract_ffmpeg(&ffmpeg_archive, ffmpeg_entry);

    write_compressed(&out_dir.join("ffmpeg.zst"), &ffmpeg_bin);
    write_compressed(&out_dir.join("ffprobe.zst"), &ffprobe_bin);
    write_compressed(&out_dir.join("ytdlp.zst"), &ytdlp_raw);

    let generated = format!(
        "// @generated by build.rs - do not edit.\n\
         pub const FFMPEG_VERSION: &str = {:?};\n\
         pub const FFMPEG_SHA256: &str = {:?};\n\
         pub const FFMPEG_LICENSE: &str = {:?};\n\
         pub const YTDLP_VERSION: &str = {:?};\n\
         pub const YTDLP_SHA256: &str = {:?};\n\
         pub const YTDLP_LICENSE: &str = {:?};\n",
        ffmpeg_entry.version,
        ffmpeg_entry.sha256,
        ffmpeg_entry.license.as_deref().unwrap_or("unknown"),
        ytdlp_entry.version,
        ytdlp_entry.sha256,
        ytdlp_entry.license.as_deref().unwrap_or("unknown"),
    );
    fs::write(out_dir.join("vendor_generated.rs"), generated).expect("write vendor_generated.rs");

    log(&format!(
        "bundling ffmpeg {} and yt-dlp {} ({platform_key})",
        ffmpeg_entry.version, ytdlp_entry.version
    ));
}

/// Routine progress output. Plain `println!` (not `cargo:warning=...`) would
/// have cargo silently swallow it anyway since it doesn't start with
/// `cargo:`, so this goes to stderr instead - visible with `-vv` or on
/// failure, exactly like other build-script chatter, and opt-in-visible via
/// `MEDIAKIT_VENDOR_VERBOSE` for anyone actively debugging the vendoring
/// step. Normal builds doing entirely routine work (cache hit or a clean
/// download) shouldn't print `warning:` lines - those should be reserved for
/// things that actually warrant a user's attention.
fn log(msg: &str) {
    if std::env::var("MEDIAKIT_VENDOR_VERBOSE").is_ok() {
        println!("cargo:warning=mediakit-core: {msg}");
    } else {
        eprintln!("mediakit-core build: {msg}");
    }
}

/// `$CARGO_HOME/mediakit-vendor`. `CARGO_HOME` itself is only set in the
/// build script's environment if the invoking user explicitly exported it
/// (cargo does not inject it) - the `CARGO` env var (path to the running
/// cargo binary) is tempting as a fallback, but is *not* reliable here: a
/// system-package cargo install (e.g. `/usr/bin/cargo` via a distro
/// package) still uses `~/.cargo` as its home, with no relation to the
/// binary's own path, unlike a rustup-managed `~/.cargo/bin/cargo`. So the
/// fallback matches cargo's own documented default instead: `$HOME/.cargo`
/// (`%USERPROFILE%\.cargo` on Windows).
fn vendor_cache_root() -> PathBuf {
    if let Ok(dir) = std::env::var("CARGO_HOME") {
        return PathBuf::from(dir).join("mediakit-vendor");
    }
    let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let home = std::env::var(home_var)
        .unwrap_or_else(|_| panic!("could not determine home directory ({home_var} not set)"));
    Path::new(&home).join(".cargo").join("mediakit-vendor")
}

fn write_compressed(path: &Path, data: &[u8]) {
    let compressed = zstd::encode_all(data, 19).expect("zstd compress vendored binary");
    fs::write(path, compressed).expect("write compressed vendored binary");
}

/// Download (or reuse a cached copy of) `url`, verify its SHA-256 against
/// `entry.sha256`, and return the raw bytes. Panics loudly - with the URL
/// and both hashes - on any checksum mismatch or download failure: a
/// corrupted, tampered, or merely-wrong vendored binary must never silently
/// ship.
fn fetch_and_verify(name: &str, entry: &VendorEntry, url: &str, cache_root: &Path) -> Vec<u8> {
    if let Ok(manual_dir) = std::env::var("MEDIAKIT_VENDOR_DIR") {
        let filename = url.rsplit('/').next().expect("vendor url has no filename");
        let manual_path = PathBuf::from(manual_dir).join(filename);
        let bytes = fs::read(&manual_path).unwrap_or_else(|e| {
            panic!(
                "MEDIAKIT_VENDOR_DIR is set but couldn't read {}: {e}",
                manual_path.display()
            )
        });
        verify_checksum(&bytes, entry, url);
        return bytes;
    }

    // Keyed on name + version + a checksum prefix, not just the URL's
    // filename: a `vendor.toml` bump to a new pinned release (new version,
    // new hash) must never resolve to a stale cache entry, and a cache hit
    // must never be trusted without knowing the exact bytes it was verified
    // against.
    let sha_prefix = &entry.sha256[..entry.sha256.len().min(16)];
    let entry_dir = cache_root.join(format!("{name}-{}-{sha_prefix}", entry.version));
    let filename = url.rsplit('/').next().expect("vendor url has no filename");
    let cache_path = entry_dir.join(filename);

    // Fast path: a fully-written cache file is only ever placed via an
    // atomic rename (see below), so if it's present and its checksum
    // matches, it's safe to reuse without taking the lock at all - reads
    // never race writes into existence, only into non-existence-then-full.
    if let Ok(bytes) = fs::read(&cache_path) {
        if sha256_hex(&bytes) == entry.sha256 {
            log(&format!("using cached {name} ({})", entry.version));
            return bytes;
        }
    }

    fs::create_dir_all(&entry_dir)
        .unwrap_or_else(|e| panic!("create vendor cache dir {}: {e}", entry_dir.display()));
    let _lock = FileLock::acquire(entry_dir.join(".lock"));

    // Re-check under the lock: another concurrent build (e.g. `cargo test`
    // building the lib and a separate test binary target at once) may have
    // finished the download while we were waiting for it.
    if let Ok(bytes) = fs::read(&cache_path) {
        if sha256_hex(&bytes) == entry.sha256 {
            log(&format!("using cached {name} ({})", entry.version));
            return bytes;
        }
    }

    log(&format!("downloading {name} {} from {url}", entry.version));
    let bytes = download(url);
    verify_checksum(&bytes, entry, url);

    // Write-then-rename so a build killed mid-download can never leave a
    // partial file that a later run's cache-hit check treats as valid.
    let tmp_path = entry_dir.join(format!("{filename}.tmp.{}", std::process::id()));
    fs::write(&tmp_path, &bytes)
        .unwrap_or_else(|e| panic!("write temp download file {}: {e}", tmp_path.display()));
    fs::rename(&tmp_path, &cache_path).unwrap_or_else(|e| {
        panic!(
            "move downloaded file into place at {}: {e}",
            cache_path.display()
        )
    });

    bytes
}

fn verify_checksum(bytes: &[u8], entry: &VendorEntry, url: &str) {
    let actual_sha256 = sha256_hex(bytes);
    if actual_sha256 != entry.sha256 {
        panic!(
            "checksum mismatch downloading vendored binary\n  \
             url:      {url}\n  \
             expected: {}\n  \
             actual:   {actual_sha256}\n\
             Refusing to bundle an unverified binary. If the upstream release \
             was legitimately updated in place, re-verify the new checksum \
             from the publisher's own checksums file and update vendor.toml. \
             If a pinned tag was pruned, try MEDIAKIT_FFMPEG_URL / \
             MEDIAKIT_YTDLP_URL to point at a mirror serving the same bytes.",
            entry.sha256
        );
    }
}

fn download(url: &str) -> Vec<u8> {
    let mut response = ureq::get(url)
        .call()
        .unwrap_or_else(|e| panic!("failed to download {url}: {e}"));
    let mut buf = Vec::new();
    response
        .body_mut()
        .as_reader()
        .read_to_end(&mut buf)
        .unwrap_or_else(|e| panic!("failed to read response body from {url}: {e}"));
    buf
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_encode(&hasher.finalize())
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

/// A cross-process advisory lock built from a plain marker file rather than
/// OS file-locking primitives, so downloading the same pinned binary from
/// two cargo target builds running concurrently (e.g. `cargo test
/// --all-targets` building the lib and a separately-profiled test binary at
/// once) can't race into a half-written cache entry. `create_new` is
/// atomic - only one concurrent caller can ever succeed in creating the
/// file - which is all the mutual exclusion this needs.
struct FileLock {
    path: PathBuf,
}

impl FileLock {
    fn acquire(path: PathBuf) -> Self {
        const STALE_AFTER: Duration = Duration::from_secs(300);
        let started = Instant::now();
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = write!(f, "{}", std::process::id());
                    return Self { path };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if started.elapsed() > STALE_AFTER {
                        // A prior build process almost certainly crashed
                        // (or was killed) while holding this lock rather
                        // than releasing it - waiting forever would hang
                        // every subsequent build. Reclaim it and retry.
                        println!(
                            "cargo:warning=mediakit-core: vendor cache lock at {} is over 5 minutes \
                             old; assuming it's stale from a killed build and reclaiming it",
                            path.display()
                        );
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(e) => panic!("failed to create vendor cache lock {}: {e}", path.display()),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Extract the `ffmpeg`/`ffprobe` binaries from a downloaded archive.
fn extract_ffmpeg(archive_bytes: &[u8], entry: &VendorEntry) -> (Vec<u8>, Vec<u8>) {
    let ffmpeg_path = entry
        .ffmpeg_path_in_archive
        .as_deref()
        .expect("ffmpeg entry missing ffmpeg_path_in_archive");
    let ffprobe_path = entry
        .ffprobe_path_in_archive
        .as_deref()
        .expect("ffmpeg entry missing ffprobe_path_in_archive");

    match entry.archive_kind.as_str() {
        "tar_xz" => {
            let ffmpeg = extract_from_tar_xz(archive_bytes, ffmpeg_path);
            let ffprobe = extract_from_tar_xz(archive_bytes, ffprobe_path);
            (ffmpeg, ffprobe)
        }
        "zip" => {
            let ffmpeg = extract_from_zip(archive_bytes, ffmpeg_path);
            let ffprobe = extract_from_zip(archive_bytes, ffprobe_path);
            (ffmpeg, ffprobe)
        }
        other => panic!("unknown archive_kind {other:?} in vendor.toml"),
    }
}

fn extract_from_tar_xz(archive_bytes: &[u8], entry_path: &str) -> Vec<u8> {
    let decompressed = xz2::read::XzDecoder::new(archive_bytes);
    let mut archive = tar::Archive::new(decompressed);
    for file in archive.entries().expect("read tar entries") {
        let mut file = file.expect("read tar entry");
        let path = file
            .path()
            .expect("tar entry path")
            .to_string_lossy()
            .into_owned();
        if path == entry_path {
            let mut buf = Vec::new();
            file.read_to_end(&mut buf).expect("read tar entry contents");
            return buf;
        }
    }
    panic!("archive did not contain expected entry {entry_path:?}");
}

fn extract_from_zip(archive_bytes: &[u8], entry_path: &str) -> Vec<u8> {
    let cursor = std::io::Cursor::new(archive_bytes);
    let mut archive = zip::ZipArchive::new(cursor).expect("read zip archive");
    let mut file = archive
        .by_name(entry_path)
        .unwrap_or_else(|_| panic!("zip archive did not contain expected entry {entry_path:?}"));
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).expect("read zip entry contents");
    buf
}
