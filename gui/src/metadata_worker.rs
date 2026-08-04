//! Fetches yt-dlp metadata (`yt-dlp -J`) on a background thread so the UI
//! never blocks on a network call, mirroring `probe_worker`'s pattern.

use mediakit_core::downloader::{self, CookieSource, Metadata};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

pub struct MetadataResult {
    pub url: String,
    pub result: Result<Metadata, String>,
}

pub struct MetadataWorker {
    sender: Sender<MetadataResult>,
    pub receiver: Receiver<MetadataResult>,
}

impl MetadataWorker {
    pub fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        Self { sender, receiver }
    }

    pub fn submit(&self, ytdlp: PathBuf, url: String, cookies: Option<CookieSource>) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let result = downloader::fetch_metadata(&ytdlp, &url, cookies.as_ref())
                .map_err(|e| e.to_string());
            let _ = sender.send(MetadataResult { url, result });
        });
    }

    pub fn poll(&self) -> Vec<MetadataResult> {
        self.receiver.try_iter().collect()
    }
}
