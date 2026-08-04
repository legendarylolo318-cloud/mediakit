//! Worker thread pool that actually runs ffmpeg for queued jobs, streaming
//! progress back through a channel so the GUI thread never blocks.

use crate::command::{build_args, EncodePass};
use crate::error::CoreError;
use crate::job::{JobId, JobProgressInfo, JobSpec};
use crate::procgroup::ProcessGroup;
use crate::progress::{self, ProgressParser};
use ffmpeg_sidecar::child::FfmpegChild;
use ffmpeg_sidecar::command::FfmpegCommand;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum EngineEvent {
    Started {
        id: JobId,
    },
    Progress {
        id: JobId,
        info: JobProgressInfo,
    },
    Done {
        id: JobId,
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
    /// A target-size job's output exceeded the target, so the engine is
    /// automatically retrying with a reduced video bitrate.
    Retrying {
        id: JobId,
        attempt: u8,
        new_video_bitrate_kbps: u64,
    },
}

/// Why a job was cancelled. Currently there's only one source of
/// cancellation, but this exists (rather than a bare `bool`/`AtomicBool`) so
/// that `Cancelled` is always emitted *because this was explicitly set*,
/// never inferred from a process's exit status - Windows has no equivalent
/// of Unix's "terminated by signal" exit-status shape, so exit-code
/// classification can never be the source of truth for "was this a user
/// cancellation."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    User,
}

/// The running child process for a job's current pass, plus a handle able
/// to kill its whole process tree (not just this direct child - see
/// [`crate::procgroup`]) in one shot.
struct ActiveChild {
    child: Arc<Mutex<FfmpegChild>>,
    group: Arc<ProcessGroup>,
}

struct ActiveJob {
    /// Populated once the current pass's ffmpeg process has actually spawned.
    /// `None` for the brief window between a job being dequeued and its
    /// first `spawn()` returning - `cancel_reason` is checked immediately
    /// after that window closes so a cancel landing during it isn't lost.
    child_slot: Arc<Mutex<Option<ActiveChild>>>,
    cancel_reason: Arc<Mutex<Option<CancelReason>>>,
}

/// Queued and in-flight jobs, behind a single lock. Keeping both under one
/// `Mutex` (rather than a separate lock per collection, as this used to be)
/// closes a real race: a worker dequeuing a job and registering it as
/// active used to be two separate critical sections, and a `cancel()` that
/// landed in the gap between them would find the job in neither place and
/// silently do nothing. With one lock, "remove from the queue" and "become
/// active" are the same atomic step, so `cancel()` can never observe that
/// gap.
struct QueueState {
    queue: VecDeque<JobSpec>,
    active: HashMap<JobId, ActiveJob>,
}

struct Shared {
    state: Mutex<QueueState>,
    condvar: Condvar,
    shutdown: AtomicBool,
    events: Sender<EngineEvent>,
    ffmpeg_path: PathBuf,
}

impl Shared {
    /// The single place a terminal event (`Done` | `Failed` | `Cancelled`)
    /// for an active job is ever sent. `HashMap::remove` only ever succeeds
    /// for the first caller, so if two code paths somehow both decide a job
    /// has reached a terminal state at once, only the first one's event
    /// actually gets sent - guaranteeing exactly one terminal event per job.
    fn emit_terminal(&self, id: JobId, event: EngineEvent) {
        let removed = self.state.lock().unwrap().active.remove(&id).is_some();
        if removed {
            let _ = self.events.send(event);
        }
    }
}

/// A pool of worker threads that pull [`JobSpec`]s off a queue and run them
/// with ffmpeg, respecting a configurable concurrency limit.
pub struct JobEngine {
    shared: Arc<Shared>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl JobEngine {
    pub fn new(ffmpeg_path: PathBuf, concurrency: usize) -> (Self, Receiver<EngineEvent>) {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(QueueState {
                queue: VecDeque::new(),
                active: HashMap::new(),
            }),
            condvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
            events: tx,
            ffmpeg_path,
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

    pub fn submit(&self, spec: JobSpec) {
        let mut state = self.shared.state.lock().unwrap();
        state.queue.push_back(spec);
        self.shared.condvar.notify_one();
    }

    /// Cancel a job, whether it's still queued or actively running. A job
    /// that has already reached a terminal state (nowhere to be found in
    /// either the queue or the active map) is a silent no-op - cancelling
    /// something that already finished is not an error.
    pub fn cancel(&self, id: JobId) {
        let mut state = self.shared.state.lock().unwrap();
        if let Some(pos) = state.queue.iter().position(|j| j.id == id) {
            state.queue.remove(pos);
            drop(state);
            let _ = self.shared.events.send(EngineEvent::Cancelled { id });
            return;
        }

        if let Some(job) = state.active.get(&id) {
            kill_active_job(job);
        }
    }

    pub fn cancel_all(&self) {
        let mut state = self.shared.state.lock().unwrap();
        let queued_ids: Vec<JobId> = state.queue.iter().map(|j| j.id).collect();
        state.queue.clear();
        for job in state.active.values() {
            kill_active_job(job);
        }
        drop(state);

        for id in queued_ids {
            let _ = self.shared.events.send(EngineEvent::Cancelled { id });
        }
    }
}

impl Drop for JobEngine {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.condvar.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        let dequeued = {
            let mut state = shared.state.lock().unwrap();
            loop {
                if let Some(spec) = state.queue.pop_front() {
                    // Register as active in the same critical section as the
                    // dequeue - see the `QueueState` doc comment for why.
                    let cancel_reason = Arc::new(Mutex::new(None));
                    let child_slot = Arc::new(Mutex::new(None));
                    state.active.insert(
                        spec.id,
                        ActiveJob {
                            child_slot: Arc::clone(&child_slot),
                            cancel_reason: Arc::clone(&cancel_reason),
                        },
                    );
                    break Some((spec, cancel_reason, child_slot));
                }
                if shared.shutdown.load(Ordering::SeqCst) {
                    break None;
                }
                state = shared.condvar.wait(state).unwrap();
            }
        };

        let Some((spec, cancel_reason, child_slot)) = dequeued else {
            break;
        };
        run_job(&shared, spec, cancel_reason, child_slot);
    }
}

/// Kill an active job's ffmpeg process (whole tree, not just the direct
/// child) if it has spawned yet, and mark it cancelled either way so a
/// spawn racing with this call kills itself immediately once it notices the
/// flag (see [`run_job`]).
fn kill_active_job(job: &ActiveJob) {
    *job.cancel_reason.lock().unwrap() = Some(CancelReason::User);
    if let Some(active) = job.child_slot.lock().unwrap().as_ref() {
        active.group.kill();
        let _ = active.child.lock().unwrap().kill();
    }
}

/// Wait for a thread with a bounded timeout so a stuck reader can never hang
/// cancellation indefinitely. If the deadline passes first, the thread is
/// left detached (it'll finish on its own eventually, once whatever it's
/// blocked on unblocks) rather than joined.
fn join_with_timeout<T: Send + 'static>(
    handle: std::thread::JoinHandle<T>,
    timeout: Duration,
) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if handle.is_finished() {
            return handle.join().ok();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// As above, but for reaping the child process itself via polling
/// `try_wait()` rather than a blocking `wait()` - so a process that somehow
/// survives being killed can't hang cancellation either.
fn wait_with_timeout(
    child: &Arc<Mutex<FfmpegChild>>,
    timeout: Duration,
) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(Some(status)) = child.lock().unwrap().as_inner_mut().try_wait() {
            return Some(status);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Reader threads and the final process reap are all bounded by this, so a
/// hung pipe or zombie process can never turn a cancellation into an
/// indefinite hang.
const REAP_TIMEOUT: Duration = Duration::from_secs(10);

enum AttemptOutcome {
    Success,
    Failed { error: String },
    Cancelled,
}

/// Run every pass of a single encode attempt (one full pass for a
/// single-pass job, or a first+second pass for two-pass), streaming
/// progress and appending captured stderr to `combined_log` as it goes.
#[allow(clippy::too_many_arguments)]
fn run_attempt(
    shared: &Shared,
    id: JobId,
    settings: &crate::command::EncodeSettings,
    passes: &[EncodePass],
    total_duration_seconds: f64,
    cancel_reason: &Arc<Mutex<Option<CancelReason>>>,
    child_slot: &Arc<Mutex<Option<ActiveChild>>>,
    combined_log: &mut String,
) -> AttemptOutcome {
    let pass_count = passes.len().max(1);

    for (pass_index, pass) in passes.iter().enumerate() {
        if cancel_reason.lock().unwrap().is_some() {
            return AttemptOutcome::Cancelled;
        }

        let args = build_args(settings, pass);
        let mut cmd = FfmpegCommand::new_with_path(&shared.ffmpeg_path);
        cmd.args(&args)
            .arg("-progress")
            .arg("pipe:1")
            .arg("-nostats");
        crate::procgroup::prepare(cmd.as_inner_mut());

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(source) => {
                // A cancel could have raced the spawn attempt itself; a
                // cancel request always produces exactly one `Cancelled`
                // event, even when nothing ever actually spawned.
                if cancel_reason.lock().unwrap().is_some() {
                    return AttemptOutcome::Cancelled;
                }
                let error = CoreError::Spawn {
                    binary: shared.ffmpeg_path.clone(),
                    source,
                }
                .to_string();
                return AttemptOutcome::Failed { error };
            }
        };

        // Best-effort: if the process couldn't be put under tree-kill
        // control, still proceed rather than failing the whole job - the
        // direct `Child::kill()` below still works for the (common) case
        // where the process never spawns children of its own.
        let group = match crate::procgroup::adopt(child.as_inner()) {
            Ok(group) => Arc::new(group),
            Err(err) => {
                tracing::warn!("process-tree kill unavailable for this job: {err}");
                Arc::new(ProcessGroup::noop())
            }
        };

        let stdout = child.take_stdout();
        let stderr = child.take_stderr();

        let child = Arc::new(Mutex::new(child));
        *child_slot.lock().unwrap() = Some(ActiveChild {
            child: Arc::clone(&child),
            group: Arc::clone(&group),
        });

        // A cancel() may have raced us and set the flag while the slot was
        // still empty (so it couldn't kill anything). Catch that here.
        if cancel_reason.lock().unwrap().is_some() {
            group.kill();
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

        let stdout_handle = stdout.map(|stdout| {
            let events = shared.events.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                let mut parser = ProgressParser::new();
                for line in reader.lines().map_while(Result::ok) {
                    if let Some(update) = parser.feed_line(&line) {
                        let pass_percent = progress::percent(&update, total_duration_seconds);
                        let overall_percent = pass_percent.map(|p| {
                            ((pass_index as f64) + p / 100.0) / (pass_count as f64) * 100.0
                        });
                        let info = JobProgressInfo {
                            percent: overall_percent,
                            eta: progress::eta(&update, total_duration_seconds),
                            speed: update.speed,
                            pass_index,
                            pass_count,
                        };
                        let _ = events.send(EngineEvent::Progress { id, info });
                    }
                }
            })
        });

        if let Some(handle) = stdout_handle {
            join_with_timeout(handle, REAP_TIMEOUT);
        }
        let stderr_log = stderr_handle
            .and_then(|h| join_with_timeout(h, REAP_TIMEOUT))
            .unwrap_or_default();
        combined_log.push_str(&stderr_log);

        let exit_status = wait_with_timeout(&child, REAP_TIMEOUT);
        *child_slot.lock().unwrap() = None;

        if cancel_reason.lock().unwrap().is_some() {
            return AttemptOutcome::Cancelled;
        }

        match exit_status {
            Some(status) if status.success() => {}
            Some(status) => {
                let error = CoreError::EncodeFailed {
                    code: status.code(),
                    stderr: combined_log.clone(),
                }
                .to_string();
                return AttemptOutcome::Failed { error };
            }
            None => {
                return AttemptOutcome::Failed {
                    error: "ffmpeg did not exit within the timeout after its pipes closed"
                        .to_string(),
                };
            }
        }
    }

    AttemptOutcome::Success
}

fn run_job(
    shared: &Shared,
    spec: JobSpec,
    cancel_reason: Arc<Mutex<Option<CancelReason>>>,
    child_slot: Arc<Mutex<Option<ActiveChild>>>,
) {
    let id = spec.id;

    let _ = shared.events.send(EngineEvent::Started { id });

    let mut settings = spec.settings.clone();
    let mut combined_log = String::new();
    let mut attempt: u8 = 0;
    let mut hw_fallback_tried = false;

    loop {
        let outcome = run_attempt(
            shared,
            id,
            &settings,
            &spec.passes,
            spec.total_duration_seconds,
            &cancel_reason,
            &child_slot,
            &mut combined_log,
        );

        match outcome {
            AttemptOutcome::Cancelled => {
                cleanup_passlogs(&spec.passes);
                shared.emit_terminal(id, EngineEvent::Cancelled { id });
                return;
            }
            AttemptOutcome::Failed { error } => {
                // Hardware encoders can fail for reasons that have nothing
                // to do with the user's settings (no compatible GPU, driver
                // issue, VRAM limits...). Try once, in software, before
                // giving up, per the "hardware is opt-in, falls back
                // automatically" requirement.
                let used_hardware = settings
                    .video
                    .as_ref()
                    .is_some_and(|v| v.hardware_encoder_override.is_some());
                if used_hardware && !hw_fallback_tried {
                    hw_fallback_tried = true;
                    if let Some(video) = settings.video.as_mut() {
                        video.hardware_encoder_override = None;
                    }
                    combined_log.push_str(&format!(
                        "\n[mediakit] hardware encode failed ({error}); retrying with the software encoder\n"
                    ));
                    continue;
                }

                cleanup_passlogs(&spec.passes);
                shared.emit_terminal(
                    id,
                    EngineEvent::Failed {
                        id,
                        error,
                        log: combined_log,
                    },
                );
                return;
            }
            AttemptOutcome::Success => {
                if let Some(policy) = &spec.target_size {
                    let actual_bytes = std::fs::metadata(&settings.output)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    if actual_bytes > policy.target_bytes && attempt < policy.max_retries {
                        attempt += 1;
                        let previous_kbps = settings
                            .video
                            .as_ref()
                            .and_then(|v| v.bitrate_kbps)
                            .unwrap_or(crate::bitrate::MIN_VIDEO_BITRATE_KBPS);
                        let new_kbps = crate::bitrate::next_retry_bitrate_kbps(
                            previous_kbps,
                            actual_bytes,
                            policy.target_bytes,
                            policy.safety_margin,
                        );
                        combined_log.push_str(&format!(
                            "\n[mediakit] output was {actual_bytes} bytes, over the {}-byte target; retrying (attempt {attempt}/{}) at {new_kbps}kbps video bitrate\n",
                            policy.target_bytes, policy.max_retries
                        ));
                        if let Some(video) = settings.video.as_mut() {
                            video.bitrate_kbps = Some(new_kbps);
                        }
                        let _ = shared.events.send(EngineEvent::Retrying {
                            id,
                            attempt,
                            new_video_bitrate_kbps: new_kbps,
                        });
                        continue;
                    }
                }

                cleanup_passlogs(&spec.passes);
                shared.emit_terminal(
                    id,
                    EngineEvent::Done {
                        id,
                        log: combined_log,
                    },
                );
                return;
            }
        }
    }
}

/// ffmpeg's two-pass log files (`{prefix}-0.log` and `{prefix}-0.log.mbtree`)
/// are internal working state, not something a user wants left behind next
/// to their output file.
fn cleanup_passlogs(passes: &[EncodePass]) {
    let mut prefixes: Vec<&std::path::Path> = Vec::new();
    for pass in passes {
        let prefix = match pass {
            EncodePass::First { passlog_prefix } | EncodePass::Second { passlog_prefix } => {
                passlog_prefix.as_path()
            }
            EncodePass::Single => continue,
        };
        if !prefixes.contains(&prefix) {
            prefixes.push(prefix);
        }
    }
    for prefix in prefixes {
        let base = prefix.to_string_lossy();
        let _ = std::fs::remove_file(format!("{base}-0.log"));
        let _ = std::fs::remove_file(format!("{base}-0.log.mbtree"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{
        AudioSettings, Container, EncodePass, EncodeSettings, Trim, VideoCodec, VideoSettings,
    };
    use crate::job::{JobSpec, TargetSizePolicy};
    use std::process::Command;

    // ---- these tests spawn a *real* ffmpeg to validate the engine
    // end-to-end; they skip (rather than fail) when ffmpeg isn't on PATH,
    // since CI images for some targets don't ship it. Cancellation
    // semantics specifically are covered by `tests/cancellation.rs`
    // instead, since those are exactly the platform-sensitive spawn/kill
    // paths that must never depend on ffmpeg or real media being present.
    // ----

    fn ffmpeg_on_path() -> Option<PathBuf> {
        crate::ffmpeg_env::locate_binary("ffmpeg", &std::env::temp_dir())
    }

    fn synth_clip(ffmpeg: &PathBuf, dest: &std::path::Path, duration_secs: u32) {
        let status = Command::new(ffmpeg)
            .args(["-y", "-f", "lavfi", "-i"])
            .arg(format!(
                "testsrc=duration={duration_secs}:size=160x90:rate=10"
            ))
            .args(["-f", "lavfi", "-i"])
            .arg(format!("sine=duration={duration_secs}"))
            .args(["-c:v", "libx264", "-c:a", "aac"])
            .arg(dest)
            .status()
            .expect("failed to spawn ffmpeg to synthesize test clip");
        assert!(status.success(), "failed to synthesize test clip");
    }

    fn base_settings(input: PathBuf, output: PathBuf) -> EncodeSettings {
        EncodeSettings {
            input,
            output,
            container: Container::Mp4,
            video: Some(VideoSettings {
                codec: VideoCodec::H264,
                crf: Some(30),
                preset: Some("ultrafast".to_string()),
                ..Default::default()
            }),
            audio: Some(AudioSettings::default()),
            trim: Trim::default(),
            overwrite: true,
            ..Default::default()
        }
    }

    #[test]
    fn runs_a_real_single_pass_job_end_to_end() {
        let Some(ffmpeg) = ffmpeg_on_path() else {
            eprintln!("skipping: ffmpeg not found on PATH");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("in.mp4");
        synth_clip(&ffmpeg, &input, 1);

        let output = tmp.path().join("out.mp4");
        let settings = base_settings(input, output.clone());
        let spec = JobSpec {
            id: 1,
            settings,
            passes: vec![EncodePass::Single],
            total_duration_seconds: 1.0,
            target_size: None,
        };

        let (engine, rx) = JobEngine::new(ffmpeg, 1);
        engine.submit(spec);

        let mut saw_started = false;
        let mut saw_done = false;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(EngineEvent::Started { .. }) => saw_started = true,
                Ok(EngineEvent::Done { .. }) => {
                    saw_done = true;
                    break;
                }
                Ok(EngineEvent::Failed { error, log, .. }) => {
                    panic!("job failed: {error}\n{log}")
                }
                Ok(_) => {}
                Err(_) => continue,
            }
        }
        assert!(saw_started, "never saw a Started event");
        assert!(saw_done, "never saw a Done event");
        assert!(output.exists(), "output file was not created");
    }

    #[test]
    fn emit_terminal_sends_exactly_once_under_concurrent_callers() {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared {
            state: Mutex::new(QueueState {
                queue: VecDeque::new(),
                active: HashMap::new(),
            }),
            condvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
            events: tx,
            ffmpeg_path: PathBuf::new(),
        });
        shared.state.lock().unwrap().active.insert(
            1,
            ActiveJob {
                child_slot: Arc::new(Mutex::new(None)),
                cancel_reason: Arc::new(Mutex::new(None)),
            },
        );

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || {
                    shared.emit_terminal(
                        1,
                        EngineEvent::Done {
                            id: 1,
                            log: String::new(),
                        },
                    );
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let mut received = 0;
        while rx.recv_timeout(Duration::from_millis(200)).is_ok() {
            received += 1;
        }
        assert_eq!(
            received, 1,
            "emit_terminal must guarantee exactly one terminal event per job, \
             even when multiple callers race to finish it"
        );
    }

    #[test]
    fn target_size_retry_reduces_bitrate_when_oversized() {
        let Some(ffmpeg) = ffmpeg_on_path() else {
            eprintln!("skipping: ffmpeg not found on PATH");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("in.mp4");
        synth_clip(&ffmpeg, &input, 5);

        let output = tmp.path().join("out.mp4");
        let mut settings = base_settings(input, output);
        // Deliberately way oversized for the target below, so the first
        // attempt is guaranteed to overshoot and trigger a retry.
        settings.video = Some(VideoSettings {
            codec: VideoCodec::H264,
            bitrate_kbps: Some(5000),
            crf: None,
            preset: Some("ultrafast".to_string()),
            ..Default::default()
        });

        let spec = JobSpec {
            id: 42,
            settings,
            passes: vec![EncodePass::Single],
            total_duration_seconds: 5.0,
            target_size: Some(TargetSizePolicy {
                target_bytes: 20 * 1024,
                safety_margin: crate::bitrate::DEFAULT_SAFETY_MARGIN,
                max_retries: 2,
            }),
        };

        let (engine, rx) = JobEngine::new(ffmpeg, 1);
        engine.submit(spec);

        let mut retries = 0;
        let mut done_log: Option<String> = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline && done_log.is_none() {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(EngineEvent::Retrying { id: 42, .. }) => retries += 1,
                Ok(EngineEvent::Done { id: 42, log }) => done_log = Some(log),
                Ok(EngineEvent::Failed { error, log, .. }) => {
                    panic!("job failed: {error}\n{log}")
                }
                _ => {}
            }
        }

        assert!(retries >= 1, "expected at least one retry, saw {retries}");
        let log = done_log.expect("job never completed");
        assert!(
            log.contains("retrying"),
            "expected retry note in log, got:\n{log}"
        );
    }

    #[test]
    fn cleanup_passlogs_removes_log_and_mbtree_files() {
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path().join("out.passlog");
        std::fs::write(format!("{}-0.log", prefix.to_string_lossy()), b"x").unwrap();
        std::fs::write(format!("{}-0.log.mbtree", prefix.to_string_lossy()), b"x").unwrap();

        cleanup_passlogs(&[
            EncodePass::First {
                passlog_prefix: prefix.clone(),
            },
            EncodePass::Second {
                passlog_prefix: prefix.clone(),
            },
        ]);

        assert!(!std::path::Path::new(&format!("{}-0.log", prefix.to_string_lossy())).exists());
        assert!(
            !std::path::Path::new(&format!("{}-0.log.mbtree", prefix.to_string_lossy())).exists()
        );
    }

    #[test]
    fn cleanup_passlogs_is_a_noop_for_single_pass() {
        // Must not panic when there's no passlog to clean up.
        cleanup_passlogs(&[EncodePass::Single]);
    }

    #[test]
    fn falls_back_to_software_when_hardware_encoder_fails() {
        let Some(ffmpeg) = ffmpeg_on_path() else {
            eprintln!("skipping: ffmpeg not found on PATH");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("in.mp4");
        synth_clip(&ffmpeg, &input, 1);

        let output = tmp.path().join("out.mp4");
        let mut settings = base_settings(input, output.clone());
        // Not a real encoder on any machine: guaranteed to fail immediately,
        // deterministically exercising the fallback path without needing
        // actual GPU hardware in CI.
        settings.video.as_mut().unwrap().hardware_encoder_override =
            Some("totally_fake_hw_encoder_xyz".to_string());

        let spec = JobSpec {
            id: 99,
            settings,
            passes: vec![EncodePass::Single],
            total_duration_seconds: 1.0,
            target_size: None,
        };

        let (engine, rx) = JobEngine::new(ffmpeg, 1);
        engine.submit(spec);

        let mut done_log: Option<String> = None;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline && done_log.is_none() {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(EngineEvent::Done { id: 99, log }) => done_log = Some(log),
                Ok(EngineEvent::Failed { id: 99, error, log }) => {
                    panic!("job failed instead of falling back: {error}\n{log}")
                }
                _ => {}
            }
        }

        let log = done_log.expect("job never completed after falling back");
        assert!(
            log.contains("hardware encode failed"),
            "expected fallback note in log, got:\n{log}"
        );
        assert!(
            output.exists(),
            "software fallback should still produce output"
        );
    }
}
