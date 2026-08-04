//! Runs yt-dlp self-update checks/downloads on a background thread so the
//! UI never blocks on a network call, mirroring `metadata_worker`'s pattern.

use mediakit_core::ytdlp_update;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

#[derive(Debug, Clone)]
pub enum UpdateOutcome {
    /// A check finished without installing anything (either the button was
    /// "Check for Updates" only, or the weekly auto-check found nothing
    /// newer).
    Checked {
        update_available: bool,
    },
    /// A new version was downloaded, verified, and installed.
    Updated {
        new_version: String,
    },
    /// A previous version was restored via rollback.
    RolledBack,
    Error(String),
}

pub struct UpdateWorker {
    sender: Sender<UpdateOutcome>,
    pub receiver: Receiver<UpdateOutcome>,
}

impl UpdateWorker {
    pub fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        Self { sender, receiver }
    }

    /// Check only - used by the "Check for Updates" button and the weekly
    /// auto-check's first step.
    pub fn check(&self, bin_dir: PathBuf, current_version: String) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let outcome = match ytdlp_update::check_for_update(&bin_dir, &current_version) {
                Ok(update_available) => UpdateOutcome::Checked { update_available },
                Err(e) => UpdateOutcome::Error(e.to_string()),
            };
            let _ = sender.send(outcome);
        });
    }

    /// Download, verify, and install the latest release unconditionally.
    pub fn update(&self, bin_dir: PathBuf) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let outcome = match ytdlp_update::perform_update(&bin_dir) {
                Ok(_) => {
                    let state = ytdlp_update::load_state(&bin_dir);
                    UpdateOutcome::Updated {
                        new_version: state.latest_known_version.unwrap_or_default(),
                    }
                }
                Err(e) => UpdateOutcome::Error(e.to_string()),
            };
            let _ = sender.send(outcome);
        });
    }

    /// Check, and only download+install if a newer version is actually
    /// available - used by the weekly auto-check so it doesn't re-download
    /// yt-dlp every week regardless of whether it changed.
    pub fn auto_check_and_update(&self, bin_dir: PathBuf, current_version: String) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let outcome = match ytdlp_update::check_for_update(&bin_dir, &current_version) {
                Ok(false) => UpdateOutcome::Checked {
                    update_available: false,
                },
                Ok(true) => match ytdlp_update::perform_update(&bin_dir) {
                    Ok(_) => {
                        let state = ytdlp_update::load_state(&bin_dir);
                        UpdateOutcome::Updated {
                            new_version: state.latest_known_version.unwrap_or_default(),
                        }
                    }
                    Err(e) => UpdateOutcome::Error(e.to_string()),
                },
                Err(e) => UpdateOutcome::Error(e.to_string()),
            };
            let _ = sender.send(outcome);
        });
    }

    pub fn rollback(&self, bin_dir: PathBuf) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let outcome = match ytdlp_update::rollback(&bin_dir) {
                Ok(_) => UpdateOutcome::RolledBack,
                Err(e) => UpdateOutcome::Error(e.to_string()),
            };
            let _ = sender.send(outcome);
        });
    }

    pub fn poll(&self) -> Vec<UpdateOutcome> {
        self.receiver.try_iter().collect()
    }
}
