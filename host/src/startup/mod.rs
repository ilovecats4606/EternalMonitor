//! A failure that ends the host must never look like a console window that
//! flashes and closes (issue #59). An error returned during startup and a
//! panic on the GUI thread are written to the session log and the upload
//! outbox synchronously; the virtual display is turned off; then the failure
//! reporter process shows the message, so a dialog never holds up cleanup or
//! exit. A native crash only writes a fixed record (see `record`), which the
//! reporter or the next launch turns into the same log line, event and message.
//! A host that ends past the exception filter is reported by its exit code.

pub(crate) mod display;
pub(crate) mod lock;
mod process;
mod record;
mod reporter;

pub(crate) use process::ProcessId;
pub use reporter::run_if_requested as run_reporter_if_requested;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static REPORTED: AtomicBool = AtomicBool::new(false);
static WINDOW_SHOWN: AtomicBool = AtomicBool::new(false);
static SHOWN_AT: OnceLock<Instant> = OnceLock::new();

/// Test hook for the failure paths. Set `ETERNAL_SIMULATE_STARTUP_FAILURE` to
/// `error`, `panic`, `hang` (30 s) or, on Windows, `crash`, `crash-thread`
/// (on another thread), `crash-heap` (while another thread holds the heap
/// lock) or `fail-fast` (past the exception filter) to fail while the window
/// renderer starts; `early-error` to fail right after the failure reporter
/// starts; `panic-later` or, on Windows, `fail-fast-later` to fail 15 s after
/// the window works; or `surface-error` to report a surface error on every
/// frame of the first two seconds. A `-quiet` suffix skips the message box.
pub const SIMULATE_ENV: &str = "ETERNAL_SIMULATE_STARTUP_FAILURE";

/// Called once the window works, so later failures read as a stop, not a
/// failed start.
pub fn mark_window_shown() {
    WINDOW_SHOWN.store(true, Ordering::Relaxed);
    let _ = SHOWN_AT.set(Instant::now());
    reporter::mark(reporter::SHOWN);
}

pub(crate) fn window_shown() -> bool {
    WINDOW_SHOWN.load(Ordering::Relaxed)
}

fn crash_dir() -> Option<PathBuf> {
    Some(crate::settings::app_data_dir()?.join("crash"))
}

/// Panics on the main thread end the GUI and the process; report those. Other
/// threads belong to the pipeline, which the supervisor restarts, so they keep
/// the plain log line. Native crashes on any thread end the process.
pub fn install_failure_hooks() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some("main") {
            after_main_panic(|| save(&panic_detail(info)), crate::vdd::disable, show);
        } else {
            tracing::error!(panic = %info, backtrace = %std::backtrace::Backtrace::force_capture(), "Host panic");
            crate::vdd::disable();
        }
        default_hook(info);
    }));
    #[cfg(windows)]
    prepare_crash_reporting();
    if simulated() == "early-error" {
        report(&format!("Simulated early failure from {SIMULATE_ENV}"));
        std::process::exit(1);
    }
}

/// Open this launch's crash record, install the exception filter and start
/// the reporter.
#[cfg(windows)]
fn prepare_crash_reporting() {
    use std::os::windows::fs::OpenOptionsExt;
    let Some(directory) = crash_dir() else {
        return;
    };
    if let Err(error) = fs::create_dir_all(&directory) {
        tracing::warn!(%error, "Could not prepare the crash record");
        return;
    }
    let owner = ProcessId::current();
    // Shared for delete, so the reporter can remove it after a crash.
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .share_mode(0x7)
        .open(reporter::owner_file(&directory, owner, reporter::RECORD));
    match file {
        Ok(file) => record::filter::install(file),
        Err(error) => tracing::warn!(%error, "Could not prepare the crash record"),
    }
    let launch_id = crate::telemetry::launch_id().unwrap_or_else(|| "-".to_string());
    reporter::start(&directory, owner, &launch_id);
}

/// Save the report, turn the virtual display off, then ask for the message:
/// the dialog never holds the cleanup, and killing a stuck app cannot skip it.
fn after_main_panic(
    save: impl FnOnce() -> Option<(&'static str, String)>,
    disable_virtual_display: impl FnOnce(),
    show: impl FnOnce(&str, &str),
) {
    let message = save();
    disable_virtual_display();
    if let Some((summary, body)) = message {
        show(summary, &body);
    }
}

/// A normal quit leaves no crash record behind, and tells the reporter.
pub fn finish() {
    reporter::mark(reporter::DONE);
    if let Some(directory) = crash_dir() {
        let owner = ProcessId::current();
        let _ = fs::remove_file(reporter::owner_file(&directory, owner, reporter::RECORD));
    }
}

/// Report crashes of earlier launches that no reporter saved, and show
/// messages that no reporter showed.
pub fn take_previous_crashes() {
    let Some(directory) = crash_dir() else {
        return;
    };
    let previous = collect_previous(&directory, ProcessId::current());
    for record in previous.records {
        let summary = summary(record.window_shown);
        let detail = native_detail(record, None);
        tracing::error!("{summary} last time: {detail}");
        let log_path = crate::logging::session_log_path();
        show(
            &format!("{summary} last time"),
            &message_body(&detail, &log_path.display().to_string(), true),
        );
    }
    for (title, body) in previous.messages {
        tracing::warn!("Showing a message from the last launch: {title}");
        show(&format!("{title} last time"), &body);
    }
}

#[derive(Debug, Default, PartialEq)]
struct Previous {
    records: Vec<record::Record>,
    messages: Vec<(String, String)>,
}

/// Crash records and message requests that earlier launches left. A record
/// is only removed once its report is saved, so any record here still needs
/// one. Files of exited launches are removed unless their reporter is still
/// handling them; files of running instances, including this one, are left
/// alone.
fn collect_previous(directory: &Path, current: ProcessId) -> Previous {
    let mut previous = Previous::default();
    let Ok(entries) = fs::read_dir(directory) else {
        return previous;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(owner) = ProcessId::from_name(&name) else {
            continue;
        };
        if owner == current || owner.is_running() || reporter::still_reporting(directory, owner) {
            continue;
        }
        match path.extension().and_then(|extension| extension.to_str()) {
            Some(reporter::RECORD) => {
                if let Some((record, _)) = fs::read(&path).ok().and_then(|b| record::parse(&b)) {
                    previous.records.push(record);
                }
            }
            Some(reporter::REQUEST) => previous.messages.extend(reporter::take_request(&path)),
            _ => {}
        }
        let _ = fs::remove_file(&path);
    }
    previous
}

/// Report an error that is about to end startup, and pass it on unchanged.
pub fn fatal<E: std::fmt::Display>(error: E) -> E {
    report(&error.to_string());
    error
}

/// Record a fatal failure and tell the user without waiting for them.
pub fn report(detail: &str) {
    if let Some((summary, body)) = save(detail) {
        show(summary, &body);
    }
}

/// Write the failure to the log and the outbox now. Only the first failure
/// per process is saved, so a panic that turns into an abort is not reported
/// twice. Returns the message to show.
fn save(detail: &str) -> Option<(&'static str, String)> {
    if REPORTED.swap(true, Ordering::SeqCst) {
        return None;
    }
    let summary = summary(window_shown());
    let log_path = crate::logging::session_log_path();
    if let Err(error) = crate::logging::append_now(&log_line(summary, detail)) {
        eprintln!("Could not write the session log: {error}");
    }
    crate::telemetry::queue_now(&format!("{summary}: {detail}"));
    eprintln!("{summary}: {detail}");
    reporter::mark(reporter::DONE);
    Some((
        summary,
        message_body(detail, &log_path.display().to_string(), false),
    ))
}

/// The reporter shows the message; without one, this process does, after
/// everything else is saved and cleaned up.
fn show(title: &str, body: &str) {
    if dialogs_wanted() && !reporter::request_message(title, body) {
        show_message(title, body);
    }
}

/// Automation runs without a desktop to dismiss a dialog.
pub(crate) fn dialogs_wanted() -> bool {
    !(std::env::var("ETERNAL_HEADLESS").is_ok_and(|v| v.trim() == "1")
        || std::env::var(SIMULATE_ENV).is_ok_and(|v| v.trim().ends_with("-quiet")))
}

fn summary(window_shown: bool) -> &'static str {
    if window_shown {
        "EternalMonitor stopped unexpectedly"
    } else {
        "EternalMonitor could not start"
    }
}

fn log_line(summary: &str, detail: &str) -> String {
    timestamped("ERROR", &format!("{summary}: {detail}"))
}

/// A line the reporter adds to the session log about its own work.
#[cfg(windows)]
fn info_line(text: &str) -> String {
    timestamped(" INFO", text)
}

fn timestamped(level: &str, text: &str) -> String {
    use tracing_subscriber::fmt::time::FormatTime;
    let mut time = String::new();
    let _ = tracing_subscriber::fmt::time::SystemTime
        .format_time(&mut tracing_subscriber::fmt::format::Writer::new(&mut time));
    // The same shape as the tracing lines, so upload levels and searches match.
    format!("{time} {level} {}", text.replace(['\r', '\n'], " "))
}

fn message_body(detail: &str, log_path: &str, previous_launch: bool) -> String {
    let next = if previous_launch {
        ""
    } else {
        "Open EternalMonitor again. "
    };
    format!(
        "{detail}\n\nThe details are saved in the log file:\n{log_path}\n\n\
         {next}If this keeps happening, send that file through the feedback \
         form at eternalmonitor.dev/download.html."
    )
}

fn panic_detail(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_string());
    match info.location() {
        Some(location) => format!(
            "Internal error: {payload} ({}:{})",
            location.file(),
            location.line()
        ),
        None => format!("Internal error: {payload}"),
    }
}

fn native_detail(record: record::Record, module: Option<(&str, u64)>) -> String {
    // No thread: the reporter saw the process end without a crash record.
    match (record.thread, record.code) {
        (0, 0) => return "The process ended without a crash record".to_string(),
        (0, code) => return format!("Exit code 0x{code:08X} without a crash record"),
        _ => {}
    }
    let place = match module {
        Some((name, offset)) => format!("in {name}+0x{offset:X}"),
        None => format!("at 0x{:016X}", record.address),
    };
    format!(
        "Windows error 0x{:08X} {place} (thread {})",
        record.code, record.thread
    )
}

fn simulated() -> String {
    let requested = std::env::var(SIMULATE_ENV).unwrap_or_default();
    requested.trim().trim_end_matches("-quiet").to_string()
}

/// Fail on purpose where the window renderer starts, when [`SIMULATE_ENV`] asks.
pub(crate) fn simulate_renderer_failure() -> Result<(), String> {
    match simulated().as_str() {
        "error" => Err(format!("Simulated renderer failure from {SIMULATE_ENV}")),
        "panic" => panic!("Simulated renderer panic from {SIMULATE_ENV}"),
        "hang" => {
            std::thread::sleep(Duration::from_secs(30));
            Ok(())
        }
        #[cfg(windows)]
        "crash" => {
            raise_access_violation();
            Ok(())
        }
        #[cfg(windows)]
        "crash-thread" => {
            let _ = std::thread::spawn(raise_access_violation).join();
            Ok(())
        }
        #[cfg(windows)]
        "fail-fast" => std::process::abort(),
        #[cfg(windows)]
        "crash-heap" => {
            // As after a fault inside the allocator: any allocation now blocks.
            static LOCKED: AtomicBool = AtomicBool::new(false);
            #[link(name = "kernel32")]
            extern "system" {
                fn GetProcessHeap() -> isize;
                fn HeapLock(heap: isize) -> i32;
            }
            std::thread::spawn(|| unsafe {
                HeapLock(GetProcessHeap());
                LOCKED.store(true, Ordering::SeqCst);
                loop {
                    std::thread::park();
                }
            });
            while !LOCKED.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }
            raise_access_violation();
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Fail on purpose while the window is up, when [`SIMULATE_ENV`] asks.
pub(crate) fn simulate_window_failure() {
    let later = SHOWN_AT
        .get()
        .is_some_and(|shown| shown.elapsed() > Duration::from_secs(15));
    match simulated().as_str() {
        "panic-later" if later => {
            panic!("Simulated panic after the window appeared, from {SIMULATE_ENV}")
        }
        #[cfg(windows)]
        "fail-fast-later" if later => std::process::abort(),
        _ => {}
    }
}

/// Count surface errors for the first two seconds of frames, when
/// [`SIMULATE_ENV`] asks.
pub(crate) fn simulate_surface_error(failures: &std::sync::atomic::AtomicU64) {
    static FIRST_FRAME: OnceLock<Instant> = OnceLock::new();
    if simulated() == "surface-error"
        && FIRST_FRAME.get_or_init(Instant::now).elapsed() < Duration::from_secs(2)
    {
        failures.fetch_add(1, Ordering::SeqCst);
    }
}

/// An access violation, as a faulting graphics driver raises.
#[cfg(windows)]
fn raise_access_violation() {
    unsafe { windows::Win32::System::Diagnostics::Debug::RaiseException(0xC000_0005, 0, None) };
}

/// Only when the reporter is not running. Windows draws a service-notification
/// box itself; a box owned by a process whose GUI thread is stuck would not
/// respond to OK.
#[cfg(windows)]
fn show_message(title: &str, body: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SERVICE_NOTIFICATION,
    };
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(body),
            &HSTRING::from(title),
            MB_OK | MB_ICONERROR | MB_SERVICE_NOTIFICATION,
        );
    }
}

#[cfg(not(windows))]
fn show_message(_title: &str, _body: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_distinguishes_a_failed_start_from_a_later_stop() {
        assert_eq!(summary(false), "EternalMonitor could not start");
        assert_eq!(summary(true), "EternalMonitor stopped unexpectedly");
    }

    #[test]
    fn log_line_is_one_error_line_the_upload_classifies() {
        let line = log_line(summary(false), "first\r\nsecond");
        assert!(!line.contains('\n') && !line.contains('\r'));
        assert!(line.contains(" ERROR EternalMonitor could not start: first  second"));
        // RFC 3339 UTC, as the tracing lines around it.
        assert!(line.split(' ').next().unwrap().ends_with('Z'));
    }

    #[test]
    fn message_names_the_error_and_the_log_file() {
        let log = r"C:\Users\a\AppData\Roaming\EternalMonitor\logs\eternal-host-session.log";
        let body = message_body("Windows error 0xC0000005 in igvk64.dll", log, false);
        assert!(body.starts_with("Windows error 0xC0000005 in igvk64.dll\n\n"));
        assert!(body.contains(log));
        assert!(body.contains("Open EternalMonitor again. If this keeps happening"));
        // Reported by the next launch, which is already open.
        let body = message_body("Windows error 0xC0000005", log, true);
        assert!(body.contains(log) && !body.contains("Open EternalMonitor again"));
    }

    #[test]
    fn native_detail_names_the_module_offset_or_the_address() {
        let record = record::Record {
            code: 0xC000_0005,
            address: 0x7FF8_1234_5678,
            thread: 6804,
            window_shown: false,
        };
        assert_eq!(
            native_detail(record, Some(("igvk64.dll", 0x1A2B3))),
            "Windows error 0xC0000005 in igvk64.dll+0x1A2B3 (thread 6804)"
        );
        assert_eq!(
            native_detail(record, None),
            "Windows error 0xC0000005 at 0x00007FF812345678 (thread 6804)"
        );
    }

    #[test]
    fn the_next_launch_takes_what_no_reporter_handled() {
        let directory = std::env::temp_dir().join(format!(
            "eternal-crash-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
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
            start: 7,
        };
        child.wait().unwrap();
        let me = ProcessId::current();
        let crashed = record::Record {
            code: 0xC000_0005,
            address: 0x1000,
            thread: 9,
            window_shown: true,
        };
        let mut bytes = [0u8; record::RECORD_LEN];
        record::format(crashed, &mut bytes);
        let write =
            |name: String, content: &[u8]| fs::write(directory.join(name), content).unwrap();
        write(format!("{exited}.rec"), &bytes);
        // A message the host asked for just before it ended, never shown.
        write(
            format!("{exited}.box"),
            b"EternalMonitor could not start\nbody",
        );
        // Acknowledged, so the process was released, but the reporter could
        // not save the report.
        let unsaved = ProcessId { start: 8, ..exited };
        write(
            format!("{unsaved}.rec"),
            &[&bytes[..], record::ACK].concat(),
        );
        let killed = ProcessId { start: 9, ..exited };
        write(format!("{killed}.rec"), b"");
        write(format!("{killed}.done"), b"");
        // A reporter, this test process standing in, still handles these.
        let reporting = ProcessId {
            start: 10,
            ..exited
        };
        write(format!("{reporting}.rec"), &bytes);
        write(format!("{reporting}.ready"), me.to_string().as_bytes());
        write(format!("{me}.rec"), &bytes);

        assert_eq!(
            collect_previous(&directory, me),
            Previous {
                records: vec![crashed, crashed],
                messages: vec![("EternalMonitor could not start".into(), "body".into())],
            }
        );
        let mut left: Vec<String> = fs::read_dir(&directory)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let mut expected = [
            format!("{reporting}.ready"),
            format!("{reporting}.rec"),
            format!("{me}.rec"),
        ];
        expected.sort();
        assert_eq!(left, expected);
        // Taken once.
        assert_eq!(collect_previous(&directory, me), Previous::default());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn panic_detail_carries_message_and_location() {
        use std::sync::{Arc, Mutex};
        let captured = Arc::new(Mutex::new(String::new()));
        let sink = captured.clone();
        // The hook is process-wide; leave other tests' panics to the old one.
        let me = std::thread::current().id();
        let previous: Arc<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync> =
            std::panic::take_hook().into();
        let fallback = previous.clone();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == me {
                *sink.lock().unwrap() = panic_detail(info);
            } else {
                fallback(info);
            }
        }));
        let _ = std::panic::catch_unwind(|| panic!("renderer {}", "lost"));
        std::panic::set_hook(Box::new(move |info| previous(info)));
        let detail = captured.lock().unwrap().clone();
        assert!(
            detail.starts_with("Internal error: renderer lost (host"),
            "{detail}"
        );
        assert!(detail.contains("mod.rs:"), "{detail}");
    }

    #[test]
    fn a_gui_panic_turns_the_virtual_display_off_before_the_message() {
        let steps = std::cell::RefCell::new(Vec::new());
        after_main_panic(
            || {
                steps.borrow_mut().push("saved");
                Some(("EternalMonitor stopped unexpectedly", "body".to_string()))
            },
            || steps.borrow_mut().push("virtual display off"),
            |title, _| {
                assert_eq!(title, "EternalMonitor stopped unexpectedly");
                steps.borrow_mut().push("message");
            },
        );
        assert_eq!(*steps.borrow(), ["saved", "virtual display off", "message"]);
        // An already reported failure still cleans up, without a second message.
        steps.borrow_mut().clear();
        after_main_panic(
            || None,
            || steps.borrow_mut().push("virtual display off"),
            |_, _| steps.borrow_mut().push("message"),
        );
        assert_eq!(*steps.borrow(), ["virtual display off"]);
    }

    #[test]
    fn simulation_is_off_unless_requested() {
        if std::env::var(SIMULATE_ENV).is_err() {
            assert!(simulate_renderer_failure().is_ok());
        }
    }
}
