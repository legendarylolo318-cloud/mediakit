//! Cross-platform "kill the whole process tree, not just the direct child"
//! helper.
//!
//! A plain `Child::kill()` only ever signals the process we spawned
//! directly. That's wrong whenever the binary on the other end is itself a
//! thin wrapper around the real work - a package-manager shim, a `cmd /c`
//! launcher, anything that execs a child rather than replacing itself -
//! because the wrapper dying leaves the real process (and its still-open
//! stdio pipes) alive, and a reader blocked on those pipes never sees EOF.
//! That's exactly the shape of bug that made cancellation hang on Windows
//! against a Chocolatey-shimmed `ffmpeg.exe`.
//!
//! [`prepare`] must be called on the `Command` before spawning; [`adopt`]
//! must be called immediately after `spawn()` succeeds. Together they put
//! every process the child tree creates (recursively, as long as none of
//! them opt out) under one handle that [`ProcessGroup::kill`] can tear down
//! in one shot.
//!
//! **Known limitation:** there's a narrow window between `spawn()`
//! returning and `adopt()` actually placing the process into a job object /
//! process group. A grandchild the direct child spawns *inside that window*
//! (e.g. a package-manager shim execing the real binary immediately on
//! startup) can escape tree-kill and survive as an orphan after cancel. In
//! practice `adopt()` runs synchronously on the very next line after
//! `spawn()`, microseconds later, so losing this race requires the child to
//! exec a grandchild essentially instantly - `cancelling_a_running_job_leaves_no_orphan_process`
//! in `core/tests/cancellation.rs` exercises exactly this shape and hasn't
//! needed to be flake-tolerant of it. The airtight fix is `CREATE_SUSPENDED`
//! (Windows) / `POSIX_SPAWN_SETSID`-then-`assign`-then-`resume` instead of
//! plain `spawn()`, which would require dropping down to raw
//! `CreateProcessW`/`posix_spawn` instead of `std::process::Command` (which
//! exposes neither); not done here because it's a substantial rewrite for a
//! race that's real but has never been observed to fire in practice.

use std::io;
use std::process::{Child, Command};

/// Prepare a not-yet-spawned command so its eventual process tree can be
/// killed as a unit. Must be called before `.spawn()`.
pub fn prepare(cmd: &mut Command) {
    imp::prepare(cmd)
}

/// Adopt a just-spawned child (whose `Command` was already [`prepare`]d) so
/// its whole tree can be killed later via [`ProcessGroup::kill`].
///
/// Set `MEDIAKIT_DISABLE_JOB_OBJECTS=1` (any nonempty value) to force this
/// to fail as if the underlying OS call had, without needing to actually
/// break job-object/process-group creation on the host - useful for
/// deliberately exercising the [`Inner::Noop`] fallback (and confirming
/// cancellation still works via a direct `Child::kill()` rather than
/// hanging) both in tests and when triaging a CI runner where real job
/// creation is failing (e.g. `AssignProcessToJobObject` returning
/// `ERROR_ACCESS_DENIED` under a restrictive parent job).
pub fn adopt(child: &Child) -> io::Result<ProcessGroup> {
    if std::env::var_os("MEDIAKIT_DISABLE_JOB_OBJECTS").is_some() {
        return Err(io::Error::other(
            "process-tree adoption forced to fail via MEDIAKIT_DISABLE_JOB_OBJECTS",
        ));
    }
    imp::adopt(child).map(|handle| ProcessGroup(Inner::Real(handle)))
}

/// A handle over a spawned child's whole process tree, able to kill all of
/// it in one call regardless of how many layers of wrapper/launcher process
/// sit between the handle we spawned and the real work.
pub struct ProcessGroup(Inner);

enum Inner {
    Real(imp::Handle),
    /// Used when [`adopt`] itself fails (should be rare - e.g. a sandboxed
    /// environment that denies job-object/process-group creation). `kill`
    /// becomes a no-op rather than losing cancellation entirely; callers
    /// always also fall back to a direct `Child::kill()` of the process
    /// they actually spawned, which still works for the common case where
    /// that process never spawned children of its own.
    Noop,
}

impl ProcessGroup {
    pub fn noop() -> Self {
        Self(Inner::Noop)
    }

    pub fn kill(&self) {
        if let Inner::Real(handle) = &self.0 {
            handle.kill();
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::io;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command};

    pub fn prepare(cmd: &mut Command) {
        // Start a new process group led by the child itself (pgid == its
        // own pid), so every descendant it spawns - unless that descendant
        // explicitly opts out with its own `setpgid` - shares one group we
        // can signal as a unit.
        cmd.process_group(0);
    }

    pub struct Handle {
        pgid: i32,
    }

    pub fn adopt(child: &Child) -> io::Result<Handle> {
        Ok(Handle {
            pgid: child.id() as i32,
        })
    }

    impl Handle {
        pub fn kill(&self) {
            // A negative pid targets the whole process group in `kill(2)`.
            unsafe {
                libc::kill(-self.pgid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::os::windows::io::AsRawHandle;
    use std::process::{Child, Command};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

    pub fn prepare(cmd: &mut Command) {
        use std::os::windows::process::CommandExt;
        // One combined `creation_flags` call, not two: it's a plain field
        // set, not an OR-in, so a second call would silently clobber
        // whatever `ffmpeg-sidecar`'s own `create_no_window()` already set
        // rather than add to it. CREATE_NO_WINDOW keeps the existing
        // no-console-flash behavior; CREATE_NEW_PROCESS_GROUP lets the
        // child (and the job it's about to be assigned to) be signalled
        // independently of ours.
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }

    pub struct Handle {
        job: HANDLE,
    }

    // The job HANDLE is just an opaque kernel object reference - safe to
    // share and kill from any thread.
    unsafe impl Send for Handle {}
    unsafe impl Sync for Handle {}

    pub fn adopt(child: &Child) -> io::Result<Handle> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }

            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set_ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            );
            if set_ok == 0 {
                let err = io::Error::last_os_error();
                CloseHandle(job);
                return Err(err);
            }

            let process_handle = child.as_raw_handle() as HANDLE;
            if AssignProcessToJobObject(job, process_handle) == 0 {
                let err = io::Error::last_os_error();
                CloseHandle(job);
                return Err(err);
            }

            Ok(Handle { job })
        }
    }

    impl Handle {
        pub fn kill(&self) {
            unsafe {
                TerminateJobObject(self.job, 1);
            }
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.job);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Console::{GenerateConsoleCtrlEvent, CTRL_BREAK_EVENT};

        /// Reproduces the exact call order the real `engine.rs` spawn site
        /// uses - `ffmpeg-sidecar`'s `FfmpegCommand::new_with_path` sets
        /// `CREATE_NO_WINDOW` alone in its own constructor *before* our code
        /// ever touches the `Command`; `prepare` then runs last, right
        /// before `spawn()`. `creation_flags` is a plain field set, not an
        /// OR, so if `prepare` ran first and sidecar's own flag-setting ran
        /// after (or if some future refactor reordered the real call site
        /// that way), `CREATE_NEW_PROCESS_GROUP` would be silently dropped.
        ///
        /// There's no public Win32 API to read back a running process's
        /// creation flags, so this checks the flag's actual, documented
        /// *effect* instead: `GenerateConsoleCtrlEvent` only succeeds when
        /// targeted at a specific nonzero process-group id if some process
        /// with that exact id is itself a process-group leader - which only
        /// happens when `CREATE_NEW_PROCESS_GROUP` actually reached
        /// `CreateProcess`. If it got clobbered, the child inherits our own
        /// process group instead and this call fails with
        /// `ERROR_INVALID_PARAMETER`.
        #[test]
        fn create_new_process_group_survives_sidecars_own_flag_set() {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/C", "ping -n 6 127.0.0.1 >NUL"]);
            // Simulates FfmpegCommand::new_with_path's constructor, which
            // runs before `prepare` at the real call site in engine.rs.
            cmd.creation_flags(CREATE_NO_WINDOW);

            prepare(&mut cmd);
            let mut child = cmd.spawn().expect("spawn under combined creation flags");
            let pid = child.id();

            let sent = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid) };

            // Clean up regardless of the assertion outcome below.
            let group = adopt(&child).expect("adopt the child we just spawned");
            group.kill();
            let _ = child.wait();

            assert_ne!(
                sent, 0,
                "CREATE_NEW_PROCESS_GROUP did not survive to spawn() - \
                 GenerateConsoleCtrlEvent targeting the child's own pid as \
                 its process group failed, meaning it inherited ours instead"
            );
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::io;
    use std::process::{Child, Command};

    pub fn prepare(_cmd: &mut Command) {}

    pub struct Handle;

    pub fn adopt(_child: &Child) -> io::Result<Handle> {
        Err(io::Error::other(
            "process-tree kill is not implemented on this platform",
        ))
    }

    impl Handle {
        pub fn kill(&self) {}
    }
}
