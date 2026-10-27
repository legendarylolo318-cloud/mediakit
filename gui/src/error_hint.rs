//! Turns a classified yt-dlp failure into one plain sentence plus the button
//! that most likely fixes it, so users aren't left parsing a traceback.

use eframe::egui;
use mediakit_core::downloader::{classify_ytdlp_error, YtdlpErrorKind};

/// What the fix button should do; the app applies it (this module has no
/// access to app state, which keeps it trivially testable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixAction {
    UpdateYtdlp,
    UseCookies,
    InstallNode,
    Retry,
}

impl FixAction {
    pub fn label(self) -> &'static str {
        match self {
            FixAction::UpdateYtdlp => "Update yt-dlp",
            FixAction::UseCookies => "Use browser cookies",
            FixAction::InstallNode => "Install Node.js",
            FixAction::Retry => "Retry",
        }
    }
}

/// One-sentence explanation and the matching fix for `kind`.
pub fn hint(kind: YtdlpErrorKind) -> (&'static str, FixAction) {
    match kind {
        YtdlpErrorKind::BotCheck => (
            "YouTube wants proof you're not a bot. Using your browser's login cookies usually fixes this.",
            FixAction::UseCookies,
        ),
        YtdlpErrorKind::Http403 => (
            "The site refused the download (HTTP 403). An out-of-date yt-dlp is the usual cause.",
            FixAction::UpdateYtdlp,
        ),
        YtdlpErrorKind::JsRuntime => (
            "YouTube's video link couldn't be unscrambled. This needs Node.js installed, plus an up-to-date yt-dlp.",
            FixAction::InstallNode,
        ),
        YtdlpErrorKind::LoginRequired => (
            "This video needs you to be signed in (age-restricted or members-only). Use your browser's cookies.",
            FixAction::UseCookies,
        ),
        YtdlpErrorKind::Unavailable => (
            "This video is private, removed or unavailable in your region.",
            FixAction::Retry,
        ),
        YtdlpErrorKind::UnsupportedUrl => (
            "yt-dlp doesn't recognise this link. Check it points at a video page.",
            FixAction::Retry,
        ),
        YtdlpErrorKind::Network => (
            "Couldn't reach the site. Check your connection and try again.",
            FixAction::Retry,
        ),
        YtdlpErrorKind::Unknown => (
            "Something went wrong. Updating yt-dlp fixes most breakages; the details below say more.",
            FixAction::UpdateYtdlp,
        ),
    }
}

/// Render the sentence and fix button for a failure whose text is `raw`
/// (stderr or an error string embedding it). Returns the clicked action.
pub fn show(ui: &mut egui::Ui, raw: &str) -> Option<FixAction> {
    let (sentence, action) = hint(classify_ytdlp_error(raw));
    let mut clicked = None;
    ui.vertical(|ui| {
        ui.colored_label(egui::Color32::from_rgb(220, 90, 90), sentence);
        ui.horizontal(|ui| {
            if ui.button(action.label()).clicked() {
                clicked = Some(action);
            }
            if action != FixAction::Retry && ui.small_button("Retry").clicked() {
                clicked = Some(FixAction::Retry);
            }
        });
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_a_sentence_and_action() {
        use YtdlpErrorKind::*;
        for kind in [
            BotCheck,
            Http403,
            JsRuntime,
            LoginRequired,
            Unavailable,
            UnsupportedUrl,
            Network,
            Unknown,
        ] {
            let (sentence, _) = hint(kind);
            assert!(!sentence.is_empty());
        }
    }

    #[test]
    fn bot_check_offers_cookies_and_403_offers_update() {
        assert_eq!(hint(YtdlpErrorKind::BotCheck).1, FixAction::UseCookies);
        assert_eq!(hint(YtdlpErrorKind::Http403).1, FixAction::UpdateYtdlp);
        assert_eq!(hint(YtdlpErrorKind::JsRuntime).1, FixAction::InstallNode);
    }
}
