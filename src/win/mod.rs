mod detach;
mod instance;
mod log_console;
pub mod tray;

use std::path::Path;

use windows::{
    Win32::UI::{
        Shell::ShellExecuteW,
        WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW, SW_SHOWNORMAL},
    },
    core::{PCWSTR, w},
};

pub use detach::{Launch, launch};
pub use instance::acquire_single_instance;
pub use log_console::attach_parent;

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

pub fn install_icon(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("tray.ico");
    let _ = crate::paths::ensure_parent(&path);
    let _ = std::fs::write(&path, include_bytes!("../../assets/tray.ico"));
    path
}

pub(crate) fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
