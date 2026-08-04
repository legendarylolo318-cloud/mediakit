//! The batch job queue: per-item status, progress, and the ffmpeg log
//! captured for each job. [`apply_event`] is the only thing that mutates a
//! [`QueueItem`]'s status, keeping the "what happened" logic in one place.

use crate::encode_config::EncodeConfig;
use mediakit_core::engine::EngineEvent;
use mediakit_core::job::{JobId, JobProgressInfo};
use mediakit_core::probe::MediaInfo;
use std::path::PathBuf;

/// A row's lifecycle: added and being ffprobed / waiting to be queued, then
/// (once the user submits it) tracking an actual engine job.
#[derive(Debug, Clone, PartialEq)]
pub enum QueueStatus {
    /// Added to the list, not yet submitted to the engine.
    NotQueued,
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

/// One row: the input file (with its probed metadata) and, once submitted,
/// the job tracking its conversion.
pub struct QueueItem {
    pub id: JobId,
    pub input: PathBuf,
    pub output: PathBuf,
    pub mode_label: String,
    pub status: QueueStatus,
    pub progress: JobProgressInfo,
    pub log: String,
    pub last_retry_note: Option<String>,

    pub info: Option<MediaInfo>,
    pub probe_error: Option<String>,
    pub probing: bool,

    /// Set when this item was created by "Download -> conversion" chaining:
    /// the exact settings to auto-submit with as soon as probing finishes,
    /// rather than waiting for the user to configure and click Convert.
    pub pending_auto_config: Option<EncodeConfig>,
    /// Delete `input` once this item's conversion finishes - set for
    /// chained items, where the download is just an intermediate file.
    pub delete_input_when_done: bool,

    /// The byte cap this job was targeting, if it's a size-targeted encode -
    /// used to show a real pass/fail badge (with byte count) once the job
    /// finishes, instead of just "done". `None` for every other mode.
    pub target_size_limit_bytes: Option<u64>,
}

impl QueueItem {
    /// A freshly-added file: not yet probed, not yet queued.
    pub fn new_pending(id: JobId, input: PathBuf) -> Self {
        Self {
            id,
            input,
            output: PathBuf::new(),
            mode_label: String::new(),
            status: QueueStatus::NotQueued,
            progress: JobProgressInfo::default(),
            log: String::new(),
            last_retry_note: None,
            info: None,
            probe_error: None,
            probing: true,
            pending_auto_config: None,
            delete_input_when_done: false,
            target_size_limit_bytes: None,
        }
    }

    pub fn file_name(&self) -> String {
        self.input
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.input.to_string_lossy().into_owned())
    }

    pub fn duration_seconds(&self) -> f64 {
        self.info
            .as_ref()
            .map(|i| i.duration_seconds)
            .unwrap_or(0.0)
    }
}

/// Apply one [`EngineEvent`] to the matching queue item, if it's still
/// present (it may have been cleared from the list already).
pub fn apply_event(items: &mut [QueueItem], event: EngineEvent) {
    let id = match &event {
        EngineEvent::Started { id }
        | EngineEvent::Progress { id, .. }
        | EngineEvent::Done { id, .. }
        | EngineEvent::Failed { id, .. }
        | EngineEvent::Cancelled { id }
        | EngineEvent::Retrying { id, .. } => *id,
    };

    let Some(item) = items.iter_mut().find(|i| i.id == id) else {
        return;
    };

    match event {
        EngineEvent::Started { .. } => item.status = QueueStatus::Running,
        EngineEvent::Progress { info, .. } => {
            item.status = QueueStatus::Running;
            item.progress = info;
        }
        EngineEvent::Done { log, .. } => {
            item.status = QueueStatus::Done;
            item.log = log;
            item.progress.percent = Some(100.0);
        }
        EngineEvent::Failed { error, log, .. } => {
            item.status = QueueStatus::Failed(error);
            item.log = log;
        }
        EngineEvent::Cancelled { .. } => item.status = QueueStatus::Cancelled,
        EngineEvent::Retrying {
            attempt,
            new_video_bitrate_kbps,
            ..
        } => {
            item.last_retry_note = Some(format!(
                "output too large, retrying (attempt {attempt}) at {new_video_bitrate_kbps} kbps"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> QueueItem {
        let mut item = QueueItem::new_pending(1, PathBuf::from("in.mp4"));
        item.output = PathBuf::from("out.mp4");
        item.mode_label = "Convert video".to_string();
        item.status = QueueStatus::Queued;
        item
    }

    #[test]
    fn started_event_marks_running() {
        let mut items = vec![item()];
        apply_event(&mut items, EngineEvent::Started { id: 1 });
        assert_eq!(items[0].status, QueueStatus::Running);
    }

    #[test]
    fn progress_event_updates_percent() {
        let mut items = vec![item()];
        let info = JobProgressInfo {
            percent: Some(42.0),
            ..Default::default()
        };
        apply_event(&mut items, EngineEvent::Progress { id: 1, info });
        assert_eq!(items[0].progress.percent, Some(42.0));
        assert_eq!(items[0].status, QueueStatus::Running);
    }

    #[test]
    fn done_event_sets_full_progress_and_log() {
        let mut items = vec![item()];
        apply_event(
            &mut items,
            EngineEvent::Done {
                id: 1,
                log: "ok".to_string(),
            },
        );
        assert_eq!(items[0].status, QueueStatus::Done);
        assert_eq!(items[0].progress.percent, Some(100.0));
        assert_eq!(items[0].log, "ok");
    }

    #[test]
    fn failed_event_carries_error_and_log() {
        let mut items = vec![item()];
        apply_event(
            &mut items,
            EngineEvent::Failed {
                id: 1,
                error: "boom".to_string(),
                log: "stderr here".to_string(),
            },
        );
        assert_eq!(items[0].status, QueueStatus::Failed("boom".to_string()));
        assert_eq!(items[0].log, "stderr here");
    }

    #[test]
    fn event_for_unknown_id_is_ignored_without_panicking() {
        let mut items = vec![item()];
        apply_event(&mut items, EngineEvent::Started { id: 999 });
        assert_eq!(items[0].status, QueueStatus::Queued);
    }

    #[test]
    fn retrying_event_sets_note_without_changing_status() {
        let mut items = vec![item()];
        apply_event(&mut items, EngineEvent::Started { id: 1 });
        apply_event(
            &mut items,
            EngineEvent::Retrying {
                id: 1,
                attempt: 1,
                new_video_bitrate_kbps: 800,
            },
        );
        assert_eq!(items[0].status, QueueStatus::Running);
        assert!(items[0].last_retry_note.as_ref().unwrap().contains("800"));
    }
}
