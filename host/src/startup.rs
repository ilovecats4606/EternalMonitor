//! A failure that ends the host must never look like a console window that
//! flashes and closes (issue #59). Every fatal path, whether an error returned
//! during startup, a panic on the GUI thread or a native crash inside a
//! driver, writes the error to the session log and the upload outbox
//! synchronously, then shows it in a message box with the log path. The
//! asynchronous log and upload workers may never run again once the process
//! is going down, so neither path relies on them.

use std::sync::atomic::{AtomicBool, Ordering};

static REPORTED: AtomicBool = AtomicBool::new(false);
static WINDOW_SHOWN: AtomicBool = AtomicBool::new(false);

/// Test hook for the failure paths. Set `ETERNAL_SIMULATE_STARTUP_FAILURE` to
/// `error`, `panic` or `crash` (a native access violation, Windows only) to fail
/// while the window renderer starts. A `-quiet` suffix skips the message box,
/// which automation cannot dismiss.
pub const SIMULATE_ENV: &str = "ETERNAL_SIMULATE_STARTUP_FAILURE";

/// Called once the window has painted, so later failures read as a stop, not a
/// failed start.
pub fn mark_window_shown() {
    WINDOW_SHOWN.store(true, Ordering::Relaxed);
}

/// Panics on the main thread end the GUI and the process; report those. Other
/// threads belong to the pipeline, which the supervisor restarts, so they keep
/// the plain log line. Native crashes on any thread end the process.
pub fn install_failure_hooks() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().name() == Some("main") {
            report(&panic_detail(info));
        } else {
            tracing::error!(panic = %info, backtrace = %std::backtrace::Backtrace::force_capture(), "Host panic");
        }
        crate::vdd::disable();
        default_hook(info);
    }));
    #[cfg(windows)]
    unsafe {
        windows::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(
            on_unhandled_exception,
        ));
    }
}

/// Report an error that is about to end startup, and pass it on unchanged.
pub fn fatal<E: std::fmt::Display>(error: E) -> E {
    report(&error.to_string());
    error
}

/// Record a fatal failure and tell the user. Only the first report per process
/// is shown, so a panic that turns into an abort is not reported twice.
pub fn report(detail: &str) {
    if REPORTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let summary = summary(WINDOW_SHOWN.load(Ordering::Relaxed));
    let log_path = crate::logging::session_log_path();
    if let Err(error) = crate::logging::append_now(&log_line(summary, detail)) {
        eprintln!("Could not write the session log: {error}");
    }
    crate::telemetry::queue_now(&format!("{summary}: {detail}"));
    eprintln!("{summary}: {detail}");
    // Automation runs without a desktop to dismiss a dialog.
    if std::env::var("ETERNAL_HEADLESS").is_ok_and(|v| v.trim() == "1")
        || std::env::var(SIMULATE_ENV).is_ok_and(|v| v.trim().ends_with("-quiet"))
    {
        return;
    }
    show_message(
        summary,
        &message_body(detail, &log_path.display().to_string()),
    );
}

fn summary(window_shown: bool) -> &'static str {
    if window_shown {
        "EternalMonitor stopped unexpectedly"
    } else {
        "EternalMonitor could not start"
    }
}

fn log_line(summary: &str, detail: &str) -> String {
    use tracing_subscriber::fmt::time::FormatTime;
    let mut time = String::new();
    let _ = tracing_subscriber::fmt::time::SystemTime
        .format_time(&mut tracing_subscriber::fmt::format::Writer::new(&mut time));
    // The same shape as the tracing lines, so upload levels and searches match.
    format!(
        "{time} ERROR {summary}: {}",
        detail.replace(['\r', '\n'], " ")
    )
}

fn message_body(detail: &str, log_path: &str) -> String {
    format!(
        "{detail}\n\nThe details are saved in the log file:\n{log_path}\n\n\
         Open EternalMonitor again. If this keeps happening, send that file \
         through the feedback form at eternalmonitor.dev/download.html."
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

/// Fail on purpose where the window renderer starts, when [`SIMULATE_ENV`] asks.
pub(crate) fn simulate_renderer_failure() -> Result<(), String> {
    let requested = std::env::var(SIMULATE_ENV).unwrap_or_default();
    match requested.trim().trim_end_matches("-quiet") {
        "error" => Err(format!("Simulated renderer failure from {SIMULATE_ENV}")),
        "panic" => panic!("Simulated renderer panic from {SIMULATE_ENV}"),
        #[cfg(windows)]
        "crash" => {
            // An access violation, as a faulting graphics driver raises.
            unsafe {
                windows::Win32::System::Diagnostics::Debug::RaiseException(0xC000_0005, 0, None)
            };
            Ok(())
        }
        _ => Ok(()),
    }
}

#[cfg(windows)]
fn show_message(title: &str, body: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SERVICE_NOTIFICATION,
    };
    // Windows draws a service-notification box itself, as it does its own
    // error popups. A box owned by this process stopped responding while
    // the GUI thread was blocked or crashing, so OK did nothing.
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

#[cfg(windows)]
unsafe extern "system" fn on_unhandled_exception(
    info: *const windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    let record = info.as_ref().and_then(|info| info.ExceptionRecord.as_ref());
    if let Some(record) = record {
        report(&native_detail(
            record.ExceptionCode.0 as u32,
            &module_name(record.ExceptionAddress),
        ));
    }
    // EXCEPTION_CONTINUE_SEARCH: let Windows end the process and record it.
    0
}

#[cfg(any(windows, test))]
fn native_detail(code: u32, module: &str) -> String {
    format!("Windows error 0x{code:08X} in {module}")
}

#[cfg(windows)]
fn module_name(address: *const core::ffi::c_void) -> String {
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    let mut module = HMODULE::default();
    let found = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::PCWSTR(address.cast()),
            &mut module,
        )
    };
    if found.is_err() {
        return format!("unknown module at {address:p}");
    }
    let mut path = [0u16; 260];
    let length = unsafe { GetModuleFileNameW(module, &mut path) } as usize;
    let path = String::from_utf16_lossy(&path[..length]);
    path.rsplit('\\').next().unwrap_or(&path).to_string()
}

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
        let body = message_body(
            "Windows error 0xC0000005 in igvk64.dll",
            r"C:\Users\a\AppData\Roaming\EternalMonitor\logs\eternal-host-session.log",
        );
        assert!(body.starts_with("Windows error 0xC0000005 in igvk64.dll\n\n"));
        assert!(body
            .contains(r"C:\Users\a\AppData\Roaming\EternalMonitor\logs\eternal-host-session.log"));
        assert!(body.contains("Open EternalMonitor again."));
    }

    #[test]
    fn native_detail_formats_the_exception_code_in_hex() {
        assert_eq!(
            native_detail(0xC000_0005, "igvk64.dll"),
            "Windows error 0xC0000005 in igvk64.dll"
        );
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
        assert!(detail.contains("startup.rs:"), "{detail}");
    }

    #[test]
    fn simulation_is_off_unless_requested() {
        if std::env::var(SIMULATE_ENV).is_err() {
            assert!(simulate_renderer_failure().is_ok());
        }
    }
}
