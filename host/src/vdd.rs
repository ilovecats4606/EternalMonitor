//! Lifecycle control for the bundled third-party virtual display driver (VDD).
//!
//! The driver is left DISABLED by default so no phantom monitor exists when EternalMonitor
//! isn't using it. We enable it only while actively capturing the virtual extended display.
//!
//! Enabling/disabling a driver device requires admin, so the installer registers two
//! "run with highest privileges" scheduled tasks (one per direction). Triggering an
//! already-elevated, pre-authorized task does not raise a UAC prompt, so the non-elevated
//! host can flip the virtual display on and off seamlessly.

#[cfg(windows)]
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
#[cfg(windows)]
use tracing::{info, warn};

#[cfg(windows)]
static TASK_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
static GATE: parking_lot::Mutex<TaskGate> = parking_lot::Mutex::new(TaskGate::new());

/// Whether the installer-owned driver tasks can be used. Stable values the GUI
/// can explain; the extended display falls back to mirroring in both failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VddTaskHealth {
    Ok,
    /// Windows reports the scheduled task does not exist (0x80070002): the
    /// app was installed without the display driver, or the tasks were removed.
    TasksMissing,
    /// A driver task is still running from an earlier request. Disabling or
    /// enabling a display device can hang until Windows restarts.
    NotResponding,
}

/// After a task times out, requests fall back to mirroring at once for this
/// long instead of each one waiting out the same hung task again.
#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
const STUCK_COOLDOWN: Duration = Duration::from_secs(60);

#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
#[derive(Debug, PartialEq, Eq)]
enum Gate {
    /// The device is already in the requested state; nothing to run.
    AlreadyDone,
    Refuse,
    Run,
}

#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskOutcome {
    Done,
    TimedOut,
    Missing,
    Failed,
}

#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
impl TaskOutcome {
    fn from_stderr(stderr: &str) -> Self {
        if stderr.contains("timed out") {
            Self::TimedOut
        } else if stderr.contains("0x80070002") {
            Self::Missing
        } else {
            Self::Failed
        }
    }
}

/// Remembers what the driver tasks last did. Every idle pipeline start used to
/// run the Disable task again (two to three seconds of PowerShell each time),
/// and a hung task made every later request wait out its timeout.
#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
struct TaskGate {
    known_enabled: Option<bool>,
    health: VddTaskHealth,
    stuck_at: Option<Instant>,
}

#[cfg_attr(not(windows), allow(dead_code))] // Driver tasks exist only on Windows.
impl TaskGate {
    const fn new() -> Self {
        Self {
            known_enabled: None,
            health: VddTaskHealth::Ok,
            stuck_at: None,
        }
    }

    fn before(&self, enabled: bool, now: Instant) -> Gate {
        if self.known_enabled == Some(enabled) {
            return Gate::AlreadyDone;
        }
        match self.health {
            VddTaskHealth::TasksMissing => Gate::Refuse,
            VddTaskHealth::NotResponding
                if self
                    .stuck_at
                    .is_some_and(|at| now.duration_since(at) < STUCK_COOLDOWN) =>
            {
                Gate::Refuse
            }
            _ => Gate::Run,
        }
    }

    fn after(&mut self, enabled: bool, outcome: TaskOutcome, now: Instant) {
        self.known_enabled = None;
        match outcome {
            TaskOutcome::Done => {
                self.known_enabled = Some(enabled);
                self.health = VddTaskHealth::Ok;
                self.stuck_at = None;
            }
            TaskOutcome::TimedOut => {
                self.health = VddTaskHealth::NotResponding;
                self.stuck_at = Some(now);
            }
            TaskOutcome::Missing => self.health = VddTaskHealth::TasksMissing,
            TaskOutcome::Failed => {}
        }
    }
}

/// Current health of the driver tasks, for the GUI.
pub fn task_health() -> VddTaskHealth {
    GATE.lock().health
}

/// Scheduled task names — must match the ones the installer registers.
pub const TASK_ENABLE: &str = "EternalMonitor VDD Enable";
pub const TASK_DISABLE: &str = "EternalMonitor VDD Disable";

/// Trigger a pre-registered scheduled task by name. Returns true on success. Missing tasks
/// (e.g. a build installed without the VDD feature) just log a warning and return false —
/// the caller falls back to the primary display.
#[cfg(windows)]
fn run_task(task: &str, mut progress: impl FnMut()) -> TaskOutcome {
    use std::os::windows::process::CommandExt;
    let _guard = loop {
        if let Some(guard) = TASK_LOCK.try_lock_for(Duration::from_millis(50)) {
            break guard;
        }
        progress();
    };
    let started = Instant::now();
    let script = format!("& {{ {} }} -Task '{}'", include_str!("vdd_task.ps1"), task);
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
        .creation_flags(0x0800_0000)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            warn!(task, %error, "Failed to invoke the VDD task");
            return TaskOutcome::Failed;
        }
    };
    // vdd_task.ps1 waits up to 10 s for an earlier instance and 15 s for its own.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                progress();
                std::thread::sleep(Duration::from_millis(50));
            }
            result => {
                warn!(task, ?result, "VDD task runner failed or timed out");
                let _ = child.kill();
                let _ = child.wait();
                return TaskOutcome::TimedOut;
            }
        }
    }

    match child.wait_with_output() {
        Ok(out) if out.status.success() => {
            info!(
                task,
                took_ms = started.elapsed().as_millis() as u64,
                "Completed VDD scheduled task"
            );
            TaskOutcome::Done
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            warn!(
                task,
                code = ?out.status.code(),
                took_ms = started.elapsed().as_millis() as u64,
                stderr = %stderr.trim_end(),
                stdout = %String::from_utf8_lossy(&out.stdout).trim_end(),
                "VDD scheduled task failed"
            );
            TaskOutcome::from_stderr(&stderr)
        }
        Err(error) => {
            warn!(task, error = %error, "Failed to read the VDD task result");
            TaskOutcome::Failed
        }
    }
}

/// Complete one installer-owned driver action while capture reports startup progress.
pub(crate) fn set_enabled(enabled: bool, progress: impl FnMut()) -> bool {
    #[cfg(windows)]
    {
        match GATE.lock().before(enabled, Instant::now()) {
            Gate::AlreadyDone => return true,
            Gate::Refuse => {
                tracing::debug!(enabled, health = ?task_health(), "Skipping VDD task");
                return false;
            }
            Gate::Run => {}
        }
        let outcome = run_task(if enabled { TASK_ENABLE } else { TASK_DISABLE }, progress);
        GATE.lock().after(enabled, outcome, Instant::now());
        outcome == TaskOutcome::Done
    }
    #[cfg(not(windows))]
    {
        let _ = (enabled, progress);
        false
    }
}

/// Disable the virtual display device (best effort — failures are non-fatal).
pub fn disable() {
    set_enabled(false, || {});
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_already_off_is_not_disabled_again() {
        let mut gate = TaskGate::new();
        let now = Instant::now();
        // Unknown at launch: the first disable runs.
        assert_eq!(gate.before(false, now), Gate::Run);
        gate.after(false, TaskOutcome::Done, now);
        assert_eq!(gate.before(false, now), Gate::AlreadyDone);
        assert_eq!(gate.before(true, now), Gate::Run);
        gate.after(true, TaskOutcome::Done, now);
        assert_eq!(gate.before(true, now), Gate::AlreadyDone);
        assert_eq!(gate.before(false, now), Gate::Run);
    }

    #[test]
    fn a_hung_task_is_not_waited_out_again_until_the_cooldown_ends() {
        let mut gate = TaskGate::new();
        let now = Instant::now();
        gate.after(true, TaskOutcome::TimedOut, now);
        assert_eq!(gate.health, VddTaskHealth::NotResponding);
        assert_eq!(
            gate.before(true, now + Duration::from_secs(5)),
            Gate::Refuse
        );
        assert_eq!(
            gate.before(false, now + Duration::from_secs(5)),
            Gate::Refuse
        );
        assert_eq!(gate.before(true, now + STUCK_COOLDOWN), Gate::Run);
        gate.after(true, TaskOutcome::Done, now + STUCK_COOLDOWN);
        assert_eq!(gate.health, VddTaskHealth::Ok);
    }

    #[test]
    fn missing_tasks_stop_further_attempts_and_failures_forget_the_state() {
        let mut gate = TaskGate::new();
        let now = Instant::now();
        gate.after(false, TaskOutcome::Done, now);
        gate.after(true, TaskOutcome::Failed, now);
        // A failed task leaves the device state unknown, so disable runs again.
        assert_eq!(gate.before(false, now), Gate::Run);
        gate.after(false, TaskOutcome::Missing, now);
        assert_eq!(gate.before(true, now + STUCK_COOLDOWN * 10), Gate::Refuse);
    }

    #[test]
    fn task_errors_are_classified_from_the_script_output() {
        assert_eq!(
            TaskOutcome::from_stderr("Previous VDD task timed out: EternalMonitor VDD Disable"),
            TaskOutcome::TimedOut
        );
        assert_eq!(
            TaskOutcome::from_stderr(
                "Das System kann die angegebene Datei nicht finden. (Ausnahme von HRESULT: 0x80070002)"
            ),
            TaskOutcome::Missing
        );
        assert_eq!(
            TaskOutcome::from_stderr("VDD task failed: x, result=1"),
            TaskOutcome::Failed
        );
    }
}
