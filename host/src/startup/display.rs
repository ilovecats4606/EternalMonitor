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
/// Why the reporter leaves the display alone after most ends: no host had it on.
pub(crate) const NO_OWNER: &str = "no host turned it on";

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
    other_host: impl FnOnce() -> Option<ProcessId>,
    disable: impl FnOnce() -> bool,
) -> Result<bool, String> {
    let path = owner_path().ok_or("there is no app data folder")?;
    disable_for_ended_at(&path, ended, other_host, disable)
}

fn disable_for_ended_at(
    path: &Path,
    ended: ProcessId,
    other_host: impl FnOnce() -> Option<ProcessId>,
    disable: impl FnOnce() -> bool,
) -> Result<bool, String> {
    match owner_at(path) {
        None => return Err(NO_OWNER.into()),
        Some(owner) if owner != ended => return Err(format!("host {owner} turned it on")),
        Some(_) => {}
    }
    if let Some(other) = other_host() {
        return Err(format!("EternalMonitor host {other} is running"));
    }
    let done = disable();
    if done {
        let _ = fs::remove_file(path);
    }
    Ok(done)
}

/// A running process of this executable other than `ended`, this process and
/// the failure reporters in `reporters`. Exited processes are still listed
/// while someone holds a handle to them; they do not count.
#[cfg(windows)]
pub(crate) fn other_host(ended: ProcessId, reporters: &[ProcessId]) -> Option<ProcessId> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::EnumProcesses;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // Unsure means another host may be using the display.
    let unknown = ProcessId { pid: 0, start: 0 };
    let Ok(exe) = std::env::current_exe() else {
        return Some(unknown);
    };
    let exe = exe.to_string_lossy().to_lowercase();
    let mut pids = vec![0u32; 8192];
    let mut bytes = 0u32;
    if unsafe { EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut bytes) }.is_err() {
        return Some(unknown);
    }
    pids.truncate(bytes as usize / 4);
    let me = std::process::id();
    pids.into_iter()
        .filter(|&pid| pid != 0 && pid != me)
        .find_map(|pid| {
            let Ok(process) =
                (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
            else {
                return None;
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
                return None;
            }
            let id = ProcessId {
                pid,
                start: super::process::start_time(pid)?,
            };
            (id != ended && !reporters.contains(&id)).then_some(id)
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

    #[cfg(windows)]
    #[test]
    fn an_exited_copy_still_held_open_is_not_another_host() {
        // The test executable stands in for the host's; `--list` exits at once.
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child.wait().unwrap();
        // `child` still holds its handle, so the process is still listed.
        let ended = ProcessId { pid: 7, start: 1 };
        assert_eq!(other_host(ended, &[]), None);
        drop(child);
    }

    #[test]
    fn the_reporter_turns_off_only_its_own_hosts_display_with_no_other_host_running() {
        let path = owner_file("ended");
        let ended = ProcessId { pid: 7, start: 1 };
        let other = ProcessId { pid: 8, start: 1 };
        // A replacement host turned it on.
        record(&path, other).unwrap();
        assert_eq!(
            disable_for_ended_at(&path, ended, || None, || panic!("disabled")),
            Err("host 8-1 turned it on".to_string())
        );
        // The ended host turned it on, but another host is running.
        record(&path, ended).unwrap();
        assert_eq!(
            disable_for_ended_at(&path, ended, || Some(other), || panic!("disabled")),
            Err("EternalMonitor host 8-1 is running".to_string())
        );
        assert_eq!(owner_at(&path), Some(ended));
        assert_eq!(
            disable_for_ended_at(&path, ended, || None, || true),
            Ok(true)
        );
        assert!(!path.exists());
        // Nobody turned it on.
        assert_eq!(
            disable_for_ended_at(&path, ended, || None, || panic!("disabled")),
            Err(NO_OWNER.to_string())
        );
    }
}
