//! The helper waits for every process running the installed app before Setup.
//! Matching is by file identity: a copy of the same program elsewhere is not
//! the app, and the app's own processes are found whatever they are called.
#![cfg(windows)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use eternal_updater::win;

#[test]
fn processes_running_the_app_are_found_by_file_identity() {
    let dir = std::env::temp_dir().join(format!("eternal-running-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let system = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap())
        .join("System32")
        .join("PING.EXE");
    // Stands in for the installed app: a copy, so its file differs from the
    // original that also runs below.
    let app = dir.join("EternalMonitor-host.exe");
    std::fs::copy(&system, &app).unwrap();
    let pinging = |exe: &std::path::Path| {
        Command::new(exe)
            .args(["-n", "4", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut original = pinging(&system);
    let mut copy = pinging(&app);
    let found = win::processes_running(&app).unwrap();
    assert_eq!(found.len(), 1, "only the copy runs the app's file");
    assert!(win::wait(&found[0], Some(Duration::from_millis(100))).is_none());
    drop(found);
    let _ = copy.wait();
    let start = Instant::now();
    while !win::processes_running(&app).unwrap().is_empty() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "the copy never left"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = original.kill();
    let _ = original.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
