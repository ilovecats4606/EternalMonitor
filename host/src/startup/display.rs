//! Which host turned the virtual display on. The driver is one device for the
//! whole machine, so the failure reporter of a host that ended abnormally
//! turns it off only when that host owned it and no other EternalMonitor host
//! of this install is running, which may be using it. Hosts only write the
//! record; a driver change never waits for it.

#![cfg_attr(not(windows), allow(dead_code))]

use super::ProcessId;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const OWNER_FILE: &str = "virtual-display-owner";

fn owner_path() -> Option<PathBuf> {
    Some(crate::settings::app_data_dir()?.join(OWNER_FILE))
}

/// Make a driver change for this host: turning the display on records this
/// process as its owner first, and a completed turn-off removes the record.
pub(crate) fn change(enabled: bool, run: impl FnOnce() -> bool) -> bool {
    match owner_path() {
        Some(path) => change_at(&path, enabled, run),
        None => run(),
    }
}

fn change_at(path: &Path, enabled: bool, run: impl FnOnce() -> bool) -> bool {
    if enabled {
        if let Err(error) = record(path, ProcessId::current()) {
            tracing::warn!(%error, "Could not record the virtual display's owner");
        }
    }
    let done = run();
    if done && !enabled {
        match fs::remove_file(path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                tracing::warn!(%error, "Could not clear the virtual display's owner")
            }
            _ => {}
        }
    }
    done
}

/// Replace the record in one step, so a reader never sees half of it.
fn record(path: &Path, owner: ProcessId) -> io::Result<()> {
    let temp = path.with_extension(format!("{owner}.tmp"));
    fs::write(&temp, owner.to_string())?;
    fs::rename(temp, path)
}

fn owner_at(path: &Path) -> Option<ProcessId> {
    ProcessId::from_name(fs::read_to_string(path).ok()?.trim())
}

/// Turn the display off after `ended` exited abnormally, if it owned the
/// display and no other host of this install runs. `Ok` says whether
/// `disable` succeeded; `Err` why it did not run.
pub(crate) fn disable_for_ended(
    ended: ProcessId,
    other_host_running: impl FnOnce() -> bool,
    disable: impl FnOnce() -> bool,
) -> Result<bool, &'static str> {
    let path = owner_path().ok_or("there is no app data folder")?;
    disable_for_ended_at(&path, ended, other_host_running, disable)
}

fn disable_for_ended_at(
    path: &Path,
    ended: ProcessId,
    other_host_running: impl FnOnce() -> bool,
    disable: impl FnOnce() -> bool,
) -> Result<bool, &'static str> {
    match owner_at(path) {
        None => return Err("no host turned it on"),
        Some(owner) if owner != ended => return Err("another host turned it on"),
        Some(_) => {}
    }
    if other_host_running() {
        return Err("another EternalMonitor host is running");
    }
    let done = disable();
    if done {
        let _ = fs::remove_file(path);
    }
    Ok(done)
}

/// Whether a process of this executable other than `ended`, this process and
/// the failure reporters in `reporters` is running.
#[cfg(windows)]
pub(crate) fn other_host_running(ended: ProcessId, reporters: &[ProcessId]) -> bool {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::EnumProcesses;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // Unsure means another host may be using the display.
    let Ok(exe) = std::env::current_exe() else {
        return true;
    };
    let exe = exe.to_string_lossy().to_lowercase();
    let mut pids = vec![0u32; 8192];
    let mut bytes = 0u32;
    if unsafe { EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut bytes) }.is_err() {
        return true;
    }
    pids.truncate(bytes as usize / 4);
    let me = std::process::id();
    pids.into_iter()
        .filter(|&pid| pid != 0 && pid != me)
        .any(|pid| {
            let Ok(process) =
                (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
            else {
                return false;
            };
            let mut name = [0u16; 1024];
            let mut length = name.len() as u32;
            let named = unsafe {
                QueryFullProcessImageNameW(
                    process,
                    PROCESS_NAME_WIN32,
                    PWSTR(name.as_mut_ptr()),
                    &mut length,
                )
            }
            .is_ok();
            unsafe {
                let _ = CloseHandle(process);
            }
            if !named || String::from_utf16_lossy(&name[..length as usize]).to_lowercase() != exe {
                return false;
            }
            let id = ProcessId {
                pid,
                start: super::process::start_time(pid).unwrap_or(0),
            };
            id != ended && !reporters.contains(&id)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "eternal-display-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn turning_the_display_on_records_the_owner_and_turning_it_off_clears_it() {
        let path = owner_file("change");
        assert!(change_at(&path, true, || true));
        assert_eq!(owner_at(&path), Some(ProcessId::current()));
        // A failed turn-off leaves the owner, so its reporter still cleans up.
        assert!(!change_at(&path, false, || false));
        assert_eq!(owner_at(&path), Some(ProcessId::current()));
        assert!(change_at(&path, false, || true));
        assert!(!path.exists());
        // A record that cannot be written does not stop the driver change.
        let unwritable = owner_file("missing").join("folder").join(OWNER_FILE);
        assert!(change_at(&unwritable, true, || true));
    }

    #[test]
    fn the_reporter_turns_off_only_its_own_hosts_display_with_no_other_host_running() {
        let path = owner_file("ended");
        let ended = ProcessId { pid: 7, start: 1 };
        let other = ProcessId { pid: 8, start: 1 };
        // A replacement host turned it on.
        record(&path, other).unwrap();
        assert_eq!(
            disable_for_ended_at(&path, ended, || false, || panic!("disabled")),
            Err("another host turned it on")
        );
        // The ended host turned it on, but another host is running.
        record(&path, ended).unwrap();
        assert_eq!(
            disable_for_ended_at(&path, ended, || true, || panic!("disabled")),
            Err("another EternalMonitor host is running")
        );
        assert_eq!(owner_at(&path), Some(ended));
        assert_eq!(
            disable_for_ended_at(&path, ended, || false, || true),
            Ok(true)
        );
        assert!(!path.exists());
        // Nobody turned it on.
        assert_eq!(
            disable_for_ended_at(&path, ended, || false, || panic!("disabled")),
            Err("no host turned it on")
        );
    }
}
