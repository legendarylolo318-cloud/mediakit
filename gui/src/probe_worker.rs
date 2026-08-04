//! Runs ffprobe for newly-added files on a background thread so the UI never
//! blocks, and reports results back through a channel polled each frame.

use mediakit_core::probe::{self, MediaInfo};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

pub struct ProbeResult {
    pub path: PathBuf,
    pub info: Result<MediaInfo, String>,
}

pub struct ProbeWorker {
    sender: Sender<ProbeResult>,
    pub receiver: Receiver<ProbeResult>,
}

impl ProbeWorker {
    pub fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        Self { sender, receiver }
    }

    /// Kick off a probe for `path` on a background thread using `ffprobe_bin`.
    pub fn submit(&self, ffprobe_bin: PathBuf, path: PathBuf) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = probe::probe(&ffprobe_bin, &path).map_err(|e| e.to_string());
            let _ = sender.send(ProbeResult { path, info: result });
        });
    }

    /// Drain any completed probe results without blocking.
    pub fn poll(&self) -> Vec<ProbeResult> {
        self.receiver.try_iter().collect()
    }
}
