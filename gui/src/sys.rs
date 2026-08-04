//! Small platform-specific helpers that don't warrant a whole crate.

use std::path::Path;

/// Open `path`'s containing folder (or `path` itself if it's already a
/// directory) in the system file manager.
pub fn open_containing_folder(path: &Path) {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."))
    };

    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(dir).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(dir).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

/// MediaKit is built with `windows_subsystem = "windows"` on Windows release
/// builds so double-clicking the exe never flashes a console. That also
/// means a plain `println!` from CLI mode goes nowhere by default when
/// there's genuinely no stdio at all - this attaches to the invoking
/// terminal's console in that case and repoints stdio at it, so
/// `mediakit --preset ... input.mp4` still prints normally from a shell. A
/// caller that already gave this process real stdio - an interactive
/// console it inherited, or stdout/stderr explicitly redirected to a
/// file/pipe - is left untouched; see the no-op early return below for why
/// that distinction matters.
#[cfg(windows)]
pub fn ensure_console_attached() {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Console::{
        AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe {
        // If stdout is already a valid handle, something already gave us
        // real stdio - inherited from an interactive console, or a caller
        // that explicitly redirected it to a file/pipe (`mediakit ... >
        // log.txt`, or a test harness capturing output). Leave it alone:
        // attaching and overwriting it unconditionally would silently
        // reroute output away from that redirect and onto the console
        // instead, breaking exactly the callers who took the trouble to
        // redirect it. Only fall back to attaching a console when there's
        // truly no stdio at all, which is what a GUI-subsystem binary gets
        // when launched with no handle inheritance (e.g. a shortcut with
        // arguments in its Target field, double-clicked from Explorer).
        let existing_stdout = GetStdHandle(STD_OUTPUT_HANDLE);
        if !existing_stdout.is_null() && existing_stdout != INVALID_HANDLE_VALUE {
            return;
        }

        // Fails (returns 0) if there's no parent console to attach to, e.g.
        // launched by double-clicking in Explorer - nothing to do then.
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }

        let conout = wide("CONOUT$");
        let out_handle = CreateFileW(
            conout.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut::<c_void>(),
        );
        if out_handle != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, out_handle);
            SetStdHandle(STD_ERROR_HANDLE, out_handle);
        }

        let conin = wide("CONIN$");
        let in_handle = CreateFileW(
            conin.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut::<c_void>(),
        );
        if in_handle != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_INPUT_HANDLE, in_handle);
        }
    }
}

#[cfg(not(windows))]
pub fn ensure_console_attached() {}
