//! One host per installation. Two hosts from the same install would share the
//! virtual display and the port, and an update from either one closes both
//! (Setup's /CLOSEAPPLICATIONS), cutting the other one's stream. A second
//! launch brings the running window forward and exits.
//!
//! The key is the executable path plus the settings folder, so test harnesses
//! that give each host its own APPDATA keep running side by side. The mutex is
//! per Windows session.
use std::path::Path;

/// `Local\EternalMonitor-<hash>` for this executable and settings folder.
pub fn mutex_name(exe: &Path, app_data: &Path) -> String {
    let key = format!(
        "{}\n{}",
        exe.display().to_string().to_lowercase(),
        app_data.display().to_string().to_lowercase()
    );
    let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
    let hex: String = digest.as_ref()[..12]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(r"Local\EternalMonitor-{hex}")
}

/// Held for the life of the process.
pub struct Guard {
    #[cfg(windows)]
    _handle: windows::Win32::Foundation::HANDLE,
}

/// `None` when another host from this installation is already running.
pub fn acquire() -> Option<Guard> {
    let exe = std::env::current_exe().ok()?;
    let Some(app_data) = crate::settings::app_data_dir() else {
        return Some(Guard::none());
    };
    acquire_named(&mutex_name(&exe, &app_data))
}

#[cfg(windows)]
impl Drop for Guard {
    fn drop(&mut self) {
        if !self._handle.is_invalid() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self._handle);
            }
        }
    }
}

impl Guard {
    fn none() -> Self {
        Self {
            #[cfg(windows)]
            _handle: windows::Win32::Foundation::HANDLE::default(),
        }
    }
}

#[cfg(windows)]
pub fn acquire_named(name: &str) -> Option<Guard> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    match unsafe { CreateMutexW(None, false, &HSTRING::from(name)) } {
        Ok(handle) if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS => {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
            None
        }
        Ok(handle) => Some(Guard { _handle: handle }),
        // Never refuse to start because the check itself failed.
        Err(error) => {
            tracing::warn!(%error, "Could not check for another EternalMonitor");
            Some(Guard::none())
        }
    }
}

#[cfg(not(windows))]
pub fn acquire_named(_name: &str) -> Option<Guard> {
    Some(Guard::none())
}

/// Restores and focuses the running host's window: one titled
/// "EternalMonitor" whose process runs this same executable.
#[cfg(windows)]
pub fn show_existing() {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, BOOL, HWND, LPARAM};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsIconic, SetForegroundWindow,
        ShowWindow, SW_RESTORE, SW_SHOW,
    };

    unsafe extern "system" fn visit(hwnd: HWND, data: LPARAM) -> BOOL {
        let exe = &*(data.0 as *const String);
        let mut title = [0u16; 64];
        let length = GetWindowTextW(hwnd, &mut title) as usize;
        if String::from_utf16_lossy(&title[..length]) != "EternalMonitor" {
            return true.into();
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == std::process::id() {
            return true.into();
        }
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return true.into();
        };
        let mut path = vec![0u16; 32_768];
        let mut size = path.len() as u32;
        let same = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(path.as_mut_ptr()),
            &mut size,
        )
        .is_ok()
            && String::from_utf16_lossy(&path[..size as usize]).to_lowercase() == *exe;
        let _ = CloseHandle(process);
        if !same {
            return true.into();
        }
        let _ = ShowWindow(
            hwnd,
            if IsIconic(hwnd).as_bool() {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        let _ = SetForegroundWindow(hwnd);
        false.into()
    }

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let exe = exe.display().to_string().to_lowercase();
    unsafe {
        let _ = EnumWindows(Some(visit), LPARAM(&exe as *const String as isize));
    }
}

#[cfg(not(windows))]
pub fn show_existing() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_is_the_install_and_its_settings() {
        let exe = Path::new(r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe");
        let data = Path::new(r"C:\Users\A\AppData\Roaming\EternalMonitor");
        let name = mutex_name(exe, data);
        assert!(name.starts_with(r"Local\EternalMonitor-"), "{name}");
        assert_eq!(
            name,
            mutex_name(
                Path::new(r"c:\program files\eternalmonitor\EternalMonitor-host.exe"),
                data
            ),
            "Windows paths ignore case"
        );
        assert_ne!(
            name,
            mutex_name(exe, Path::new(r"D:\test\appdata\EternalMonitor"))
        );
        assert_ne!(
            name,
            mutex_name(Path::new(r"D:\other\EternalMonitor-host.exe"), data)
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_second_host_from_the_same_install_is_refused() {
        let name = format!(r"Local\EternalMonitor-test-{}", std::process::id());
        let first = acquire_named(&name).expect("first");
        assert!(acquire_named(&name).is_none(), "second");
        drop(first);
        // Windows frees the name with its last handle; a later start succeeds.
        let mut guard = None;
        for _ in 0..20 {
            guard = acquire_named(&name);
            if guard.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(guard.is_some(), "after the first exits");
    }
}
