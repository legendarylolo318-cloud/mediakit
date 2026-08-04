//! The download job queue - rendered in the Download tab using the same
//! status/progress/cancel/log UX as the conversion queue, but kept as its
//! own list since a download's shape (URL, no probed media metadata) is
//! different enough from a conversion (input file) to force into one
//! struct without a pile of fields that only apply to one kind.

use mediakit_core::download_engine::DownloadEvent;
use mediakit_core::downloader::DownloadStatus as YtDlpStatus;
use mediakit_core::job::JobId;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum DownloadQueueStatus {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

pub struct DownloadQueueItem {
    pub id: JobId,
    pub url: String,
    pub title: String,
    pub output_path: Option<PathBuf>,
    pub status: DownloadQueueStatus,
    pub percent: Option<f64>,
    pub speed: Option<String>,
    pub eta: Option<String>,
    pub log: String,
    /// Set when this download should feed directly into a conversion job
    /// once it finishes (see `core::presets` chaining in app.rs).
    pub chain_to: Option<String>,
}

impl DownloadQueueItem {
    pub fn new(id: JobId, url: String, title: String) -> Self {
        Self {
            id,
            url,
            title,
            output_path: None,
            status: DownloadQueueStatus::Queued,
            percent: None,
            speed: None,
            eta: None,
            log: String::new(),
            chain_to: None,
        }
    }
}

pub fn apply_event(items: &mut [DownloadQueueItem], event: DownloadEvent) {
    let id = match &event {
        DownloadEvent::Started { id }
        | DownloadEvent::Progress { id, .. }
        | DownloadEvent::Done { id, .. }
        | DownloadEvent::Failed { id, .. }
        | DownloadEvent::Cancelled { id } => *id,
    };
    let Some(item) = items.iter_mut().find(|i| i.id == id) else {
        return;
    };

    match event {
        DownloadEvent::Started { .. } => item.status = DownloadQueueStatus::Running,
        DownloadEvent::Progress { info, .. } => {
            item.status = DownloadQueueStatus::Running;
            item.percent = info.percent;
            item.speed = info.speed;
            item.eta = info.eta;
            if info.status == YtDlpStatus::Finished {
                item.percent = Some(100.0);
            }
        }
        DownloadEvent::Done {
            output_path, log, ..
        } => {
            item.status = DownloadQueueStatus::Done;
            item.output_path = output_path;
            item.log = log;
            item.percent = Some(100.0);
        }
        DownloadEvent::Failed { error, log, .. } => {
            item.status = DownloadQueueStatus::Failed(error);
            item.log = log;
        }
        DownloadEvent::Cancelled { .. } => item.status = DownloadQueueStatus::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediakit_core::downloader::DownloadProgress;

    fn item() -> DownloadQueueItem {
        DownloadQueueItem::new(1, "https://example.com/x".to_string(), "Title".to_string())
    }

    #[test]
    fn started_marks_running() {
        let mut items = vec![item()];
        apply_event(&mut items, DownloadEvent::Started { id: 1 });
        assert_eq!(items[0].status, DownloadQueueStatus::Running);
    }

    #[test]
    fn progress_updates_percent_speed_eta() {
        let mut items = vec![item()];
        apply_event(
            &mut items,
            DownloadEvent::Progress {
                id: 1,
                info: DownloadProgress {
                    percent: Some(42.0),
                    speed: Some("1MiB/s".to_string()),
                    eta: Some("00:05".to_string()),
                    status: YtDlpStatus::Downloading,
                },
            },
        );
        assert_eq!(items[0].percent, Some(42.0));
        assert_eq!(items[0].speed.as_deref(), Some("1MiB/s"));
    }

    #[test]
    fn done_sets_output_path_and_full_progress() {
        let mut items = vec![item()];
        apply_event(
            &mut items,
            DownloadEvent::Done {
                id: 1,
                output_path: Some(PathBuf::from("/tmp/x.mp4")),
                log: "ok".to_string(),
            },
        );
        assert_eq!(items[0].status, DownloadQueueStatus::Done);
        assert_eq!(items[0].output_path, Some(PathBuf::from("/tmp/x.mp4")));
        assert_eq!(items[0].percent, Some(100.0));
    }

    #[test]
    fn unknown_id_is_ignored() {
        let mut items = vec![item()];
        apply_event(&mut items, DownloadEvent::Started { id: 999 });
        assert_eq!(items[0].status, DownloadQueueStatus::Queued);
    }
}
