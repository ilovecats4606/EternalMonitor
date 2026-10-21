//! The failure reporter: a second copy of the host, started with
//! `--failure-reporter`, that outlives the main process. A native crash leaves
//! only a fixed record behind; the reporter resolves the faulting module while
//! the crashed process waits, writes the log line and the upload event, turns
//! the virtual display off and shows the message. Panics and startup errors
//! hand it just the message, so a dialog never holds up cleanup or exit.

use super::process::ProcessId;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const REPORTER_ARG: &str = "--failure-reporter";

static REPORTER: OnceLock<ProcessId> = OnceLock::new();
static REQUESTS: OnceLock<PathBuf> = OnceLock::new();

pub(super) fn crash_file(directory: &Path, owner: ProcessId) -> PathBuf {
    directory.join(format!("{owner}.rec"))
}

#[cfg(any(windows, test))]
fn request_file(directory: &Path, owner: ProcessId) -> PathBuf {
    directory.join(format!("{owner}.box"))
}

/// Start the reporter for this process. Messages fall back to an in-process
/// box when it cannot start.
#[cfg(windows)]
pub(super) fn start(directory: &Path, owner: ProcessId, launch_id: &str) {
    use std::os::windows::process::CommandExt;
    let _ = REQUESTS.set(request_file(directory, owner));
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let spawned = std::process::Command::new(exe)
        .arg(REPORTER_ARG)
        .arg(owner.pid.to_string())
        .arg(owner.start.to_string())
        .arg(directory)
        .arg(launch_id)
        // CREATE_NO_WINDOW: no second console.
        .creation_flags(0x0800_0000)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    match spawned {
        Ok(child) => {
            let id = ProcessId {
                pid: child.id(),
                start: super::process::start_time(child.id()).unwrap_or(0),
            };
            let _ = REPORTER.set(id);
            tracing::info!(pid = id.pid, "Failure reporter started");
        }
        Err(error) => tracing::warn!(%error, "Could not start the failure reporter"),
    }
}

/// Ask the reporter to show a message. False when there is no reporter to ask.
pub(super) fn request_message(title: &str, body: &str) -> bool {
    match (REPORTER.get(), REQUESTS.get()) {
        (Some(reporter), Some(path)) if reporter.is_running() => {
            write_request(path, title, body).is_ok()
        }
        _ => false,
    }
}

fn write_request(path: &Path, title: &str, body: &str) -> std::io::Result<()> {
    let temp = path.with_extension("tmp");
    fs::write(&temp, format!("{}\n{body}", title.replace('\n', " ")))?;
    fs::rename(temp, path)
}

#[cfg(any(windows, test))]
fn take_request(path: &Path) -> Option<(String, String)> {
    let text = fs::read_to_string(path).ok()?;
    let _ = fs::remove_file(path);
    let (title, body) = text.split_once('\n')?;
    Some((title.to_string(), body.to_string()))
}

/// Run as the reporter when the command line asks for it; never returns then.
pub fn run_if_requested() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some(REPORTER_ARG) {
        std::process::exit(run(&args[2..]));
    }
}

#[cfg(not(windows))]
fn run(_args: &[String]) -> i32 {
    0
}

#[cfg(windows)]
fn run(args: &[String]) -> i32 {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_VM_READ,
    };
    let [pid, start, directory, launch_id] = args else {
        return 2;
    };
    let (Ok(pid), Ok(start)) = (pid.parse(), start.parse()) else {
        return 2;
    };
    let owner = ProcessId { pid, start };
    let directory = PathBuf::from(directory);
    let process = unsafe {
        OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
            false,
            owner.pid,
        )
    };
    let Ok(process) = process else {
        return 1;
    };
    if !owner.is_running() {
        return 1;
    }
    let crash = crash_file(&directory, owner);
    let requests = request_file(&directory, owner);
    loop {
        let exited = unsafe { WaitForSingleObject(process, 100) } == WAIT_OBJECT_0;
        if let Some((title, body)) = take_request(&requests) {
            show(&title, &body);
        }
        if let Some((record, false)) = fs::read(&crash).ok().and_then(|b| super::record::parse(&b))
        {
            report_crash(process, record, &crash, launch_id);
            break;
        }
        if exited {
            break;
        }
    }
    unsafe {
        let _ = CloseHandle(process);
    }
    0
}

#[cfg(windows)]
fn report_crash(
    process: windows::Win32::Foundation::HANDLE,
    record: super::record::Record,
    crash: &Path,
    launch_id: &str,
) {
    use std::io::Write;
    let module = module_at(process, record.address);
    let detail = super::native_detail(record, module.as_ref().map(|(n, o)| (n.as_str(), *o)));
    let summary = super::summary(record.window_shown);
    let _ = crate::logging::append_now(&super::log_line(summary, &detail));
    if launch_id != "-" {
        crate::telemetry::queue_for_launch(launch_id, 1, &format!("{summary}: {detail}"));
    }
    if let Ok(mut file) = fs::OpenOptions::new().append(true).open(crash) {
        let _ = file.write_all(super::record::ACK);
    }
    // The crashed process ends once its filter sees the acknowledgement; a
    // process hung by its own fault does not hold the cleanup for long.
    unsafe {
        windows::Win32::System::Threading::WaitForSingleObject(process, 10_000);
    }
    crate::vdd::disable();
    let _ = fs::remove_file(crash);
    if super::dialogs_wanted() {
        let log_path = crate::logging::session_log_path();
        show(
            summary,
            &super::message_body(&detail, &log_path.display().to_string(), false),
        );
    }
}

/// The module of another process that contains `address`, and the offset.
#[cfg(windows)]
fn module_at(process: windows::Win32::Foundation::HANDLE, address: u64) -> Option<(String, u64)> {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::ProcessStatus::{
        EnumProcessModulesEx, GetModuleBaseNameW, GetModuleInformation, LIST_MODULES_ALL,
        MODULEINFO,
    };
    let mut modules = vec![HMODULE::default(); 1024];
    let mut needed = 0u32;
    unsafe {
        EnumProcessModulesEx(
            process,
            modules.as_mut_ptr(),
            (modules.len() * std::mem::size_of::<HMODULE>()) as u32,
            &mut needed,
            LIST_MODULES_ALL,
        )
        .ok()?;
    }
    modules.truncate(needed as usize / std::mem::size_of::<HMODULE>());
    for module in modules {
        let mut info = MODULEINFO::default();
        if unsafe {
            GetModuleInformation(
                process,
                module,
                &mut info,
                std::mem::size_of::<MODULEINFO>() as u32,
            )
        }
        .is_err()
        {
            continue;
        }
        let base = info.lpBaseOfDll as u64;
        if (base..base + u64::from(info.SizeOfImage)).contains(&address) {
            let mut name = [0u16; 260];
            let length = unsafe { GetModuleBaseNameW(process, module, &mut name) } as usize;
            return Some((String::from_utf16_lossy(&name[..length]), address - base));
        }
    }
    None
}

#[cfg(windows)]
fn show(title: &str, body: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(body),
            &HSTRING::from(title),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_request_is_read_once() {
        let directory = std::env::temp_dir().join(format!(
            "eternal-request-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = request_file(&directory, ProcessId { pid: 1, start: 2 });
        write_request(&path, "EternalMonitor\nstopped", "line one\nline two").unwrap();
        assert_eq!(
            take_request(&path),
            Some(("EternalMonitor stopped".into(), "line one\nline two".into()))
        );
        assert_eq!(take_request(&path), None);
        fs::remove_dir_all(directory).unwrap();
    }
}
