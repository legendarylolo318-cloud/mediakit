//! Per-site tweaks to the yt-dlp invocation for sites whose defaults give
//! a poor result out of the box. yt-dlp does all the actual extraction;
//! this only nudges format choice and file naming.
//!
//! Currently: TikTok.
//! - TikTok serves most videos as both H.264 and H.265 ("bytevc1"), and
//!   yt-dlp's default sort usually picks the H.265 one. That file won't
//!   play in Discord's web/desktop client, in Firefox, or on Windows
//!   without the paid HEVC extension. [`tiktok_extra_args`] prefers H.264.
//! - A TikTok "title" is the whole caption (emoji, hashtags, ...), and
//!   lots of posts share the same caption (`#fyp`), so the default
//!   `%(title)s.%(ext)s` template gives long names that overwrite each
//!   other. [`tiktok_output_template`] adds the uploader and the post id.

use crate::downloader::FormatSelection;

/// Lower-cased hostname of `url`, without userinfo or port.
fn host_of(url: &str) -> String {
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);
    host.split(':').next().unwrap_or(host).to_ascii_lowercase()
}

/// `tiktok.com` and its subdomains (`www.`, `m.`, and the `vm.`/`vt.`
/// share-link shorteners, which yt-dlp follows itself). Hostname match only.
pub fn is_tiktok_url(url: &str) -> bool {
    let host = host_of(url);
    host == "tiktok.com" || host.ends_with(".tiktok.com")
}

/// A TikTok photo/slideshow post (`/@user/photo/<id>`) rather than a video.
pub fn is_tiktok_photo_url(url: &str) -> bool {
    is_tiktok_url(url)
        && url
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .contains("/photo/")
}

/// Extra yt-dlp args for a TikTok download. With `prefer_h264`, formats are
/// sorted H.264-first (then by resolution/bitrate as usual), so the file
/// plays everywhere, sometimes at 540p instead of an H.265 720p. Custom
/// format selectors and audio-only downloads are left alone.
pub fn tiktok_extra_args(prefer_h264: bool, format: &FormatSelection) -> Vec<String> {
    let video = matches!(
        format,
        FormatSelection::Best | FormatSelection::BestUpTo1080p | FormatSelection::BestUpTo720p
    );
    if prefer_h264 && video {
        vec!["--format-sort".to_string(), "vcodec:h264".to_string()]
    } else {
        Vec::new()
    }
}

/// The filename template used for TikTok when the user hasn't set their
/// own: `uploader - caption (trimmed to 60 bytes) [post id].ext`.
pub const TIKTOK_TEMPLATE: &str = "%(uploader)s - %(title).60B [%(id)s].%(ext)s";

/// Swap the default `%(title)s.%(ext)s` file name at the end of
/// `output_template` (which may be prefixed by an output directory) for
/// [`TIKTOK_TEMPLATE`]. A custom template is returned unchanged.
pub fn tiktok_output_template(output_template: &str) -> String {
    const DEFAULT: &str = "%(title)s.%(ext)s";
    match output_template.strip_suffix(DEFAULT) {
        Some(dir) if dir.is_empty() || dir.ends_with(['/', '\\']) => {
            format!("{dir}{TIKTOK_TEMPLATE}")
        }
        _ => output_template.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_tiktok_hosts_only() {
        for url in [
            "https://www.tiktok.com/@scout2015/video/6718335390845095173",
            "https://tiktok.com/@a/video/1",
            "https://vm.tiktok.com/ZMabc123/",
            "https://vt.tiktok.com/ZSabc123/",
            "https://m.tiktok.com/v/1.html",
            "HTTPS://WWW.TIKTOK.COM:443/@a/video/1",
        ] {
            assert!(is_tiktok_url(url), "{url}");
        }
        for url in [
            "https://www.youtube.com/watch?v=tiktok.com",
            "https://nottiktok.com/@a/video/1",
            "https://tiktok.com.evil.example/@a",
            "https://example.com/?u=https://www.tiktok.com/@a/video/1",
        ] {
            assert!(!is_tiktok_url(url), "{url}");
        }
    }

    #[test]
    fn detects_photo_posts() {
        assert!(is_tiktok_photo_url(
            "https://www.tiktok.com/@a/photo/7300000000000000000"
        ));
        assert!(!is_tiktok_photo_url(
            "https://www.tiktok.com/@a/video/7300000000000000000"
        ));
        assert!(!is_tiktok_photo_url(
            "https://www.tiktok.com/@a/video/1?ref=/photo/"
        ));
        assert!(!is_tiktok_photo_url("https://example.com/@a/photo/1"));
    }

    #[test]
    fn h264_sort_only_for_video_presets() {
        let sort = vec!["--format-sort".to_string(), "vcodec:h264".to_string()];
        assert_eq!(tiktok_extra_args(true, &FormatSelection::Best), sort);
        assert_eq!(
            tiktok_extra_args(true, &FormatSelection::BestUpTo720p),
            sort
        );
        assert!(tiktok_extra_args(false, &FormatSelection::Best).is_empty());
        assert!(tiktok_extra_args(true, &FormatSelection::AudioOnlyMp3).is_empty());
        assert!(tiktok_extra_args(true, &FormatSelection::Custom("18".into())).is_empty());
    }

    #[test]
    fn only_the_default_template_is_replaced() {
        assert_eq!(
            tiktok_output_template("/home/u/Videos/%(title)s.%(ext)s"),
            format!("/home/u/Videos/{TIKTOK_TEMPLATE}")
        );
        assert_eq!(
            tiktok_output_template(r"C:\Users\u\Videos\%(title)s.%(ext)s"),
            format!(r"C:\Users\u\Videos\{TIKTOK_TEMPLATE}")
        );
        assert_eq!(tiktok_output_template("%(title)s.%(ext)s"), TIKTOK_TEMPLATE);
        for custom in ["/v/%(id)s.%(ext)s", "/v/%(uploader)s-%(title)s.%(ext)s"] {
            assert_eq!(tiktok_output_template(custom), custom);
        }
    }
}
