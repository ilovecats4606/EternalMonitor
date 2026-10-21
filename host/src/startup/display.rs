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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
}
