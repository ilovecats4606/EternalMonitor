//! An exclusive lock on a small state file, shared by every EternalMonitor
//! process of the user. The lock belongs to the open handle, so a holder that
//! crashes releases it.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

pub(crate) struct Locked(File);

/// Lock `path`, creating it, waiting at most `wait` for another holder and
/// calling `waiting` meanwhile.
pub(crate) fn lock(path: &Path, wait: Duration, mut waiting: impl FnMut()) -> io::Result<Locked> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Locked(file)),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                waiting();
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("{} is locked by another process", path.display()),
                ))
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

impl Locked {
    pub(crate) fn read(&mut self) -> io::Result<String> {
        let mut text = String::new();
        self.0.seek(SeekFrom::Start(0))?;
        self.0.read_to_string(&mut text)?;
        Ok(text)
    }

    pub(crate) fn write(&mut self, text: &str) -> io::Result<()> {
        self.0.set_len(0)?;
        self.0.seek(SeekFrom::Start(0))?;
        self.0.write_all(text.as_bytes())?;
        self.0.sync_data()
    }
}

/// Unlock explicitly: on Unix a child being started at that moment can still
/// share the handle, and with it the lock.
impl Drop for Locked {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_waits_and_then_gives_up() {
        let path = std::env::temp_dir().join(format!(
            "eternal-lock-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut first = lock(&path, Duration::ZERO, || {}).unwrap();
        first.write("owner").unwrap();
        let mut waited = 0;
        let second = lock(&path, Duration::from_millis(100), || waited += 1);
        assert_eq!(second.err().unwrap().kind(), io::ErrorKind::TimedOut);
        assert!(waited > 0);
        drop(first);
        let mut second = lock(&path, Duration::ZERO, || {}).unwrap();
        assert_eq!(second.read().unwrap(), "owner");
        drop(second);
        std::fs::remove_file(path).unwrap();
    }
}
