//! Inject only into the focused test window created by Test-PenInput.ps1.
#[cfg(windows)]
fn main() {
    use eternal_host::input::{CaptureGeometry, InputRelay};
    use eternal_wire::v2::control::InputEvent;
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).unwrap() };
    tracing_subscriber::fmt().init();
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "mapping" || mode == "stroke" {
        return placement(&mode);
    }
    let args: Vec<i32> = std::env::args()
        .skip(1)
        .map(|s| s.parse().unwrap())
        .collect();
    assert_eq!(
        args.len(),
        5,
        "expected probe PID, left, top, width, height"
    );
    std::env::set_var("ETERNAL_INPUT_WINDOW_PID", args[0].to_string());
    std::env::set_var("ETERNAL_INPUT_RECORDER_LOG", "1");
    let output = CaptureGeometry {
        left: args[1],
        top: args[2],
        width: args[3] as u32,
        height: args[4] as u32,
    };
    let mut relay = InputRelay::default();
    assert!(relay.pen_available(), "native pen creation failed");
    let mut event = InputEvent {
        input_ver: 2,
        kind: 1,
        phase: 1,
        buttons: 0,
        event_id: 1,
        x_norm: 10000,
        y_norm: 32768,
        pressure_x1000: 0,
        scroll_dx: 0,
        scroll_dy: 0,
        keycode: 0,
        modifiers: 0,
        client_time_us: 0,
        tilt_x: -40,
        tilt_y: 30,
    };
    relay.relay(1, &event, output);
    std::thread::sleep(std::time::Duration::from_millis(30));
    for step in 1..=10 {
        event.event_id += 1;
        event.phase = if step == 1 { 0 } else { 1 };
        event.buttons = 1;
        event.x_norm = 10000 + step * 3000;
        event.pressure_x1000 = step * 100;
        event.tilt_x = if step < 6 { -40 } else { 40 };
        relay.relay(1, &event, output);
        std::thread::sleep(std::time::Duration::from_millis(30));
    }
    event.event_id += 1;
    event.phase = 2;
    relay.relay(1, &event, output);
    std::thread::sleep(std::time::Duration::from_millis(30));
    event.event_id += 1;
    event.phase = 0;
    relay.relay(1, &event, output);
    // An entire coalesced batch arrives without sleeps. Every sample must
    // survive Windows' injection timestamp resolution.
    for _ in 0..32 {
        event.event_id += 1;
        event.phase = 1;
        relay.relay(1, &event, output);
    }
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        relay.keep_pen_alive();
    }
    relay.reset(); // disconnect mid-stroke must deliver a cancelled pen-up
    std::thread::sleep(std::time::Duration::from_millis(100));
}

/// Placement checks for scripts/win/Test-PenMapping.ps1, which covers one
/// whole monitor with its probe window and passes that monitor's rect.
///
/// `mapping <pid> <left> <top> <width> <height> <nx,ny>...` taps the pen, then
/// a touch, at each normalized point. `stroke <pid> <left> <top> <width>
/// <height> <steps>` draws one horizontal stroke across the centre, one step
/// every 20 ms, while the script changes the display layout underneath it.
#[cfg(windows)]
fn placement(mode: &str) {
    use eternal_host::input::{CaptureGeometry, InputRelay, KIND_PENCIL, KIND_TOUCH};
    use eternal_wire::v2::control::InputEvent;
    use std::time::Duration;
    let args: Vec<String> = std::env::args().skip(2).collect();
    assert!(
        args.len() >= 6,
        "expected PID, left, top, width, height and points or steps"
    );
    let number = |index: usize| args[index].parse::<i32>().unwrap();
    std::env::set_var("ETERNAL_INPUT_WINDOW_PID", &args[0]);
    std::env::set_var("ETERNAL_INPUT_RECORDER_LOG", "1");
    let output = CaptureGeometry {
        left: number(1),
        top: number(2),
        width: number(3) as u32,
        height: number(4) as u32,
    };
    let mut relay = InputRelay::default();
    assert!(relay.pen_available(), "native pen creation failed");
    let mut event = InputEvent {
        input_ver: 2,
        kind: KIND_PENCIL,
        phase: 0,
        buttons: 1,
        event_id: 0,
        x_norm: 0,
        y_norm: 0,
        pressure_x1000: 500,
        scroll_dx: 0,
        scroll_dy: 0,
        keycode: 0,
        modifiers: 0,
        client_time_us: 0,
        tilt_x: 0,
        tilt_y: 0,
    };
    let send = |relay: &mut InputRelay, event: &mut InputEvent, phase: u8, wait_ms: u64| {
        event.event_id += 1;
        event.phase = phase;
        relay.relay(1, event, output);
        std::thread::sleep(Duration::from_millis(wait_ms));
    };
    if mode == "mapping" {
        let points: Vec<(u16, u16)> = args[5..]
            .iter()
            .map(|point| {
                let (x, y) = point.split_once(',').expect("points are nx,ny");
                (x.parse().unwrap(), y.parse().unwrap())
            })
            .collect();
        for &(x, y) in &points {
            (event.x_norm, event.y_norm) = (x, y);
            send(&mut relay, &mut event, 0, 60);
            send(&mut relay, &mut event, 1, 60);
            send(&mut relay, &mut event, 2, 150);
        }
        event.input_ver = 1;
        event.kind = KIND_TOUCH;
        event.pressure_x1000 = 0;
        for &(x, y) in &points {
            (event.x_norm, event.y_norm) = (x, y);
            send(&mut relay, &mut event, 0, 60);
            // A second click at the same spot within the double-click time
            // is still a separate button-down for the probe.
            send(&mut relay, &mut event, 2, 600);
        }
    } else {
        let steps = number(5).max(2) as u16;
        event.y_norm = 32_768;
        for step in 0..steps {
            event.x_norm = 6_000 + step * (54_000 / (steps - 1));
            send(&mut relay, &mut event, if step == 0 { 0 } else { 1 }, 20);
        }
        send(&mut relay, &mut event, 2, 150);
    }
    relay.reset();
    std::thread::sleep(Duration::from_millis(100));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Run scripts/win/Test-PenInput.ps1 on an interactive Windows desktop.");
}
