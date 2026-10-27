//! Optional clipboard watcher: polls the system clipboard on a background
//! thread and reports newly copied video-looking links so the UI can offer to
//! download them. Off unless the user turns it on in Settings.

use crate::paste::is_http_url;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Hosts we offer to download from out of the box.
const BUILTIN_HOSTS: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "twitter.com",
    "x.com",
    "tiktok.com",
    "instagram.com",
    "reddit.com",
    "redd.it",
    "twitch.tv",
    "vimeo.com",
    "soundcloud.com",
];

fn host_of(url: &str) -> Option<String> {
    let rest = url.split("://").nth(1)?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    let host = authority.split(':').next()?.to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// Split the user's editable host list (commas, spaces or newlines).
pub fn parse_host_list(text: &str) -> Vec<String> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .map(|h| h.trim().trim_start_matches("www.").to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect()
}

/// Whether `clip` is a single http(s) link on a built-in or user-listed host.
pub fn is_offerable(clip: &str, extra_hosts: &[String]) -> bool {
    let clip = clip.trim();
    if !is_http_url(clip) {
        return false;
    }
    let Some(host) = host_of(clip) else {
        return false;
    };
    let matches = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    BUILTIN_HOSTS.iter().any(|d| matches(d)) || extra_hosts.iter().any(|d| matches(d))
}

/// Shorten a URL for the toast, keeping the start (host) readable.
pub fn shorten(url: &str, max_chars: usize) -> String {
    if url.chars().count() <= max_chars {
        return url.to_string();
    }
    let kept: String = url.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{kept}\u{2026}")
}

pub struct ClipboardWatcher {
    stop: Arc<AtomicBool>,
    pub receiver: Receiver<String>,
    sender: Sender<String>,
}

impl ClipboardWatcher {
    pub fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        Self {
            stop: Arc::new(AtomicBool::new(true)),
            receiver,
            sender,
        }
    }

    pub fn is_running(&self) -> bool {
        !self.stop.load(Ordering::Relaxed)
    }

    /// Start polling (a no-op if already running). The clipboard's current
    /// contents count as "already seen", so enabling the toggle doesn't
    /// immediately prompt for something copied earlier. `extra_hosts` is read
    /// once per start; the caller restarts the watcher when the list changes.
    pub fn start(&mut self, extra_hosts: Vec<String>) {
        if self.is_running() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Arc::clone(&stop);
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let Ok(mut clipboard) = arboard::Clipboard::new() else {
                return;
            };
            let mut last_seen = clipboard.get_text().ok();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(POLL_INTERVAL);
                let Ok(text) = clipboard.get_text() else {
                    continue;
                };
                if last_seen.as_deref() == Some(text.as_str()) {
                    continue;
                }
                last_seen = Some(text.clone());
                if is_offerable(&text, &extra_hosts)
                    && sender.send(text.trim().to_string()).is_err()
                {
                    return;
                }
            }
        });
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn poll(&self) -> Vec<String> {
        self.receiver.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offers_supported_hosts_and_subdomains() {
        for url in [
            "https://www.youtube.com/watch?v=abc",
            "https://youtu.be/abc",
            "https://x.com/u/status/1",
            "https://vm.tiktok.com/abc",
            "https://old.reddit.com/r/x/comments/1",
            "https://soundcloud.com/a/b",
        ] {
            assert!(is_offerable(url, &[]), "{url}");
        }
    }

    #[test]
    fn ignores_other_hosts_lookalikes_and_non_links() {
        assert!(!is_offerable("https://example.com/video", &[]));
        assert!(!is_offerable("https://notyoutube.com/watch", &[]));
        assert!(!is_offerable("https://evil.com/?u=youtube.com", &[]));
        assert!(!is_offerable("watch https://youtu.be/abc", &[]));
        assert!(!is_offerable("just text", &[]));
    }

    #[test]
    fn user_list_extends_the_builtin_hosts() {
        let extra = parse_host_list("www.Bilibili.com, nicovideo.jp\nexample.org");
        assert_eq!(extra, ["bilibili.com", "nicovideo.jp", "example.org"]);
        assert!(is_offerable("https://www.bilibili.com/video/BV1", &extra));
        assert!(!is_offerable("https://bilibili.com/x", &[]));
    }

    #[test]
    fn shortens_long_urls_only() {
        assert_eq!(shorten("https://a.com/x", 40), "https://a.com/x");
        let s = shorten("https://www.youtube.com/watch?v=abcdefghijklmnop", 20);
        assert_eq!(s.chars().count(), 20);
        assert!(s.ends_with('\u{2026}'));
    }
}
