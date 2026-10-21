//! A process named by its id and start time, so a reused process id never
//! makes a file written by a dead process look like a running one's.

use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessId {
    pub pid: u32,
    /// Creation time in 100 ns units on Windows; 0 where it is not tracked.
    pub start: u64,
}

impl ProcessId {
    pub(crate) fn current() -> Self {
        let pid = std::process::id();
        Self {
            pid,
            start: start_time(pid).unwrap_or(0),
        }
    }

    /// Parse the `<pid>-<start>` prefix of a file name.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        let mut parts = name.splitn(3, ['-', '.']);
        let pid = parts.next()?.parse().ok()?;
        let start = parts.next()?.parse().ok()?;
        Some(Self { pid, start })
    }

    pub(crate) fn is_running(self) -> bool {
        start_time(self.pid) == Some(self.start)
    }
}

impl fmt::Display for ProcessId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.pid, self.start)
    }
}

/// The start time of a running process, or `None` once it has exited.
#[cfg(windows)]
pub(crate) fn start_time(pid: u32) -> Option<u64> {
    use windows::Win32::Foundation::{CloseHandle, FILETIME, STILL_ACTIVE};
    use windows::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut code = 0u32;
        let mut times = [FILETIME::default(); 4];
        let [created, exited, kernel, user] = &mut times;
        let alive = GetExitCodeProcess(process, &mut code).is_ok()
            && code == STILL_ACTIVE.0 as u32
            && GetProcessTimes(process, created, exited, kernel, user).is_ok();
        let _ = CloseHandle(process);
        alive.then(|| (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime))
    }
}

#[cfg(unix)]
pub(crate) fn start_time(pid: u32) -> Option<u64> {
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    alive.then_some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_and_reject_other_files() {
        let id = ProcessId {
            pid: 4321,
            start: 133_000_000_000,
        };
        assert_eq!(ProcessId::from_name(&format!("{id}.rec")), Some(id));
        assert_eq!(ProcessId::from_name(&format!("{id}-vulkan.json")), Some(id));
        assert_eq!(ProcessId::from_name("renderer-history.json"), None);
    }

    #[test]
    fn only_the_live_process_with_the_same_start_counts_as_running() {
        let me = ProcessId::current();
        assert!(me.is_running());
        assert!(!ProcessId {
            start: me.start + 1,
            ..me
        }
        .is_running());
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/c", "exit"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap();
        let exited = ProcessId {
            pid: child.id(),
            start: start_time(child.id()).unwrap_or(0),
        };
        child.wait().unwrap();
        assert!(!exited.is_running());
    }
}
