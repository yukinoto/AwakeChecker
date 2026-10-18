#[cfg(windows)]
mod detach;
#[cfg(windows)]
mod instance;
#[cfg(windows)]
mod log_console;
#[cfg(windows)]
pub mod tray;

#[cfg(windows)]
use std::path::Path;

#[cfg(windows)]
use windows::{
    Win32::UI::{
        Shell::ShellExecuteW,
        WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW, SW_SHOWNORMAL},
    },
    core::{PCWSTR, w},
};

#[cfg(windows)]
pub use detach::{Launch, launch};
#[cfg(windows)]
pub use instance::acquire_single_instance;
#[cfg(windows)]
pub use log_console::attach_parent;

#[cfg(windows)]
pub fn open_url(url: &str) {
    let wide = to_wide(url);
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

#[cfg(windows)]
pub fn report_fatal(message: &str) {
    let text = to_wide(message);
    let title = to_wide("AwakeChecker");
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
pub enum Launch {
    ParentExited,
    Run { foreground: bool },
}

#[cfg(not(windows))]
pub fn launch() -> Launch {
    Launch::Run { foreground: true }
}

#[cfg(not(windows))]
pub fn acquire_single_instance() -> bool {
    true
}

#[cfg(not(windows))]
pub fn open_url(_url: &str) {}

#[cfg(not(windows))]
pub fn attach_parent() {}

#[cfg(not(windows))]
pub fn report_fatal(message: &str) {
    eprintln!("AwakeChecker: {message}");
}

#[cfg(not(windows))]
pub mod tray {
    use std::path::Path;

    pub fn run(_url: &str, _data_dir: &Path) {}
}

#[cfg(windows)]
pub fn install_icon(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("tray.ico");
    let _ = crate::paths::ensure_parent(&path);
    let _ = std::fs::write(&path, include_bytes!("../../assets/tray.ico"));
    path
}

#[cfg(windows)]
pub(crate) fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
