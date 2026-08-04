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

use std::io;
use std::process::{Child, Command};

/// Prepare a not-yet-spawned command so its eventual process tree can be
/// killed as a unit. Must be called before `.spawn()`.
pub fn prepare(cmd: &mut Command) {
    imp::prepare(cmd)
}

/// Adopt a just-spawned child (whose `Command` was already [`prepare`]d) so
/// its whole tree can be killed later via [`ProcessGroup::kill`].
pub fn adopt(child: &Child) -> io::Result<ProcessGroup> {
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
