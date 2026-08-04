//! Worker thread pool that runs yt-dlp for queued download jobs, mirroring
//! [`crate::engine::JobEngine`]'s structure (same cancellation-race fix and
//! all) so downloads get the same reliability as conversions, just backed
//! by a separate pool since network-bound downloads and CPU-bound encodes
//! have very different concurrency sweet spots.

use crate::downloader::{self, DownloadOptions, DownloadProgress, FormatSelection};
use crate::error::CoreError;
use crate::job::JobId;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

#[derive(Debug, Clone)]
pub struct DownloadSpec {
    pub id: JobId,
    pub url: String,
    pub format: FormatSelection,
    pub ffmpeg_dir: PathBuf,
    pub options: DownloadOptions,
}

#[derive(Debug, Clone)]
pub enum DownloadEvent {
    Started {
        id: JobId,
    },
    Progress {
        id: JobId,
        info: DownloadProgress,
    },
    Done {
        id: JobId,
        output_path: Option<PathBuf>,
        log: String,
    },
    Failed {
        id: JobId,
        error: String,
        log: String,
    },
    Cancelled {
        id: JobId,
    },
}

struct ActiveDownload {
    child_slot: Arc<Mutex<Option<Arc<Mutex<std::process::Child>>>>>,
    cancelled: Arc<AtomicBool>,
}

struct Shared {
    queue: Mutex<VecDeque<DownloadSpec>>,
    condvar: Condvar,
    shutdown: AtomicBool,
    active: Mutex<HashMap<JobId, ActiveDownload>>,
    events: Sender<DownloadEvent>,
    ytdlp_path: PathBuf,
}

pub struct DownloadEngine {
    shared: Arc<Shared>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl DownloadEngine {
    pub fn new(ytdlp_path: PathBuf, concurrency: usize) -> (Self, Receiver<DownloadEvent>) {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            condvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
            active: Mutex::new(HashMap::new()),
            events: tx,
            ytdlp_path,
        });

        let concurrency = concurrency.max(1);
        let workers = (0..concurrency)
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || worker_loop(shared))
            })
            .collect();

        (Self { shared, workers }, rx)
    }

    pub fn submit(&self, spec: DownloadSpec) {
        let mut queue = self.shared.queue.lock().unwrap();
        queue.push_back(spec);
        self.shared.condvar.notify_one();
    }

    pub fn cancel(&self, id: JobId) {
        {
            let mut queue = self.shared.queue.lock().unwrap();
            if let Some(pos) = queue.iter().position(|j| j.id == id) {
                queue.remove(pos);
                let _ = self.shared.events.send(DownloadEvent::Cancelled { id });
                return;
            }
        }

        let active = self.shared.active.lock().unwrap();
        if let Some(job) = active.get(&id) {
            kill_active(job);
        }
    }

    pub fn cancel_all(&self) {
        let queued_ids: Vec<JobId> = {
            let mut queue = self.shared.queue.lock().unwrap();
            let ids = queue.iter().map(|j| j.id).collect::<Vec<_>>();
            queue.clear();
            ids
        };
        for id in queued_ids {
            let _ = self.shared.events.send(DownloadEvent::Cancelled { id });
        }

        let active = self.shared.active.lock().unwrap();
        for job in active.values() {
            kill_active(job);
        }
    }
}

impl Drop for DownloadEngine {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.condvar.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn kill_active(job: &ActiveDownload) {
    job.cancelled.store(true, Ordering::SeqCst);
    if let Some(child) = job.child_slot.lock().unwrap().as_ref() {
        let _ = child.lock().unwrap().kill();
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let spec = {
            let mut queue = shared.queue.lock().unwrap();
            loop {
                if let Some(spec) = queue.pop_front() {
                    break Some(spec);
                }
                if shared.shutdown.load(Ordering::SeqCst) {
                    break None;
                }
                queue = shared.condvar.wait(queue).unwrap();
            }
        };

        let Some(spec) = spec else { break };
        run_download(&shared, spec);
    }
}

fn run_download(shared: &Shared, spec: DownloadSpec) {
    let id = spec.id;

    // Same race-avoidance approach as `engine::run_job`: register before
    // announcing Started, so a cancel() landing in the dequeue/spawn gap is
    // never silently dropped.
    let cancelled = Arc::new(AtomicBool::new(false));
    let child_slot: Arc<Mutex<Option<Arc<Mutex<std::process::Child>>>>> =
        Arc::new(Mutex::new(None));
    shared.active.lock().unwrap().insert(
        id,
        ActiveDownload {
            child_slot: Arc::clone(&child_slot),
            cancelled: Arc::clone(&cancelled),
        },
    );

    let _ = shared.events.send(DownloadEvent::Started { id });

    if cancelled.load(Ordering::SeqCst) {
        shared.active.lock().unwrap().remove(&id);
        let _ = shared.events.send(DownloadEvent::Cancelled { id });
        return;
    }

    let args =
        downloader::build_download_args(&spec.url, &spec.format, &spec.ffmpeg_dir, &spec.options);

    let mut cmd = Command::new(&shared.ytdlp_path);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::sys::no_console_window(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(source) => {
            shared.active.lock().unwrap().remove(&id);
            let error = CoreError::Spawn {
                binary: shared.ytdlp_path.clone(),
                source,
            }
            .to_string();
            let _ = shared.events.send(DownloadEvent::Failed {
                id,
                error,
                log: String::new(),
            });
            return;
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let child = Arc::new(Mutex::new(child));
    *child_slot.lock().unwrap() = Some(Arc::clone(&child));
    if cancelled.load(Ordering::SeqCst) {
        let _ = child.lock().unwrap().kill();
    }

    let stderr_handle = stderr.map(|stderr| {
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            let mut lines = String::new();
            for line in reader.lines().map_while(Result::ok) {
                lines.push_str(&line);
                lines.push('\n');
            }
            lines
        })
    });

    let mut output_path: Option<PathBuf> = None;
    let mut stdout_log = String::new();

    if let Some(stdout) = stdout {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if let Some(path) = downloader::parse_final_path_line(&line) {
                output_path = Some(path);
                continue;
            }
            if let Some(progress) = downloader::parse_progress_line(&line) {
                let _ = shared
                    .events
                    .send(DownloadEvent::Progress { id, info: progress });
                continue;
            }
            stdout_log.push_str(&line);
            stdout_log.push('\n');
        }
    }

    let stderr_log = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let mut combined_log = stdout_log;
    combined_log.push_str(&stderr_log);

    let exit_status = child.lock().unwrap().wait();
    shared.active.lock().unwrap().remove(&id);

    if cancelled.load(Ordering::SeqCst) {
        let _ = shared.events.send(DownloadEvent::Cancelled { id });
        return;
    }

    match exit_status {
        Ok(status) if status.success() => {
            let _ = shared.events.send(DownloadEvent::Done {
                id,
                output_path,
                log: combined_log,
            });
        }
        Ok(status) => {
            let error = format!("yt-dlp exited with {:?}:\n{combined_log}", status.code());
            let _ = shared.events.send(DownloadEvent::Failed {
                id,
                error,
                log: combined_log,
            });
        }
        Err(err) => {
            let _ = shared.events.send(DownloadEvent::Failed {
                id,
                error: err.to_string(),
                log: combined_log,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloader::DownloadOptions;
    use std::time::{Duration, Instant};

    fn ytdlp_on_path() -> Option<PathBuf> {
        crate::ffmpeg_env::locate_binary("yt-dlp", &std::env::temp_dir())
    }

    /// This test hits the real network (a stable, well-known public test
    /// video used throughout yt-dlp's own examples), so it's opt-in via an
    /// env var rather than part of the default `cargo test` run - CI
    /// shouldn't depend on third-party network availability.
    #[test]
    fn downloads_a_real_short_clip_end_to_end() {
        if std::env::var("MEDIAKIT_TEST_NETWORK").is_err() {
            eprintln!("skipping: set MEDIAKIT_TEST_NETWORK=1 to run network-dependent tests");
            return;
        }
        let Some(ytdlp) = ytdlp_on_path() else {
            eprintln!("skipping: yt-dlp not found on PATH");
            return;
        };
        let Some(ffmpeg) = crate::ffmpeg_env::locate_binary("ffmpeg", &std::env::temp_dir()) else {
            eprintln!("skipping: ffmpeg not found on PATH");
            return;
        };

        let tmp = tempfile::tempdir().unwrap();
        let (engine, rx) = DownloadEngine::new(ytdlp, 1);
        engine.submit(DownloadSpec {
            id: 1,
            url: "https://www.youtube.com/watch?v=aqz-KE-bpKQ".to_string(),
            format: FormatSelection::Custom("worst".to_string()),
            ffmpeg_dir: ffmpeg.parent().unwrap().to_path_buf(),
            options: DownloadOptions {
                output_template: tmp
                    .path()
                    .join("test.%(ext)s")
                    .to_string_lossy()
                    .into_owned(),
                ..Default::default()
            },
        });

        let mut done_path = None;
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline && done_path.is_none() {
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(DownloadEvent::Done { output_path, .. }) => done_path = Some(output_path),
                Ok(DownloadEvent::Failed { error, log, .. }) => {
                    panic!("download failed: {error}\n{log}")
                }
                _ => {}
            }
        }

        let path = done_path
            .expect("download never completed")
            .expect("no output path reported");
        assert!(path.is_file(), "expected downloaded file at {path:?}");
    }

    #[test]
    fn cancel_removes_a_still_queued_download() {
        // Deterministic, no network needed: cancel before the worker (which
        // is busy on an intentionally-bogus first job that will fail fast)
        // ever dequeues the second one.
        let (engine, rx) = DownloadEngine::new(PathBuf::from("definitely-not-a-real-binary"), 1);
        engine.submit(DownloadSpec {
            id: 1,
            url: "https://example.com/a".to_string(),
            format: FormatSelection::Best,
            ffmpeg_dir: PathBuf::from("/nonexistent"),
            options: DownloadOptions::default(),
        });
        engine.submit(DownloadSpec {
            id: 2,
            url: "https://example.com/b".to_string(),
            format: FormatSelection::Best,
            ffmpeg_dir: PathBuf::from("/nonexistent"),
            options: DownloadOptions::default(),
        });
        engine.cancel(2);

        let mut saw_cancelled = false;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(DownloadEvent::Cancelled { id: 2 }) => {
                    saw_cancelled = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(saw_cancelled);
    }
}
