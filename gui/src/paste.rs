//! Decides what a paste into the app window means: links to download, files
//! to convert, or nothing we should react to.

use std::path::PathBuf;

#[derive(Debug, PartialEq, Eq)]
pub enum Paste {
    /// One or more http(s) URLs, one per entry.
    Urls(Vec<String>),
    /// Existing local files.
    Files(Vec<PathBuf>),
    Other,
}

/// True for a string that is a single http(s) URL (no whitespace, a host).
pub fn is_http_url(s: &str) -> bool {
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"));
    matches!(rest, Some(r) if !r.is_empty() && !r.starts_with('/') && !s.contains(char::is_whitespace))
}

/// Strip a `file://` prefix if present (file managers copy URIs on Linux).
fn as_path(line: &str) -> PathBuf {
    match line.strip_prefix("file://") {
        Some(rest) => PathBuf::from(rest),
        None => PathBuf::from(line),
    }
}

/// Classify pasted text. Every non-blank line must be of the same kind;
/// mixed or unrecognised content is `Other`, so ordinary prose containing a
/// link is never hijacked. `file_exists` is injected for testing.
pub fn classify(text: &str, file_exists: impl Fn(&std::path::Path) -> bool) -> Paste {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return Paste::Other;
    }
    if lines.iter().all(|l| is_http_url(l)) {
        return Paste::Urls(lines.iter().map(|l| l.to_string()).collect());
    }
    let paths: Vec<PathBuf> = lines.iter().map(|l| as_path(l)).collect();
    if paths.iter().all(|p| file_exists(p)) {
        return Paste::Files(paths);
    }
    Paste::Other
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn never(_: &Path) -> bool {
        false
    }

    #[test]
    fn single_and_multiple_urls() {
        assert_eq!(
            classify("https://youtu.be/abc\n", never),
            Paste::Urls(vec!["https://youtu.be/abc".into()])
        );
        let p = classify("https://a.com/x\n\n http://b.com/y ", never);
        assert_eq!(
            p,
            Paste::Urls(vec!["https://a.com/x".into(), "http://b.com/y".into()])
        );
    }

    #[test]
    fn prose_with_a_link_is_not_hijacked() {
        assert_eq!(classify("look at https://a.com/x ok", never), Paste::Other);
        assert_eq!(
            classify("https://a.com/x\nand some text", never),
            Paste::Other
        );
        assert_eq!(classify("", never), Paste::Other);
        assert_eq!(classify("https://", never), Paste::Other);
    }

    #[test]
    fn existing_paths_and_file_uris_are_files() {
        let exists = |p: &Path| p.starts_with("/home/me");
        assert_eq!(
            classify("/home/me/a.mp4\nfile:///home/me/b.mkv", exists),
            Paste::Files(vec!["/home/me/a.mp4".into(), "/home/me/b.mkv".into()])
        );
        assert_eq!(classify("/home/me/a.mp4\n/nope", exists), Paste::Other);
    }
}
