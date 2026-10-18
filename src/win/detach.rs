use std::{env, mem::size_of, os::windows::process::CommandExt, process::Command};

use windows::{
    Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CreateProcessW, DETACHED_PROCESS,
            PROCESS_INFORMATION, STARTUPINFOW,
        },
    },
    core::PWSTR,
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub enum Launch {
    ParentExited,
    Run { foreground: bool },
}

pub fn launch() -> Launch {
    let args: Vec<String> = env::args().skip(1).collect();
    let foreground = args.iter().any(|arg| arg == "--foreground");
    let background = args.iter().any(|arg| arg == "--background");
    if foreground || background {
        return Launch::Run { foreground };
    }
    if spawn_detached() {
        Launch::ParentExited
    } else {
        Launch::Run { foreground: false }
    }
}

fn spawn_detached() -> bool {
    let Ok(exe) = env::current_exe() else {
        return false;
    };
    let exe = exe.to_string_lossy().to_string();
    if spawn_breakaway(&exe) {
        return true;
    }
    Command::new("cmd")
        .args(["/c", "start", "", &exe, "--background"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .is_ok()
}

fn spawn_breakaway(exe: &str) -> bool {
    let mut command = format!("\"{exe}\" --background\0")
        .encode_utf16()
        .collect::<Vec<_>>();
    unsafe {
        let startup = STARTUPINFOW {
            cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap_or(0),
            ..STARTUPINFOW::default()
        };
        let mut process = PROCESS_INFORMATION::default();
        let flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB;
        if CreateProcessW(
            None,
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            flags,
            None,
            None,
            &startup,
            &mut process,
        )
        .is_err()
        {
            return false;
        }
        let _ = CloseHandle(process.hProcess);
        let _ = CloseHandle(process.hThread);
        true
    }
}
