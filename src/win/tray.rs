use std::{
    path::Path,
    sync::{Mutex, OnceLock},
};

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
                Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
                DispatchMessageW, GetCursorPos, GetMessageW, HICON, HWND_MESSAGE, IMAGE_ICON,
                LR_DEFAULTSIZE, LR_LOADFROMFILE, LoadImageW, MF_STRING, PostMessageW,
                PostQuitMessage, RegisterClassW, SetForegroundWindow, SetTimer, TPM_BOTTOMALIGN,
                TPM_RIGHTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_COMMAND,
                WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WM_TIMER,
                WM_USER, WNDCLASSW, WS_OVERLAPPEDWINDOW,
            },
        },
    },
    core::{PCWSTR, w},
};

use crate::{live::presence_code, win::to_wide};

const WM_TRAY: u32 = WM_USER + 20;
const ID_LOG: usize = 1001;
const ID_OPEN: usize = 1002;
const ID_EXIT: usize = 1003;
const TIMER_TIP: usize = 1;

struct Tray {
    hwnd: HWND,
    icon: HICON,
    url: String,
    last_code: u8,
}

unsafe impl Send for Tray {}
unsafe impl Sync for Tray {}

static TRAY: OnceLock<Mutex<Tray>> = OnceLock::new();

pub fn run(url: &str, data_dir: &Path) {
    let icon_path = crate::win::install_icon(data_dir);
    unsafe {
        if let Err(err) = setup(&icon_path, url) {
            crate::win::report_fatal(&format!("托盘初始化失败：{err}"));
            return;
        }
        let mut message = windows::Win32::UI::WindowsAndMessaging::MSG::default();
        while GetMessageW(&mut message, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        crate::win::log_console::close();
        remove_icon();
    }
}

unsafe fn setup(icon_path: &Path, url: &str) -> windows::core::Result<()> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let class_name = w!("AwakeCheckerTray");
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: class_name,
            ..WNDCLASSW::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            Default::default(),
            class_name,
            w!("AwakeChecker"),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(HINSTANCE(instance.0)),
            None,
        )?;
        tracing::info!(hwnd = hwnd.0 as isize, "托盘窗口已创建");
        let wide = to_wide(&icon_path.display().to_string());
        let icon = LoadImageW(
            None,
            PCWSTR(wide.as_ptr()),
            IMAGE_ICON,
            0,
            0,
            LR_LOADFROMFILE | LR_DEFAULTSIZE,
        )?;
        let tray = Tray {
            hwnd,
            icon: HICON(icon.0),
            url: url.to_string(),
            last_code: 255,
        };
        notify(&tray, NIM_ADD)?;
        SetTimer(Some(hwnd), TIMER_TIP, 1000, None);
        let _ = TRAY.set(Mutex::new(tray));
        Ok(())
    }
}

unsafe fn notify(
    tray: &Tray,
    message: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE,
) -> windows::core::Result<()> {
    unsafe {
        let mut data = NOTIFYICONDATAW {
            cbSize: u32::try_from(std::mem::size_of::<NOTIFYICONDATAW>()).unwrap_or(0),
            hWnd: tray.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: tray.icon,
            szTip: tip_array(),
            ..NOTIFYICONDATAW::default()
        };
        data.Anonymous.uVersion = 4;
        if !Shell_NotifyIconW(message, &data).as_bool() && message == NIM_ADD {
            return Err(windows::core::Error::from_thread());
        }
        Ok(())
    }
}

fn tip_array() -> [u16; 128] {
    let text = match presence_code() {
        1 => "AwakeChecker：在线",
        2 => "AwakeChecker：离开",
        _ => "AwakeChecker",
    };
    let mut tip = [0u16; 128];
    for (index, unit) in text.encode_utf16().take(127).enumerate() {
        tip[index] = unit;
    }
    tip
}

fn remove_icon() {
    let Some(tray) = TRAY.get() else {
        return;
    };
    let Ok(tray) = tray.lock() else {
        return;
    };
    unsafe {
        let _ = notify(&tray, NIM_DELETE);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAY => {
                let mouse = lparam.0 as u32;
                if mouse == WM_LBUTTONUP || mouse == WM_LBUTTONDBLCLK {
                    crate::win::log_console::toggle();
                } else if mouse == WM_RBUTTONUP {
                    show_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                match (wparam.0 & 0xffff) as u32 {
                    1001 => crate::win::log_console::toggle(),
                    1002 => open_from_tray(),
                    1003 => PostQuitMessage(0),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_TIMER => {
                refresh_tip();
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let log = to_wide("查看日志");
        let open = to_wide("打开界面");
        let exit = to_wide("退出");
        let _ = AppendMenuW(menu, MF_STRING, ID_LOG, PCWSTR(log.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN, PCWSTR(open.as_ptr()));
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, PCWSTR(exit.as_ptr()));
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_RIGHTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
            point.x,
            point.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

fn open_from_tray() {
    let Some(tray) = TRAY.get() else {
        return;
    };
    let Ok(tray) = tray.lock() else {
        return;
    };
    crate::win::open_url(&tray.url);
}

fn refresh_tip() {
    let code = presence_code();
    let Some(slot) = TRAY.get() else {
        return;
    };
    let Ok(mut tray) = slot.lock() else {
        return;
    };
    if tray.last_code == code {
        return;
    }
    tray.last_code = code;
    unsafe {
        let _ = notify(&tray, NIM_MODIFY);
    }
}
