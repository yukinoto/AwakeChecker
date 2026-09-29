use windows::{
    Win32::{
        Foundation::{CloseHandle, GetLastError, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::{CreateMutexW, WaitForSingleObject},
    },
    core::w,
};

pub fn acquire_single_instance() -> bool {
    unsafe {
        let Ok(handle) = CreateMutexW(None, false, w!("Local\\AwakeChecker")) else {
            return true;
        };
        let wait = WaitForSingleObject(handle, 0);
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            // HANDLE is Copy and does not close itself. Leaving it open keeps the mutex
            // owned until this process exits.
            true
        } else {
            let _ = CloseHandle(handle);
            let _ = GetLastError();
            wait != WAIT_TIMEOUT
        }
    }
}
