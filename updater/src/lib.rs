//! EternalMonitor-updater.exe, the native helper that installs an update.
//!
//! The host downloads and verifies EternalMonitor-Setup.exe, copies this
//! helper out of the install folder (Setup replaces that folder), starts it and
//! waits for [`READY`]. Before saying it, the helper opens the installer so no
//! other process can write, rename or delete it, and checks its SHA-256 from
//! that open handle. It keeps the handle until Setup has finished, so the
//! bytes it checked are the bytes Windows runs, including Setup's elevated
//! restart. After [`READY`] the host closes; from then on every path, failures
//! included, ends by starting the installed app again.
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The line the helper prints once the installer is locked and verified.
pub const READY: &str = "ready";
/// Inno Setup flags for an in-place upgrade: a progress window and the UAC
/// prompt, no wizard pages and no message boxes. Setup does not relaunch the
/// app in silent mode (`skipifsilent`); the helper does that.
pub const SETUP_FLAGS: &[&str] = &[
    "/SP-",
    "/SILENT",
    "/SUPPRESSMSGBOXES",
    "/NORESTART",
    "/CLOSEAPPLICATIONS",
    "/NORESTARTAPPLICATIONS",
];
/// How long the helper waits for the host to close before running Setup,
/// which closes anything still holding its files.
const HOST_EXIT_WAIT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperArgs {
    pub installer: PathBuf,
    pub sha256: String,
    /// The installed host, started again at the end.
    pub app: PathBuf,
    pub host_pid: u32,
    pub log: PathBuf,
}

impl HelperArgs {
    pub fn to_args(&self) -> Vec<OsString> {
        vec![
            "--installer".into(),
            self.installer.clone().into(),
            "--sha256".into(),
            self.sha256.clone().into(),
            "--app".into(),
            self.app.clone().into(),
            "--host-pid".into(),
            self.host_pid.to_string().into(),
            "--log".into(),
            self.log.clone().into(),
        ]
    }

    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut args = args.into_iter();
        let (mut installer, mut sha256, mut app, mut host_pid, mut log) =
            (None, None, None, None, None);
        while let Some(name) = args.next() {
            let value = args
                .next()
                .ok_or_else(|| format!("{name:?} needs a value"))?;
            match name.to_str() {
                Some("--installer") => installer = Some(PathBuf::from(value)),
                Some("--sha256") => sha256 = value.into_string().ok(),
                Some("--app") => app = Some(PathBuf::from(value)),
                Some("--host-pid") => host_pid = value.to_str().and_then(|v| v.parse().ok()),
                Some("--log") => log = Some(PathBuf::from(value)),
                _ => return Err(format!("unknown argument {name:?}")),
            }
        }
        let sha256 = sha256
            .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or("--sha256 must be 64 hex digits")?
            .to_ascii_lowercase();
        Ok(Self {
            installer: installer.ok_or("--installer is required")?,
            sha256,
            app: app.ok_or("--app is required")?,
            host_pid: host_pid.ok_or("--host-pid is required")?,
            log: log.ok_or("--log is required")?,
        })
    }
}

/// Opens `path` for reading so that, while the handle lives, no other
/// process can open it for writing or rename or delete it (Windows sharing
/// rules). Reading and running it stay allowed.
pub fn open_locked(path: &Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        options.share_mode(FILE_SHARE_READ);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file"));
    }
    Ok(file)
}

/// SHA-256 of the whole file, read through `file` itself.
pub fn sha256_hex(file: &mut File) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The locked installer, or why it may not run.
pub fn open_verified(path: &Path, sha256: &str) -> Result<File, String> {
    let mut file = open_locked(path).map_err(|error| format!("cannot lock installer: {error}"))?;
    let actual =
        sha256_hex(&mut file).map_err(|error| format!("cannot read installer: {error}"))?;
    if actual != sha256 {
        return Err(format!("installer SHA-256 is {actual}, expected {sha256}"));
    }
    Ok(file)
}

/// Runs the helper. Returns its exit code; `out` is the pipe to the host.
pub fn run(args: HelperArgs, out: &mut dyn Write) -> i32 {
    let mut log = Log::open(&args.log);
    let installer = match open_verified(&args.installer, &args.sha256) {
        Ok(file) => file,
        Err(error) => {
            // The host is still open and shows the failure; nothing to relaunch.
            log.note(&format!("refused: {error}"));
            let _ = writeln!(out, "error: {error}");
            return 3;
        }
    };
    log.note("installer locked and verified");
    if writeln!(out, "{READY}").and_then(|()| out.flush()).is_err() {
        log.note("host is gone before the update started; not installing");
        return 4;
    }
    let installed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        wait_for_exit(args.host_pid, HOST_EXIT_WAIT);
        log.note("EternalMonitor closed");
        run_setup(&args, &installer)
    }));
    match installed {
        Ok(Ok(code)) => log.note(&format!("Setup exit code {code}")),
        Ok(Err(error)) => log.note(&format!("Setup did not run: {error}")),
        Err(_) => log.note("the helper failed while installing"),
    }
    drop(installer);
    // Whatever happened, the user gets EternalMonitor back.
    match relaunch(&args.app) {
        Ok(()) => {
            log.note("EternalMonitor started");
            0
        }
        Err(error) => {
            log.note(&format!("could not start EternalMonitor: {error}"));
            5
        }
    }
}

fn run_setup(args: &HelperArgs, installer: &File) -> Result<i32, String> {
    let setup_log = args.log.with_file_name("setup.log");
    let mut command = Command::new(&args.installer);
    command.args(SETUP_FLAGS);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.raw_arg(format!("/LOG=\"{}\"", setup_log.display()));
    }
    #[cfg(not(windows))]
    command.arg(format!("/LOG={}", setup_log.display()));
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot start Setup: {error}"))?;
    if let Err(error) = same_image(&child, installer) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    let status = child
        .wait()
        .map_err(|error| format!("cannot wait for Setup: {error}"))?;
    Ok(status.code().unwrap_or(-1))
}

/// Windows starts programs by path. Make sure the process it started is the
/// file this helper holds locked, not something put at that path since.
#[cfg(windows)]
fn same_image(child: &std::process::Child, installer: &File) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED};
    use windows::Win32::System::Threading::{QueryFullProcessImageNameW, PROCESS_NAME_WIN32};

    let mut locked = vec![0u16; 32_768];
    let length = unsafe {
        GetFinalPathNameByHandleW(
            HANDLE(installer.as_raw_handle()),
            &mut locked,
            FILE_NAME_NORMALIZED,
        )
    } as usize;
    if length == 0 || length >= locked.len() {
        return Err("cannot read the installer's path".into());
    }
    let mut image = vec![0u16; 32_768];
    let mut size = image.len() as u32;
    unsafe {
        QueryFullProcessImageNameW(
            HANDLE(child.as_raw_handle()),
            PROCESS_NAME_WIN32,
            PWSTR(image.as_mut_ptr()),
            &mut size,
        )
    }
    .map_err(|error| format!("cannot read Setup's path: {error}"))?;
    let normal = |wide: &[u16]| {
        let text = String::from_utf16_lossy(wide).to_lowercase();
        text.strip_prefix(r"\\?\")
            .map(str::to_string)
            .unwrap_or(text)
    };
    let (locked, image) = (normal(&locked[..length]), normal(&image[..size as usize]));
    if locked == image {
        Ok(())
    } else {
        Err(format!("Setup started from {image}, not {locked}"))
    }
}

#[cfg(not(windows))]
fn same_image(_child: &std::process::Child, _installer: &File) -> Result<(), String> {
    Ok(())
}

fn relaunch(app: &Path) -> io::Result<()> {
    let mut command = Command::new(app);
    if let Some(dir) = app.parent() {
        command.current_dir(dir);
    }
    command.spawn().map(drop)
}

#[cfg(windows)]
fn wait_for_exit(pid: u32, timeout: Duration) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    // Already gone if it cannot be opened.
    if let Ok(process) = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) } {
        unsafe {
            WaitForSingleObject(process, timeout.as_millis() as u32);
            let _ = CloseHandle(process);
        }
    }
}

#[cfg(not(windows))]
fn wait_for_exit(pid: u32, timeout: Duration) {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Lines in update.log next to the installer, for Copy logs and support.
struct Log(Option<File>);

impl Log {
    fn open(path: &Path) -> Self {
        Self(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok(),
        )
    }

    fn note(&mut self, text: &str) {
        if let Some(file) = &mut self.0 {
            let at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let _ = writeln!(file, "{at} {text}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("eternal-updater-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn arguments_round_trip_and_are_checked() {
        let args = HelperArgs {
            installer: PathBuf::from(r"C:\Users\A B\EternalMonitor-Setup-0.3.2.exe"),
            sha256: HELLO_SHA.into(),
            app: PathBuf::from(r"C:\Program Files\EternalMonitor\EternalMonitor-host.exe"),
            host_pid: 42,
            log: PathBuf::from("update.log"),
        };
        assert_eq!(HelperArgs::parse(args.to_args()), Ok(args.clone()));
        let mut bad = args.to_args();
        bad[3] = "abc".into();
        assert!(HelperArgs::parse(bad).is_err());
        let mut missing = args.to_args();
        missing.truncate(8);
        assert!(HelperArgs::parse(missing).is_err());
        assert!(HelperArgs::parse(vec!["--other".into(), "x".into()]).is_err());
    }

    #[test]
    fn verification_reads_the_locked_file() {
        let dir = temp("verify");
        let path = dir.join("setup.exe");
        std::fs::write(&path, b"hello").unwrap();
        assert!(open_verified(&path, HELLO_SHA).is_ok());
        assert!(open_verified(&path, &"0".repeat(64))
            .unwrap_err()
            .contains("SHA-256"));
        assert!(open_verified(&dir.join("missing.exe"), HELLO_SHA).is_err());
        assert!(
            open_verified(&dir, HELLO_SHA).is_err(),
            "a directory is not an installer"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The fix for a swap between the last hash check and Setup starting:
    /// while the helper holds the installer, nobody can change it.
    #[cfg(windows)]
    #[test]
    fn a_locked_installer_cannot_be_replaced() {
        let dir = temp("lock");
        let path = dir.join("setup.exe");
        std::fs::write(&path, b"hello").unwrap();
        let locked = open_verified(&path, HELLO_SHA).unwrap();
        assert!(std::fs::write(&path, b"evil!").is_err(), "overwrite");
        assert!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .is_err(),
            "append"
        );
        assert!(
            std::fs::rename(&path, dir.join("moved.exe")).is_err(),
            "rename"
        );
        assert!(std::fs::remove_file(&path).is_err(), "delete");
        assert!(
            std::fs::rename(&dir, dir.with_extension("moved")).is_err(),
            "rename the folder"
        );
        // Reading, which is what Windows needs to run it, still works.
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        drop(locked);
        std::fs::write(&path, b"later").unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
