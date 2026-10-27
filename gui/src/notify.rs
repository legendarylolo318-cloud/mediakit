//! Desktop notifications for finished work. One notification per batch, not
//! per file: the app calls [`summarize`] when a queue goes idle.

/// Title and body for a queue that just went idle having finished
/// `succeeded` jobs and failed `failed` ones. `what` is "Conversion" or
/// "Download"; `only_name` is the file/title when exactly one job ran.
pub fn summarize(
    what: &str,
    succeeded: usize,
    failed: usize,
    only_name: Option<&str>,
) -> (String, String) {
    match (succeeded, failed) {
        (1, 0) => (
            format!("{what} finished"),
            only_name.unwrap_or("1 file is ready").to_string(),
        ),
        (0, 1) => (
            format!("{what} failed"),
            only_name.unwrap_or("1 file failed").to_string(),
        ),
        (s, 0) => (format!("{what} finished"), format!("{s} files are ready")),
        (0, f) => (format!("{what} failed"), format!("{f} files failed")),
        (s, f) => (
            format!("{what} finished"),
            format!("{s} succeeded, {f} failed"),
        ),
    }
}

/// Fire-and-forget: some Linux notification backends block until the
/// notification is dismissed, so never do this on the UI thread. Failures
/// (no notification daemon, etc.) are deliberately ignored.
pub fn send(title: String, body: String) {
    std::thread::spawn(move || {
        let _ = notify_rust::Notification::new()
            .appname("MediaKit")
            .summary(&title)
            .body(&body)
            .show();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_job_names_the_file() {
        assert_eq!(
            summarize("Download", 1, 0, Some("clip.mp4")),
            ("Download finished".to_string(), "clip.mp4".to_string())
        );
        assert_eq!(
            summarize("Conversion", 0, 1, Some("a.mov")).0,
            "Conversion failed"
        );
    }

    #[test]
    fn batches_get_one_summary() {
        assert_eq!(summarize("Conversion", 5, 0, None).1, "5 files are ready");
        assert_eq!(summarize("Conversion", 0, 3, None).1, "3 files failed");
        assert_eq!(
            summarize("Conversion", 4, 2, None).1,
            "4 succeeded, 2 failed"
        );
    }
}
