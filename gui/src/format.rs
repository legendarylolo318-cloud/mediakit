//! Human-readable formatting helpers for the metadata table.

pub fn duration(seconds: f64) -> String {
    if seconds <= 0.0 {
        return "-".to_string();
    }
    let total = seconds.round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn file_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;
    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }
    if unit_index == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{size:.2} {}", UNITS[unit_index])
    }
}

pub fn bitrate(bps: Option<u64>) -> String {
    match bps {
        None => "-".to_string(),
        Some(bps) if bps >= 1_000_000 => format!("{:.2} Mbps", bps as f64 / 1_000_000.0),
        Some(bps) => format!("{:.0} kbps", bps as f64 / 1_000.0),
    }
}

pub fn fps(fps: f64) -> String {
    if fps <= 0.0 {
        "-".to_string()
    } else {
        format!("{fps:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_formats_hours_minutes_seconds() {
        assert_eq!(duration(0.0), "-");
        assert_eq!(duration(45.0), "0:45");
        assert_eq!(duration(65.0), "1:05");
        assert_eq!(duration(3661.0), "1:01:01");
    }

    #[test]
    fn file_size_scales_units() {
        assert_eq!(file_size(500), "500 B");
        assert_eq!(file_size(2048), "2.00 KB");
        assert_eq!(file_size(5 * 1024 * 1024), "5.00 MB");
    }

    #[test]
    fn bitrate_switches_between_kbps_and_mbps() {
        assert_eq!(bitrate(None), "-");
        assert_eq!(bitrate(Some(128_000)), "128 kbps");
        assert_eq!(bitrate(Some(5_000_000)), "5.00 Mbps");
    }
}
