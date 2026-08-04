//! Test-only stand-in for ffmpeg, used exclusively by `mediakit-core`'s own
//! cancellation tests (see `engine.rs`) so they don't depend on a real
//! ffmpeg binary or real media being available in CI.
//!
//! It deliberately mimics the exact shape of bug that broke cancellation on
//! Windows: it always relaunches itself once before doing any work, the way
//! a package-manager shim (e.g. Chocolatey's `ffmpeg.exe`) launches the real
//! tool as a child process rather than being it. Killing only the direct
//! child (the outer/shim copy) leaves the inner copy - and its inherited,
//! still-open stdout pipe - alive, so a naive "kill the direct child"
//! cancellation would hang against this binary exactly like it did against
//! a choco-installed ffmpeg. Tree-kill (see `crate::procgroup`) is required
//! to actually stop it.
//!
//! All ffmpeg-style CLI args (`-i`, `-c:v`, ..., the output path) are
//! accepted and ignored. Only `--mk-`-prefixed args are interpreted, and
//! only by the inner copy.
//!
//! Behavior of the inner copy, once running:
//! - `--mk-pidfile=PATH`: write its own pid to `PATH`, so a test can check
//!   whether it's still alive after a supposed cancellation. (Not derived
//!   from the output arg's position - `mediakit-core::engine` appends its
//!   own trailing `-progress pipe:1 -nostats` after the real ffmpeg args,
//!   so nothing about this binary's argv shape is stable enough to guess a
//!   path from positionally.)
//! - `--mk-exit-code=N`: exit immediately with code `N` (simulates ffmpeg
//!   failing outright, or a spawn racing a cancel).
//! - `--mk-succeed-after-ms=N`: after N milliseconds of emitting progress,
//!   emit a final block and exit 0 (simulates a job finishing naturally).
//! - Otherwise: emit a `-progress pipe:1`-shaped block every 100ms
//!   indefinitely, up to a generous safety cap, so a test can never leave a
//!   truly unkillable process behind even if it forgets to cancel.

use std::io::Write;
use std::time::{Duration, Instant};

const INNER_MARKER_ENV: &str = "MEDIAKIT_SLEEPER_INNER";
const TICK: Duration = Duration::from_millis(100);
const SAFETY_CAP_TICKS: u64 = 1200; // 120s

fn main() {
    if std::env::var_os(INNER_MARKER_ENV).is_none() {
        run_outer();
    } else {
        run_inner();
    }
}

/// Relaunch this same binary as the "inner" copy, forwarding our args and
/// inheriting stdio, then mirror its exit status - i.e. behave exactly like
/// a shim/launcher wrapper around the real work.
fn run_outer() -> ! {
    let exe = std::env::current_exe().expect("sleeper: failed to resolve its own exe path");
    let status = std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .env(INNER_MARKER_ENV, "1")
        .status()
        .expect("sleeper: failed to relaunch itself as the inner copy");
    std::process::exit(status.code().unwrap_or(1));
}

fn run_inner() -> ! {
    let args: Vec<String> = std::env::args().collect();

    let mut exit_code: Option<i32> = None;
    let mut succeed_after: Option<Duration> = None;
    let mut pid_file: Option<&str> = None;
    for arg in &args[1..] {
        if let Some(v) = arg.strip_prefix("--mk-exit-code=") {
            exit_code = v.parse().ok();
        } else if let Some(v) = arg.strip_prefix("--mk-succeed-after-ms=") {
            succeed_after = v.parse().ok().map(Duration::from_millis);
        } else if let Some(v) = arg.strip_prefix("--mk-pidfile=") {
            pid_file = Some(v);
        }
    }

    if let Some(pid_file) = pid_file {
        let _ = std::fs::write(pid_file, std::process::id().to_string());
    }

    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "mk-sleeper-pid={}", std::process::id());
    let _ = stdout.flush();

    if let Some(code) = exit_code {
        eprintln!("sleeper: exiting immediately with code {code} (--mk-exit-code)");
        std::process::exit(code);
    }

    let start = Instant::now();
    for tick in 1..=SAFETY_CAP_TICKS {
        if let Some(after) = succeed_after {
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
