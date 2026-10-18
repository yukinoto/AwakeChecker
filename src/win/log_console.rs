use std::{
    io::Error,
    process::{Child, Command},
    sync::Mutex,
};

use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole, SetConsoleOutputCP};

const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

static VIEWER: Mutex<Option<Child>> = Mutex::new(None);

pub fn attach_parent() {
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_ok() {
            let _ = SetConsoleOutputCP(65001);
            let _ = crate::logbuf::attach_console();
        }
    }
}

pub fn close() {
    let mut slot = VIEWER.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(mut child) = slot.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

pub fn toggle() {
    let Some(path) = crate::logbuf::path() else {
        return;
    };
    let mut slot = VIEWER.lock().unwrap_or_else(|err| err.into_inner());
    if let Some(child) = slot.as_mut()
        && matches!(child.try_wait(), Ok(None))
    {
        return;
    }
    match spawn_viewer(&path) {
        Ok(child) => {
            tracing::info!(pid = child.id(), "日志控制台已显示");
            *slot = Some(child);
        }
        Err(err) => tracing::warn!(error = %err, "无法打开日志控制台"),
    }
}

fn spawn_viewer(path: &std::path::Path) -> Result<Child, Error> {
    let quoted = path.display().to_string().replace('\'', "''");
    let script = format!(
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; [Console]::Title = 'AwakeChecker 日志'; Get-Content -LiteralPath '{quoted}' -Wait -Tail 400 -Encoding utf8"
    );
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        return Command::new("powershell.exe")
            .args(["-NoProfile", "-NoExit", "-Command", &script])
            .creation_flags(CREATE_NEW_CONSOLE | CREATE_NEW_PROCESS_GROUP)
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = script;
        Err(Error::other("日志窗口只在 Windows 上打开"))
    }
}
