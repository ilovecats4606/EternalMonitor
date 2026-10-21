//! Windows mouse/keyboard and native Windows Ink pen injection.

use tracing::{debug, info, warn};
use windows::Win32::Foundation::{ERROR_NOT_READY, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Controls::{
    CreateSyntheticPointerDevice, DestroySyntheticPointerDevice, HSYNTHETICPOINTERDEVICE,
    POINTER_FEEDBACK_NONE, POINTER_TYPE_INFO, POINTER_TYPE_INFO_0,
};
use windows::Win32::UI::HiDpi::{
    AreDpiAwarenessContextsEqual, GetAwarenessFromDpiAwarenessContext,
    GetThreadDpiAwarenessContext, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DPI_AWARENESS_PER_MONITOR_AWARE,
    DPI_AWARENESS_SYSTEM_AWARE, DPI_AWARENESS_UNAWARE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY,
};
use windows::Win32::UI::Input::Pointer::{
    InjectSyntheticPointerInput, POINTER_FLAGS, POINTER_FLAG_CANCELED, POINTER_FLAG_DOWN,
    POINTER_FLAG_FIRSTBUTTON, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE, POINTER_FLAG_PRIMARY,
    POINTER_FLAG_UP, POINTER_FLAG_UPDATE, POINTER_INFO, POINTER_PEN_INFO,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowTextW,
    GetWindowThreadProcessId, PEN_MASK_PRESSURE, PEN_MASK_TILT_X, PEN_MASK_TILT_Y, PT_PEN,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::pen_contact::{
    pointer_flag, NotReadyRetry, Outcome, PenContact, PenTarget, PointerFrame, PointerKind,
};
use super::{CaptureGeometry, Injection, PenSample, VirtualScreen};

// Frames are built, and tested, without the SDK.
const _: () = {
    assert!(POINTER_FLAG_INRANGE.0 == pointer_flag::INRANGE);
    assert!(POINTER_FLAG_INCONTACT.0 == pointer_flag::INCONTACT);
    assert!(POINTER_FLAG_FIRSTBUTTON.0 == pointer_flag::FIRSTBUTTON);
    assert!(POINTER_FLAG_PRIMARY.0 == pointer_flag::PRIMARY);
    assert!(POINTER_FLAG_CANCELED.0 == pointer_flag::CANCELED);
    assert!(POINTER_FLAG_DOWN.0 == pointer_flag::DOWN);
    assert!(POINTER_FLAG_UPDATE.0 == pointer_flag::UPDATE);
    assert!(POINTER_FLAG_UP.0 == pointer_flag::UP);
};

/// One device per relay, retained across samples and destroyed at shutdown.
#[derive(Debug)]
pub struct PenDevice {
    device: Device,
    /// What Windows believes about the pen; chooses every frame.
    contact: PenContact,
}

#[derive(Debug)]
struct Device {
    handle: Option<HSYNTHETICPOINTERDEVICE>,
    /// Error codes already logged, so a repeated refusal logs once.
    warned: Vec<windows::core::HRESULT>,
    retry: NotReadyRetry,
}

// This is a process-owned User32 handle, not a pointer we dereference. Tokio
// may move the relay between threads; &mut self serializes every API call.
unsafe impl Send for PenDevice {}

impl Default for PenDevice {
    fn default() -> Self {
        Self {
            device: Device {
                handle: create_device(),
                warned: Vec::new(),
                retry: NotReadyRetry::default(),
            },
            contact: PenContact::default(),
        }
    }
}

fn create_device() -> Option<HSYNTHETICPOINTERDEVICE> {
    match unsafe { CreateSyntheticPointerDevice(PT_PEN, 1, POINTER_FEEDBACK_NONE) } {
        Ok(handle) => Some(handle),
        Err(error) => {
            warn!(%error, "Native Windows pen input unavailable");
            None
        }
    }
}

/// Where a frame was aimed, for the once-per-error warning.
#[derive(Clone, Copy)]
struct FrameContext {
    screen: VirtualScreen,
    output: Option<CaptureGeometry>,
}

/// The device as `PenContact` sees it for one batch.
struct Target<'a> {
    device: &'a mut Device,
    context: FrameContext,
}

impl PenTarget for Target<'_> {
    fn screen(&self) -> VirtualScreen {
        self.context.screen
    }

    fn inject(&mut self, frame: PenSample, from: PointerKind) -> Outcome {
        let Device {
            handle,
            warned,
            retry,
        } = self.device;
        match *handle {
            Some(handle) => inject_frame(handle, frame, from, self.context, warned, retry),
            None => Outcome::Rejected,
        }
    }

    fn recreate(&mut self) {
        if let Some(handle) = self.device.handle.take() {
            unsafe { DestroySyntheticPointerDevice(handle) };
        }
        self.device.handle = create_device();
        info!(
            available = self.device.handle.is_some(),
            "Recreated the native pen device to end a pointer Windows may still hold"
        );
    }
}

impl PenDevice {
    pub fn available(&self) -> bool {
        self.device.handle.is_some()
    }

    fn inject(&mut self, sample: PenSample, context: FrameContext) {
        if !self.available() {
            return;
        }
        let mut target = Target {
            device: &mut self.device,
            context,
        };
        self.contact.deliver(sample, context.output, &mut target);
    }

    /// The session ended or was taken over; see `PenContact::reset`.
    pub fn reset(&mut self) {
        if !self.available() {
            return;
        }
        let mut target = Target {
            device: &mut self.device,
            context: FrameContext {
                screen: virtual_screen(),
                output: None,
            },
        };
        self.contact.reset(&mut target);
    }

    /// Deliver samples Windows was not ready for, mapped onto `output`, the
    /// current capture rect. Called on every refresh tick.
    pub fn flush(&mut self, output: Option<CaptureGeometry>) {
        if !self.available() || self.contact.pending().is_none() {
            return;
        }
        let mut target = Target {
            device: &mut self.device,
            context: FrameContext {
                screen: virtual_screen(),
                output,
            },
        };
        self.contact.flush(output, &mut target);
    }
}

fn inject_frame(
    handle: HSYNTHETICPOINTERDEVICE,
    frame: PenSample,
    from: PointerKind,
    context: FrameContext,
    warned: &mut Vec<windows::core::HRESULT>,
    retry: &mut NotReadyRetry,
) -> Outcome {
    let screen = context.screen;
    let pointer = PointerFrame::new(frame, screen);
    let (x, y) = pointer.location;
    let info = POINTER_TYPE_INFO {
        r#type: PT_PEN,
        Anonymous: POINTER_TYPE_INFO_0 {
            penInfo: POINTER_PEN_INFO {
                pointerInfo: POINTER_INFO {
                    pointerType: PT_PEN,
                    pointerId: 1,
                    pointerFlags: POINTER_FLAGS(pointer.flags),
                    ptPixelLocation: POINT { x, y },
                    ..Default::default()
                },
                penMask: PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y,
                pressure: pointer.pressure,
                tiltX: pointer.tilt_x,
                tiltY: pointer.tilt_y,
                ..Default::default()
            },
        },
    };
    // Past the retry window, PenContact keeps the frame for the next tick.
    let not_ready = windows::core::HRESULT::from_win32(ERROR_NOT_READY.0);
    let result = retry.call(
        || unsafe { InjectSyntheticPointerInput(handle, &[info]) },
        |error| error.code() == not_ready,
    );
    let error = match result {
        Ok(()) => {
            super::log_injections(&[Injection::Pen(frame)]);
            return Outcome::Injected;
        }
        Err(error) => error,
    };
    if !warned.contains(&error.code()) {
        warned.push(error.code());
        // E_ACCESSDENIED: the secure desktop or an elevated window has focus;
        // SendInput is blocked there too, so there is no useful fallback.
        // Other codes point at the frame, its starting state or the geometry.
        let output = context.output.unwrap_or(CaptureGeometry {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
        });
        warn!(
            %error,
            phase = ?frame.phase,
            from = ?from,
            pressure = frame.pressure,
            desktop_x = frame.x,
            desktop_y = frame.y,
            location_x = x,
            location_y = y,
            virtual_left = screen.left,
            virtual_top = screen.top,
            virtual_width = screen.width,
            virtual_height = screen.height,
            output_left = output.left,
            output_top = output.top,
            output_width = output.width,
            output_height = output.height,
            dpi_awareness = dpi_awareness(),
            "Windows rejected native pen input"
        );
    }
    if error.code() == not_ready {
        Outcome::NotReady
    } else {
        Outcome::Rejected
    }
}

impl Drop for PenDevice {
    fn drop(&mut self) {
        if let Some(handle) = self.device.handle.take() {
            unsafe { DestroySyntheticPointerDevice(handle) };
        }
    }
}

/// The Windows virtual screen (all monitors' bounding box) for absolute
/// pointer mapping. Queried per batch — display topology can change under us.
pub fn virtual_screen() -> VirtualScreen {
    unsafe {
        VirtualScreen {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as u32,
        }
    }
}

fn keyboard_input(scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
    dx: i32,
    dy: i32,
    data: i32,
) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// The injecting thread's DPI awareness. Anything other than per-monitor
/// means Windows rescales the coordinates we pass.
pub fn dpi_awareness() -> &'static str {
    unsafe {
        let context = GetThreadDpiAwarenessContext();
        if AreDpiAwarenessContextsEqual(context, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
            .as_bool()
        {
            return "per_monitor_v2";
        }
        match GetAwarenessFromDpiAwarenessContext(context) {
            DPI_AWARENESS_PER_MONITOR_AWARE => "per_monitor",
            DPI_AWARENESS_SYSTEM_AWARE => "system",
            DPI_AWARENESS_UNAWARE => "unaware",
            _ => "unknown",
        }
    }
}

/// Desktop duplication, monitor rects, the virtual screen and every injected
/// coordinate must share one physical pixel space. Call before any window,
/// DXGI factory or GUI exists; a manifest or an earlier call wins otherwise.
pub fn become_per_monitor_dpi_aware() {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Execute a resolved injection batch. `output` is the captured display the
/// batch was mapped onto, for diagnostics.
pub fn inject(injections: &[Injection], pen: &mut PenDevice, output: Option<CaptureGeometry>) {
    if !probe_guard_allows(injections) {
        warn!("Input probe guard rejected an injection batch");
        return;
    }
    let screen = virtual_screen();
    let mut inputs: Vec<INPUT> = Vec::with_capacity(injections.len() * 2);
    for injection in injections {
        let input = match *injection {
            Injection::Pen(sample) => {
                // Flush keyboard releases first when a session is cancelled.
                if !inputs.is_empty() {
                    unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
                    inputs.clear();
                }
                pen.inject(sample, FrameContext { screen, output });
                continue;
            }
            Injection::KeyDown { scan, extended } | Injection::KeyUp { scan, extended } => {
                let mut flags = KEYEVENTF_SCANCODE;
                if extended {
                    flags |= KEYEVENTF_EXTENDEDKEY;
                }
                if matches!(injection, Injection::KeyUp { .. }) {
                    flags |= KEYEVENTF_KEYUP;
                }
                keyboard_input(scan, flags)
            }
            Injection::Unicode(unit) => {
                inputs.push(keyboard_input(unit, KEYEVENTF_UNICODE));
                keyboard_input(unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)
            }
            Injection::MoveAbs { x, y } => mouse_input(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::LeftDown { x, y } => mouse_input(
                MOUSEEVENTF_LEFTDOWN | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::LeftUp { x, y } => mouse_input(
                MOUSEEVENTF_LEFTUP | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::RightDown { x, y } => mouse_input(
                MOUSEEVENTF_RIGHTDOWN | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::RightUp { x, y } => mouse_input(
                MOUSEEVENTF_RIGHTUP | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::Wheel { delta } => mouse_input(MOUSEEVENTF_WHEEL, 0, 0, delta),
            Injection::MiddleDown { x, y } => mouse_input(
                MOUSEEVENTF_MIDDLEDOWN | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::MiddleUp { x, y } => mouse_input(
                MOUSEEVENTF_MIDDLEUP | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::HWheel { delta } => mouse_input(MOUSEEVENTF_HWHEEL, 0, 0, delta),
        };
        inputs.push(input);
    }
    if inputs.is_empty() {
        return;
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == inputs.len() as u32 {
        super::log_injections(injections);
    } else {
        debug!(
            sent,
            requested = inputs.len(),
            "SendInput injected fewer events than requested"
        );
    }
}

fn probe_guard_allows(injections: &[Injection]) -> bool {
    // An absent variable leaves the normal product input path unchanged. A
    // malformed value fails closed, as does focus moving away from the probe.
    let Ok(value) = std::env::var("ETERNAL_INPUT_WINDOW_PID") else {
        return true;
    };
    let expected_pid = value.parse::<u32>().unwrap_or(0);
    unsafe {
        let window = GetForegroundWindow();
        let mut foreground_pid = 0;
        GetWindowThreadProcessId(window, Some(&mut foreground_pid));
        let mut title = [0u16; 128];
        let length = GetWindowTextW(window, &mut title).max(0) as usize;
        if String::from_utf16_lossy(&title[..length]) != "EternalMonitor input probe" {
            return false;
        }
        let mut rect = RECT::default();
        let mut origin = POINT::default();
        let mut cursor = POINT::default();
        if GetClientRect(window, &mut rect).is_err()
            || !ClientToScreen(window, &mut origin).as_bool()
            || GetCursorPos(&mut cursor).is_err()
        {
            return false;
        }
        super::probe_guard::permits(
            expected_pid,
            foreground_pid,
            super::CaptureGeometry {
                left: origin.x,
                top: origin.y,
                width: (rect.right - rect.left).max(0) as u32,
                height: (rect.bottom - rect.top).max(0) as u32,
            },
            virtual_screen(),
            (cursor.x, cursor.y),
            injections,
        )
    }
}
