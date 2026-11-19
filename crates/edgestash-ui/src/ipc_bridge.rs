use std::io;
use std::path::{Path, PathBuf};

use edgestash_ipc::pipe::PipeClient;
use edgestash_ipc::protocol::{IpcRequest, IpcResponse};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SW_RESTORE, SW_SHOWNORMAL, SetForegroundWindow, ShowWindow,
};
use windows::core::{PCWSTR, w};

/// Abstraction over daemon Named Pipe IPC to support deterministic unit testing.
pub trait DaemonClient: Send + Sync {
    fn send_request(&self, req: &IpcRequest) -> io::Result<IpcResponse>;
}

/// Real Windows Named Pipe client targeting `edgestash-daemon`.
pub struct PipeDaemonClient {
    pipe_name: String,
}

impl PipeDaemonClient {
    pub fn new(pipe_name: impl Into<String>) -> Self {
        Self {
            pipe_name: pipe_name.into(),
        }
    }
}

impl DaemonClient for PipeDaemonClient {
    fn send_request(&self, req: &IpcRequest) -> io::Result<IpcResponse> {
        PipeClient::new(&self.pipe_name).send(req)
    }
}

/// Resolves the `edgestash-daemon.exe` sibling binary alongside the current executable,
/// falling back to `PATH` when `current_exe` is unavailable.
pub fn resolve_daemon_binary_path(current_exe: Option<&Path>) -> PathBuf {
    if let Some(dir) = current_exe.and_then(|p| p.parent())
        && !dir.as_os_str().is_empty()
    {
        return dir.join("edgestash-daemon.exe");
    }
    PathBuf::from("edgestash-daemon.exe")
}

/// Launches `edgestash-daemon.exe` via `ShellExecuteW` so Windows prompts UAC if needed.
pub fn launch_daemon_process() -> Result<(), String> {
    let exe_path = std::env::current_exe().ok();
    let daemon_path = resolve_daemon_binary_path(exe_path.as_deref());
    let wide_exe: Vec<u16> = daemon_path
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let work_dir = daemon_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| {
            p.as_os_str()
                .to_string_lossy()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        });
    let dir_ptr = work_dir
        .as_ref()
        .map(|v| PCWSTR(v.as_ptr()))
        .unwrap_or(PCWSTR::null());

    let res = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide_exe.as_ptr()),
            PCWSTR::null(),
            dir_ptr,
            SW_SHOWNORMAL,
        )
    };
    if (res.0 as usize) > 32 {
        Ok(())
    } else {
        Err(format!(
            "Failed to start {} (ShellExecute code {})",
            daemon_path.display(),
            res.0 as isize
        ))
    }
}

/// Finds an existing window by exact title and restores/foregrounds it if present.
pub fn bring_existing_window_to_front(window_title: &str) -> bool {
    let wide_title: Vec<u16> = window_title
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let Ok(hwnd) = (unsafe { FindWindowW(None, PCWSTR(wide_title.as_ptr())) }) else {
        return false;
    };
    if hwnd.0.is_null() {
        return false;
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
    }
    true
}
