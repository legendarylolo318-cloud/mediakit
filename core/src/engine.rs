//! Worker thread pool that actually runs ffmpeg for queued jobs, streaming
//! progress back through a channel so the GUI thread never blocks.

use crate::command::{build_args, EncodePass};
use crate::error::CoreError;
use crate::job::{JobId, JobProgressInfo, JobSpec};
use crate::progress::{self, ProgressParser};
use ffmpeg_sidecar::child::FfmpegChild;
use ffmpeg_sidecar::command::FfmpegCommand;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

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

struct ActiveJob {
    /// Populated once the current pass's ffmpeg process has actually spawned.
    /// `None` for the brief window between a job being dequeued and its
    /// first `spawn()` returning - `cancelled` is checked immediately after
    /// that window closes so a cancel landing during it isn't lost.
    child_slot: Arc<Mutex<Option<Arc<Mutex<FfmpegChild>>>>>,
    cancelled: Arc<AtomicBool>,
}

struct Shared {
    queue: Mutex<VecDeque<JobSpec>>,
    condvar: Condvar,
    shutdown: AtomicBool,
    active: Mutex<HashMap<JobId, ActiveJob>>,
    events: Sender<EngineEvent>,
    ffmpeg_path: PathBuf,
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
            queue: Mutex::new(VecDeque::new()),
            condvar: Condvar::new(),
            shutdown: AtomicBool::new(false),
            active: Mutex::new(HashMap::new()),
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
        let mut queue = self.shared.queue.lock().unwrap();
        queue.push_back(spec);
        self.shared.condvar.notify_one();
    }

    /// Cancel a job, whether it's still queued or actively running.
    pub fn cancel(&self, id: JobId) {
        {
            let mut queue = self.shared.queue.lock().unwrap();
            if let Some(pos) = queue.iter().position(|j| j.id == id) {
                queue.remove(pos);
                let _ = self.shared.events.send(EngineEvent::Cancelled { id });
                return;
            }
        }

        let active = self.shared.active.lock().unwrap();
        if let Some(job) = active.get(&id) {
            kill_active_job(job);
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
            let _ = self.shared.events.send(EngineEvent::Cancelled { id });
        }

        let active = self.shared.active.lock().unwrap();
        for job in active.values() {
            kill_active_job(job);
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
        run_job(&shared, spec);
    }
}

/// Kill an active job's ffmpeg process if it has spawned yet, and mark it
/// cancelled either way so a spawn racing with this call kills itself
/// immediately once it notices the flag (see [`run_job`]).
fn kill_active_job(job: &ActiveJob) {
    job.cancelled.store(true, Ordering::SeqCst);
    if let Some(child) = job.child_slot.lock().unwrap().as_ref() {
        let _ = child.lock().unwrap().kill();
    }
}

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
    cancelled: &Arc<AtomicBool>,
    child_slot: &Arc<Mutex<Option<Arc<Mutex<FfmpegChild>>>>>,
    combined_log: &mut String,
) -> AttemptOutcome {
    let pass_count = passes.len().max(1);

    for (pass_index, pass) in passes.iter().enumerate() {
        if cancelled.load(Ordering::SeqCst) {
            return AttemptOutcome::Cancelled;
        }

        let args = build_args(settings, pass);
        let mut cmd = FfmpegCommand::new_with_path(&shared.ffmpeg_path);
        cmd.args(&args)
            .arg("-progress")
            .arg("pipe:1")
            .arg("-nostats");

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(source) => {
                let error = CoreError::Spawn {
                    binary: shared.ffmpeg_path.clone(),
                    source,
                }
                .to_string();
                return AttemptOutcome::Failed { error };
            }
        };

        let stdout = child.take_stdout();
        let stderr = child.take_stderr();

        let child = Arc::new(Mutex::new(child));
        *child_slot.lock().unwrap() = Some(Arc::clone(&child));

        // A cancel() may have raced us and set the flag while the slot was
        // still empty (so it couldn't kill anything). Catch that here.
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

        if let Some(stdout) = stdout {
            let reader = BufReader::new(stdout);
            let mut parser = ProgressParser::new();
            for line in reader.lines().map_while(Result::ok) {
                if let Some(update) = parser.feed_line(&line) {
                    let pass_percent = progress::percent(&update, total_duration_seconds);
                    let overall_percent = pass_percent
                        .map(|p| ((pass_index as f64) + p / 100.0) / (pass_count as f64) * 100.0);
                    let info = JobProgressInfo {
                        percent: overall_percent,
                        eta: progress::eta(&update, total_duration_seconds),
                        speed: update.speed,
                        pass_index,
                        pass_count,
                    };
                    let _ = shared.events.send(EngineEvent::Progress { id, info });
                }
            }
        }

        let stderr_log = stderr_handle
            .and_then(|h| h.join().ok())
            .unwrap_or_default();
        combined_log.push_str(&stderr_log);

        let exit_status = child.lock().unwrap().wait();
        *child_slot.lock().unwrap() = None;

        if cancelled.load(Ordering::SeqCst) {
            return AttemptOutcome::Cancelled;
        }

        match exit_status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let error = CoreError::EncodeFailed {
                    code: status.code(),
                    stderr: combined_log.clone(),
                }
                .to_string();
                return AttemptOutcome::Failed { error };
            }
            Err(err) => {
                return AttemptOutcome::Failed {
                    error: err.to_string(),
                };
            }
        }
    }

    AttemptOutcome::Success
}

fn run_job(shared: &Shared, spec: JobSpec) {
    let id = spec.id;

    // Register the job (with an empty child slot) *before* announcing it has
    // started, so a `cancel()` that arrives in the gap between dequeuing and
    // the first successful `spawn()` is never silently dropped.
    let cancelled = Arc::new(AtomicBool::new(false));
    let child_slot: Arc<Mutex<Option<Arc<Mutex<FfmpegChild>>>>> = Arc::new(Mutex::new(None));
    shared.active.lock().unwrap().insert(
        id,
        ActiveJob {
            child_slot: Arc::clone(&child_slot),
            cancelled: Arc::clone(&cancelled),
        },
    );

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
            &cancelled,
            &child_slot,
            &mut combined_log,
        );

        match outcome {
            AttemptOutcome::Cancelled => {
                shared.active.lock().unwrap().remove(&id);
                cleanup_passlogs(&spec.passes);
                let _ = shared.events.send(EngineEvent::Cancelled { id });
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

                shared.active.lock().unwrap().remove(&id);
                cleanup_passlogs(&spec.passes);
                let _ = shared.events.send(EngineEvent::Failed {
                    id,
                    error,
                    log: combined_log,
                });
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

                shared.active.lock().unwrap().remove(&id);
                cleanup_passlogs(&spec.passes);
                let _ = shared.events.send(EngineEvent::Done {
                    id,
                    log: combined_log,
                });
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
    use std::time::{Duration, Instant};

    /// These tests spawn a *real* ffmpeg to validate the engine end-to-end.
    /// They skip (rather than fail) when ffmpeg isn't on PATH, since CI
    /// images for some targets don't ship it.
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
    fn cancel_kills_a_running_job() {
        let Some(ffmpeg) = ffmpeg_on_path() else {
            eprintln!("skipping: ffmpeg not found on PATH");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("in.mp4");
        synth_clip(&ffmpeg, &input, 20);

        let output = tmp.path().join("out.mp4");
        // Deliberately expensive settings (upscale + slowest preset) so the
        // encode takes long enough in wall-clock time for the cancel to
        // reliably land while it's still running rather than racing a
        // near-instant encode of a tiny clip.
        let mut settings = base_settings(input, output);
        settings.video = Some(VideoSettings {
            codec: VideoCodec::H264,
            crf: Some(18),
            preset: Some("veryslow".to_string()),
            width: Some(1920),
            height: Some(1080),
            ..Default::default()
        });
        let spec = JobSpec {
            id: 7,
            settings,
            passes: vec![EncodePass::Single],
            total_duration_seconds: 20.0,
            target_size: None,
        };

        let (engine, rx) = JobEngine::new(ffmpeg, 1);
        engine.submit(spec);

        // Wait for a real Progress update (not just Started) so the cancel
        // lands while the worker is genuinely blocked reading ffmpeg's
        // stdout mid-encode, not just during the dequeue/spawn window.
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Ok(EngineEvent::Progress { info, .. }) = rx.recv_timeout(Duration::from_secs(1))
            {
                if info.percent.is_some() {
                    break;
                }
            }
        }

        engine.cancel(7);

        let mut saw_cancelled = false;
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(EngineEvent::Cancelled { id: 7 }) => {
                    saw_cancelled = true;
                    break;
                }
                Ok(EngineEvent::Done { .. }) => panic!("job completed instead of being cancelled"),
                _ => {}
            }
        }
        assert!(saw_cancelled, "never saw a Cancelled event");
    }

    #[test]
    fn cancel_removes_a_still_queued_job_without_running_it() {
        let Some(ffmpeg) = ffmpeg_on_path() else {
            eprintln!("skipping: ffmpeg not found on PATH");
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let input = tmp.path().join("in.mp4");
        synth_clip(&ffmpeg, &input, 3);

        // Single worker, occupied by job A, so job B never leaves the queue
        // until we've already cancelled it.
        let (engine, rx) = JobEngine::new(ffmpeg, 1);

        let settings_a = base_settings(input.clone(), tmp.path().join("out_a.mp4"));
        engine.submit(JobSpec {
            id: 1,
            settings: settings_a,
            passes: vec![EncodePass::Single],
            total_duration_seconds: 3.0,
            target_size: None,
        });

        let settings_b = base_settings(input, tmp.path().join("out_b.mp4"));
        engine.submit(JobSpec {
            id: 2,
            settings: settings_b,
            passes: vec![EncodePass::Single],
            total_duration_seconds: 3.0,
            target_size: None,
        });

        engine.cancel(2);

        let mut saw_b_cancelled = false;
        let mut saw_b_started = false;
        let mut saw_a_done = false;
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline && !saw_a_done {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(EngineEvent::Cancelled { id: 2 }) => saw_b_cancelled = true,
                Ok(EngineEvent::Started { id: 2 }) => saw_b_started = true,
                Ok(EngineEvent::Done { id: 1, .. }) => saw_a_done = true,
                Ok(EngineEvent::Failed { id: 1, error, log }) => {
                    panic!("job A failed: {error}\n{log}")
                }
                _ => {}
            }
        }

        assert!(saw_b_cancelled, "queued job was never reported cancelled");
        assert!(!saw_b_started, "cancelled job should never have started");
        assert!(saw_a_done, "unrelated job A should have completed normally");
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
