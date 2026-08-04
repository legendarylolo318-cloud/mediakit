//! Parses ffmpeg's machine-readable progress stream (`-progress pipe:1
//! -nostats`), which emits a block of `key=value` lines terminated by
//! `progress=continue` or `progress=end` each time ffmpeg flushes progress.

use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgressUpdate {
    pub frame: Option<u64>,
    pub fps: Option<f64>,
    pub out_time_seconds: Option<f64>,
    pub total_size_bytes: Option<u64>,
    pub bitrate_kbps: Option<f64>,
    pub speed: Option<f64>,
    pub done: bool,
}

#[derive(Debug, Default)]
pub struct ProgressParser {
    current: ProgressUpdate,
}

impl ProgressParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one line of ffmpeg's `-progress` output. Returns a completed
    /// [`ProgressUpdate`] whenever a `progress=` terminator line is seen.
    pub fn feed_line(&mut self, line: &str) -> Option<ProgressUpdate> {
        let line = line.trim();
        let (key, value) = line.split_once('=')?;
        let key = key.trim();
        let value = value.trim();

        match key {
            "frame" => self.current.frame = value.parse().ok(),
            "fps" => self.current.fps = value.parse().ok(),
            "out_time_us" | "out_time_ms" => {
                if let Ok(us) = value.parse::<i64>() {
                    self.current.out_time_seconds = Some(us as f64 / 1_000_000.0);
                }
            }
            "out_time" => {
                if self.current.out_time_seconds.is_none() {
                    self.current.out_time_seconds = parse_timecode(value);
                }
            }
            "total_size" => self.current.total_size_bytes = value.parse().ok(),
            "bitrate" => {
                // e.g. "1234.5kbits/s" or "N/A"
                let numeric = value.trim_end_matches("kbits/s");
                self.current.bitrate_kbps = numeric.parse().ok();
            }
            "speed" => {
                // e.g. "2.05x" or "N/A"
                let numeric = value.trim_end_matches('x');
                self.current.speed = numeric.parse().ok();
            }
            "progress" => {
                self.current.done = value == "end";
                let finished = std::mem::take(&mut self.current);
                return Some(finished);
            }
            _ => {}
        }
        None
    }
}

/// Parses ffmpeg's `HH:MM:SS.ss` timecode format into seconds.
fn parse_timecode(s: &str) -> Option<f64> {
    let mut parts = s.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

/// Percent complete (0-100), clamped, given the media's total duration.
pub fn percent(update: &ProgressUpdate, total_duration_seconds: f64) -> Option<f64> {
    if total_duration_seconds <= 0.0 {
        return None;
    }
    let out_time = update.out_time_seconds?;
    Some((out_time / total_duration_seconds * 100.0).clamp(0.0, 100.0))
}

/// Estimated time remaining, given the media's total duration.
pub fn eta(update: &ProgressUpdate, total_duration_seconds: f64) -> Option<Duration> {
    let out_time = update.out_time_seconds?;
    let speed = update.speed?;
    if speed <= 0.0 {
        return None;
    }
    let remaining_source_seconds = (total_duration_seconds - out_time).max(0.0);
    let remaining_wall_seconds = remaining_source_seconds / speed;
    Some(Duration::from_secs_f64(remaining_wall_seconds.max(0.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(parser: &mut ProgressParser, lines: &[&str]) -> Option<ProgressUpdate> {
        let mut last = None;
        for line in lines {
            if let Some(update) = parser.feed_line(line) {
                last = Some(update);
            }
        }
        last
    }

    #[test]
    fn parses_a_full_progress_block() {
        let mut parser = ProgressParser::new();
        let update = feed_all(
            &mut parser,
            &[
                "frame=120",
                "fps=29.97",
                "stream_0_0_q=23.0",
                "bitrate=1234.5kbits/s",
                "total_size=1048576",
                "out_time_us=4000000",
                "out_time_ms=4000000",
                "out_time=00:00:04.000000",
                "dup_frames=0",
                "drop_frames=0",
                "speed=2.05x",
                "progress=continue",
            ],
        )
        .expect("should complete a block");

        assert_eq!(update.frame, Some(120));
        assert_eq!(update.fps, Some(29.97));
        assert_eq!(update.out_time_seconds, Some(4.0));
        assert_eq!(update.total_size_bytes, Some(1_048_576));
        assert_eq!(update.bitrate_kbps, Some(1234.5));
        assert_eq!(update.speed, Some(2.05));
        assert!(!update.done);
    }

    #[test]
    fn progress_end_marks_done() {
        let mut parser = ProgressParser::new();
        let update = feed_all(&mut parser, &["out_time_us=9000000", "progress=end"]).unwrap();
        assert!(update.done);
        assert_eq!(update.out_time_seconds, Some(9.0));
    }

    #[test]
    fn na_values_are_ignored_gracefully() {
        let mut parser = ProgressParser::new();
        let update = feed_all(
            &mut parser,
            &["bitrate=N/A", "speed=N/A", "progress=continue"],
        )
        .unwrap();
        assert_eq!(update.bitrate_kbps, None);
        assert_eq!(update.speed, None);
    }

    #[test]
    fn percent_and_eta_compute_from_duration_and_speed() {
        let update = ProgressUpdate {
            out_time_seconds: Some(30.0),
            speed: Some(2.0),
            ..Default::default()
        };
        assert_eq!(percent(&update, 60.0), Some(50.0));
        assert_eq!(eta(&update, 60.0), Some(Duration::from_secs(15)));
    }

    #[test]
    fn percent_clamps_to_100() {
        let update = ProgressUpdate {
            out_time_seconds: Some(65.0),
            ..Default::default()
        };
        assert_eq!(percent(&update, 60.0), Some(100.0));
    }

    #[test]
    fn parser_resets_state_between_blocks() {
        let mut parser = ProgressParser::new();
        let first = feed_all(&mut parser, &["frame=10", "progress=continue"]).unwrap();
        assert_eq!(first.frame, Some(10));

        // A second block that doesn't repeat `frame` should not inherit it.
        let second = feed_all(&mut parser, &["fps=25", "progress=continue"]).unwrap();
        assert_eq!(second.frame, None);
        assert_eq!(second.fps, Some(25.0));
    }
}
