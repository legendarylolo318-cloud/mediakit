//! Target-size bitrate math: the core of every "Discord 10 MB"-style
//! preset. Deliberately has zero I/O so it's trivial to unit test.

/// Container/muxing overhead fudge factor: encoders can't hit an exact byte
/// count, so we undershoot the raw math by this fraction.
pub const DEFAULT_SAFETY_MARGIN: f64 = 0.95;

/// Below this, video is unwatchable mush; better to tell the user to
/// downscale/reduce fps than to silently produce garbage.
pub const MIN_VIDEO_BITRATE_KBPS: u64 = 64;

/// "Retry with reduced bitrate up to 3 attempts" per the product spec.
pub const DEFAULT_MAX_RETRIES: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TargetSizeParams {
    pub target_bytes: u64,
    pub duration_seconds: f64,
    pub audio_bitrate_kbps: u64,
    pub safety_margin: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitrateResult {
    pub video_bitrate_kbps: u64,
    /// `true` if the computed bitrate was clamped up to
    /// [`MIN_VIDEO_BITRATE_KBPS`] because the honest math came out lower -
    /// a signal the caller should offer to downscale resolution/fps instead.
    pub hit_floor: bool,
}

/// Compute the video bitrate needed to hit `target_bytes` over
/// `duration_seconds`, after reserving room for the audio track and a
/// safety margin for container/muxing overhead.
pub fn compute_target_bitrate(params: &TargetSizeParams) -> BitrateResult {
    if params.duration_seconds <= 0.0 {
        return BitrateResult {
            video_bitrate_kbps: MIN_VIDEO_BITRATE_KBPS,
            hit_floor: true,
        };
    }

    let total_bitrate_bps =
        (params.target_bytes as f64 * 8.0 * params.safety_margin) / params.duration_seconds;
    let total_bitrate_kbps = total_bitrate_bps / 1000.0;
    let video_kbps = total_bitrate_kbps - params.audio_bitrate_kbps as f64;

    if video_kbps <= MIN_VIDEO_BITRATE_KBPS as f64 {
        BitrateResult {
            video_bitrate_kbps: MIN_VIDEO_BITRATE_KBPS,
            hit_floor: true,
        }
    } else {
        BitrateResult {
            video_bitrate_kbps: video_kbps.round() as u64,
            hit_floor: false,
        }
    }
}

/// Given a previous attempt's actual output size, compute a reduced video
/// bitrate for the next retry, scaling proportionally to how far over
/// target the previous attempt landed.
pub fn next_retry_bitrate_kbps(
    previous_video_bitrate_kbps: u64,
    actual_bytes: u64,
    target_bytes: u64,
    safety_margin: f64,
) -> u64 {
    if actual_bytes == 0 || target_bytes == 0 {
        return MIN_VIDEO_BITRATE_KBPS.min(previous_video_bitrate_kbps.max(1));
    }
    let ratio = (target_bytes as f64 / actual_bytes as f64) * safety_margin;
    let new_kbps = (previous_video_bitrate_kbps as f64 * ratio).floor();
    (new_kbps as i64).max(MIN_VIDEO_BITRATE_KBPS as i64) as u64
}

/// Halve a resolution for the "bitrate is absurdly low, downscale instead"
/// suggestion, rounding down to even numbers (required by yuv420p and most
/// hardware encoders) and never going below a 2x2 floor.
pub fn suggest_halved_resolution(width: u32, height: u32) -> (u32, u32) {
    let round_down_even = |x: u32| if x.is_multiple_of(2) { x } else { x - 1 };
    let new_w = round_down_even((width / 2).max(2));
    let new_h = round_down_even((height / 2).max(2));
    (new_w, new_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_10mb_example_matches_hand_computed_value() {
        // 10 MB target, 60s clip, 128kbps audio, default safety margin.
        let params = TargetSizeParams {
            target_bytes: 10 * 1024 * 1024,
            duration_seconds: 60.0,
            audio_bitrate_kbps: 128,
            safety_margin: DEFAULT_SAFETY_MARGIN,
        };
        let result = compute_target_bitrate(&params);

        // total_bitrate_bps = 10*1024*1024*8*0.95 / 60 = 1_328_196.27
        // total_kbps = 1328.20, minus 128 audio = ~1200 kbps video
        assert!(!result.hit_floor);
        assert!(
            (result.video_bitrate_kbps as i64 - 1200).abs() <= 1,
            "got {}",
            result.video_bitrate_kbps
        );
    }

    #[test]
    fn long_duration_tiny_target_hits_floor() {
        // A 10-minute clip squeezed into 1 MB will never fit; math should
        // clamp to the floor rather than go negative/zero.
        let params = TargetSizeParams {
            target_bytes: 1024 * 1024,
            duration_seconds: 600.0,
            audio_bitrate_kbps: 128,
            safety_margin: DEFAULT_SAFETY_MARGIN,
        };
        let result = compute_target_bitrate(&params);
        assert!(result.hit_floor);
        assert_eq!(result.video_bitrate_kbps, MIN_VIDEO_BITRATE_KBPS);
    }

    #[test]
    fn zero_duration_is_handled_without_panicking() {
        let params = TargetSizeParams {
            target_bytes: 1024,
            duration_seconds: 0.0,
            audio_bitrate_kbps: 128,
            safety_margin: DEFAULT_SAFETY_MARGIN,
        };
        let result = compute_target_bitrate(&params);
        assert!(result.hit_floor);
    }

    #[test]
    fn higher_safety_margin_yields_higher_bitrate() {
        let low_margin = compute_target_bitrate(&TargetSizeParams {
            target_bytes: 50 * 1024 * 1024,
            duration_seconds: 120.0,
            audio_bitrate_kbps: 128,
            safety_margin: 0.80,
        });
        let high_margin = compute_target_bitrate(&TargetSizeParams {
            target_bytes: 50 * 1024 * 1024,
            duration_seconds: 120.0,
            audio_bitrate_kbps: 128,
            safety_margin: 0.99,
        });
        assert!(high_margin.video_bitrate_kbps > low_margin.video_bitrate_kbps);
    }

    #[test]
    fn retry_scales_bitrate_down_proportionally_when_oversized() {
        // Target 10 MB, actual came out at 12 MB (20% over) at 1000kbps.
        let target = 10 * 1024 * 1024;
        let actual = 12 * 1024 * 1024;
        let next = next_retry_bitrate_kbps(1000, actual, target, DEFAULT_SAFETY_MARGIN);
        // Expect roughly 1000 * (10/12) * 0.95 ~= 791
        assert!(next < 1000, "should reduce bitrate: got {next}");
        assert!((next as i64 - 791).abs() <= 2, "got {next}, expected ~791");
    }

    #[test]
    fn retry_never_goes_below_floor() {
        let next = next_retry_bitrate_kbps(70, 100 * 1024 * 1024, 1024, DEFAULT_SAFETY_MARGIN);
        assert_eq!(next, MIN_VIDEO_BITRATE_KBPS);
    }

    #[test]
    fn retry_handles_zero_actual_bytes_without_panicking() {
        let next = next_retry_bitrate_kbps(500, 0, 1024, DEFAULT_SAFETY_MARGIN);
        assert!(next >= MIN_VIDEO_BITRATE_KBPS || next >= 1);
    }

    #[test]
    fn suggest_halved_resolution_rounds_to_even() {
        assert_eq!(suggest_halved_resolution(1920, 1080), (960, 540));
        // Odd inputs should round the halved value down to even.
        assert_eq!(suggest_halved_resolution(1921, 1081), (960, 540));
    }

    #[test]
    fn suggest_halved_resolution_never_goes_below_floor() {
        assert_eq!(suggest_halved_resolution(2, 2), (2, 2));
        assert_eq!(suggest_halved_resolution(3, 3), (2, 2));
    }
}
