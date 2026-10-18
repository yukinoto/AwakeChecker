#[cfg(windows)]
use std::mem::size_of;

#[cfg(windows)]
use anyhow::Context;
#[cfg(windows)]
use windows::{
    Win32::{
        System::{
            RemoteDesktop::{
                WTS_CURRENT_SERVER_HANDLE, WTS_INFO_CLASS, WTSFreeMemory,
                WTSQuerySessionInformationW,
            },
            SystemInformation::GetTickCount,
        },
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    },
    core::PWSTR,
};

#[cfg(windows)]
const WTS_CURRENT_SESSION: u32 = u32::MAX;
#[cfg(windows)]
const WTS_SESSION_INFO_EX: i32 = 25;
#[cfg(windows)]
const WTS_SESSIONSTATE_UNLOCK: i32 = 1;

#[cfg(windows)]
#[repr(C)]
struct WtsInfoEx {
    level: u32,
    session_id: u32,
    session_state: i32,
    session_flags: i32,
}

pub fn sample_activity() -> anyhow::Result<(u64, bool)> {
    #[cfg(windows)]
    {
        let idle_secs = idle_seconds().context("读取最后一次输入失败")?;
        let locked = session_locked().unwrap_or_else(|err| {
            tracing::warn!(error = %err, "读取锁屏状态失败，先按未锁屏处理");
            false
        });
        Ok((idle_secs, locked))
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("键鼠和锁屏采样只在 Windows 上可用");
    }
}

#[cfg(windows)]
fn idle_seconds() -> anyhow::Result<u64> {
    unsafe {
        let mut info = LASTINPUTINFO {
            cbSize: u32::try_from(size_of::<LASTINPUTINFO>()).unwrap_or(8),
            dwTime: 0,
        };
        if !GetLastInputInfo(&mut info).as_bool() {
            anyhow::bail!("GetLastInputInfo 失败");
        }
        let now = GetTickCount();
        let idle_ms = now.wrapping_sub(info.dwTime);
        Ok(u64::from(idle_ms) / 1000)
    }
}

#[cfg(windows)]
fn session_locked() -> anyhow::Result<bool> {
    unsafe {
        let mut buffer = PWSTR(std::ptr::null_mut());
        let mut bytes = 0u32;
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            WTS_CURRENT_SESSION,
            WTS_INFO_CLASS(WTS_SESSION_INFO_EX),
            &mut buffer,
            &mut bytes,
        )
        .map_err(|err| anyhow::anyhow!("WTSQuerySessionInformationW: {err}"))?;
        if buffer.0.is_null() {
            anyhow::bail!("锁屏查询没有返回数据");
        }
        let locked = if bytes as usize >= size_of::<WtsInfoEx>() {
            let info = &*(buffer.0 as *const WtsInfoEx);
            // Windows 8 起文档里的标志值和实际相反：0 是未锁屏，1 是已锁屏，-1 是未知。
            info.level == 1 && info.session_flags == WTS_SESSIONSTATE_UNLOCK
        } else {
            false
        };
        WTSFreeMemory(buffer.0.cast());
        Ok(locked)
    }
}
