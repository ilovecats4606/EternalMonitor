//! A second launch from the same install must leave the running host alone:
//! its session log (which Copy logs, the Performance page and crash messages
//! read) must not be rotated away. Runs the real binary twice, headless.
#![cfg(windows)]

use std::net::UdpSocket;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn host(app_data: &Path, port: u16) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_eternal-host"));
    command
        .arg(port.to_string())
        .env("APPDATA", app_data)
        .env("ETERNAL_HEADLESS", "1")
        .env("ETERNAL_CAPTURE", "synthetic")
        .env("ETERNAL_SYNTH_SIZE", "320x180")
        .env("ETERNAL_ENCODER", "libx264")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn a_second_launch_leaves_the_running_hosts_log_in_place() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("em-second-launch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let logs = dir.join("EternalMonitor").join("logs");
    let log = logs.join("eternal-host-session.log");

    let mut first = host(&dir, free_port()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !read(&log).contains("UDP transport ready") {
        assert!(Instant::now() < deadline, "the first host never got ready");
        std::thread::sleep(Duration::from_millis(100));
    }

    let second = host(&dir, free_port()).status().unwrap();
    let after = read(&log);
    let rotated: Vec<_> = std::fs::read_dir(&logs)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "eternal-host-session.log")
        .collect();
    let first_alive = first.try_wait().unwrap().is_none();
    let _ = first.kill();
    let _ = first.wait();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(second.success(), "the second launch failed: {second:?}");
    assert!(first_alive, "the second launch ended the first host");
    assert!(
        rotated.is_empty(),
        "the second launch rotated the running host's log: {rotated:?}"
    );
    assert!(
        after.contains("UDP transport ready"),
        "the running host's log was replaced"
    );
}
