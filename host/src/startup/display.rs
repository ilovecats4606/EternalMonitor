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
/// `disable` succeeded; `Err` says what happened instead, as a log note.
pub(crate) fn disable_for_ended(
    ended: ProcessId,
    other_host: impl FnMut() -> Option<ProcessId>,
    disable: impl FnOnce() -> bool,
) -> Result<bool, String> {
    let path = owner_path().ok_or("there is no app data folder")?;
    disable_for_ended_at(&path, ended, other_host, disable)
}

fn disable_for_ended_at(
    path: &Path,
    ended: ProcessId,
    mut other_host: impl FnMut() -> Option<ProcessId>,
    disable: impl FnOnce() -> bool,
) -> Result<bool, String> {
    let check = |other_host: &mut dyn FnMut() -> Option<ProcessId>| {
        match owner_at(path) {
            None => return Err(NO_OWNER.to_string()),
            Some(owner) if owner != ended => {
                return Err(format!(
                    "left the virtual display on: host {owner} turned it on"
                ))
            }
            Some(_) => {}
        }
        match other_host() {
            Some(other) => Err(format!(
                "left the virtual display on: EternalMonitor host {other} is running"
            )),
            None => Ok(()),
        }
    };
    check(&mut other_host)?;
    // Again just before the driver changes: the process scan takes a while,
    // and a replacement host may have started or turned the display on.
    check(&mut other_host)?;
    let done = disable();
    if done && !remove_if_owned(path, ended) {
        return Err(format!(
            "turned the virtual display off, but {} turned it on meanwhile; its record stays",
            owner_at(path).map_or("another host".to_string(), |o| format!("host {o}"))
        ));
    }
    Ok(done)
}

/// Remove the record only if it still names `owner`. It is moved aside in
/// one step first, so a record a replacement host writes meanwhile is never
/// the one removed; a record moved aside by mistake goes back unless a newer
/// one has taken its place.
fn remove_if_owned(path: &Path, owner: ProcessId) -> bool {
    let aside = path.with_extension(format!("{}.claim", ProcessId::current()));
    if fs::rename(path, &aside).is_err() {
        return false;
    }
    if owner_at(&aside) == Some(owner) {
        let _ = fs::remove_file(&aside);
        return true;
    }
    // A hard link, unlike a rename, never replaces a newer record.
    if fs::hard_link(&aside, path).is_err() && !path.exists() {
        let _ = fs::rename(&aside, path);
    }
    let _ = fs::remove_file(&aside);
    false
}

/// A file's identity: its volume and its index on that volume. Paths that
/// differ in case, prefix (`\\?\`), short names or links name one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileId {
    volume: u32,
    index: u64,
}

/// This host's executable: its file and the names a process image path can
/// end in when it is that file, the long and the 8.3 short one.
struct ThisExe {
    id: FileId,
    names: Vec<String>,
}

/// Whether a process whose image is `image` may run this executable. Only
/// images with one of its names are compared by identity; one that cannot be
/// resolved may be it.
fn may_be(image: &str, exe: &ThisExe, file_id: impl FnOnce(&str) -> Option<FileId>) -> bool {
    let name = image
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(image)
        .to_lowercase();
    exe.names.contains(&name) && file_id(image).is_none_or(|id| id == exe.id)
}

#[cfg(windows)]
fn file_id(path: &str) -> Option<FileId> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    // FILE_READ_ATTRIBUTES, shared with everything a running image allows.
    let file = fs::OpenOptions::new()
        .access_mode(0x80)
        .share_mode(0x7)
        .open(path)
        .ok()?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok()?;
    Some(FileId {
        volume: info.dwVolumeSerialNumber,
        index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    })
}

#[cfg(windows)]
impl ThisExe {
    fn current() -> Option<Self> {
        use windows::core::HSTRING;
        use windows::Win32::Storage::FileSystem::{GetLongPathNameW, GetShortPathNameW};
        let exe = std::env::current_exe().ok()?;
        let wide = HSTRING::from(exe.as_os_str());
        let mut names = vec![exe.file_name()?.to_string_lossy().to_lowercase()];
        for convert in [GetLongPathNameW, GetShortPathNameW] {
            let mut buffer = [0u16; 1024];
            let length = unsafe { convert(&wide, Some(&mut buffer)) } as usize;
            if length > 0 && length < buffer.len() {
                let path = String::from_utf16_lossy(&buffer[..length]);
                let name = path.rsplit('\\').next().unwrap_or(&path).to_lowercase();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        Some(Self {
            id: file_id(&exe.to_string_lossy())?,
            names,
        })
    }
}

/// Every process id, or `None` when the list may be incomplete.
#[cfg(windows)]
fn process_ids() -> Option<Vec<u32>> {
    use windows::Win32::System::ProcessStatus::EnumProcesses;
    let mut pids = vec![0u32; 4096];
    while pids.len() <= 65_536 {
        let mut bytes = 0u32;
        unsafe { EnumProcesses(pids.as_mut_ptr(), (pids.len() * 4) as u32, &mut bytes) }.ok()?;
        let count = bytes as usize / 4;
        // A full buffer may have left processes out.
        if count < pids.len() {
            pids.truncate(count);
            return Some(pids);
        }
        pids = vec![0u32; pids.len() * 2];
    }
    None
}

/// A running process that may run this executable other than `ended`, this
/// process and the failure reporters in `reporters`. When unsure, it names
/// process 0: another host may be using the display. Exited processes are
/// still listed while someone holds a handle to them; they do not count.
#[cfg(windows)]
pub(crate) fn other_host(ended: ProcessId, reporters: &[ProcessId]) -> Option<ProcessId> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let unknown = Some(ProcessId { pid: 0, start: 0 });
    let (Some(exe), Some(pids)) = (ThisExe::current(), process_ids()) else {
        return unknown;
    };
    let me = std::process::id();
    pids.into_iter()
        .filter(|&pid| pid != 0 && pid != me)
        .find_map(|pid| {
            // A host of this user can always be opened; processes of other
            // users and protected ones cannot be ours.
            let process =
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
            let mut name = [0u16; 1024];
            let mut length = name.len() as u32;
            let image = unsafe {
                QueryFullProcessImageNameW(
                    process,
                    PROCESS_NAME_WIN32,
                    PWSTR(name.as_mut_ptr()),
                    &mut length,
                )
            }
            .is_ok()
            .then(|| String::from_utf16_lossy(&name[..length as usize]));
            let start = super::process::start_time_of(process);
            unsafe {
                let _ = CloseHandle(process);
            }
            // No image: System and the kernel's own processes.
            let id = ProcessId { pid, start: start? };
            (id != ended && !reporters.contains(&id) && may_be(&image?, &exe, file_id))
                .then_some(id)
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

    #[cfg(windows)]
    #[test]
    fn another_spelling_of_this_executable_is_the_same_file() {
        let exe = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let this = ThisExe::current().unwrap();
        let spelled = format!(r"\\?\{}", exe.to_uppercase());
        assert_eq!(file_id(&spelled), Some(this.id));
        assert!(may_be(&spelled, &this, file_id));
        assert!(!may_be(r"C:\Windows\System32\cmd.exe", &this, file_id));
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
            Err("left the virtual display on: host 8-1 turned it on".to_string())
        );
        // The ended host turned it on, but another host is running.
        record(&path, ended).unwrap();
        assert_eq!(
            disable_for_ended_at(&path, ended, || Some(other), || panic!("disabled")),
            Err("left the virtual display on: EternalMonitor host 8-1 is running".to_string())
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

    #[test]
    fn a_host_that_starts_during_the_check_keeps_its_display() {
        let path = owner_file("starting");
        let ended = ProcessId { pid: 7, start: 1 };
        let replacement = ProcessId { pid: 8, start: 1 };
        record(&path, ended).unwrap();
        // B is not running at the first scan; by the second it has turned
        // the display on and written its record.
        let mut scans = 0;
        let result = disable_for_ended_at(
            &path,
            ended,
            || {
                scans += 1;
                if scans == 1 {
                    record(&path, replacement).unwrap();
                }
                None
            },
            || panic!("disabled"),
        );
        assert_eq!(
            result,
            Err("left the virtual display on: host 8-1 turned it on".to_string())
        );
        assert_eq!(owner_at(&path), Some(replacement));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn a_host_that_turns_the_display_on_during_the_turn_off_keeps_its_record() {
        let path = owner_file("during");
        let ended = ProcessId { pid: 7, start: 1 };
        let replacement = ProcessId { pid: 8, start: 1 };
        record(&path, ended).unwrap();
        let result = disable_for_ended_at(
            &path,
            ended,
            || None,
            || {
                record(&path, replacement).unwrap();
                true
            },
        );
        assert_eq!(
            result,
            Err(
                "turned the virtual display off, but host 8-1 turned it on meanwhile; its record stays"
                    .to_string()
            )
        );
        assert_eq!(owner_at(&path), Some(replacement));
        // Nothing is left beside it.
        let parent = path.parent().unwrap();
        let prefix = path.file_name().unwrap().to_string_lossy().into_owned();
        let aside = fs::read_dir(parent)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
            .count();
        assert_eq!(aside, 1);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn only_images_named_like_this_executable_are_compared_and_unknown_ones_count() {
        let id = FileId {
            volume: 0xAB,
            index: 42,
        };
        let exe = ThisExe {
            id,
            names: vec!["eternal-host.exe".into(), "eterna~1.exe".into()],
        };
        let same = |_: &str| Some(id);
        let other = |_: &str| Some(FileId { index: 43, ..id });
        let unknown = |_: &str| None;
        // A short name, a long-path prefix and another case of this file.
        assert!(may_be(r"D:\AGENTW~1\EM-STA~1\ETERNA~1.EXE", &exe, same));
        assert!(may_be(r"\\?\D:\AgentWork\Eternal-Host.EXE", &exe, same));
        // Another copy of the executable is another install.
        assert!(!may_be(r"D:\copy\eternal-host.exe", &exe, other));
        // Named like it but not resolvable: it may be a host.
        assert!(may_be(r"D:\gone\eternal-host.exe", &exe, unknown));
        // Other programs are never opened.
        assert!(!may_be(
            r"C:\Windows\explorer.exe",
            &exe,
            |_: &str| -> Option<FileId> { panic!("opened") }
        ));
    }
}
