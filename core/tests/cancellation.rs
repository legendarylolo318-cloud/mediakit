//! Cancellation semantics, exercised against this same test binary acting
//! as its own stand-in for ffmpeg - no separately compiled helper binary,
//! so nothing can ever be "not built yet" out from under these tests.
//!
//! `harness = false` (set in `core/Cargo.toml`'s `[[test]]` entry for this
//! file) means this file owns `main()` directly instead of Cargo's
//! auto-generated libtest harness. That's required, not just convenient:
//! `mediakit_core::engine::JobEngine` spawns its child by handing it
//! ffmpeg-shaped argv (`-y -i <input> -c:v libx264 ... -progress pipe:1
//! -nostats`) - it has no concept of "this path is actually a test binary
//! in disguise" and no hook to override that. A normal `#[test]`-harnessed
//! binary re-exec'd with that argv would have libtest's own arg parser
//! choke on `-i`/`-c:v`/`-progress` as unrecognized flags before any test
//! function ever ran. Owning `main()` lets us react to the
//! `MEDIAKIT_SLEEPER_CHILD` env var - inherited from the parent process,
//! not read from argv - before touching `std::env::args()` at all, so it
//! doesn't matter what argv the engine thinks it's passing to "ffmpeg".
//!
//! It deliberately mimics the exact shape of bug that broke cancellation on
//! Windows: when told to, it relaunches itself once before doing any real
//! work, the way a package-manager shim (e.g. Chocolatey's `ffmpeg.exe`)
//! launches the real tool as a child process rather than being it. Killing
//! only the direct child (the outer/shim copy) leaves the inner copy - and
//! its inherited, still-open stdout pipe - alive, so a naive "kill the
//! direct child" cancellation would hang against this binary exactly like
//! it did against a choco-installed ffmpeg. Tree-kill
//! (`mediakit_core::procgroup`) is required to actually stop it.

use mediakit_core::command::{
    AudioSettings, Container, EncodePass, EncodeSettings, Trim, VideoSettings,
};
use mediakit_core::engine::{EngineEvent, JobEngine};
use mediakit_core::job::{JobId, JobSpec};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Set (to any value) in a process that should behave as the sleeper child
/// instead of running tests. Inherited by whatever `JobEngine` spawns once
/// we set it in our own environment, further down in `main`.
const CHILD_ENV: &str = "MEDIAKIT_SLEEPER_CHILD";
/// Set on a child that should itself relaunch a grandchild to do the real
/// work, simulating a shim/launcher wrapper - see the module doc.
const GRANDCHILD_ENV: &str = "MEDIAKIT_SLEEPER_SPAWN_GRANDCHILD";
/// Milliseconds after which the child exits on its own (simulating a job
/// finishing naturally) instead of running until killed.
const EXIT_AFTER_MS_ENV: &str = "MEDIAKIT_SLEEPER_EXIT_AFTER_MS";
/// Path the child writes its own pid to on startup, so a test can check
/// whether it's still alive after a supposed cancellation.
const PIDFILE_ENV: &str = "MEDIAKIT_SLEEPER_PIDFILE";
/// Set (in *this* process's own env, not the child's - `procgroup::adopt`
/// runs here, not inside the spawned child) to force process-tree adoption
/// to fail, exercising the `ProcessGroup::noop()` fallback deliberately -
/// see `cancel_still_works_when_job_objects_are_disabled`.
const DISABLE_JOB_OBJECTS_ENV: &str = "MEDIAKIT_DISABLE_JOB_OBJECTS";

const TICK: Duration = Duration::from_millis(100);
const SAFETY_CAP_TICKS: u64 = 1200; // 120s, so a test that forgets to kill this can never hang CI indefinitely.

fn main() {
    if std::env::var_os(CHILD_ENV).is_some() {
        sleeper_child_main();
    }

    // Every process this binary spawns via `JobEngine` from here on
    // inherits this and re-enters as a sleeper child instead of trying to
    // run as a test binary - see the module doc for why this has to be an
    // environment variable rather than an argv flag.
    std::env::set_var(CHILD_ENV, "1");

    let tests: &[(&str, fn())] = &[
        // Runs first and in isolation from `JobEngine` entirely, so a
        // failure here unambiguously means "the child never launched",
        // never mistakable for a cancellation-logic bug in the tests that
        // follow.
        (
            "sleeper_child_actually_starts",
            sleeper_child_actually_starts,
        ),
        ("cancel_kills_a_running_job", cancel_kills_a_running_job),
        (
            "cancel_before_spawn_completes_still_reports_cancelled",
            cancel_before_spawn_completes_still_reports_cancelled,
        ),
        (
            "cancel_after_natural_completion_is_a_safe_noop",
            cancel_after_natural_completion_is_a_safe_noop,
        ),
        (
            "cancel_removes_a_still_queued_job_without_running_it",
            cancel_removes_a_still_queued_job_without_running_it,
        ),
        (
            "cancel_all_cancels_the_whole_queue",
            cancel_all_cancels_the_whole_queue,
        ),
        (
            "cancelling_a_running_job_leaves_no_orphan_process",
            cancelling_a_running_job_leaves_no_orphan_process,
        ),
        (
            "cancel_still_works_when_job_objects_are_disabled",
            cancel_still_works_when_job_objects_are_disabled,
        ),
    ];

    let mut failed = Vec::new();
    for (name, test_fn) in tests {
        print!("test {name} ... ");
        let _ = std::io::stdout().flush();
        // Clear per-test env config so a prior test's settings (exit
        // timing, grandchild spawning, pidfile path) can never leak into
        // the next one.
        std::env::remove_var(GRANDCHILD_ENV);
        std::env::remove_var(EXIT_AFTER_MS_ENV);
        std::env::remove_var(PIDFILE_ENV);
        std::env::remove_var(DISABLE_JOB_OBJECTS_ENV);
        match std::panic::catch_unwind(test_fn) {
            Ok(()) => println!("ok"),
            Err(_) => {
                println!("FAILED");
                failed.push(*name);
            }
        }
    }

    println!();
    println!(
        "test result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        tests.len() - failed.len(),
        failed.len()
    );
    if !failed.is_empty() {
        eprintln!("failures:");
        for name in &failed {
            eprintln!("    {name}");
        }
        std::process::exit(101);
    }
}

fn sleeper_child_main() -> ! {
    if std::env::var_os(GRANDCHILD_ENV).is_some() {
        // Behave like a launcher/shim: relaunch this same binary as the
        // grandchild (which does the real work below), inheriting stdio,
        // and mirror its exit status.
        let exe =
            std::env::current_exe().expect("sleeper child: failed to resolve its own exe path");
        let mut cmd = std::process::Command::new(exe);
        cmd.env(CHILD_ENV, "1");
        cmd.env_remove(GRANDCHILD_ENV);
        let status = cmd
            .status()
            .expect("sleeper child: failed to relaunch itself as the grandchild");
        std::process::exit(status.code().unwrap_or(1));
    }

    if let Some(pid_file) = std::env::var_os(PIDFILE_ENV) {
        let _ = std::fs::write(pid_file, std::process::id().to_string());
    }

    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "MEDIAKIT_SLEEPER_READY pid={}", std::process::id());
    let _ = stdout.flush();

    let exit_after = std::env::var(EXIT_AFTER_MS_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis);

    let start = Instant::now();
    for tick in 1..=SAFETY_CAP_TICKS {
        if let Some(after) = exit_after {
            if start.elapsed() >= after {
                break;
            }
        }
        let out_time_us = tick * (TICK.as_micros() as u64);
        let _ = writeln!(stdout, "frame={tick}");
        let _ = writeln!(stdout, "out_time_us={out_time_us}");
        let _ = writeln!(stdout, "speed=1.0x");
        let _ = writeln!(stdout, "progress=continue");
        let _ = stdout.flush();
        std::thread::sleep(TICK);
    }

    let _ = writeln!(stdout, "progress=end");
    let _ = stdout.flush();
    std::process::exit(0);
}

// ---------------------------------------------------------------------

fn sleeper_path() -> PathBuf {
    std::env::current_exe().expect("failed to resolve current test exe path")
}

fn sleeper_settings(output: PathBuf) -> EncodeSettings {
    EncodeSettings {
        input: PathBuf::from("unused-input"),
        output,
        container: Container::Mp4,
        video: Some(VideoSettings::default()),
        audio: Some(AudioSettings::default()),
        trim: Trim::default(),
        overwrite: true,
        ..Default::default()
    }
}

/// A job that runs this binary (re-exec'd as a sleeper child, see `main`)
/// in place of ffmpeg - real process, real pipes, real cross-platform
/// spawn/kill behavior, but no dependency on ffmpeg being installed, on
/// real media, or on any separately built artifact.
fn sleeper_spec(id: JobId, output: PathBuf) -> JobSpec {
    JobSpec {
        id,
        settings: sleeper_settings(output),
        passes: vec![EncodePass::Single],
        total_duration_seconds: 120.0,
        target_size: None,
    }
}

/// Drain events off `rx` into `events`, stopping as soon as `done` returns
/// true for the events accumulated so far (checked before *and* after each
/// receive, so already-buffered events count immediately). Returns whether
/// `done` was satisfied before `timeout` elapsed.
fn wait_until(
    rx: &Receiver<EngineEvent>,
    events: &mut Vec<EngineEvent>,
    timeout: Duration,
    mut done: impl FnMut(&[EngineEvent]) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if done(events) {
            return true;
        }
        if Instant::now() >= deadline {
            return done(events);
        }
        if let Ok(event) = rx.recv_timeout(Duration::from_millis(100)) {
            events.push(event);
        }
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    const STILL_ACTIVE: u32 = 259;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE
    }
}

// ---------------------------------------------------------------------
// Guard test: proves the re-exec mechanism itself works, independent of
// `JobEngine`, so "the sleeper child never launched" can never again
// masquerade as a cancellation-logic failure in the tests below.
fn sleeper_child_actually_starts() {
    let mut child = std::process::Command::new(sleeper_path())
        .env(CHILD_ENV, "1")
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn the sleeper child directly");

    let stdout = child.stdout.take().expect("child stdout was not piped");
    let mut line = String::new();
    BufReader::new(stdout)
        .read_line(&mut line)
        .expect("failed to read the child's first line of output");

    let pid = line
        .trim()
        .strip_prefix("MEDIAKIT_SLEEPER_READY pid=")
        .and_then(|s| s.parse::<u32>().ok());

    let _ = child.kill();
    let _ = child.wait();

    let pid =
        pid.unwrap_or_else(|| panic!("expected a MEDIAKIT_SLEEPER_READY line, got: {line:?}"));
    assert!(pid > 0, "child reported a bogus pid: {pid}");
}

fn cancel_kills_a_running_job() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("out.mp4");
    let spec = sleeper_spec(7, output);

    let (engine, rx) = JobEngine::new(sleeper_path(), 1);
    engine.submit(spec);

    // Wait for a real Progress update (not just Started) so the cancel
    // lands while the worker is genuinely blocked reading the child's
    // stdout mid-"encode", not just during the dequeue/spawn window.
    let mut events = Vec::new();
    let saw_progress = wait_until(&rx, &mut events, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Progress { info, .. } if info.percent.is_some()))
    });
    assert!(
        saw_progress,
        "never saw a real Progress update before cancelling; observed:\n{events:#?}"
    );

    engine.cancel(7);

    let saw_cancelled = wait_until(&rx, &mut events, Duration::from_secs(15), |events| {
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Cancelled { id: 7 }))
    });
    assert!(
        saw_cancelled,
        "never saw a Cancelled event; observed events:\n{events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EngineEvent::Done { .. } | EngineEvent::Failed { .. })),
        "a cancelled job must never also report Done/Failed; observed:\n{events:#?}"
    );
}

fn cancel_before_spawn_completes_still_reports_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("out.mp4");
    let spec = sleeper_spec(1, output);

    let (engine, rx) = JobEngine::new(sleeper_path(), 1);
    engine.submit(spec);
    // Deliberately no synchronization: cancel as fast as possible after
    // submit, so this often lands in the middle of the dequeue-then-spawn
    // window rather than comfortably before or after it - exactly the race
    // that used to be able to silently drop a cancel.
    engine.cancel(1);

    let mut events = Vec::new();
    let cancelled = wait_until(&rx, &mut events, Duration::from_secs(15), |events| {
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Cancelled { id: 1 }))
    });
    assert!(
        cancelled,
        "never saw a Cancelled event; observed:\n{events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EngineEvent::Done { .. } | EngineEvent::Failed { .. })),
        "a cancelled job must never also report Done/Failed; observed:\n{events:#?}"
    );
}

fn cancel_after_natural_completion_is_a_safe_noop() {
    std::env::set_var(EXIT_AFTER_MS_ENV, "100");

    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("out.mp4");
    let spec = sleeper_spec(1, output);

    let (engine, rx) = JobEngine::new(sleeper_path(), 1);
    engine.submit(spec);

    let mut events = Vec::new();
    let done = wait_until(&rx, &mut events, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Done { id: 1, .. }))
    });
    assert!(
        done,
        "job never completed naturally; observed:\n{events:#?}"
    );

    // The job has already reached a terminal state and been removed from
    // `active` by the time this runs - must be a silent no-op, not a
    // second event.
    engine.cancel(1);

    let mut after = Vec::new();
    let saw_more = wait_until(&rx, &mut after, Duration::from_secs(2), |after| {
        !after.is_empty()
    });
    assert!(
        !saw_more,
        "cancelling an already-finished job must not emit another event; observed:\n{after:#?}"
    );
}

fn cancel_removes_a_still_queued_job_without_running_it() {
    let tmp = tempfile::tempdir().unwrap();

    // Single worker, occupied by job A, so job B never leaves the queue
    // until we've already cancelled it (and therefore never spawns a
    // process at all).
    let (engine, rx) = JobEngine::new(sleeper_path(), 1);

    std::env::set_var(EXIT_AFTER_MS_ENV, "300");
    engine.submit(sleeper_spec(1, tmp.path().join("out_a.mp4")));
    engine.submit(sleeper_spec(2, tmp.path().join("out_b.mp4")));

    engine.cancel(2);

    let mut events = Vec::new();
    let a_done = wait_until(&rx, &mut events, Duration::from_secs(15), |events| {
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Done { id: 1, .. }))
    });
    assert!(
        a_done,
        "unrelated job A should have completed normally; observed:\n{events:#?}"
    );

    assert!(
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Cancelled { id: 2 })),
        "queued job was never reported cancelled; observed:\n{events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EngineEvent::Started { id: 2 })),
        "cancelled job should never have started; observed:\n{events:#?}"
    );
}

fn cancel_all_cancels_the_whole_queue() {
    let tmp = tempfile::tempdir().unwrap();
    let (engine, rx) = JobEngine::new(sleeper_path(), 1);

    engine.submit(sleeper_spec(1, tmp.path().join("out1.mp4")));
    engine.submit(sleeper_spec(2, tmp.path().join("out2.mp4")));
    engine.submit(sleeper_spec(3, tmp.path().join("out3.mp4")));

    // Let job 1 actually start running before cancelling everything, so
    // this exercises both "kill a running job" and "drop queued jobs" in
    // the same call.
    let mut events = Vec::new();
    wait_until(&rx, &mut events, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Started { id: 1 }))
    });

    engine.cancel_all();

    let all_cancelled = wait_until(&rx, &mut events, Duration::from_secs(15), |events| {
        [1u64, 2, 3].iter().all(|id| {
            events
                .iter()
                .any(|e| matches!(e, EngineEvent::Cancelled { id: got } if got == id))
        })
    });
    assert!(
        all_cancelled,
        "not every job was reported cancelled; observed:\n{events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EngineEvent::Done { .. } | EngineEvent::Failed { .. })),
        "cancel_all should never let a job finish; observed:\n{events:#?}"
    );
}

fn cancelling_a_running_job_leaves_no_orphan_process() {
    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("out.mp4");
    // The child spawns a grandchild the same way it was spawned (see
    // `sleeper_child_main`), simulating a shim wrapping the real tool, so
    // the pid written here belongs to that grandchild - the process a
    // naive "kill only the direct child" strategy would orphan.
    let pid_file = tmp.path().join("inner.pid");
    std::env::set_var(GRANDCHILD_ENV, "1");
    std::env::set_var(PIDFILE_ENV, &pid_file);

    let spec = sleeper_spec(1, output);
    let (engine, rx) = JobEngine::new(sleeper_path(), 1);
    engine.submit(spec);

    let mut events = Vec::new();
    let saw_progress = wait_until(&rx, &mut events, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Progress { info, .. } if info.percent.is_some()))
    });
    assert!(
        saw_progress,
        "job never produced progress; observed:\n{events:#?}"
    );

    let inner_pid: u32 = {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(contents) = std::fs::read_to_string(&pid_file) {
                if let Ok(pid) = contents.trim().parse() {
                    break pid;
                }
            }
            assert!(
                Instant::now() < deadline,
                "the grandchild never wrote its pid file"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    assert!(
        process_is_alive(inner_pid),
        "the grandchild should be running before cancel"
    );

    engine.cancel(1);
    let cancelled = wait_until(&rx, &mut events, Duration::from_secs(15), |events| {
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Cancelled { id: 1 }))
    });
    assert!(
        cancelled,
        "never saw a Cancelled event; observed:\n{events:#?}"
    );

    // Give the OS a brief moment to finish tearing the tree down.
    let deadline = Instant::now() + Duration::from_secs(3);
    while process_is_alive(inner_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        !process_is_alive(inner_pid),
        "the grandchild (the 'real' worker behind the shim) survived cancellation as an \
         orphan - only the direct child was killed"
    );

    let _ = std::fs::remove_file(&pid_file);
}

/// When process-tree adoption fails (real-world cause: `AssignProcessToJobObject`
/// denied by a restrictive parent job, as can happen on some CI runners -
/// see `MEDIAKIT_DISABLE_JOB_OBJECTS`), cancellation must still complete via
/// a direct `Child::kill()` rather than hang. This deliberately does *not*
/// use the grandchild-shim setup the orphan test above does: without a real
/// process group/job object, a grandchild genuinely can escape (that's the
/// documented limitation), so this only asserts the thing that must never
/// regress - the direct child dies and `Cancelled` is still reported
/// promptly.
fn cancel_still_works_when_job_objects_are_disabled() {
    std::env::set_var(DISABLE_JOB_OBJECTS_ENV, "1");

    let tmp = tempfile::tempdir().unwrap();
    let output = tmp.path().join("out.mp4");
    let spec = sleeper_spec(1, output);

    let (engine, rx) = JobEngine::new(sleeper_path(), 1);
    engine.submit(spec);

    let mut events = Vec::new();
    let saw_progress = wait_until(&rx, &mut events, Duration::from_secs(10), |events| {
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Progress { info, .. } if info.percent.is_some()))
    });
    assert!(
        saw_progress,
        "job never produced progress; observed:\n{events:#?}"
    );

    engine.cancel(1);

    let cancelled = wait_until(&rx, &mut events, Duration::from_secs(15), |events| {
        events
            .iter()
            .any(|e| matches!(e, EngineEvent::Cancelled { id: 1 }))
    });
    assert!(
        cancelled,
        "cancel with job objects disabled must still report Cancelled promptly (not hang); \
         observed:\n{events:#?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EngineEvent::Done { .. } | EngineEvent::Failed { .. })),
        "a cancelled job must never also report Done/Failed; observed:\n{events:#?}"
    );
}
