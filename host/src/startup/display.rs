//! Who turned the virtual display on. The driver is one device for the whole
//! machine, so the failure reporter of a host that ended turns it off only
//! when that host owned it or its owner is no longer running: a replacement
//! host may have turned it on in the meantime. Every driver change holds the
//! record's lock, so a check and the change it decides cannot interleave with
//! another process's change.

#![cfg_attr(not(windows), allow(dead_code))]

use super::lock::lock;
use super::ProcessId;
use std::path::{Path, PathBuf};
use std::time::Duration;

const OWNER_FILE: &str = "virtual-display-owner";
/// A host waits this long for another process's driver change, then goes on
/// without the record.
const HOST_WAIT: Duration = Duration::from_secs(30);
/// A host changing the driver for longer than this owns the display.
const REPORTER_WAIT: Duration = Duration::from_secs(25);

fn owner_path() -> Option<PathBuf> {
    Some(crate::settings::app_data_dir()?.join(OWNER_FILE))
}

/// Make a driver change for this host: turning the display on records this
/// process as its owner first, and a completed turn-off clears the record.
pub(crate) fn change<P: FnMut()>(
    enabled: bool,
    progress: P,
    run: impl FnOnce(&mut P) -> bool,
) -> bool {
    match owner_path() {
        Some(path) => change_at(&path, enabled, progress, run),
        None => {
            let mut progress = progress;
            run(&mut progress)
        }
    }
}

fn change_at<P: FnMut()>(
    path: &Path,
    enabled: bool,
    mut progress: P,
    run: impl FnOnce(&mut P) -> bool,
) -> bool {
    let mut record = lock(path, HOST_WAIT, &mut progress)
        .inspect_err(
            |error| tracing::warn!(%error, "Changing the virtual display without its owner record"),
        )
        .ok();
    if enabled {
        if let Some(record) = &mut record {
            let _ = record.write(&ProcessId::current().to_string());
        }
    }
    let done = run(&mut progress);
    if done && !enabled {
        if let Some(record) = &mut record {
            let _ = record.write("");
        }
    }
    done
}

/// Turn the display off after `ended` has exited, unless a running host owns
/// it now. `Ok` says whether `disable` succeeded; `Err` why it did not run.
pub(crate) fn disable_for_ended(
    ended: ProcessId,
    disable: impl FnOnce() -> bool,
) -> Result<bool, &'static str> {
    let path = owner_path().ok_or("there is no app data folder")?;
    disable_for_ended_at(&path, ended, REPORTER_WAIT, disable)
}

fn disable_for_ended_at(
    path: &Path,
    ended: ProcessId,
    wait: Duration,
    disable: impl FnOnce() -> bool,
) -> Result<bool, &'static str> {
    let mut record = lock(path, wait, || {}).map_err(|_| "a running host is changing it")?;
    let owner = record
        .read()
        .ok()
        .and_then(|text| ProcessId::from_name(text.trim()))
        .ok_or("no host turned it on")?;
    if owner != ended && owner.is_running() {
        return Err("a running host owns it");
    }
    let done = disable();
    if done {
        let _ = record.write("");
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Long enough for a child process started by another test to let go.
    const WAIT: Duration = Duration::from_secs(1);

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

    fn exited_process() -> ProcessId {
        let mut child = std::process::Command::new(if cfg!(windows) { "cmd" } else { "true" })
            .args(if cfg!(windows) {
                &["/c", "exit"][..]
            } else {
                &[][..]
            })
            .spawn()
            .unwrap();
        let id = ProcessId {
            pid: child.id(),
            start: 1,
        };
        child.wait().unwrap();
        id
    }

    #[test]
    fn turning_the_display_on_records_the_owner_and_turning_it_off_clears_it() {
        let path = owner_file("change");
        assert!(change_at(&path, true, || {}, |_| true));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            ProcessId::current().to_string()
        );
        // A failed turn-off leaves the owner, so its reporter still cleans up.
        assert!(!change_at(&path, false, || {}, |_| false));
        assert!(!fs::read_to_string(&path).unwrap().is_empty());
        assert!(change_at(&path, false, || {}, |_| true));
        assert_eq!(fs::read_to_string(&path).unwrap(), "");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn the_reporter_leaves_a_display_that_a_running_host_owns() {
        let path = owner_file("replaced");
        let ended = exited_process();
        // The replacement host: this test process.
        fs::write(&path, ProcessId::current().to_string()).unwrap();
        let mut ran = false;
        let result = disable_for_ended_at(&path, ended, WAIT, || {
            ran = true;
            true
        });
        assert_eq!(result, Err("a running host owns it"));
        assert!(!ran);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            ProcessId::current().to_string()
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn the_reporter_turns_off_a_display_its_host_or_an_exited_host_owned() {
        let path = owner_file("ended");
        let ended = exited_process();
        for owner in [ended, exited_process()] {
            fs::write(&path, owner.to_string()).unwrap();
            assert_eq!(disable_for_ended_at(&path, ended, WAIT, || true), Ok(true));
            assert_eq!(fs::read_to_string(&path).unwrap(), "");
        }
        // Nobody turned it on.
        assert_eq!(
            disable_for_ended_at(&path, ended, WAIT, || panic!("disabled")),
            Err("no host turned it on")
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn the_reporter_leaves_a_display_that_a_host_is_changing() {
        let path = owner_file("busy");
        let ended = exited_process();
        fs::write(&path, ended.to_string()).unwrap();
        let busy = lock(&path, Duration::ZERO, || {}).unwrap();
        assert_eq!(
            disable_for_ended_at(&path, ended, Duration::from_millis(50), || panic!(
                "disabled"
            )),
            Err("a running host is changing it")
        );
        drop(busy);
        fs::remove_file(path).unwrap();
    }
}
