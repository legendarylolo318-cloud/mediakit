//! The big preset cards at the top of the Convert tab. Which card is
//! highlighted is derived from the config (not stored), so it can never
//! disagree with what will actually run.

use crate::encode_config::Mode;
use mediakit_core::command::AudioCodec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card {
    DiscordClip,
    Gif,
    Mp3,
    Compress,
    Image,
}

impl Card {
    pub const ALL: [Card; 5] = [
        Card::DiscordClip,
        Card::Gif,
        Card::Mp3,
        Card::Compress,
        Card::Image,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Card::DiscordClip => "Discord clip",
            Card::Gif => "Video \u{2192} GIF",
            Card::Mp3 => "Extract MP3",
            Card::Compress => "Compress",
            Card::Image => "Convert image",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Card::DiscordClip => "Shrink a video under a size limit",
            Card::Gif => "Turn a clip into a looping GIF",
            Card::Mp3 => "Pull the audio out as an MP3",
            Card::Compress => "Re-encode a video to save space",
            Card::Image => "Change an image's format",
        }
    }

    /// The card matching the current mode, if any. `None` means one of the
    /// "More\u{2026}" presets is active.
    pub fn for_mode(mode: Mode, audio: AudioCodec) -> Option<Card> {
        match mode {
            Mode::TargetSize => Some(Card::DiscordClip),
            Mode::VideoToGif => Some(Card::Gif),
            Mode::ExtractAudio if audio == AudioCodec::Mp3 => Some(Card::Mp3),
            Mode::Video => Some(Card::Compress),
            Mode::ConvertImage => Some(Card::Image),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_card_round_trips_through_its_mode() {
        assert_eq!(
            Card::for_mode(Mode::TargetSize, AudioCodec::Mp3),
            Some(Card::DiscordClip)
        );
        assert_eq!(
            Card::for_mode(Mode::ExtractAudio, AudioCodec::Mp3),
            Some(Card::Mp3)
        );
        assert_eq!(
            Card::for_mode(Mode::Video, AudioCodec::Mp3),
            Some(Card::Compress)
        );
    }

    #[test]
    fn extract_audio_in_another_codec_and_more_presets_highlight_nothing() {
        assert_eq!(Card::for_mode(Mode::ExtractAudio, AudioCodec::Flac), None);
        assert_eq!(Card::for_mode(Mode::Reverse, AudioCodec::Mp3), None);
    }
}
