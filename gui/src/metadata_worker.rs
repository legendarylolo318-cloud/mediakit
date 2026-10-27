//! Fetches yt-dlp metadata (`yt-dlp -J`) on a background thread so the UI
//! never blocks on a network call, mirroring `probe_worker`'s pattern.

use mediakit_core::downloader::{self, CookieSource, Metadata};
use mediakit_core::pot_provider::{self, PotProviderConfig, PotProviderManager};
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

    /// `pot` carries what the YouTube arg helper needs (manager, config, app
    /// data dir); the helper runs here on the worker thread so a cold
    /// PO-token server start never blocks the UI. `None` (no app data dir)
    /// means no extra args.
    pub fn submit(
        &self,
        ytdlp: PathBuf,
        url: String,
        cookies: Option<CookieSource>,
        pot: Option<(PotProviderManager, PotProviderConfig, PathBuf)>,
    ) {
        let sender = self.sender.clone();
        std::thread::spawn(move || {
            let extra_args = match &pot {
                Some((manager, config, dir)) => pot_provider::prepare_youtube_args(
                    &url,
                    manager,
                    *config,
                    dir,
                    pot_provider::DEFAULT_WAIT_BUDGET,
                ),
                None => Vec::new(),
            };
            let result = downloader::fetch_metadata(&ytdlp, &url, cookies.as_ref(), &extra_args)
                .map_err(|e| e.to_string());
            let _ = sender.send(MetadataResult { url, result });
        });
    }

    pub fn poll(&self) -> Vec<MetadataResult> {
        self.receiver.try_iter().collect()
    }
}
