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

/// Show `path` selected in the system file manager (not just its folder).
/// Falls back to opening the containing folder when the file manager has no
/// "select this file" entry point.
pub fn reveal_in_file_manager(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        // `/select,<path>` must be one argument; explorer tolerates its
        // non-zero exit status, so only a spawn failure falls through.
        let mut arg = std::ffi::OsString::from("/select,");
        arg.push(path);
        if std::process::Command::new("explorer")
            .arg(arg)
            .spawn()
            .is_ok()
        {
            return;
        }
    }
    #[cfg(target_os = "macos")]
    {
        if std::process::Command::new("open")
            .arg("-R")
            .arg(path)
            .spawn()
            .is_ok()
        {
            return;
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // `dbus-send --print-reply` waits for the file manager (up to its
        // 25 s default timeout when it has to launch one first), so this
        // runs on its own thread rather than freezing the UI that clicked.
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            if !show_items_over_dbus(&path) {
                open_containing_folder(&path);
            }
        });
    }
    #[cfg(not(all(unix, not(target_os = "macos"))))]
    open_containing_folder(path);
}

/// Ask the freedesktop FileManager1 service to show `path` selected.
#[cfg(all(unix, not(target_os = "macos")))]
fn show_items_over_dbus(path: &Path) -> bool {
    let uri = format!("file://{}", percent_encode_path(&path.to_string_lossy()));
    std::process::Command::new("dbus-send")
        .args([
            "--session",
            "--print-reply",
            "--dest=org.freedesktop.FileManager1",
            "--type=method_call",
            "/org/freedesktop/FileManager1",
            "org.freedesktop.FileManager1.ShowItems",
        ])
        .arg(format!("array:string:{uri}"))
        .arg("string:")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Percent-encode everything but unreserved characters and `/`, as needed
/// for `file://` URIs (spaces and non-ASCII are common in media filenames).
// Only the Linux paths build `file://` URIs; the tests cover it everywhere.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Put `path` on the clipboard as a file, so it can be pasted straight into
/// Discord. `false` when this system has no way to do that; the caller then
/// copies the path as text through egui, which (unlike a short-lived
/// `arboard::Clipboard`) keeps serving the selection on X11/Wayland.
pub fn copy_file_to_clipboard(path: &Path) -> bool {
    copy_as_file(path)
}

/// Linux: `text/uri-list` via `wl-copy` (Wayland) or `xclip` (X11), whichever
/// is installed for the running session. Both daemonise to keep serving the
/// selection, so we only wait for the short-lived launcher process.
#[cfg(all(unix, not(target_os = "macos")))]
fn copy_as_file(path: &Path) -> bool {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let uri_list = format!(
        "file://{}\r\n",
        percent_encode_path(&path.to_string_lossy())
    );
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let attempts: [(&str, &[&str]); 2] = [
        ("wl-copy", &["--type", "text/uri-list"]),
        ("xclip", &["-selection", "clipboard", "-t", "text/uri-list"]),
    ];
    // Prefer the tool matching the session, but try the other too (XWayland).
    let order: Vec<usize> = if wayland { vec![0, 1] } else { vec![1, 0] };
    for i in order {
        let (tool, args) = attempts[i];
        let Ok(mut child) = Command::new(tool)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let wrote = child
            .stdin
            .take()
            .is_some_and(|mut stdin| stdin.write_all(uri_list.as_bytes()).is_ok());
        // Reap in the background; xclip may linger while it owns the selection.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        if wrote {
            return true;
        }
    }
    false
}

/// Windows: `CF_HDROP` (a `DROPFILES` header followed by a double-NUL
/// terminated UTF-16 path list), which Explorer, Discord and most apps
/// accept as a pasted file.
#[cfg(windows)]
fn copy_as_file(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{GlobalFree, POINT};
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows_sys::Win32::UI::Shell::DROPFILES;

    const CF_HDROP: u32 = 15;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0, 0]).collect();
    let header = std::mem::size_of::<DROPFILES>();
    let total = header + wide.len() * 2;
    // SAFETY: standard clipboard sequence. The HGLOBAL is filled while locked
    // and then owned by the clipboard on success; on any failure before
    // SetClipboardData succeeds we free it ourselves.
    unsafe {
        let hmem = GlobalAlloc(GMEM_MOVEABLE, total);
        if hmem.is_null() {
            return false;
        }
        let ptr = GlobalLock(hmem) as *mut u8;
        if ptr.is_null() {
            GlobalFree(hmem);
            return false;
        }
        let drop = DROPFILES {
            pFiles: header as u32,
            pt: POINT { x: 0, y: 0 },
            fNC: 0,
            fWide: 1,
        };
        std::ptr::copy_nonoverlapping(&drop as *const DROPFILES as *const u8, ptr, header);
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr.add(header), wide.len() * 2);
        GlobalUnlock(hmem);

        if OpenClipboard(std::ptr::null_mut()) == 0 {
            GlobalFree(hmem);
            return false;
        }
        EmptyClipboard();
        let ok = !SetClipboardData(CF_HDROP, hmem).is_null();
        CloseClipboard();
        if !ok {
            GlobalFree(hmem);
        }
        ok
    }
}

/// No file-clipboard support on other platforms: the caller falls back to
/// copying the path as text.
#[cfg(target_os = "macos")]
fn copy_as_file(_path: &Path) -> bool {
    false
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_keeps_slashes_and_escapes_the_rest() {
        assert_eq!(
            percent_encode_path("/home/me/a b.mp4"),
            "/home/me/a%20b.mp4"
        );
        assert_eq!(percent_encode_path("/x/\u{e9}.mp4"), "/x/%C3%A9.mp4");
        assert_eq!(percent_encode_path("/a-b_c.d~e"), "/a-b_c.d~e");
    }
}
