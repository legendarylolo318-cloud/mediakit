use crate::clipboard_watch::{self, ClipboardWatcher};
use crate::download_queue::{DownloadQueueItem, DownloadQueueStatus};
use crate::encode_config::{EncodeConfig, Mode, TargetSizeChoice};
use crate::error_hint::{self, FixAction};
use crate::format;
use crate::metadata_worker::MetadataWorker;
use crate::notify;
use crate::output::{OutputLocation, OutputSettings};
use crate::paste::{self, Paste};
use crate::preset_cards::Card;
use crate::probe_worker::ProbeWorker;
use crate::queue::{QueueItem, QueueStatus};
use crate::settings::{PersistedSettings, ThemeChoice};
use crate::status::{self, Health, StatusInputs};
use crate::sys;
use crate::timeline::{self, ThumbCache};
use crate::update_worker::{UpdateOutcome, UpdateWorker};
use eframe::egui;
use mediakit_core::command::{AudioCodec, Container, VideoCodec};
use mediakit_core::download_engine::{DownloadEngine, DownloadEvent, DownloadSpec};
use mediakit_core::downloader::{
    self, CookieSource, DownloadOptions, FormatSelection, Metadata, PlaylistEntry, SubtitleOptions,
    VideoMetadata,
};
use mediakit_core::engine::{EngineEvent, JobEngine};
use mediakit_core::ffmpeg_env::{self, FfmpegEnv};
use mediakit_core::hwaccel::{self, HardwareEncoder};
use mediakit_core::job::{JobId, JobSpec};
use mediakit_core::presets::{FlipMode, ImageFormat, Rotation};
use mediakit_core::size_presets::{self, SizePreset, SizePresetsConfig};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

enum FfmpegStatus {
    Ready(Box<FfmpegEnv>),
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppTab {
    Convert,
    Download,
}

enum DownloadCardState {
    Fetching,
    Video(VideoMetadata),
    Playlist {
        title: String,
        entries: Vec<PlaylistEntryUi>,
    },
    Error(String),
}

struct PlaylistEntryUi {
    entry: PlaylistEntry,
    selected: bool,
}

struct DownloadCard {
    url: String,
    state: DownloadCardState,
    /// Queue straight away once metadata arrives, if it turns out to be a
    /// single video (the main Download button's behaviour).
    auto_queue: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatChoice {
    Best,
    BestUpTo1080p,
    BestUpTo720p,
    AudioOnlyMp3,
    AudioOnlyBest,
    Custom,
}

impl FormatChoice {
    const ALL: [FormatChoice; 6] = [
        FormatChoice::Best,
        FormatChoice::BestUpTo1080p,
        FormatChoice::BestUpTo720p,
        FormatChoice::AudioOnlyMp3,
        FormatChoice::AudioOnlyBest,
        FormatChoice::Custom,
    ];

    fn label(self) -> &'static str {
        match self {
            FormatChoice::Best => "Best",
            FormatChoice::BestUpTo1080p => "Best <=1080p",
            FormatChoice::BestUpTo720p => "Best <=720p",
            FormatChoice::AudioOnlyMp3 => "Audio only (mp3)",
            FormatChoice::AudioOnlyBest => "Audio only (best)",
            FormatChoice::Custom => "Custom",
        }
    }

    fn resolve(self, custom_text: &str) -> FormatSelection {
        match self {
            FormatChoice::Best => FormatSelection::Best,
            FormatChoice::BestUpTo1080p => FormatSelection::BestUpTo1080p,
            FormatChoice::BestUpTo720p => FormatSelection::BestUpTo720p,
            FormatChoice::AudioOnlyMp3 => FormatSelection::AudioOnlyMp3,
            FormatChoice::AudioOnlyBest => FormatSelection::AudioOnlyBest,
            FormatChoice::Custom => FormatSelection::Custom(custom_text.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CookieChoice {
    None,
    Browser,
    File,
}

/// After-download chaining target (Download -> conversion in one click).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainChoice {
    None,
    Discord10Mb,
    VideoToGif,
    ExtractMp3,
}

impl ChainChoice {
    const ALL: [ChainChoice; 4] = [
        ChainChoice::None,
        ChainChoice::Discord10Mb,
        ChainChoice::VideoToGif,
        ChainChoice::ExtractMp3,
    ];

    fn label(self) -> &'static str {
        match self {
            ChainChoice::None => "Just download",
            ChainChoice::Discord10Mb => "-> Discord 10 MB",
            ChainChoice::VideoToGif => "-> GIF",
            ChainChoice::ExtractMp3 => "-> Extract MP3",
        }
    }
}

const SHORTCUT_ADD_FILES: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O);
const SHORTCUT_CONVERT_ALL: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Enter);
const SHORTCUT_CANCEL_ALL: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Period);
const SHORTCUT_CLEAR: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::W);

pub struct MediaKitApp {
    ffmpeg_status: FfmpegStatus,
    items: Vec<QueueItem>,
    probe_worker: ProbeWorker,
    next_id: JobId,

    engine: Option<JobEngine>,
    engine_rx: Option<Receiver<EngineEvent>>,
    concurrency: usize,

    config: EncodeConfig,
    output_settings: OutputSettings,
    output_folder_display: String,
    hardware_encoders: Vec<HardwareEncoder>,

    theme: ThemeChoice,
    current_window_size: (f32, f32),

    show_log_for: Option<JobId>,
    show_licenses: bool,
    show_settings: bool,
    settings_message: Option<String>,

    override_ffmpeg: Option<PathBuf>,
    override_ffprobe: Option<PathBuf>,
    override_ytdlp: Option<PathBuf>,

    active_tab: AppTab,

    download_engine: Option<DownloadEngine>,
    download_engine_rx: Option<Receiver<DownloadEvent>>,
    metadata_worker: MetadataWorker,
    download_url_input: String,
    download_cards: Vec<DownloadCard>,
    download_items: Vec<DownloadQueueItem>,
    download_show_log_for: Option<JobId>,
    /// Set by the "Use browser cookies" fix button; makes the Download
    /// options section open itself on the next frame.
    open_download_options: bool,
    download_filename_template: String,
    advanced_open: bool,
    notifications_enabled: bool,
    // Per-run tallies driving the finish notification (see
    // `notify_when_idle`): jobs finished since the queue last went idle.
    convert_was_active: bool,
    convert_run: (usize, usize, Option<String>),
    download_was_active: bool,
    download_run: (usize, usize, Option<String>),
    /// Result of the last "Copy file" click, shown as a tooltip hint.
    copy_note: Option<String>,
    thumb_cache: ThumbCache,
    clipboard_watcher: ClipboardWatcher,
    clipboard_watch_enabled: bool,
    clipboard_extra_hosts: String,
    /// Host list the running watcher was started with, to notice edits.
    clipboard_hosts_applied: String,
    /// The link the toast is offering, and every link already offered or
    /// downloaded, so none prompts twice.
    clipboard_offers: clipboard_watch::Offers,

    download_format: FormatChoice,
    download_custom_format: String,
    download_embed_thumbnail: bool,
    download_embed_metadata: bool,
    download_subtitles: bool,
    download_sub_langs: String,
    download_auto_subs: bool,
    download_sponsorblock: bool,
    download_rate_limit_kbps: String,
    download_concurrent_fragments: String,
    download_chain: ChainChoice,
    download_output_dir: PathBuf,

    cookie_choice: CookieChoice,
    cookie_browser: String,
    cookie_browser_profile: String,
    cookie_file: Option<PathBuf>,

    update_worker: UpdateWorker,
    ytdlp_auto_update_enabled: bool,
    ytdlp_update_message: Option<String>,
    ytdlp_update_in_progress: bool,
    auto_update_check_started: bool,

    /// Target-size presets loaded from (and, via Settings -> Presets,
    /// editable back into) `presets.toml` in the config dir - see
    /// `mediakit_core::size_presets`. Never hardcode a platform's byte cap
    /// in this file; add/edit an entry there instead.
    size_presets: SizePresetsConfig,
    show_presets_editor: bool,
    presets_editor_message: Option<String>,
    new_preset_id: String,
    new_preset_name: String,
    new_preset_mib: u64,

    /// Whether the user has dismissed the Download tab's one-time
    /// ToS/copyright responsibility notice - see `download_responsibility_notice`.
    download_responsibility_acknowledged: bool,

    pot_manager: mediakit_core::pot_provider::PotProviderManager,
    pot_config: mediakit_core::pot_provider::PotProviderConfig,

    /// Job ids submitted by the last "Convert All" click, still tracked
    /// until every one of them reaches a terminal state - see
    /// `convert_all` and `poll_batch_completion`. Empty when no batch is
    /// in flight.
    batch_ids: std::collections::HashSet<JobId>,
    /// "n succeeded / n failed" for the most recently finished "Convert
    /// All" batch, shown in the status bar until the next one starts.
    batch_summary: Option<String>,
}

impl MediaKitApp {
    pub fn new(cc: &eframe::CreationContext<'_>, persisted: PersistedSettings) -> Self {
        // Probe for Node off the UI thread so the status dot can read it.
        std::thread::spawn(mediakit_core::pot_provider::node_available_cached);
        let overrides = mediakit_core::ffmpeg_env::ToolOverrides {
            ffmpeg: persisted.override_ffmpeg_path.clone(),
            ffprobe: persisted.override_ffprobe_path.clone(),
            ytdlp: persisted.override_ytdlp_path.clone(),
        };
        let ffmpeg_status = match ffmpeg_env::app_data_dir() {
            Ok(dir) => match FfmpegEnv::detect_cached_with_overrides(&dir, &overrides) {
                Ok(env) => FfmpegStatus::Ready(Box::new(env)),
                Err(_) => FfmpegStatus::NotFound,
            },
            Err(_) => FfmpegStatus::NotFound,
        };

        let concurrency = persisted.concurrency.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get().clamp(1, 4))
                .unwrap_or(2)
        });

        let (engine, engine_rx) = match &ffmpeg_status {
            FfmpegStatus::Ready(env) => {
                let (engine, rx) = JobEngine::new(env.ffmpeg_path.clone(), concurrency);
                (Some(engine), Some(rx))
            }
            FfmpegStatus::NotFound => (None, None),
        };

        let hardware_encoders = match &ffmpeg_status {
            FfmpegStatus::Ready(env) => hwaccel::detect_hardware_encoders(&env.encoders),
            FfmpegStatus::NotFound => Vec::new(),
        };

        let (download_engine, download_engine_rx) = match &ffmpeg_status {
            FfmpegStatus::Ready(env) => match &env.ytdlp_path {
                Some(ytdlp) => {
                    let (engine, rx) = DownloadEngine::new(ytdlp.clone(), 2);
                    (Some(engine), Some(rx))
                }
                None => (None, None),
            },
            FfmpegStatus::NotFound => (None, None),
        };

        apply_theme(&cc.egui_ctx, persisted.theme);

        let size_presets = size_presets::config_dir()
            .map(|dir| size_presets::load_or_seed(&dir))
            .unwrap_or_else(SizePresetsConfig::defaults);
        let mut config = EncodeConfig::default();
        if let Some(first) = size_presets.presets.first() {
            config.apply_size_preset(first, size_presets.safety_margin_fraction());
            config.mode = Mode::Video; // apply_size_preset also sets Mode::TargetSize; start on Video like before.
        }

        let mut output_settings = OutputSettings::default();
        let mut output_folder_display = String::new();
        if let Some(dir) = &persisted.custom_output_folder {
            output_folder_display = dir.to_string_lossy().into_owned();
            output_settings.location = OutputLocation::Custom(dir.clone());
        }
        if let Some(template) = persisted.filename_template.clone() {
            output_settings.filename_template = template;
        }
        if let Some(overwrite) = persisted.overwrite_existing {
            output_settings.overwrite_existing = overwrite;
        }

        Self {
            ffmpeg_status,
            items: Vec::new(),
            probe_worker: ProbeWorker::new(),
            next_id: 1,
            engine,
            engine_rx,
            concurrency,
            config,
            output_settings,
            output_folder_display,
            hardware_encoders,
            theme: persisted.theme,
            current_window_size: (persisted.window.width, persisted.window.height),
            show_log_for: None,
            show_licenses: false,
            show_settings: false,
            settings_message: None,
            override_ffmpeg: persisted.override_ffmpeg_path,
            override_ffprobe: persisted.override_ffprobe_path,
            override_ytdlp: persisted.override_ytdlp_path,

            active_tab: AppTab::Convert,
            download_engine,
            download_engine_rx,
            metadata_worker: MetadataWorker::new(),
            download_url_input: String::new(),
            download_cards: Vec::new(),
            download_items: Vec::new(),
            download_show_log_for: None,
            open_download_options: false,
            download_filename_template: "%(title)s.%(ext)s".to_string(),
            advanced_open: persisted.advanced_open.unwrap_or(false),
            notifications_enabled: persisted.notifications_enabled.unwrap_or(true),
            convert_was_active: false,
            convert_run: (0, 0, None),
            download_was_active: false,
            download_run: (0, 0, None),
            copy_note: None,
            thumb_cache: ThumbCache::new(),
            clipboard_watcher: ClipboardWatcher::new(),
            clipboard_watch_enabled: persisted.clipboard_watch_enabled.unwrap_or(false),
            clipboard_extra_hosts: persisted.clipboard_extra_hosts.clone().unwrap_or_default(),
            clipboard_hosts_applied: String::new(),
            clipboard_offers: clipboard_watch::Offers::default(),

            download_format: FormatChoice::Best,
            download_custom_format: String::new(),
            download_embed_thumbnail: false,
            download_embed_metadata: false,
            download_subtitles: false,
            download_sub_langs: "en".to_string(),
            download_auto_subs: false,
            download_sponsorblock: false,
            download_rate_limit_kbps: String::new(),
            download_concurrent_fragments: String::new(),
            download_chain: ChainChoice::None,
            download_output_dir: persisted
                .download_output_dir
                .clone()
                .unwrap_or_else(default_download_dir),

            cookie_choice: CookieChoice::None,
            cookie_browser: "firefox".to_string(),
            cookie_browser_profile: String::new(),
            cookie_file: None,

            update_worker: UpdateWorker::new(),
            ytdlp_auto_update_enabled: persisted.ytdlp_auto_update_enabled.unwrap_or(true),
            ytdlp_update_message: None,
            ytdlp_update_in_progress: false,
            auto_update_check_started: false,

            size_presets,
            show_presets_editor: false,
            presets_editor_message: None,
            new_preset_id: String::new(),
            new_preset_name: String::new(),
            new_preset_mib: 10,

            download_responsibility_acknowledged: persisted.download_responsibility_acknowledged,

            pot_manager: mediakit_core::pot_provider::PotProviderManager::new(),
            pot_config: mediakit_core::pot_provider::PotProviderConfig {
                enabled: persisted.pot_provider_enabled.unwrap_or(true),
                port: persisted
                    .pot_provider_port
                    .unwrap_or(mediakit_core::pot_provider::DEFAULT_PORT),
            },

            batch_ids: std::collections::HashSet::new(),
            batch_summary: None,
        }
    }

    fn persisted_settings(&self) -> PersistedSettings {
        PersistedSettings {
            window: crate::settings::WindowState {
                width: self.current_window_size.0,
                height: self.current_window_size.1,
            },
            theme: self.theme,
            concurrency: Some(self.concurrency),
            custom_output_folder: match &self.output_settings.location {
                OutputLocation::Custom(dir) => Some(dir.clone()),
                OutputLocation::SameAsSource => None,
            },
            filename_template: Some(self.output_settings.filename_template.clone()),
            overwrite_existing: Some(self.output_settings.overwrite_existing),
            override_ffmpeg_path: self.override_ffmpeg.clone(),
            override_ffprobe_path: self.override_ffprobe.clone(),
            override_ytdlp_path: self.override_ytdlp.clone(),
            ytdlp_auto_update_enabled: Some(self.ytdlp_auto_update_enabled),
            advanced_open: Some(self.advanced_open),
            notifications_enabled: Some(self.notifications_enabled),
            clipboard_watch_enabled: Some(self.clipboard_watch_enabled),
            clipboard_extra_hosts: Some(self.clipboard_extra_hosts.clone()),
            download_responsibility_acknowledged: self.download_responsibility_acknowledged,
            download_output_dir: Some(self.download_output_dir.clone()),
            pot_provider_enabled: Some(self.pot_config.enabled),
            pot_provider_port: Some(self.pot_config.port),
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let (add_files, convert_all, cancel_all, clear) = ctx.input_mut(|i| {
            (
                i.consume_shortcut(&SHORTCUT_ADD_FILES),
                i.consume_shortcut(&SHORTCUT_CONVERT_ALL),
                i.consume_shortcut(&SHORTCUT_CANCEL_ALL),
                i.consume_shortcut(&SHORTCUT_CLEAR),
            )
        });
        if add_files {
            if let Some(paths) = rfd::FileDialog::new().pick_files() {
                self.add_files(paths);
            }
        }
        if convert_all && !self.items.is_empty() && !self.jobs_active() {
            self.convert_all();
        }
        if cancel_all {
            self.cancel_all();
        }
        if clear {
            self.cancel_all();
            self.items.clear();
        }
    }

    fn ffprobe_path(&self) -> Option<PathBuf> {
        match &self.ffmpeg_status {
            FfmpegStatus::Ready(env) => Some(env.ffprobe_path.clone()),
            _ => None,
        }
    }

    fn add_files(&mut self, paths: Vec<PathBuf>) {
        let Some(ffprobe) = self.ffprobe_path() else {
            return;
        };
        for path in paths {
            if self.items.iter().any(|i| i.input == path) {
                continue;
            }
            let id = self.next_id;
            self.next_id += 1;
            self.probe_worker.submit(ffprobe.clone(), path.clone());
            self.items.push(QueueItem::new_pending(id, path));
        }
    }

    fn poll_probe_results(&mut self) {
        for result in self.probe_worker.poll() {
            let Some(idx) = self.items.iter().position(|i| i.input == result.path) else {
                continue;
            };
            let item = &mut self.items[idx];
            item.probing = false;
            match result.info {
                Ok(info) => item.info = Some(info),
                Err(err) => item.probe_error = Some(err),
            }

            if let Some(config) = self.items[idx].pending_auto_config.take() {
                self.submit_item_with_config(idx, &config);
            }
        }
    }

    fn poll_engine_events(&mut self) {
        let Some(rx) = &self.engine_rx else { return };
        let events: Vec<EngineEvent> = rx.try_iter().collect();
        let mut newly_done: Vec<usize> = Vec::new();
        for event in events {
            match &event {
                EngineEvent::Done { id, .. } => {
                    if let Some(idx) = self.items.iter().position(|i| i.id == *id) {
                        newly_done.push(idx);
                        self.convert_run.0 += 1;
                        self.convert_run.2 = file_name_of(&self.items[idx].output);
                    }
                }
                EngineEvent::Failed { id, .. } => {
                    self.convert_run.1 += 1;
                    self.convert_run.2 = self
                        .items
                        .iter()
                        .find(|i| i.id == *id)
                        .and_then(|i| file_name_of(&i.input));
                }
                _ => {}
            }
            crate::queue::apply_event(&mut self.items, event);
        }
        for idx in newly_done {
            if let Some(item) = self.items.get(idx) {
                if item.delete_input_when_done {
                    let _ = std::fs::remove_file(&item.input);
                }
            }
        }
    }

    fn submit_item(&mut self, idx: usize) {
        let config = self.config.clone();
        self.submit_item_with_config(idx, &config);
    }

    fn submit_item_with_config(&mut self, idx: usize, config: &EncodeConfig) {
        let Some(engine) = &self.engine else { return };
        let ext = config.output_extension();
        let (input, duration) = {
            let item = &self.items[idx];
            (item.input.clone(), item.duration_seconds())
        };
        let output = self.output_settings.resolve(&input, ext);
        let built = config.build(input, output.clone(), duration);

        let item = &mut self.items[idx];
        item.output = output;
        item.mode_label = config.mode.label().to_string();
        item.status = QueueStatus::Queued;
        item.log.clear();
        item.last_retry_note = None;
        item.target_size_limit_bytes = built.target_size.as_ref().map(|t| t.target_bytes);

        engine.submit(JobSpec {
            id: item.id,
            settings: built.settings,
            passes: built.passes,
            total_duration_seconds: duration,
            target_size: built.target_size,
        });
    }

    /// Runs the current conversion settings against every convertible file
    /// in the queue. Submission itself is instant (each item just gets
    /// pushed onto `JobEngine`'s queue); the engine's own worker pool - sized
    /// by `self.concurrency` - is what actually bounds how many run at once
    /// and keeps this off the UI thread. `batch_ids` is how
    /// `poll_batch_completion` later knows when to report a summary.
    fn convert_all(&mut self) {
        self.batch_summary = None;
        self.batch_ids.clear();
        for idx in 0..self.items.len() {
            if matches!(
                self.items[idx].status,
                QueueStatus::NotQueued | QueueStatus::Failed(_) | QueueStatus::Cancelled
            ) {
                self.batch_ids.insert(self.items[idx].id);
                self.submit_item(idx);
            }
        }
    }

    /// Once every job in the in-flight "Convert All" batch has reached a
    /// terminal state, compute and store a "n succeeded / n failed" summary
    /// and clear the batch. A no-op while any batch member is still
    /// queued/running, and when there's no batch in flight at all.
    /// Send one notification when a queue goes from busy to idle having
    /// finished something - so a Convert All batch or a pile of downloads
    /// yields a single summary, never one per file.
    fn notify_when_idle(&mut self) {
        let convert_active = self.jobs_active();
        if self.convert_was_active && !convert_active {
            let (ok, failed, name) = std::mem::take(&mut self.convert_run);
            if self.notifications_enabled && ok + failed > 0 {
                let (t, b) = notify::summarize("Conversion", ok, failed, name.as_deref());
                notify::send(t, b);
            }
        }
        self.convert_was_active = convert_active;

        let download_active = self.download_jobs_active();
        if self.download_was_active && !download_active {
            let (ok, failed, name) = std::mem::take(&mut self.download_run);
            if self.notifications_enabled && ok + failed > 0 {
                let (t, b) = notify::summarize("Download", ok, failed, name.as_deref());
                notify::send(t, b);
            }
        }
        self.download_was_active = download_active;
    }

    fn poll_batch_completion(&mut self) {
        if self.batch_ids.is_empty() {
            return;
        }
        let still_running = self.items.iter().any(|i| {
            self.batch_ids.contains(&i.id)
                && matches!(i.status, QueueStatus::Queued | QueueStatus::Running)
        });
        if still_running {
            return;
        }

        let mut succeeded = 0;
        let mut failed = 0;
        for item in &self.items {
            if !self.batch_ids.contains(&item.id) {
                continue;
            }
            match item.status {
                QueueStatus::Done => succeeded += 1,
                QueueStatus::Failed(_) | QueueStatus::Cancelled => failed += 1,
                _ => {}
            }
        }
        self.batch_summary = Some(format!(
            "Convert All finished: {succeeded} succeeded, {failed} failed"
        ));
        self.batch_ids.clear();
    }

    fn cancel_all(&mut self) {
        if let Some(engine) = &self.engine {
            engine.cancel_all();
        }
    }

    fn jobs_active(&self) -> bool {
        self.items
            .iter()
            .any(|i| matches!(i.status, QueueStatus::Queued | QueueStatus::Running))
    }

    /// Re-run tool detection (after an override change or a re-extract) and
    /// rebuild the job engine to point at whatever was just detected.
    /// Refuses while jobs are active, since recreating the engine would
    /// block the UI thread waiting for in-flight ffmpeg processes to exit.
    fn redetect_tools(&mut self) -> Result<(), &'static str> {
        if self.jobs_active() {
            return Err("Cancel or wait for active jobs before changing tool paths.");
        }

        let overrides = mediakit_core::ffmpeg_env::ToolOverrides {
            ffmpeg: self.override_ffmpeg.clone(),
            ffprobe: self.override_ffprobe.clone(),
            ytdlp: self.override_ytdlp.clone(),
        };
        let Ok(dir) = ffmpeg_env::app_data_dir() else {
            return Err("Could not resolve the app data directory.");
        };
        self.ffmpeg_status = match FfmpegEnv::detect_cached_with_overrides(&dir, &overrides) {
            Ok(env) => FfmpegStatus::Ready(Box::new(env)),
            Err(_) => FfmpegStatus::NotFound,
        };

        self.hardware_encoders = match &self.ffmpeg_status {
            FfmpegStatus::Ready(env) => hwaccel::detect_hardware_encoders(&env.encoders),
            FfmpegStatus::NotFound => Vec::new(),
        };

        // Drop the old engine (clean - no jobs are active) and build a new
        // one pointed at the freshly-detected ffmpeg path.
        self.engine = None;
        self.engine_rx = None;
        self.download_engine = None;
        self.download_engine_rx = None;
        if let FfmpegStatus::Ready(env) = &self.ffmpeg_status {
            let (engine, rx) = JobEngine::new(env.ffmpeg_path.clone(), self.concurrency);
            self.engine = Some(engine);
            self.engine_rx = Some(rx);
            if let Some(ytdlp) = &env.ytdlp_path {
                let (dl_engine, dl_rx) = DownloadEngine::new(ytdlp.clone(), 2);
                self.download_engine = Some(dl_engine);
                self.download_engine_rx = Some(dl_rx);
            }
        }

        Ok(())
    }

    /// Once per session: if auto-updates are on and it's been >= 24 h since
    /// the last check (per the state file `perform_update`/
    /// `check_for_update` maintain), kick off a background check-and-install.
    /// A no-op otherwise, so this is cheap to call on every frame until it
    /// fires.
    fn maybe_start_auto_update_check(&mut self) {
        if self.auto_update_check_started || !self.ytdlp_auto_update_enabled {
            return;
        }
        let FfmpegStatus::Ready(env) = &self.ffmpeg_status else {
            return;
        };
        let Some(current_version) = env.ytdlp_version.clone() else {
            return;
        };
        let Ok(bin_dir) = ffmpeg_env::app_data_dir() else {
            return;
        };

        let state = mediakit_core::ytdlp_update::load_state(&bin_dir);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let due = mediakit_core::ytdlp_update::auto_check_due(
            state.last_checked_unix,
            now,
            mediakit_core::ytdlp_update::AUTO_CHECK_INTERVAL_SECS,
        );

        self.auto_update_check_started = true;
        if due {
            self.ytdlp_update_in_progress = true;
            self.update_worker
                .auto_check_and_update(bin_dir, current_version);
        }
    }

    fn poll_update_events(&mut self) {
        for outcome in self.update_worker.poll() {
            self.ytdlp_update_in_progress = false;
            match outcome {
                UpdateOutcome::Checked { update_available } => {
                    self.ytdlp_update_message = Some(if update_available {
                        "A newer yt-dlp is available.".to_string()
                    } else {
                        "yt-dlp is already up to date.".to_string()
                    });
                }
                UpdateOutcome::Updated { new_version } => {
                    self.ytdlp_update_message = Some(format!("Updated yt-dlp to {new_version}."));
                    let _ = self.redetect_tools();
                }
                UpdateOutcome::RolledBack => {
                    self.ytdlp_update_message =
                        Some("Rolled back to the previous yt-dlp version.".to_string());
                    let _ = self.redetect_tools();
                }
                UpdateOutcome::Error(e) => {
                    self.ytdlp_update_message = Some(format!("yt-dlp update failed: {e}"));
                }
            }
        }
    }

    fn download_jobs_active(&self) -> bool {
        self.download_items.iter().any(|i| {
            matches!(
                i.status,
                DownloadQueueStatus::Queued | DownloadQueueStatus::Running
            )
        })
    }

    fn poll_metadata(&mut self) {
        let mut to_queue: Vec<(String, String)> = Vec::new();
        for result in self.metadata_worker.poll() {
            let Some(idx) = self.download_cards.iter().position(|c| c.url == result.url) else {
                continue;
            };
            let card = &mut self.download_cards[idx];
            match result.result {
                Ok(Metadata::Video(meta)) if card.auto_queue => {
                    to_queue.push((card.url.clone(), meta.title));
                    self.download_cards.remove(idx);
                }
                Ok(Metadata::Video(meta)) => card.state = DownloadCardState::Video(meta),
                Ok(Metadata::Playlist(playlist)) => {
                    card.state = DownloadCardState::Playlist {
                        title: playlist.title,
                        entries: playlist
                            .entries
                            .into_iter()
                            .map(|entry| PlaylistEntryUi {
                                entry,
                                selected: true,
                            })
                            .collect(),
                    }
                }
                Err(e) => card.state = DownloadCardState::Error(e),
            }
        }
        for (url, title) in to_queue {
            self.queue_download(url, title);
        }
    }

    fn poll_download_events(&mut self) {
        let Some(rx) = &self.download_engine_rx else {
            return;
        };
        let events: Vec<DownloadEvent> = rx.try_iter().collect();
        let mut newly_done: Vec<JobId> = Vec::new();
        for event in events {
            match &event {
                DownloadEvent::Done { id, .. } => {
                    newly_done.push(*id);
                    self.download_run.0 += 1;
                    self.download_run.2 = self
                        .download_items
                        .iter()
                        .find(|i| i.id == *id)
                        .map(|i| i.title.clone());
                }
                DownloadEvent::Failed { id, .. } => {
                    self.download_run.1 += 1;
                    self.download_run.2 = self
                        .download_items
                        .iter()
                        .find(|i| i.id == *id)
                        .map(|i| i.title.clone());
                }
                _ => {}
            }
            crate::download_queue::apply_event(&mut self.download_items, event);
        }
        for id in newly_done {
            self.chain_download_if_requested(id);
        }
    }

    /// If a just-finished download was queued with an "after download ->
    /// preset" chain target, feed the downloaded file straight into a
    /// conversion job. The intermediate download stays on disk (it's the
    /// user's own downloaded media, not a temp file), but the conversion
    /// output follows the normal output-settings/template.
    fn chain_download_if_requested(&mut self, download_id: JobId) {
        let Some(download_item) = self.download_items.iter().find(|i| i.id == download_id) else {
            return;
        };
        let Some(chain) = download_item.chain_to.clone() else {
            return;
        };
        let Some(output_path) = download_item.output_path.clone() else {
            return;
        };
        if !output_path.is_file() {
            return;
        }

        let mut config = EncodeConfig::default();
        match chain.as_str() {
            "discord10mb" => {
                let preset = self
                    .size_presets
                    .find("discord-free")
                    .or_else(|| self.size_presets.presets.first());
                if let Some(preset) = preset {
                    config.apply_size_preset(preset, self.size_presets.safety_margin_fraction());
                } else {
                    config.mode = Mode::TargetSize;
                }
            }
            "gif" => config.mode = Mode::VideoToGif,
            "mp3" => {
                config.mode = Mode::ExtractAudio;
                config.extract_audio_codec = AudioCodec::Mp3;
            }
            _ => return,
        }

        let Some(ffprobe) = self.ffprobe_path() else {
            return;
        };
        let id = self.next_id;
        self.next_id += 1;
        let mut item = QueueItem::new_pending(id, output_path.clone());
        // Auto-submitted once probing finishes (see `poll_probe_results`),
        // using this item's own config rather than whatever's currently in
        // the Advanced panel - queuing several different chained downloads
        // at once must not let them clobber each other's settings.
        item.pending_auto_config = Some(config);
        item.delete_input_when_done = true;
        self.items.push(item);
        self.probe_worker.submit(ffprobe, output_path);
    }

    fn ytdlp_path(&self) -> Option<PathBuf> {
        match &self.ffmpeg_status {
            FfmpegStatus::Ready(env) => env.ytdlp_path.clone(),
            FfmpegStatus::NotFound => None,
        }
    }

    fn current_cookie_source(&self) -> Option<CookieSource> {
        match self.cookie_choice {
            CookieChoice::None => None,
            CookieChoice::Browser => Some(CookieSource::Browser {
                browser: self.cookie_browser.clone(),
                profile: (!self.cookie_browser_profile.is_empty())
                    .then(|| self.cookie_browser_profile.clone()),
            }),
            CookieChoice::File => self.cookie_file.clone().map(CookieSource::File),
        }
    }

    /// Fetch metadata for every URL in the paste box. With `auto_queue`,
    /// single videos are queued as soon as their metadata arrives; playlists
    /// always stop at the per-item checkbox list.
    fn fetch_download_urls(&mut self, auto_queue: bool) {
        let Some(ytdlp) = self.ytdlp_path() else {
            return;
        };
        let cookies = self.current_cookie_source();
        for line in self.download_url_input.lines() {
            let url = line.trim().to_string();
            if url.is_empty() {
                continue;
            }
            self.clipboard_offers.handled(&url);
            if self.download_cards.iter().any(|c| c.url == url) {
                continue;
            }
            self.metadata_worker.submit(
                ytdlp.clone(),
                url.clone(),
                cookies.clone(),
                self.pot_context(),
            );
            self.download_cards.push(DownloadCard {
                url,
                state: DownloadCardState::Fetching,
                auto_queue,
            });
        }
        self.download_url_input.clear();
    }

    /// Downloads that will be chained into a conversion go into a temp
    /// directory rather than the user's normal download location, since
    /// they're an intermediate file that gets deleted once the chained
    /// conversion finishes.
    fn chain_temp_dir() -> PathBuf {
        std::env::temp_dir().join("mediakit-chained-downloads")
    }

    fn current_download_options(&self) -> DownloadOptions {
        let template = match self.download_filename_template.trim() {
            "" => "%(title)s.%(ext)s",
            t => t,
        };
        let output_template = if self.download_chain != ChainChoice::None {
            let _ = std::fs::create_dir_all(Self::chain_temp_dir());
            Self::chain_temp_dir()
                .join(template)
                .to_string_lossy()
                .into_owned()
        } else {
            let _ = std::fs::create_dir_all(&self.download_output_dir);
            self.download_output_dir
                .join(template)
                .to_string_lossy()
                .into_owned()
        };

        DownloadOptions {
            embed_thumbnail: self.download_embed_thumbnail,
            embed_metadata: self.download_embed_metadata,
            subtitles: self.download_subtitles.then(|| SubtitleOptions {
                languages: self
                    .download_sub_langs
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                auto_subs: self.download_auto_subs,
            }),
            sponsorblock_remove: self.download_sponsorblock,
            rate_limit_kbps: self.download_rate_limit_kbps.trim().parse().ok(),
            concurrent_fragments: self.download_concurrent_fragments.trim().parse().ok(),
            cookies: self.current_cookie_source(),
            output_template,
            extra_args: Vec::new(),
        }
    }

    /// What background threads need to build YouTube args (see
    /// `pot_provider::prepare_youtube_args`); `None` if the app data dir
    /// can't be resolved.
    fn pot_context(
        &self,
    ) -> Option<(
        mediakit_core::pot_provider::PotProviderManager,
        mediakit_core::pot_provider::PotProviderConfig,
        PathBuf,
    )> {
        let dir = ffmpeg_env::app_data_dir().ok()?;
        Some((self.pot_manager.clone(), self.pot_config, dir))
    }

    fn queue_download(&mut self, url: String, title: String) {
        let Some(engine) = &self.download_engine else {
            return;
        };
        let id = self.next_id;
        self.next_id += 1;

        let format = self.download_format.resolve(&self.download_custom_format);
        let options = self.current_download_options();
        let ffmpeg_dir = match &self.ffmpeg_status {
            FfmpegStatus::Ready(env) => env
                .ffmpeg_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default(),
            FfmpegStatus::NotFound => PathBuf::new(),
        };

        let mut item = DownloadQueueItem::new(id, url.clone(), title);
        if self.download_chain != ChainChoice::None {
            item.chain_to = Some(
                match self.download_chain {
                    ChainChoice::Discord10Mb => "discord10mb",
                    ChainChoice::VideoToGif => "gif",
                    ChainChoice::ExtractMp3 => "mp3",
                    ChainChoice::None => unreachable!(),
                }
                .to_string(),
            );
        }
        self.download_items.push(item);

        let spec = DownloadSpec {
            id,
            url: url.clone(),
            format,
            ffmpeg_dir,
            options,
        };

        // A YouTube URL is handed to a background thread instead of
        // submitted directly: the shared helper starts the PO-token server
        // (a no-op if already starting/ready) and *waits* for it - up to
        // DEFAULT_WAIT_BUDGET - so the args are attached to the very
        // download that triggered the server start. The JS-runtime args are
        // attached even if the server is disabled or fails. The wait runs
        // off the GUI thread via `DownloadEngine::handle()`, so the queue
        // item just shows "queued" a little longer on a cold start.
        if mediakit_core::pot_provider::is_youtube_url(&url) {
            let pot = self.pot_context();
            let handle = engine.handle();
            std::thread::spawn(move || {
                let mut spec = spec;
                spec.options.extra_args = match pot {
                    Some((manager, config, app_data_dir)) => {
                        mediakit_core::pot_provider::prepare_youtube_args(
                            &spec.url,
                            &manager,
                            config,
                            &app_data_dir,
                            mediakit_core::pot_provider::DEFAULT_WAIT_BUDGET,
                        )
                    }
                    None => mediakit_core::pot_provider::base_args_for_url(&spec.url),
                };
                handle.submit(spec);
            });
            return;
        }

        engine.submit(spec);
    }

    /// Carry out a non-Retry fix button. Retry needs the failed item's URL,
    /// so each call site handles it itself.
    fn apply_fix(&mut self, ctx: &egui::Context, action: FixAction) {
        match action {
            FixAction::UpdateYtdlp => {
                if let (Ok(bin_dir), false) =
                    (ffmpeg_env::app_data_dir(), self.ytdlp_update_in_progress)
                {
                    self.ytdlp_update_in_progress = true;
                    self.ytdlp_update_message = None;
                    self.update_worker.update(bin_dir);
                }
            }
            FixAction::UseCookies => {
                self.active_tab = AppTab::Download;
                self.open_download_options = true;
            }
            FixAction::InstallNode => ctx.open_url(egui::OpenUrl::new_tab("https://nodejs.org/")),
            FixAction::Retry => {}
        }
    }

    fn cancel_all_downloads(&mut self) {
        if let Some(engine) = &self.download_engine {
            engine.cancel_all();
        }
    }

    /// One-time notice shown before the Download tab can be used at all:
    /// MediaKit only ever shells out to yt-dlp (no site-specific extraction
    /// or DRM workarounds of its own), and the user - not MediaKit - is
    /// responsible for complying with the ToS/copyright of whatever they
    /// download. Dismissing it is persisted, so this shows once ever.
    fn download_responsibility_notice(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.heading("Before you use the Download tab");
            ui.add_space(8.0);
            ui.label(
                "MediaKit only shells out to yt-dlp - it doesn't implement any site's \
                 extraction logic itself, and it will not attempt to bypass DRM or other \
                 access controls.",
            );
            ui.add_space(4.0);
            ui.label(
                "You're responsible for complying with the terms of service and copyright \
                 law of whatever site or content you download from.",
            );
            ui.add_space(16.0);
            if ui.button("I understand").clicked() {
                self.download_responsibility_acknowledged = true;
            }
        });
    }

    fn download_tab_ui(&mut self, ui: &mut egui::Ui) {
        if !self.download_responsibility_acknowledged {
            self.download_responsibility_notice(ui);
            return;
        }
        if self.ytdlp_path().is_none() {
            ui.colored_label(
                egui::Color32::from_rgb(220, 90, 90),
                "yt-dlp not found (checked app data dir, next to exe, and PATH). \
                 The Download tab needs it - see Settings -> Tools.",
            );
            return;
        }

        ui.add(
            egui::TextEdit::multiline(&mut self.download_url_input)
                .desired_rows(4)
                .desired_width(f32::INFINITY)
                .hint_text("Paste one or more links here (one per line)"),
        );
        ui.horizontal(|ui| {
            ui.label("Format:");
            let before = self.download_format;
            egui::ComboBox::new("download_format_combo", "")
                .selected_text(self.download_format.label())
                .show_ui(ui, |ui| {
                    for choice in FormatChoice::ALL {
                        ui.selectable_value(&mut self.download_format, choice, choice.label());
                    }
                });
            if self.download_format == FormatChoice::Custom && before != FormatChoice::Custom {
                self.open_download_options = true;
            }
            ui.label("Then:");
            egui::ComboBox::new("download_chain_combo", "")
                .selected_text(self.download_chain.label())
                .show_ui(ui, |ui| {
                    for choice in ChainChoice::ALL {
                        ui.selectable_value(&mut self.download_chain, choice, choice.label());
                    }
                });
            if ui.small_button("Paste from clipboard").clicked() {
                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                    if let Ok(text) = clipboard.get_text() {
                        if !self.download_url_input.is_empty()
                            && !self.download_url_input.ends_with('\n')
                        {
                            self.download_url_input.push('\n');
                        }
                        self.download_url_input.push_str(&text);
                    }
                }
            }
        });
        self.download_output_dir_row(ui);
        let can_download = !self.download_url_input.trim().is_empty();
        if ui
            .add_enabled(
                can_download,
                egui::Button::new(egui::RichText::new("Download").size(18.0).strong())
                    .min_size(egui::vec2(ui.available_width(), 44.0)),
            )
            .clicked()
        {
            self.fetch_download_urls(true);
        }

        ui.separator();
        self.download_options_panel(ui);
        ui.separator();

        if !self.download_cards.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("download_cards_scroll")
                .max_height(260.0)
                .show(ui, |ui| {
                    self.download_cards_ui(ui);
                });
            ui.separator();
        }

        self.download_queue_ui(ui);
    }

    fn download_cards_ui(&mut self, ui: &mut egui::Ui) {
        let node_found = mediakit_core::pot_provider::node_available_if_known() == Some(true);
        let mut to_remove_card: Option<usize> = None;
        let mut card_fix: Option<(usize, FixAction)> = None;
        let mut to_queue: Vec<(String, String)> = Vec::new();

        for (idx, card) in self.download_cards.iter_mut().enumerate() {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(&card.url);
                    if ui.small_button("\u{2716}").clicked() {
                        to_remove_card = Some(idx);
                    }
                });
                match &mut card.state {
                    DownloadCardState::Fetching => {
                        ui.label("Fetching metadata\u{2026}");
                    }
                    DownloadCardState::Error(e) => {
                        if let Some(action) = error_hint::show(ui, e, node_found) {
                            card_fix = Some((idx, action));
                        }
                        egui::CollapsingHeader::new("Details")
                            .id_salt(("fetch_err_details", idx))
                            .show(ui, |ui| {
                                ui.label(e.as_str());
                            });
                    }
                    DownloadCardState::Video(meta) => {
                        ui.horizontal(|ui| {
                            ui.strong(&meta.title);
                            if let Some(uploader) = &meta.uploader {
                                ui.label(uploader);
                            }
                            if let Some(d) = meta.duration_seconds {
                                ui.label(format::duration(d));
                            }
                        });
                        if ui.button("Queue Download").clicked() {
                            to_queue.push((card.url.clone(), meta.title.clone()));
                        }
                    }
                    DownloadCardState::Playlist { title, entries } => {
                        ui.strong(format!("Playlist: {title} ({} items)", entries.len()));
                        ui.horizontal(|ui| {
                            if ui.small_button("Select all").clicked() {
                                for e in entries.iter_mut() {
                                    e.selected = true;
                                }
                            }
                            if ui.small_button("Select none").clicked() {
                                for e in entries.iter_mut() {
                                    e.selected = false;
                                }
                            }
                        });
                        egui::ScrollArea::vertical()
                            .id_salt(format!("playlist_entries_{idx}"))
                            .max_height(150.0)
                            .show(ui, |ui| {
                                for entry_ui in entries.iter_mut() {
                                    ui.checkbox(&mut entry_ui.selected, &entry_ui.entry.title);
                                }
                            });
                        if ui.button("Queue Selected").clicked() {
                            for e in entries.iter().filter(|e| e.selected) {
                                to_queue.push((e.entry.url.clone(), e.entry.title.clone()));
                            }
                        }
                    }
                }
            });
        }

        if let Some((idx, action)) = card_fix {
            let url = self.download_cards[idx].url.clone();
            if action == FixAction::Retry {
                // Drop the failed card and fetch again.
                self.download_cards.remove(idx);
                self.download_url_input = url;
                self.fetch_download_urls(true);
            } else {
                self.apply_fix(&ui.ctx().clone(), action);
            }
        }
        if let Some(idx) = to_remove_card {
            if idx < self.download_cards.len() {
                self.download_cards.remove(idx);
            }
        }
        for (url, title) in to_queue {
            self.queue_download(url, title);
        }
    }

    fn download_output_dir_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Save to:");
            ui.label(self.download_output_dir.to_string_lossy());
            if ui.button("Browse\u{2026}").clicked() {
                if let Some(dir) = rfd::FileDialog::new()
                    .set_directory(&self.download_output_dir)
                    .pick_folder()
                {
                    self.download_output_dir = dir;
                }
            }
        });
    }

    fn download_options_panel(&mut self, ui: &mut egui::Ui) {
        let force_open = std::mem::take(&mut self.open_download_options).then_some(true);
        egui::CollapsingHeader::new("Options")
            .default_open(false)
            .open(force_open)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Custom format selector:")
                        .on_hover_text("Used when Format is set to Custom (yt-dlp -f syntax)");
                    ui.text_edit_singleline(&mut self.download_custom_format);
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.download_embed_thumbnail, "Embed thumbnail");
                    ui.checkbox(&mut self.download_embed_metadata, "Embed metadata");
                    ui.checkbox(&mut self.download_sponsorblock, "SponsorBlock remove");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.download_subtitles, "Subtitles");
                    if self.download_subtitles {
                        ui.label("Languages:");
                        ui.text_edit_singleline(&mut self.download_sub_langs);
                        ui.checkbox(&mut self.download_auto_subs, "Auto-generated");
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Rate limit (KB/s):");
                    ui.text_edit_singleline(&mut self.download_rate_limit_kbps);
                    ui.label("Concurrent fragments:");
                    ui.text_edit_singleline(&mut self.download_concurrent_fragments);
                });
                ui.horizontal(|ui| {
                    ui.label("Filename template:");
                    ui.text_edit_singleline(&mut self.download_filename_template);
                });
                self.cookie_options_ui(ui);
                self.download_command_preview(ui);
            });
    }

    fn cookie_options_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Login cookies:");
            egui::ComboBox::new("cookie_choice_combo", "")
                .selected_text(match self.cookie_choice {
                    CookieChoice::None => "None",
                    CookieChoice::Browser => "From browser",
                    CookieChoice::File => "cookies.txt file",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.cookie_choice, CookieChoice::None, "None");
                    ui.selectable_value(
                        &mut self.cookie_choice,
                        CookieChoice::Browser,
                        "From browser",
                    );
                    ui.selectable_value(
                        &mut self.cookie_choice,
                        CookieChoice::File,
                        "cookies.txt file",
                    );
                });
            match self.cookie_choice {
                CookieChoice::Browser => {
                    egui::ComboBox::new("cookie_browser_combo", "")
                        .selected_text(self.cookie_browser.clone())
                        .show_ui(ui, |ui| {
                            for b in ["firefox", "chrome", "chromium", "edge", "brave", "safari"] {
                                ui.selectable_value(&mut self.cookie_browser, b.to_string(), b);
                            }
                        });
                    ui.label("Profile (optional):");
                    ui.text_edit_singleline(&mut self.cookie_browser_profile);
                }
                CookieChoice::File => {
                    let label = self
                        .cookie_file
                        .as_ref()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "(none)".to_string());
                    ui.label(label);
                    if ui.small_button("Browse\u{2026}").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            self.cookie_file = Some(path);
                        }
                    }
                }
                CookieChoice::None => {}
            }
        });
        ui.label(
            "Cookie values are never stored or logged by MediaKit, and are redacted from the \
             command preview below.",
        );
    }

    fn download_command_preview(&self, ui: &mut egui::Ui) {
        let Some(ytdlp) = self.ytdlp_path() else {
            return;
        };
        let ffmpeg_dir = match &self.ffmpeg_status {
            FfmpegStatus::Ready(env) => env
                .ffmpeg_path
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default(),
            FfmpegStatus::NotFound => return,
        };
        let format = self.download_format.resolve(&self.download_custom_format);
        let options = self.current_download_options();
        let sample_url = self
            .download_cards
            .first()
            .map(|c| c.url.clone())
            .unwrap_or_else(|| "<url>".to_string());
        let args =
            downloader::build_download_args_redacted(&sample_url, &format, &ffmpeg_dir, &options);
        let preview = mediakit_core::command::preview_command_string(&ytdlp, &args);
        ui.separator();
        ui.label("Command preview:");
        ui.add(
            egui::TextEdit::multiline(&mut preview.clone())
                .desired_rows(2)
                .font(egui::TextStyle::Monospace),
        );
        if ui.button("Copy command").clicked() {
            ui.ctx().copy_text(preview);
        }
    }

    fn download_queue_ui(&mut self, ui: &mut egui::Ui) {
        let node_found = mediakit_core::pot_provider::node_available_if_known() == Some(true);
        if self.download_items.is_empty() {
            return;
        }

        ui.horizontal(|ui| {
            ui.heading("Downloads");
            if ui.button("Cancel All").clicked() {
                self.cancel_all_downloads();
            }
            if ui.button("Clear").clicked() {
                self.cancel_all_downloads();
                self.download_items.clear();
            }
        });

        let mut to_cancel: Option<JobId> = None;
        let mut to_open_folder: Option<PathBuf> = None;
        let mut to_copy: Option<PathBuf> = None;
        let copy_hint = self.copy_note.clone();
        let mut to_show_log: Option<JobId> = None;
        let mut to_fix: Option<(JobId, FixAction)> = None;

        egui::ScrollArea::vertical()
            .id_salt("download_queue_scroll")
            .show(ui, |ui| {
                egui::Grid::new("download_queue_grid")
                    .striped(true)
                    .num_columns(5)
                    .show(ui, |ui| {
                        ui.strong("Title");
                        ui.strong("Status");
                        ui.strong("Progress");
                        ui.strong("Speed / ETA");
                        ui.strong("Actions");
                        ui.end_row();

                        for item in &self.download_items {
                            let mut fix_action: Option<(JobId, FixAction)> = None;
                            ui.label(&item.title).on_hover_text(&item.url);

                            match &item.status {
                                DownloadQueueStatus::Queued => {
                                    ui.label("queued");
                                }
                                DownloadQueueStatus::Running => {
                                    ui.label("running");
                                }
                                DownloadQueueStatus::Done => {
                                    ui.colored_label(egui::Color32::from_rgb(80, 180, 100), "done");
                                }
                                DownloadQueueStatus::Failed(err) => {
                                    let (sentence, action) = error_hint::hint(
                                        downloader::classify_ytdlp_error(&format!(
                                            "{err}\n{}",
                                            item.log
                                        )),
                                        node_found,
                                    );
                                    fix_action = Some((item.id, action));
                                    ui.vertical(|ui| {
                                        ui.set_max_width(260.0);
                                        ui.colored_label(
                                            egui::Color32::from_rgb(220, 90, 90),
                                            "failed",
                                        )
                                        .on_hover_text(err);
                                        ui.small(sentence);
                                    });
                                }
                                DownloadQueueStatus::Cancelled => {
                                    ui.label("cancelled");
                                }
                            }

                            let percent = item.percent.unwrap_or(0.0);
                            ui.add(
                                egui::ProgressBar::new((percent / 100.0) as f32)
                                    .text(format!("{percent:.0}%")),
                            );

                            ui.label(format!(
                                "{} / {}",
                                item.speed.as_deref().unwrap_or("-"),
                                item.eta.as_deref().unwrap_or("-")
                            ));

                            ui.horizontal(|ui| {
                                if matches!(
                                    item.status,
                                    DownloadQueueStatus::Queued | DownloadQueueStatus::Running
                                ) && ui.small_button("Cancel").clicked()
                                {
                                    to_cancel = Some(item.id);
                                }
                                if item.status == DownloadQueueStatus::Done {
                                    if let Some(path) = &item.output_path {
                                        if ui.small_button("Open folder").clicked() {
                                            to_open_folder = Some(path.clone());
                                        }
                                        if copy_file_button(ui, &copy_hint) {
                                            to_copy = Some(path.clone());
                                        }
                                    }
                                }
                                if let (DownloadQueueStatus::Failed(_), Some((_, action))) =
                                    (&item.status, fix_action)
                                {
                                    if ui.small_button(action.label()).clicked() {
                                        to_fix = Some((item.id, action));
                                    }
                                    if action != FixAction::Retry
                                        && ui.small_button("Retry").clicked()
                                    {
                                        to_fix = Some((item.id, FixAction::Retry));
                                    }
                                }
                                if !item.log.is_empty() && ui.small_button("Show log").clicked() {
                                    to_show_log = Some(item.id);
                                }
                            });
                            ui.end_row();
                        }
                    });
            });

        if let Some((id, action)) = to_fix {
            if action == FixAction::Retry {
                if let Some(pos) = self.download_items.iter().position(|i| i.id == id) {
                    let item = self.download_items.remove(pos);
                    self.queue_download(item.url, item.title);
                }
            } else {
                self.apply_fix(&ui.ctx().clone(), action);
            }
        }
        if let Some(id) = to_cancel {
            if let Some(engine) = &self.download_engine {
                engine.cancel(id);
            }
        }
        if let Some(path) = to_open_folder {
            sys::reveal_in_file_manager(&path);
        }
        if let Some(path) = to_copy {
            self.copy_file(ui.ctx(), &path);
        }
        if let Some(id) = to_show_log {
            self.download_show_log_for = Some(id);
        }
    }

    fn download_log_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.download_show_log_for else {
            return;
        };
        let Some(item) = self.download_items.iter().find(|i| i.id == id) else {
            self.download_show_log_for = None;
            return;
        };
        let mut open = true;
        egui::Window::new(format!("Log: {}", item.title))
            .order(egui::Order::Foreground)
            .open(&mut open)
            .default_size([600.0, 400.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut item.log.clone())
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                });
            });
        if !open {
            self.download_show_log_for = None;
        }
    }

    fn top_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top_panel").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("MediaKit");
                ui.separator();
                ui.selectable_value(&mut self.active_tab, AppTab::Convert, "Convert");
                ui.selectable_value(&mut self.active_tab, AppTab::Download, "Download");
                ui.separator();
                if ui
                    .button("Add Files\u{2026}")
                    .on_hover_text("Ctrl+O")
                    .clicked()
                {
                    if let Some(paths) = rfd::FileDialog::new().pick_files() {
                        self.add_files(paths);
                    }
                }
                ui.separator();
                let convert_all_enabled = !self.items.is_empty() && !self.jobs_active();
                if ui
                    .add_enabled(convert_all_enabled, egui::Button::new("Convert All"))
                    .on_hover_text("Ctrl+Enter")
                    .clicked()
                {
                    self.convert_all();
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let can_cancel = self.jobs_active();
                    ui.menu_button("\u{2026}", |ui| {
                        if ui.button("Presets\u{2026}").clicked() {
                            self.show_presets_editor = true;
                            ui.close();
                        }
                        if ui.button("Licenses").clicked() {
                            self.show_licenses = true;
                            ui.close();
                        }
                        ui.separator();
                        if ui
                            .add(egui::Button::new("Clear queue").shortcut_text("Ctrl+W"))
                            .clicked()
                        {
                            self.cancel_all();
                            self.items.clear();
                            ui.close();
                        }
                        if ui
                            .add_enabled(
                                can_cancel,
                                egui::Button::new("Cancel all").shortcut_text("Ctrl+."),
                            )
                            .clicked()
                        {
                            self.cancel_all();
                            ui.close();
                        }
                    });
                    if ui.button("Settings").clicked() {
                        self.show_settings = true;
                    }
                });
            });
        });
    }

    fn status_inputs(&self) -> StatusInputs {
        let (ffmpeg_version, ytdlp_version) = match &self.ffmpeg_status {
            FfmpegStatus::Ready(env) => (Some(env.version.clone()), env.ytdlp_version.clone()),
            FfmpegStatus::NotFound => (None, None),
        };
        StatusInputs {
            ffmpeg_version,
            ytdlp_version,
            node_available: mediakit_core::pot_provider::node_available_if_known(),
            pot_enabled: self.pot_config.enabled,
            pot_state: self.pot_manager.state(),
        }
    }

    /// The one status dot: colour from [`status::health`], tooltip listing
    /// every component, click for a popup with the same details plus the
    /// relevant actions.
    fn status_dot(&mut self, ui: &mut egui::Ui) {
        let inputs = self.status_inputs();
        if inputs.node_available.is_none() {
            // The startup Node probe can't wake the UI when it finishes.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        let health = status::health(&inputs);
        let color = match health {
            Health::Green => egui::Color32::from_rgb(80, 180, 100),
            Health::Yellow => egui::Color32::from_rgb(220, 180, 80),
            Health::Red => egui::Color32::from_rgb(220, 90, 90),
            Health::Grey => egui::Color32::GRAY,
        };
        let components = status::components(&inputs);
        let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
        ui.painter().circle_filled(rect.center(), 6.0, color);
        let response = response.on_hover_ui(|ui| {
            for c in &components {
                ui.label(format!("{}: {}", c.name, c.detail));
            }
        });
        let mut restart = false;
        let mut open_settings = false;
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(260.0);
            for c in &components {
                ui.label(format!("{}: {}", c.name, c.detail));
            }
            ui.separator();
            if self.pot_config.enabled && ui.button("Restart PO-token server").clicked() {
                restart = true;
                ui.close();
            }
            if ui.button("Open Settings (Tools)").clicked() {
                open_settings = true;
                ui.close();
            }
        });
        if restart {
            self.pot_manager.shutdown();
            if let Ok(dir) = ffmpeg_env::app_data_dir() {
                self.pot_manager
                    .ensure_started(self.pot_config, dir.clone(), dir);
            }
        }
        if open_settings {
            self.show_settings = true;
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                let queued = self
                    .items
                    .iter()
                    .filter(|i| i.status == QueueStatus::Queued)
                    .count();
                let running = self
                    .items
                    .iter()
                    .filter(|i| i.status == QueueStatus::Running)
                    .count();
                let done = self
                    .items
                    .iter()
                    .filter(|i| i.status == QueueStatus::Done)
                    .count();
                let failed = self
                    .items
                    .iter()
                    .filter(|i| matches!(i.status, QueueStatus::Failed(_)))
                    .count();
                ui.label(format!(
                    "{} items - {queued} queued, {running} running, {done} done, {failed} failed",
                    self.items.len()
                ));

                if !self.batch_ids.is_empty() {
                    let batch_total = self.batch_ids.len();
                    let batch_done = self
                        .items
                        .iter()
                        .filter(|i| {
                            self.batch_ids.contains(&i.id)
                                && !matches!(i.status, QueueStatus::Queued | QueueStatus::Running)
                        })
                        .count();
                    ui.separator();
                    ui.add(
                        egui::ProgressBar::new(batch_done as f32 / batch_total as f32)
                            .text(format!("Convert All: {batch_done}/{batch_total}")),
                    );
                } else if let Some(summary) = &self.batch_summary {
                    ui.separator();
                    ui.label(summary);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.status_dot(ui);
                });
            });
        });
    }

    /// Large clickable cards for the common jobs, then a "More\u{2026}" menu
    /// holding every other preset so nothing is unreachable.
    fn preset_row(&mut self, ui: &mut egui::Ui) {
        let active = Card::for_mode(self.config.mode, self.config.extract_audio_codec);
        ui.horizontal_wrapped(|ui| {
            for card in Card::ALL {
                let text = egui::RichText::new(format!("{}\n{}", card.name(), card.description()))
                    .size(13.0);
                let button = egui::Button::new(text)
                    .selected(active == Some(card))
                    .min_size(egui::vec2(170.0, 52.0));
                if ui.add(button).clicked() {
                    self.select_card(card);
                }
            }
            let more_label = if active.is_none() {
                format!("More: {}\u{2026}", self.config.mode.label())
            } else {
                "More\u{2026}".to_string()
            };
            ui.menu_button(more_label, |ui| self.more_presets_menu(ui));
        });
    }

    fn select_card(&mut self, card: Card) {
        match card {
            Card::DiscordClip => {
                // Keep whichever size preset is already chosen; only fall
                // back to the first configured one when nothing is.
                let safety_margin = self.size_presets.safety_margin_fraction();
                if matches!(
                    self.config.target_size_choice,
                    TargetSizeChoice::Preset { .. }
                ) {
                    self.config.mode = Mode::TargetSize;
                } else if let Some(preset) = self.size_presets.presets.first().cloned() {
                    self.config.apply_size_preset(&preset, safety_margin);
                } else {
                    self.config.mode = Mode::TargetSize;
                }
            }
            Card::Gif => self.config.mode = Mode::VideoToGif,
            Card::Mp3 => {
                self.config.mode = Mode::ExtractAudio;
                self.config.extract_audio_codec = AudioCodec::Mp3;
            }
            Card::Compress => self.config.mode = Mode::Video,
            Card::Image => self.config.mode = Mode::ConvertImage,
        }
    }

    fn more_presets_menu(&mut self, ui: &mut egui::Ui) {
        // Data-driven, not hardcoded: every size button comes from
        // presets.toml (Settings -> Presets), so platform caps changing
        // (Discord alone has moved three times) never needs a code
        // change, just an edit to that file.
        let safety_margin = self.size_presets.safety_margin_fraction();
        for preset in self.size_presets.presets.clone() {
            if ui.button(&preset.display_name).clicked() {
                self.config.apply_size_preset(&preset, safety_margin);
                ui.close();
            }
        }
        ui.separator();
        for mode in Mode::ALL {
            if ui
                .selectable_label(self.config.mode == mode, mode.label())
                .clicked()
            {
                self.config.mode = mode;
                if mode == Mode::GifToVideo {
                    self.config.gif_to_video_container = Container::Mp4;
                    self.config.gif_to_video_codec = VideoCodec::H264;
                }
                ui.close();
            }
        }
    }

    /// The controls that matter for the selected preset, shown right under
    /// the cards. Codec/CRF/trim/hardware/raw args live in Advanced.
    fn primary_controls(&mut self, ui: &mut egui::Ui) {
        match self.config.mode {
            Mode::Video => {
                ui.label("Re-encodes with the quality set under Advanced.");
            }
            Mode::TargetSize => self.target_size_controls(ui),
            Mode::VideoToGif => self.gif_controls(ui),
            Mode::ImageToGif => self.image_to_gif_controls(ui),
            Mode::GifToVideo => self.gif_to_video_controls(ui),
            Mode::ExtractAudio => self.extract_audio_controls(ui),
            Mode::ConvertImage => self.convert_image_controls(ui),
            Mode::RotateFlip => self.rotate_flip_controls(ui),
            Mode::SpeedChange => self.speed_controls(ui),
            Mode::Mute | Mode::StripMetadata | Mode::Reverse => {
                ui.label("No additional options for this preset.");
            }
        }
    }

    fn advanced_panel(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("Advanced")
            .id_salt("convert_advanced")
            .default_open(self.advanced_open)
            .show(ui, |ui| {
                match self.config.mode {
                    Mode::Video => self.video_controls(ui),
                    Mode::TargetSize => {
                        self.hardware_accel_controls(ui);
                        self.audio_controls(ui);
                    }
                    _ => {}
                }

                // Image -> GIF's duration field above *is* its trim (via
                // `presets::image_to_gif`'s own `-loop 1` + `-to`) - the
                // generic trim controls would just be dead/confusing here.
                if self.config.mode != Mode::ImageToGif {
                    ui.separator();
                    self.trim_controls(ui);
                }

                ui.separator();
                ui.checkbox(
                    &mut self.config.disable_compat_defaults,
                    "Skip compatibility defaults (forced yuv420p + faststart on H.264 MP4)",
                )
                .on_hover_text(
                    "MediaKit forces yuv420p pixel format and -movflags +faststart on H.264 \
                     MP4 output by default - that's what keeps files playable in Discord, \
                     QuickTime, and browsers instead of just playing locally. Check this to \
                     opt out (e.g. if you need a non-4:2:0 pixel format).",
                );

                ui.separator();
                ui.label("Custom ffmpeg args (appended verbatim):");
                ui.text_edit_singleline(&mut self.config.custom_args);

                ui.separator();
                self.command_preview(ui);
            });
        // Remember whether it was left open (saved with the other settings).
        let id = ui.make_persistent_id("convert_advanced");
        if let Some(state) = egui::collapsing_header::CollapsingState::load(ui.ctx(), id) {
            self.advanced_open = state.is_open();
        }
    }

    fn video_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Container:");
            egui::ComboBox::new("container_combo", "")
                .selected_text(self.config.container.extension())
                .show_ui(ui, |ui| {
                    for c in [Container::Mp4, Container::WebM, Container::Mkv] {
                        ui.selectable_value(&mut self.config.container, c, c.extension());
                    }
                });
            ui.label("Codec:");
            egui::ComboBox::new("video_codec_combo", "")
                .selected_text(video_codec_label(self.config.video_codec))
                .show_ui(ui, |ui| {
                    for c in [
                        VideoCodec::H264,
                        VideoCodec::H265,
                        VideoCodec::Vp9,
                        VideoCodec::Av1,
                    ] {
                        ui.selectable_value(&mut self.config.video_codec, c, video_codec_label(c));
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label("CRF (quality):");
            ui.add(egui::Slider::new(&mut self.config.crf, 0..=51));
            ui.label("Speed preset:");
            egui::ComboBox::new("speed_preset_combo", "")
                .selected_text(self.config.speed_preset.clone())
                .show_ui(ui, |ui| {
                    for p in [
                        "ultrafast",
                        "superfast",
                        "veryfast",
                        "faster",
                        "fast",
                        "medium",
                        "slow",
                        "slower",
                        "veryslow",
                    ] {
                        ui.selectable_value(&mut self.config.speed_preset, p.to_string(), p);
                    }
                });
        });
        self.resolution_controls(ui);
        ui.horizontal(|ui| {
            ui.label("FPS:");
            let mut fps_enabled = self.config.fps.is_some();
            if ui.checkbox(&mut fps_enabled, "").changed() {
                self.config.fps = if fps_enabled { Some(30.0) } else { None };
            }
            if let Some(fps) = self.config.fps.as_mut() {
                ui.add(egui::DragValue::new(fps).range(1.0..=240.0));
            }
        });
        self.hardware_accel_controls(ui);
        self.audio_controls(ui);
    }

    fn resolution_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Width:");
            let mut w = self.config.width.unwrap_or(0);
            if ui
                .add(egui::DragValue::new(&mut w).range(0..=7680))
                .changed()
            {
                self.config.width = if w == 0 { None } else { Some(w) };
            }
            ui.label("Height:");
            let mut h = self.config.height.unwrap_or(0);
            if ui
                .add(egui::DragValue::new(&mut h).range(0..=4320))
                .changed()
            {
                self.config.height = if h == 0 { None } else { Some(h) };
            }
            ui.checkbox(&mut self.config.keep_aspect, "Keep aspect ratio");
        });
    }

    fn audio_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Audio codec:");
            egui::ComboBox::new("audio_codec_combo", "")
                .selected_text(audio_codec_label(self.config.audio_codec))
                .show_ui(ui, |ui| {
                    for c in [AudioCodec::Aac, AudioCodec::Opus, AudioCodec::Mp3] {
                        ui.selectable_value(&mut self.config.audio_codec, c, audio_codec_label(c));
                    }
                });
            ui.label("Bitrate (kbps):");
            ui.add(egui::DragValue::new(&mut self.config.audio_bitrate_kbps).range(32..=320));
        });
    }

    fn target_size_controls(&mut self, ui: &mut egui::Ui) {
        let safety_margin = self.size_presets.safety_margin_fraction();
        ui.horizontal(|ui| {
            ui.label("Target:");
            egui::ComboBox::new("target_size_combo", "")
                .selected_text(self.config.target_size_choice.label().to_string())
                .show_ui(ui, |ui| {
                    for preset in self.size_presets.presets.clone() {
                        let choice = TargetSizeChoice::from_preset(&preset);
                        if ui
                            .selectable_value(
                                &mut self.config.target_size_choice,
                                choice,
                                &preset.display_name,
                            )
                            .clicked()
                        {
                            self.config.target_size_safety_margin = safety_margin;
                        }
                    }
                    if ui
                        .selectable_value(
                            &mut self.config.target_size_choice,
                            TargetSizeChoice::Custom,
                            "Custom target size\u{2026}",
                        )
                        .clicked()
                    {
                        self.config.target_size_safety_margin = safety_margin;
                    }
                });
            if self.config.target_size_choice == TargetSizeChoice::Custom {
                ui.label("MiB:").on_hover_text(
                    "MiB = 1024x1024 bytes (matches what file managers/du report), \
                     not decimal MB.",
                );
                ui.add(egui::DragValue::new(&mut self.config.custom_target_mib).range(1..=200_000));
            }
        });
    }

    /// Hardware encoder picker for whichever [`VideoCodec`] is currently
    /// selected. Always defaults to software (`None`) for reliability;
    /// hardware is strictly opt-in, and the engine falls back to software
    /// automatically if the hardware encode fails (see `core::engine`).
    fn hardware_accel_controls(&mut self, ui: &mut egui::Ui) {
        let available = hwaccel::for_codec(&self.hardware_encoders, self.config.video_codec);

        // Selecting a codec the current hardware pick doesn't support (e.g.
        // switching from H.264 to VP9 with NVENC selected) silently reverts
        // to software rather than submitting a mismatched encoder.
        if let Some(current) = &self.config.hardware_encoder {
            if !available.iter().any(|e| &e.encoder_name == current) {
                self.config.hardware_encoder = None;
            }
        }

        if available.is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            ui.label("Hardware acceleration:");
            let selected_text = self
                .config
                .hardware_encoder
                .as_deref()
                .unwrap_or("Software (reliable)")
                .to_string();
            egui::ComboBox::new("hwaccel_combo", "")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.config.hardware_encoder,
                        None,
                        "Software (reliable)",
                    );
                    for enc in &available {
                        ui.selectable_value(
                            &mut self.config.hardware_encoder,
                            Some(enc.encoder_name.clone()),
                            format!("{} ({})", enc.api.label(), enc.encoder_name),
                        );
                    }
                });
        });
    }

    fn gif_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("FPS:");
            ui.add(egui::DragValue::new(&mut self.config.gif_fps).range(1..=60));
            ui.label("Width:");
            ui.add(egui::DragValue::new(&mut self.config.gif_width).range(16..=1920));
            ui.checkbox(&mut self.config.gif_dither, "Dither");
        });
    }

    fn image_to_gif_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Duration (s):");
            ui.add(
                egui::DragValue::new(&mut self.config.image_gif_duration_seconds)
                    .range(0.1..=60.0)
                    .speed(0.1),
            );
            ui.label("FPS:");
            ui.add(egui::DragValue::new(&mut self.config.image_gif_fps).range(1..=60));
            ui.label("Width:");
            ui.add(egui::DragValue::new(&mut self.config.image_gif_width).range(16..=1920));
        });
        ui.label("Turns one still image into a short, looping GIF of the chosen length.");
    }

    fn gif_to_video_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Container:");
            egui::ComboBox::new("gif_container_combo", "")
                .selected_text(self.config.gif_to_video_container.extension())
                .show_ui(ui, |ui| {
                    for c in [Container::Mp4, Container::WebM, Container::Mkv] {
                        ui.selectable_value(
                            &mut self.config.gif_to_video_container,
                            c,
                            c.extension(),
                        );
                    }
                });
            ui.label("Codec:");
            egui::ComboBox::new("gif_codec_combo", "")
                .selected_text(video_codec_label(self.config.gif_to_video_codec))
                .show_ui(ui, |ui| {
                    for c in [VideoCodec::H264, VideoCodec::Vp9] {
                        ui.selectable_value(
                            &mut self.config.gif_to_video_codec,
                            c,
                            video_codec_label(c),
                        );
                    }
                });
        });
    }

    fn extract_audio_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Format:");
            egui::ComboBox::new("extract_audio_combo", "")
                .selected_text(audio_codec_label(self.config.extract_audio_codec))
                .show_ui(ui, |ui| {
                    for c in [
                        AudioCodec::Mp3,
                        AudioCodec::Opus,
                        AudioCodec::Flac,
                        AudioCodec::Pcm,
                    ] {
                        ui.selectable_value(
                            &mut self.config.extract_audio_codec,
                            c,
                            audio_codec_label(c),
                        );
                    }
                });
            ui.label("Bitrate (kbps):");
            ui.add(
                egui::DragValue::new(&mut self.config.extract_audio_bitrate_kbps).range(32..=320),
            );
        });
    }

    fn convert_image_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Format:");
            egui::ComboBox::new("image_format_combo", "")
                .selected_text(image_format_label(self.config.image_format))
                .show_ui(ui, |ui| {
                    for f in [
                        ImageFormat::Png,
                        ImageFormat::Jpg,
                        ImageFormat::Webp,
                        ImageFormat::Avif,
                        ImageFormat::Bmp,
                        ImageFormat::Ico,
                    ] {
                        ui.selectable_value(
                            &mut self.config.image_format,
                            f,
                            image_format_label(f),
                        );
                    }
                });
            ui.label("Quality:");
            ui.add(egui::Slider::new(&mut self.config.image_quality, 0..=100));
        });
    }

    fn rotate_flip_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Rotate:");
            egui::ComboBox::new("rotation_combo", "")
                .selected_text(rotation_label(self.config.rotation))
                .show_ui(ui, |ui| {
                    for r in [
                        Rotation::None,
                        Rotation::Cw90,
                        Rotation::Ccw90,
                        Rotation::Rotate180,
                    ] {
                        ui.selectable_value(&mut self.config.rotation, r, rotation_label(r));
                    }
                });
            ui.label("Flip:");
            egui::ComboBox::new("flip_combo", "")
                .selected_text(flip_label(self.config.flip))
                .show_ui(ui, |ui| {
                    for f in [
                        FlipMode::None,
                        FlipMode::Horizontal,
                        FlipMode::Vertical,
                        FlipMode::Both,
                    ] {
                        ui.selectable_value(&mut self.config.flip, f, flip_label(f));
                    }
                });
        });
    }

    fn speed_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Speed factor:");
            ui.add(egui::Slider::new(&mut self.config.speed_factor, 0.25..=4.0).logarithmic(true));
        });
    }

    /// Single-file video jobs get the thumbnail timeline; everything else
    /// (several files selected, audio/image input, not probed yet) falls back
    /// to the text fields alone.
    fn timeline_target(&self) -> Option<(PathBuf, f64)> {
        let [item] = self.items.as_slice() else {
            return None;
        };
        let info = item.info.as_ref()?;
        (info.video.is_some() && info.duration_seconds > 0.0)
            .then(|| (item.input.clone(), info.duration_seconds))
    }

    fn trim_controls(&mut self, ui: &mut egui::Ui) {
        let target = self.timeline_target();
        if let (Some((path, duration)), FfmpegStatus::Ready(env)) = (&target, &self.ffmpeg_status) {
            self.thumb_cache.request(&env.ffmpeg_path, path, *duration);
            if self.thumb_cache.poll(ui.ctx()) {
                ui.ctx().request_repaint();
            } else if self.thumb_cache.is_loading(path) {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            }
            let start = self.config.trim_start_seconds.unwrap_or(0.0);
            let end = self
                .config
                .trim_end_seconds
                .unwrap_or(*duration)
                .min(*duration);
            if let Some((s, e)) = timeline::show(ui, &self.thumb_cache, path, *duration, start, end)
            {
                self.config.trim_start_seconds = (s > 0.0).then_some(s);
                self.config.trim_end_seconds = (e < *duration).then_some(e);
            }
            ui.label(format!(
                "Selected: {} ({} -> {})",
                format::duration((end - start).max(0.0)),
                format::duration(start),
                format::duration(end)
            ));
        }
        ui.horizontal(|ui| {
            ui.label("Trim start (s):");
            let mut start = self.config.trim_start_seconds.unwrap_or(0.0);
            if ui
                .add(egui::DragValue::new(&mut start).range(0.0..=100000.0))
                .changed()
            {
                self.config.trim_start_seconds = if start <= 0.0 { None } else { Some(start) };
            }
            ui.label("Trim end (s):");
            let mut end = self.config.trim_end_seconds.unwrap_or(0.0);
            if ui
                .add(egui::DragValue::new(&mut end).range(0.0..=100000.0))
                .changed()
            {
                self.config.trim_end_seconds = if end <= 0.0 { None } else { Some(end) };
            }
        });
    }

    fn command_preview(&mut self, ui: &mut egui::Ui) {
        let Some(ffmpeg_path) = (match &self.ffmpeg_status {
            FfmpegStatus::Ready(env) => Some(env.ffmpeg_path.clone()),
            FfmpegStatus::NotFound => None,
        }) else {
            return;
        };

        let sample_input = self
            .items
            .first()
            .map(|i| i.input.clone())
            .unwrap_or_else(|| PathBuf::from("input"));
        let duration = self
            .items
            .first()
            .map(|i| i.duration_seconds())
            .unwrap_or(0.0);
        let ext = self.config.output_extension();
        let output = self.output_settings.resolve(&sample_input, ext);
        let built = self.config.build(sample_input, output, duration);
        let args = mediakit_core::command::build_args(&built.settings, &built.passes[0]);
        let preview = mediakit_core::command::preview_command_string(&ffmpeg_path, &args);

        ui.label("Command preview:");
        ui.add(
            egui::TextEdit::multiline(&mut preview.clone())
                .desired_rows(2)
                .font(egui::TextStyle::Monospace),
        );
        if ui.button("Copy command").clicked() {
            ui.ctx().copy_text(preview);
        }
    }

    fn output_settings_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let mut same_as_source =
                matches!(self.output_settings.location, OutputLocation::SameAsSource);
            if ui
                .radio_value(&mut same_as_source, true, "Same folder as source")
                .clicked()
            {
                self.output_settings.location = OutputLocation::SameAsSource;
            }
            if ui
                .radio_value(&mut same_as_source, false, "Custom folder")
                .clicked()
            {
                self.output_settings.location =
                    OutputLocation::Custom(PathBuf::from(&self.output_folder_display));
            }
            if !same_as_source {
                ui.label(&self.output_folder_display);
                if ui.button("Browse\u{2026}").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.output_folder_display = dir.to_string_lossy().into_owned();
                        self.output_settings.location = OutputLocation::Custom(dir);
                    }
                }
            }
            ui.label("Filename template:");
            ui.text_edit_singleline(&mut self.output_settings.filename_template);
            ui.checkbox(
                &mut self.output_settings.overwrite_existing,
                "Overwrite existing",
            );
            ui.separator();
            ui.label("Concurrent jobs:");
            ui.add(egui::DragValue::new(&mut self.concurrency).range(1..=16))
                .on_hover_text("Applies the next time MediaKit starts");

            ui.separator();
            ui.checkbox(
                &mut self.pot_config.enabled,
                "Enable YouTube PO-token server",
            )
            .on_hover_text(
                "Runs a small local helper (bundled from bgutil-ytdlp-pot-provider) that gives \
                 yt-dlp a proof-of-origin token for YouTube, working around 'Sign in to confirm \
                 you're not a bot' 403s. Needs Node.js >= 22 on PATH the first time it's used - \
                 if that's missing, YouTube downloads still work, just without this. Starts \
                 lazily on the first YouTube URL, never touches your system yt-dlp/PATH/config.",
            );
            if self.pot_config.enabled {
                ui.horizontal(|ui| {
                    ui.label("Port:");
                    ui.add(egui::DragValue::new(&mut self.pot_config.port).range(1..=65535));
                });
            }
        });
    }

    fn central_panel(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| match self.active_tab {
            AppTab::Convert => self.convert_tab_ui(ui),
            AppTab::Download => self.download_tab_ui(ui),
        });
    }

    fn convert_tab_ui(&mut self, ui: &mut egui::Ui) {
        self.preset_row(ui);
        ui.separator();
        self.primary_controls(ui);
        ui.separator();
        self.advanced_panel(ui);
        ui.separator();
        self.output_settings_row(ui);
        ui.separator();

        if self.items.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("Drag & drop media files here, or click \"Add Files\u{2026}\"");
            });
            return;
        }

        let mut to_remove: Option<usize> = None;
        let mut to_move_up: Option<usize> = None;
        let mut to_move_down: Option<usize> = None;
        let mut to_submit: Option<usize> = None;
        let mut to_cancel: Option<JobId> = None;
        let mut to_open_folder: Option<PathBuf> = None;
        let mut to_copy: Option<PathBuf> = None;
        let copy_hint = self.copy_note.clone();
        let mut to_show_log: Option<JobId> = None;

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("queue_grid")
                .striped(true)
                .num_columns(6)
                .show(ui, |ui| {
                    ui.strong("File");
                    ui.strong("Info");
                    ui.strong("Status");
                    ui.strong("Progress");
                    ui.strong("Actions");
                    ui.strong("");
                    ui.end_row();

                    for (idx, item) in self.items.iter().enumerate() {
                        ui.label(item.file_name())
                            .on_hover_text(item.input.to_string_lossy());

                        if item.probing {
                            ui.label("probing\u{2026}");
                        } else if let Some(err) = &item.probe_error {
                            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), err);
                        } else if let Some(info) = &item.info {
                            let res = info
                                .video
                                .as_ref()
                                .map(|v| {
                                    format!("{}x{} @ {}", v.width, v.height, format::fps(v.fps))
                                })
                                .unwrap_or_default();
                            let text = format!(
                                "{} {} {} {}",
                                format::duration(info.duration_seconds),
                                res,
                                format::bitrate(info.overall_bitrate_bps),
                                format::file_size(info.file_size_bytes),
                            );
                            ui.label(text);
                        } else {
                            ui.label("-");
                        }

                        match &item.status {
                            QueueStatus::NotQueued => {
                                ui.label("not queued");
                            }
                            QueueStatus::Queued => {
                                ui.label("queued");
                            }
                            QueueStatus::Running => {
                                ui.label("running");
                            }
                            QueueStatus::Done => {
                                match item.target_size_limit_bytes.and_then(|limit| {
                                    std::fs::metadata(&item.output)
                                        .ok()
                                        .map(|m| size_presets::check_output_size(m.len(), limit))
                                }) {
                                    Some(check) if check.passed => {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(80, 180, 100),
                                            format!(
                                                "done - PASS ({})",
                                                format::file_size(check.actual_bytes)
                                            ),
                                        );
                                    }
                                    Some(check) => {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(220, 140, 60),
                                            format!(
                                                "done - OVER ({} > {})",
                                                format::file_size(check.actual_bytes),
                                                format::file_size(check.limit_bytes)
                                            ),
                                        );
                                    }
                                    None => {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(80, 180, 100),
                                            "done",
                                        );
                                    }
                                }
                            }
                            QueueStatus::Failed(err) => {
                                ui.colored_label(egui::Color32::from_rgb(220, 90, 90), "failed")
                                    .on_hover_text(err);
                            }
                            QueueStatus::Cancelled => {
                                ui.label("cancelled");
                            }
                        }

                        let percent = item.progress.percent.unwrap_or(0.0);
                        ui.add(
                            egui::ProgressBar::new((percent / 100.0) as f32)
                                .text(format!("{percent:.0}%")),
                        );

                        ui.horizontal(|ui| {
                            if matches!(
                                item.status,
                                QueueStatus::NotQueued
                                    | QueueStatus::Failed(_)
                                    | QueueStatus::Cancelled
                            ) && ui.small_button("Convert").clicked()
                            {
                                to_submit = Some(idx);
                            }
                            if matches!(item.status, QueueStatus::Queued | QueueStatus::Running)
                                && ui.small_button("Cancel").clicked()
                            {
                                to_cancel = Some(item.id);
                            }
                            if item.status == QueueStatus::Done {
                                if ui.small_button("Open folder").clicked() {
                                    to_open_folder = Some(item.output.clone());
                                }
                                if copy_file_button(ui, &copy_hint) {
                                    to_copy = Some(item.output.clone());
                                }
                            }
                            if !item.log.is_empty() && ui.small_button("Show log").clicked() {
                                to_show_log = Some(item.id);
                            }
                            if ui.small_button("\u{2191}").clicked() {
                                to_move_up = Some(idx);
                            }
                            if ui.small_button("\u{2193}").clicked() {
                                to_move_down = Some(idx);
                            }
                            if ui.small_button("\u{2716}").clicked() {
                                to_remove = Some(idx);
                            }
                        });

                        if let Some(note) = &item.last_retry_note {
                            ui.label(note);
                        } else {
                            ui.label("");
                        }

                        ui.end_row();
                    }
                });
        });

        if let Some(idx) = to_submit {
            self.submit_item(idx);
        }
        if let Some(id) = to_cancel {
            if let Some(engine) = &self.engine {
                engine.cancel(id);
            }
        }
        if let Some(path) = to_open_folder {
            sys::reveal_in_file_manager(&path);
        }
        if let Some(path) = to_copy {
            self.copy_file(ui.ctx(), &path);
        }
        if let Some(id) = to_show_log {
            self.show_log_for = Some(id);
        }
        if let Some(idx) = to_remove {
            if let Some(item) = self.items.get(idx) {
                if let Some(engine) = &self.engine {
                    engine.cancel(item.id);
                }
            }
            self.items.remove(idx);
        }
        if let Some(idx) = to_move_up {
            if idx > 0 {
                self.items.swap(idx, idx - 1);
            }
        }
        if let Some(idx) = to_move_down {
            if idx + 1 < self.items.len() {
                self.items.swap(idx, idx + 1);
            }
        }
    }

    fn log_window(&mut self, ctx: &egui::Context) {
        let Some(id) = self.show_log_for else { return };
        let Some(item) = self.items.iter().find(|i| i.id == id) else {
            self.show_log_for = None;
            return;
        };
        let mut open = true;
        egui::Window::new(format!("Log: {}", item.file_name()))
            .order(egui::Order::Foreground)
            .open(&mut open)
            .default_size([600.0, 400.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut item.log.clone())
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY),
                    );
                });
            });
        if !open {
            self.show_log_for = None;
        }
    }

    fn licenses_window(&mut self, ctx: &egui::Context) {
        if !self.show_licenses {
            return;
        }
        let mut open = true;
        egui::Window::new("Licenses")
            .order(egui::Order::Foreground)
            .open(&mut open)
            .default_size([640.0, 480.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.label(
                        "MediaKit's own source is MIT-licensed. It bundles pre-built \
                         ffmpeg/ffprobe (GPL, for full libx264/libx265 quality) and \
                         yt-dlp (public domain) so nothing needs to be installed \
                         separately, and can optionally download/build a YouTube \
                         PO-token helper (GPL) on first use. Full texts and where to \
                         get each project's source are in THIRD_PARTY_LICENSES/ next \
                         to the app.",
                    );
                    ui.separator();

                    license_section(
                        ui,
                        "MediaKit (this app) - MIT",
                        include_str!("../../LICENSE"),
                    );
                    license_section(
                        ui,
                        "ffmpeg / ffprobe - GPL-3.0-or-later",
                        include_str!("../../THIRD_PARTY_LICENSES/ffmpeg-GPL-3.0.txt"),
                    );
                    license_section(
                        ui,
                        "x264 - GPL-2.0-or-later",
                        include_str!("../../THIRD_PARTY_LICENSES/x264-GPL-2.0.txt"),
                    );
                    license_section(
                        ui,
                        "yt-dlp - Unlicense",
                        include_str!("../../THIRD_PARTY_LICENSES/yt-dlp-Unlicense.txt"),
                    );
                    license_section(
                        ui,
                        "bgutil-ytdlp-pot-provider (optional, downloaded on first use) - GPL-3.0-only",
                        include_str!(
                            "../../THIRD_PARTY_LICENSES/bgutil-ytdlp-pot-provider-GPL-3.0.txt"
                        ),
                    );
                });
            });
        if !open {
            self.show_licenses = false;
        }
    }

    fn save_size_presets(&mut self) {
        let Some(dir) = size_presets::config_dir() else {
            self.presets_editor_message =
                Some("Could not resolve the config directory.".to_string());
            return;
        };
        if let Err(e) = size_presets::save(&dir, &self.size_presets) {
            self.presets_editor_message = Some(format!("Failed to save presets.toml: {e}"));
        }
    }

    /// Settings -> Presets: add/rename/reorder/delete the target-size
    /// presets in `presets.toml`, and edit the shared safety margin. Every
    /// edit here is written straight back to that file - it's the only
    /// place any of these numbers live (see `size_presets`).
    fn presets_editor_window(&mut self, ctx: &egui::Context) {
        if !self.show_presets_editor {
            return;
        }
        let mut open = true;
        let mut changed = false;
        egui::Window::new("Presets")
            .order(egui::Order::Foreground)
            .open(&mut open)
            .default_size([600.0, 480.0])
            .show(ctx, |ui| {
                ui.label(
                    "Target-size presets used by the Convert tab's quick buttons, the \
                     \"Target file size\" mode, and the CLI's --size-preset. Stored in \
                     presets.toml in your config directory - edit that file directly, or \
                     manage it here.",
                );
                ui.separator();

                ui.horizontal(|ui| {
                    ui.label("Safety margin:");
                    let mut margin = self.size_presets.safety_margin_percent;
                    if ui
                        .add(
                            egui::DragValue::new(&mut margin)
                                .range(50.0..=100.0)
                                .suffix("%"),
                        )
                        .on_hover_text(
                            "Fraction of a preset's limit actually targeted, leaving headroom \
                             for container/muxing overhead so real output lands under the cap.",
                        )
                        .changed()
                    {
                        self.size_presets.safety_margin_percent = margin;
                        changed = true;
                    }
                });

                ui.separator();

                let mut to_delete: Option<usize> = None;
                let mut to_move_up: Option<usize> = None;
                let mut to_move_down: Option<usize> = None;

                egui::Grid::new("presets_editor_grid")
                    .num_columns(5)
                    .striped(true)
                    .show(ui, |ui| {
                        ui.strong("Display name");
                        ui.strong("MiB");
                        ui.strong("Note");
                        ui.strong("Reorder");
                        ui.strong("");
                        ui.end_row();

                        let len = self.size_presets.presets.len();
                        for i in 0..len {
                            let preset = &mut self.size_presets.presets[i];
                            if ui.text_edit_singleline(&mut preset.display_name).changed() {
                                changed = true;
                            }
                            let mut mib = preset.limit_bytes as f64 / size_presets::MIB as f64;
                            if ui
                                .add(
                                    egui::DragValue::new(&mut mib)
                                        .range(0.01..=1_000_000.0)
                                        .speed(1.0),
                                )
                                .changed()
                            {
                                preset.limit_bytes =
                                    (mib * size_presets::MIB as f64).round() as u64;
                                changed = true;
                            }
                            if ui.text_edit_singleline(&mut preset.note).changed() {
                                changed = true;
                            }
                            ui.horizontal(|ui| {
                                if ui.small_button("\u{2191}").clicked() && i > 0 {
                                    to_move_up = Some(i);
                                }
                                if ui.small_button("\u{2193}").clicked() && i + 1 < len {
                                    to_move_down = Some(i);
                                }
                            });
                            if ui.small_button("\u{2716}").clicked() {
                                to_delete = Some(i);
                            }
                            ui.end_row();
                        }
                    });

                if let Some(i) = to_delete {
                    self.size_presets.presets.remove(i);
                    changed = true;
                }
                if let Some(i) = to_move_up {
                    self.size_presets.presets.swap(i, i - 1);
                    changed = true;
                }
                if let Some(i) = to_move_down {
                    self.size_presets.presets.swap(i, i + 1);
                    changed = true;
                }

                ui.separator();
                ui.label("Add a preset:");
                ui.horizontal(|ui| {
                    ui.label("id:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_preset_id)
                            .desired_width(100.0)
                            .hint_text("auto from name"),
                    );
                    ui.label("name:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_preset_name).desired_width(160.0),
                    );
                    ui.label("MiB:");
                    ui.add(egui::DragValue::new(&mut self.new_preset_mib).range(1..=1_000_000));
                    if ui.button("Add").clicked() {
                        let name = self.new_preset_name.trim().to_string();
                        let id = if self.new_preset_id.trim().is_empty() {
                            name.to_lowercase().replace(' ', "-")
                        } else {
                            self.new_preset_id.trim().to_string()
                        };
                        if id.is_empty() || name.is_empty() {
                            self.presets_editor_message =
                                Some("A name (and id) are required.".to_string());
                        } else if self.size_presets.find(&id).is_some() {
                            self.presets_editor_message =
                                Some(format!("id '{id}' already exists."));
                        } else {
                            self.size_presets.presets.push(SizePreset {
                                id,
                                display_name: name,
                                limit_bytes: self.new_preset_mib * size_presets::MIB,
                                note: String::new(),
                            });
                            self.new_preset_id.clear();
                            self.new_preset_name.clear();
                            self.new_preset_mib = 10;
                            self.presets_editor_message = None;
                            changed = true;
                        }
                    }
                });

                ui.separator();
                if ui.button("Restore defaults").clicked() {
                    match size_presets::config_dir() {
                        Some(dir) => match size_presets::restore_defaults(&dir) {
                            Ok(defaults) => {
                                self.size_presets = defaults;
                                self.presets_editor_message =
                                    Some("Restored defaults.".to_string());
                            }
                            Err(e) => {
                                self.presets_editor_message =
                                    Some(format!("Failed to restore defaults: {e}"));
                            }
                        },
                        None => {
                            self.presets_editor_message =
                                Some("Could not resolve the config directory.".to_string());
                        }
                    }
                }
                if let Some(msg) = &self.presets_editor_message {
                    ui.colored_label(egui::Color32::from_rgb(220, 180, 90), msg);
                }
            });
        if !open {
            self.show_presets_editor = false;
        }
        if changed {
            self.save_size_presets();
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            return;
        }
        let mut open = true;
        egui::Window::new("Settings")
            .order(egui::Order::Foreground)
            .open(&mut open)
            .default_size([560.0, 420.0])
            .show(ctx, |ui| {
                ui.heading("Appearance");
                ui.horizontal(|ui| {
                    ui.label("Theme:");
                    egui::ComboBox::new("theme_combo", "")
                        .selected_text(theme_label(self.theme))
                        .show_ui(ui, |ui| {
                            for choice in ThemeChoice::ALL {
                                if ui
                                    .selectable_value(&mut self.theme, choice, theme_label(choice))
                                    .changed()
                                {
                                    apply_theme(ui.ctx(), choice);
                                }
                            }
                        });
                });
                ui.add_space(8.0);
                ui.heading("Behavior");
                ui.checkbox(
                    &mut self.notifications_enabled,
                    "Notify me when conversions or downloads finish",
                );
                ui.checkbox(
                    &mut self.clipboard_watch_enabled,
                    "Offer to download links I copy",
                )
                .on_hover_text(
                    "Checks the clipboard about once a second for video links \
                     (YouTube, TikTok, Reddit, ...) and shows a small prompt.",
                );
                if self.clipboard_watch_enabled {
                    ui.horizontal(|ui| {
                        ui.label("Also offer links from:");
                        ui.text_edit_singleline(&mut self.clipboard_extra_hosts)
                            .on_hover_text("Extra hosts, e.g. bilibili.com, nicovideo.jp");
                    });
                }
                ui.add_space(8.0);
                ui.heading("Tools");
                ui.label(
                    "MediaKit bundles its own ffmpeg/ffprobe/yt-dlp, extracted here on \
                     first launch. Override any of them to use your own build instead.",
                );
                ui.separator();

                self.tool_row(
                    ui,
                    "ffmpeg",
                    |env| Some(env.ffmpeg_path.clone()),
                    |env| Some(env.version.clone()),
                );
                self.override_row(ui, "ffmpeg", |app| &mut app.override_ffmpeg);

                ui.add_space(6.0);
                self.tool_row(
                    ui,
                    "ffprobe",
                    |env| Some(env.ffprobe_path.clone()),
                    |env| Some(env.version.clone()),
                );
                self.override_row(ui, "ffprobe", |app| &mut app.override_ffprobe);

                ui.add_space(6.0);
                self.tool_row(
                    ui,
                    "yt-dlp",
                    |env| env.ytdlp_path.clone(),
                    |env| env.ytdlp_version.clone(),
                );
                self.override_row(ui, "yt-dlp", |app| &mut app.override_ytdlp);
                self.ytdlp_update_row(ui);

                ui.add_space(10.0);
                if ui.button("Re-extract bundled binaries").clicked() {
                    if self.jobs_active() {
                        self.settings_message =
                            Some("Cancel or wait for active jobs first.".to_string());
                    } else if let Ok(dir) = ffmpeg_env::app_data_dir() {
                        match mediakit_core::vendor::force_reextract(&dir) {
                            Ok(Some(_)) => {
                                self.settings_message =
                                    Some("Re-extracted successfully.".to_string());
                                let _ = self.redetect_tools();
                            }
                            Ok(None) => {
                                self.settings_message = Some(
                                    "This is a slim build; nothing to re-extract.".to_string(),
                                );
                            }
                            Err(e) => {
                                self.settings_message = Some(format!("Re-extract failed: {e}"));
                            }
                        }
                    }
                }
                if let Some(msg) = &self.settings_message {
                    ui.colored_label(egui::Color32::from_rgb(220, 180, 90), msg);
                }
            });
        if !open {
            self.show_settings = false;
        }
    }

    fn tool_row(
        &self,
        ui: &mut egui::Ui,
        label: &str,
        path_of: impl Fn(&FfmpegEnv) -> Option<PathBuf>,
        version_of: impl Fn(&FfmpegEnv) -> Option<String>,
    ) {
        ui.horizontal(|ui| {
            ui.strong(label);
            match &self.ffmpeg_status {
                FfmpegStatus::Ready(env) => {
                    let path = path_of(env);
                    let version = version_of(env);
                    match (path, version) {
                        (Some(path), Some(version)) => {
                            ui.label(version);
                            ui.label(path.to_string_lossy().into_owned());
                        }
                        (Some(path), None) => {
                            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), "not found");
                            ui.label(path.to_string_lossy().into_owned());
                        }
                        _ => {
                            ui.colored_label(egui::Color32::from_rgb(220, 90, 90), "not found");
                        }
                    }
                }
                FfmpegStatus::NotFound => {
                    ui.colored_label(egui::Color32::from_rgb(220, 90, 90), "not found");
                }
            }
        });
    }

    fn override_row(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        field: impl Fn(&mut Self) -> &mut Option<PathBuf>,
    ) {
        ui.horizontal(|ui| {
            ui.label("  override:");
            let current = field(self).clone();
            match &current {
                Some(p) => {
                    ui.label(p.to_string_lossy().into_owned());
                    if ui.small_button("Clear").clicked() {
                        *field(self) = None;
                        if let Err(e) = self.redetect_tools() {
                            self.settings_message = Some(e.to_string());
                        }
                    }
                }
                None => {
                    ui.label("(none)");
                }
            }
            if ui.small_button("Browse\u{2026}").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_file() {
                    *field(self) = Some(path);
                    if let Err(e) = self.redetect_tools() {
                        self.settings_message = Some(e.to_string());
                    } else {
                        self.settings_message = Some(format!("{label} override applied."));
                    }
                }
            }
        });
    }

    /// The "keep yt-dlp fresh" row under Settings -> Tools: manual check
    /// button, release staleness, auto-check toggle, and rollback.
    /// yt-dlp is the one bundled tool that's expected to go stale on its
    /// own (site extractors break as sites change), unlike ffmpeg.
    fn ytdlp_update_row(&mut self, ui: &mut egui::Ui) {
        let Ok(bin_dir) = ffmpeg_env::app_data_dir() else {
            return;
        };
        let current_version = match &self.ffmpeg_status {
            FfmpegStatus::Ready(env) => env.ytdlp_version.clone(),
            FfmpegStatus::NotFound => None,
        };
        let Some(current_version) = current_version else {
            return;
        };

        ui.horizontal(|ui| {
            ui.label("  ");
            if self.ytdlp_update_in_progress {
                ui.spinner();
                ui.label("Checking\u{2026}");
            } else {
                if ui.small_button("Check for Updates").clicked() {
                    self.ytdlp_update_in_progress = true;
                    self.ytdlp_update_message = None;
                    self.update_worker.check(bin_dir.clone(), current_version);
                }
                if ui.small_button("Install Latest").clicked() {
                    self.ytdlp_update_in_progress = true;
                    self.ytdlp_update_message = None;
                    self.update_worker.update(bin_dir.clone());
                }
                if bin_dir
                    .join(if cfg!(windows) {
                        "yt-dlp.exe.previous"
                    } else {
                        "yt-dlp.previous"
                    })
                    .is_file()
                    && ui.small_button("Roll Back").clicked()
                {
                    self.ytdlp_update_in_progress = true;
                    self.ytdlp_update_message = None;
                    self.update_worker.rollback(bin_dir.clone());
                }
            }
            if ui
                .checkbox(
                    &mut self.ytdlp_auto_update_enabled,
                    "Check for updates at launch (daily) and auto-install",
                )
                .changed()
            {
                self.auto_update_check_started = false;
            }
        });

        let state = mediakit_core::ytdlp_update::load_state(&bin_dir);
        if let Some(published_at) = &state.latest_known_published_at {
            if let Some(days) = mediakit_core::ytdlp_update::days_since(published_at) {
                ui.horizontal(|ui| {
                    ui.label("  ");
                    let text = format!("Latest known release: {days} day(s) ago.");
                    if days > 30 {
                        ui.colored_label(
                            egui::Color32::from_rgb(220, 180, 90),
                            format!("{text} Consider checking for updates."),
                        );
                    } else {
                        ui.label(text);
                    }
                });
            }
        }
        if let Some(msg) = &self.ytdlp_update_message {
            ui.horizontal(|ui| {
                ui.label("  ");
                ui.label(msg);
            });
        }
    }

    /// Start/stop/restart the background clipboard poller to match the
    /// Settings toggle and host list, and collect any new offers.
    fn update_clipboard_watcher(&mut self, ctx: &egui::Context) {
        let hosts_changed = self.clipboard_hosts_applied != self.clipboard_extra_hosts;
        if !self.clipboard_watch_enabled {
            self.clipboard_watcher.stop();
            self.clipboard_offers.current = None;
            return;
        }
        if self.clipboard_watcher.is_running() && hosts_changed {
            self.clipboard_watcher.stop();
        }
        if !self.clipboard_watcher.is_running() {
            self.clipboard_hosts_applied = self.clipboard_extra_hosts.clone();
            self.clipboard_watcher
                .start(clipboard_watch::parse_host_list(
                    &self.clipboard_extra_hosts,
                ));
        }
        for url in self.clipboard_watcher.poll() {
            self.clipboard_offers.copied(url);
        }
        // The poller thread can't wake the UI, so check back periodically.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }

    /// Small dismissible "Download <url>?" prompt in the bottom-right corner.
    fn clipboard_toast(&mut self, ctx: &egui::Context) {
        let Some(url) = self.clipboard_offers.current.clone() else {
            return;
        };
        let mut download = false;
        let mut dismiss = false;
        egui::Area::new(egui::Id::new("clipboard_toast"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -40.0])
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(format!("Download {}?", clipboard_watch::shorten(&url, 48)))
                        .on_hover_text(&url);
                    ui.horizontal(|ui| {
                        download = ui.button("Download").clicked();
                        dismiss = ui.button("Dismiss").clicked();
                    });
                });
            });
        if download {
            self.active_tab = AppTab::Download;
            self.download_url_input = url;
            if self.download_responsibility_acknowledged && self.ytdlp_path().is_some() {
                self.fetch_download_urls(true);
            }
        }
        if download || dismiss {
            self.clipboard_offers.current = None;
        }
    }

    /// Ctrl+V with the window focused and no text field editing: links go to
    /// the Download tab and start downloading, file paths join the Convert
    /// queue. A paste aimed at a focused text field is never touched - egui
    /// reports those as keyboard-wanting, so we bail out before looking.
    fn handle_global_paste(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() || !ctx.input(|i| i.focused) {
            return;
        }
        let pasted: Vec<String> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Paste(text) => Some(text.clone()),
                    _ => None,
                })
                .collect()
        });
        for text in pasted {
            match paste::classify(&text, |p| p.is_file()) {
                Paste::Urls(urls) => {
                    self.active_tab = AppTab::Download;
                    self.download_url_input = urls.join("\n");
                    if self.download_responsibility_acknowledged && self.ytdlp_path().is_some() {
                        self.fetch_download_urls(true);
                    }
                }
                Paste::Files(paths) => {
                    self.active_tab = AppTab::Convert;
                    self.add_files(paths);
                }
                Paste::Other => {}
            }
        }
    }

    /// "Copy file": the file itself where the platform supports it,
    /// otherwise its path as text.
    fn copy_file(&mut self, ctx: &egui::Context, path: &std::path::Path) {
        let note = if sys::copy_file_to_clipboard(path) {
            "Copied the file - paste it with Ctrl+V."
        } else {
            ctx.copy_text(path.to_string_lossy().into_owned());
            "Couldn't copy the file itself on this system; copied its path as text."
        };
        self.copy_note = Some(note.to_string());
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.add_files(dropped);
        }
    }
}

/// "Copy file" button. Its tooltip explains the fallback and shows how the
/// previous copy went.
fn copy_file_button(ui: &mut egui::Ui, last_result: &Option<String>) -> bool {
    let response = ui.small_button("Copy file");
    response.clone().on_hover_ui(|ui| {
        ui.label(
            "Copies the file so you can paste it straight into Discord or a folder. \
             If this system can't copy files, the path is copied as text instead.",
        );
        if let Some(note) = last_result {
            ui.separator();
            ui.label(note);
        }
    });
    response.clicked()
}

fn file_name_of(path: &std::path::Path) -> Option<String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// The OS's Downloads folder, falling back to the home directory and then
/// the current directory if that's not available (e.g. `XDG_DOWNLOAD_DIR`
/// unset on a minimal Linux setup).
fn default_download_dir() -> PathBuf {
    if let Some(dirs) = directories::UserDirs::new() {
        if let Some(dir) = dirs.download_dir() {
            return dir.to_path_buf();
        }
        return dirs.home_dir().to_path_buf();
    }
    std::env::current_dir().unwrap_or_default()
}

fn license_section(ui: &mut egui::Ui, title: &str, text: &str) {
    egui::CollapsingHeader::new(title)
        .default_open(false)
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut text.to_string())
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY),
            );
        });
}

fn theme_label(theme: ThemeChoice) -> &'static str {
    match theme {
        ThemeChoice::System => "System",
        ThemeChoice::Light => "Light",
        ThemeChoice::Dark => "Dark",
        ThemeChoice::Nord => "Nord",
        ThemeChoice::SolarizedDark => "Solarized Dark",
        ThemeChoice::HighContrast => "High Contrast",
        ThemeChoice::Pink => "Pink",
    }
}

fn video_codec_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::H265 => "H.265",
        VideoCodec::Vp9 => "VP9",
        VideoCodec::Av1 => "AV1",
        _ => "other",
    }
}

fn audio_codec_label(codec: AudioCodec) -> &'static str {
    match codec {
        AudioCodec::Aac => "AAC",
        AudioCodec::Opus => "Opus",
        AudioCodec::Mp3 => "MP3",
        AudioCodec::Flac => "FLAC",
        AudioCodec::Pcm => "WAV (PCM)",
        AudioCodec::Copy => "Copy",
    }
}

fn image_format_label(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpg => "JPG",
        ImageFormat::Webp => "WebP",
        ImageFormat::Avif => "AVIF",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Ico => "ICO",
    }
}

fn rotation_label(rotation: Rotation) -> &'static str {
    match rotation {
        Rotation::None => "None",
        Rotation::Cw90 => "90 CW",
        Rotation::Ccw90 => "90 CCW",
        Rotation::Rotate180 => "180",
    }
}

fn flip_label(flip: FlipMode) -> &'static str {
    match flip {
        FlipMode::None => "None",
        FlipMode::Horizontal => "Horizontal",
        FlipMode::Vertical => "Vertical",
        FlipMode::Both => "Both",
    }
}

/// Build a full custom [`egui::Visuals`] palette from a handful of named
/// colors, for the themes beyond egui's own built-in light/dark (which stay
/// on `ctx.set_theme` so they keep tracking the OS preference for `System`).
fn themed_visuals(
    base_bg: egui::Color32,
    elevated_bg: egui::Color32,
    extreme_bg: egui::Color32,
    text: egui::Color32,
    accent: egui::Color32,
) -> egui::Visuals {
    let mut visuals = egui::Visuals::dark();
    visuals.override_text_color = Some(text);
    visuals.panel_fill = base_bg;
    visuals.window_fill = base_bg;
    visuals.extreme_bg_color = extreme_bg;
    visuals.faint_bg_color = elevated_bg;
    visuals.code_bg_color = extreme_bg;
    visuals.hyperlink_color = accent;
    visuals.selection.bg_fill = accent.gamma_multiply(0.55);
    visuals.selection.stroke.color = accent;
    visuals.widgets.noninteractive.bg_fill = base_bg;
    visuals.widgets.noninteractive.weak_bg_fill = base_bg;
    visuals.widgets.inactive.bg_fill = elevated_bg;
    visuals.widgets.inactive.weak_bg_fill = elevated_bg;
    visuals.widgets.hovered.bg_fill = accent.gamma_multiply(0.45);
    visuals.widgets.hovered.weak_bg_fill = accent.gamma_multiply(0.35);
    visuals.widgets.hovered.fg_stroke.color = text;
    visuals.widgets.active.bg_fill = accent;
    visuals.widgets.active.weak_bg_fill = accent;
    visuals.widgets.active.fg_stroke.color = base_bg;
    visuals
}

fn nord_visuals() -> egui::Visuals {
    themed_visuals(
        egui::Color32::from_rgb(0x2E, 0x34, 0x40),
        egui::Color32::from_rgb(0x3B, 0x42, 0x52),
        egui::Color32::from_rgb(0x2A, 0x2F, 0x3A),
        egui::Color32::from_rgb(0xE5, 0xE9, 0xF0),
        egui::Color32::from_rgb(0x88, 0xC0, 0xD0),
    )
}

fn solarized_dark_visuals() -> egui::Visuals {
    themed_visuals(
        egui::Color32::from_rgb(0x00, 0x2B, 0x36),
        egui::Color32::from_rgb(0x07, 0x36, 0x42),
        egui::Color32::from_rgb(0x00, 0x1F, 0x27),
        egui::Color32::from_rgb(0x93, 0xA1, 0xA1),
        egui::Color32::from_rgb(0x26, 0x8B, 0xD2),
    )
}

fn high_contrast_visuals() -> egui::Visuals {
    themed_visuals(
        egui::Color32::BLACK,
        egui::Color32::from_rgb(0x1A, 0x1A, 0x1A),
        egui::Color32::BLACK,
        egui::Color32::WHITE,
        egui::Color32::from_rgb(0xFF, 0xD1, 0x00),
    )
}

/// Dark-first pink palette: a near-black plum base (never pure black, so
/// panels/cards still read as distinct layers) with a hot-pink accent.
/// Every pairing below was checked against WCAG AA (4.5:1 for text, 3:1 for
/// large/UI elements): body text on any of the three background layers is
/// ~13-16:1, the accent used as hyperlink/link text on `base_bg` is ~5.9:1,
/// and the darkened accent shades `themed_visuals` derives for
/// hover/selection fills stay at 6.7-8.4:1 against `text`.
fn pink_visuals() -> egui::Visuals {
    themed_visuals(
        egui::Color32::from_rgb(0x1F, 0x14, 0x20),
        egui::Color32::from_rgb(0x2B, 0x1B, 0x2E),
        egui::Color32::from_rgb(0x15, 0x0E, 0x17),
        egui::Color32::from_rgb(0xF6, 0xE6, 0xF2),
        egui::Color32::from_rgb(0xFF, 0x4F, 0xA3),
    )
}

fn apply_theme(ctx: &egui::Context, theme: ThemeChoice) {
    match theme {
        ThemeChoice::System => ctx.set_theme(egui::ThemePreference::System),
        ThemeChoice::Light => ctx.set_theme(egui::ThemePreference::Light),
        ThemeChoice::Dark => ctx.set_theme(egui::ThemePreference::Dark),
        // Custom palettes are all dark-based - force the Dark theme slot
        // first so `set_visuals` (which writes into `self.theme()`'s slot)
        // always lands in the right one regardless of the OS's own
        // light/dark setting.
        ThemeChoice::Nord => {
            ctx.set_theme(egui::ThemePreference::Dark);
            ctx.set_visuals(nord_visuals());
        }
        ThemeChoice::SolarizedDark => {
            ctx.set_theme(egui::ThemePreference::Dark);
            ctx.set_visuals(solarized_dark_visuals());
        }
        ThemeChoice::HighContrast => {
            ctx.set_theme(egui::ThemePreference::Dark);
            ctx.set_visuals(high_contrast_visuals());
        }
        ThemeChoice::Pink => {
            ctx.set_theme(egui::ThemePreference::Dark);
            ctx.set_visuals(pink_visuals());
        }
    }
    apply_button_style(ctx);
}

/// Rounder, roomier buttons/widgets than egui's fairly tight defaults -
/// applied on top of whatever palette `apply_theme` just picked, for every
/// theme (not just the custom ones above). Touches both the light and dark
/// style slots so it sticks regardless of which one ends up active.
fn apply_button_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        let radius = egui::CornerRadius::same(6);
        style.visuals.widgets.inactive.corner_radius = radius;
        style.visuals.widgets.hovered.corner_radius = radius;
        style.visuals.widgets.active.corner_radius = radius;
        style.visuals.widgets.noninteractive.corner_radius = radius;
        style.visuals.widgets.open.corner_radius = radius;
        style.visuals.window_corner_radius = egui::CornerRadius::same(8);
        style.visuals.menu_corner_radius = egui::CornerRadius::same(8);
    });
}

impl eframe::App for MediaKitApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_probe_results();
        self.poll_engine_events();
        self.poll_batch_completion();
        self.poll_metadata();
        self.poll_download_events();
        self.poll_update_events();
        self.notify_when_idle();
        self.maybe_start_auto_update_check();
        self.handle_dropped_files(&ctx);
        self.handle_global_paste(&ctx);
        self.update_clipboard_watcher(&ctx);
        self.handle_shortcuts(&ctx);

        let screen = ctx.input(|i| i.viewport_rect());
        self.current_window_size = (screen.width(), screen.height());

        self.top_panel(ui);
        self.status_bar(ui);
        self.central_panel(ui);
        self.log_window(&ctx);
        self.licenses_window(&ctx);
        self.settings_window(&ctx);
        self.presets_editor_window(&ctx);
        self.download_log_window(&ctx);
        self.clipboard_toast(&ctx);

        let still_working = self
            .items
            .iter()
            .any(|i| i.probing || matches!(i.status, QueueStatus::Queued | QueueStatus::Running))
            || self.download_jobs_active()
            || self.pot_manager.state() == mediakit_core::pot_provider::PotProviderState::Starting;
        if still_working {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self) {
        self.pot_manager.shutdown();
        crate::settings::save(&self.persisted_settings());
    }
}
