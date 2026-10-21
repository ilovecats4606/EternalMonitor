//! The synthetic pen device behind `PenContact`: created at startup,
//! recreated to end a pointer Windows may still hold, and asked for again
//! after Windows refused to create one. Portable, so tests can make Windows
//! refuse; `windows_inject.rs` supplies the Win32 calls.

use std::time::{Duration, Instant};

use tracing::{info, warn};

use super::pen_contact::{Outcome, PenContact, PenTarget, PointerKind};
use super::{pen_to_mouse, CaptureGeometry, Injection, PenPhase, PenSample, VirtualScreen};

/// After Windows refuses a device, ask again at most this often.
const RETRY_INTERVAL: Duration = Duration::from_secs(1);

/// The calls `PenDevice` makes into Windows.
pub trait PenApi {
    type Handle: Copy + std::fmt::Debug;
    type Error: std::fmt::Display;
    fn create(&mut self) -> Result<Self::Handle, Self::Error>;
    fn destroy(&mut self, handle: Self::Handle);
    /// The virtual screen that frame locations are measured on now.
    fn screen(&self) -> VirtualScreen;
    /// Inject one frame. `output` is the capture rect, for diagnostics.
    fn inject(
        &mut self,
        handle: Self::Handle,
        frame: PenSample,
        from: PointerKind,
        screen: VirtualScreen,
        output: Option<CaptureGeometry>,
    ) -> Outcome;
}

/// One device per relay, retained across samples and destroyed at shutdown.
#[derive(Debug)]
pub struct PenDevice<A: PenApi> {
    slot: Slot<A>,
    /// What Windows believes about the pen; chooses every frame.
    contact: PenContact,
    /// Whether the mouse fallback holds the left button for a stroke.
    mouse_pressed: bool,
}

#[derive(Debug)]
struct Slot<A: PenApi> {
    api: A,
    handle: Option<A::Handle>,
    /// When Windows last refused to create the device, while it is missing.
    failed_at: Option<Instant>,
}

impl<A: PenApi> Slot<A> {
    /// Log the first refusal of a streak and the recovery, not every retry.
    fn create(&mut self, now: Instant) {
        match self.api.create() {
            Ok(handle) => {
                if self.failed_at.take().is_some() {
                    info!("Native Windows pen input is available again");
                }
                self.handle = Some(handle);
            }
            Err(error) => {
                if self.failed_at.is_none() {
                    warn!(%error, "Native Windows pen input unavailable; Pencil uses the mouse");
                }
                self.failed_at = Some(now);
            }
        }
    }

    fn destroy(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.api.destroy(handle);
        }
    }
}

/// The device as `PenContact` sees it for one batch.
struct Target<'a, A: PenApi> {
    slot: &'a mut Slot<A>,
    screen: VirtualScreen,
    output: Option<CaptureGeometry>,
}

impl<A: PenApi> PenTarget for Target<'_, A> {
    fn screen(&self) -> VirtualScreen {
        self.screen
    }

    fn inject(&mut self, frame: PenSample, from: PointerKind) -> Outcome {
        match self.slot.handle {
            Some(handle) => self
                .slot
                .api
                .inject(handle, frame, from, self.screen, self.output),
            None => Outcome::Rejected,
        }
    }

    fn available(&self) -> bool {
        self.slot.handle.is_some()
    }

    fn recreate(&mut self) {
        self.slot.destroy();
        self.slot.create(Instant::now());
        info!(
            available = self.slot.handle.is_some(),
            "Recreated the native pen device to end a pointer Windows may still hold"
        );
    }
}

impl<A: PenApi> PenDevice<A> {
    pub fn new(api: A) -> Self {
        let mut slot = Slot {
            api,
            handle: None,
            failed_at: None,
        };
        slot.create(Instant::now());
        Self {
            slot,
            contact: PenContact::default(),
            mouse_pressed: false,
        }
    }

    pub fn available(&self) -> bool {
        self.slot.handle.is_some()
    }

    /// Where no stroke is in progress (a pen down or a hover), ask again for
    /// a device Windows refused, at most once per `RETRY_INTERVAL`.
    pub fn revive(&mut self, now: Instant) {
        let waiting = self
            .slot
            .failed_at
            .is_some_and(|at| now.saturating_duration_since(at) < RETRY_INTERVAL);
        if self.available() || waiting {
            return;
        }
        self.slot.create(now);
        // A new device holds no pointer.
        self.contact = PenContact::default();
    }

    /// Deliver one pen sample to this device, or return mouse input for it
    /// while the device is missing. A missing device is asked for again
    /// where a stroke or hover starts; the relay has ended any earlier
    /// stroke there, so the mouse holds no button for it.
    ///
    /// Every sample goes to exactly one of the two, in order. Delivering one
    /// can drain samples that were waiting for Windows, and ending an old
    /// pointer on the way can lose the device. `PenContact` then stops, and
    /// every sample it has not delivered, this one included, continues on
    /// the mouse.
    pub fn deliver(
        &mut self,
        sample: PenSample,
        screen: VirtualScreen,
        output: Option<CaptureGeometry>,
        now: Instant,
    ) -> Vec<Injection> {
        if matches!(sample.phase, PenPhase::Down | PenPhase::Hover) {
            self.revive(now);
        }
        let mut mouse = Vec::new();
        if self.available() {
            let mut target = Target {
                slot: &mut self.slot,
                screen,
                output,
            };
            self.contact.deliver(sample, output, &mut target);
            self.fall_back(output, screen, &mut mouse);
        } else {
            self.mouse_for(sample, screen, &mut mouse);
        }
        mouse
    }

    /// The session ended or was taken over; see `PenContact::reset`.
    pub fn reset(&mut self) {
        if !self.available() {
            return;
        }
        let mut target = Target {
            screen: self.slot.api.screen(),
            slot: &mut self.slot,
            output: None,
        };
        self.contact.reset(&mut target);
    }

    /// Deliver samples Windows was not ready for, mapped onto `output`, the
    /// current capture rect. Called on every refresh tick. Returns mouse
    /// input for those that find the device gone, as `deliver` does.
    pub fn flush(&mut self, output: Option<CaptureGeometry>) -> Vec<Injection> {
        let mut mouse = Vec::new();
        if !self.available() || self.contact.pending().is_none() {
            return mouse;
        }
        let screen = self.slot.api.screen();
        let mut target = Target {
            slot: &mut self.slot,
            screen,
            output,
        };
        self.contact.flush(output, &mut target);
        self.fall_back(output, screen, &mut mouse);
        mouse
    }

    /// If the device is gone, send the samples `PenContact` could not
    /// deliver to the mouse, oldest first.
    fn fall_back(
        &mut self,
        output: Option<CaptureGeometry>,
        screen: VirtualScreen,
        mouse: &mut Vec<Injection>,
    ) {
        if self.available() {
            return;
        }
        for sample in self.contact.take_waiting(output) {
            self.mouse_for(sample, screen, mouse);
        }
    }

    /// The mouse equivalent of `sample`, given whether the mouse already
    /// holds the button. A stroke whose native contact ended with the device
    /// is pressed again at its next sample, and an end releases only a
    /// button the mouse holds, so the mouse is never pressed twice or
    /// released without a press.
    fn mouse_for(&mut self, sample: PenSample, screen: VirtualScreen, mouse: &mut Vec<Injection>) {
        let phase = match (sample.phase, self.mouse_pressed) {
            (PenPhase::Down | PenPhase::Move, false) => PenPhase::Down,
            (PenPhase::Down | PenPhase::Move, true) => PenPhase::Move,
            (PenPhase::Hover, false) => PenPhase::Hover,
            (PenPhase::Up | PenPhase::Cancel | PenPhase::Leave | PenPhase::Hover, true) => {
                PenPhase::Up
            }
            (PenPhase::Up | PenPhase::Cancel | PenPhase::Leave, false) => return,
        };
        self.mouse_pressed = matches!(phase, PenPhase::Down | PenPhase::Move);
        mouse.extend(pen_to_mouse(PenSample { phase, ..sample }, screen));
    }
}

impl<A: PenApi> Drop for PenDevice<A> {
    fn drop(&mut self) {
        self.slot.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    const SCREEN: VirtualScreen = VirtualScreen {
        left: 0,
        top: 0,
        width: 1920,
        height: 1080,
    };

    /// Windows creating devices as scripted (true once the script runs
    /// out), and answering frames on a live device with `answer`.
    #[derive(Debug)]
    struct FakeApi {
        creations: VecDeque<bool>,
        created: usize,
        destroyed: Vec<usize>,
        answer: Outcome,
        frames: Vec<(usize, PenPhase)>,
    }

    impl FakeApi {
        fn new(creations: &[bool]) -> Self {
            Self {
                creations: creations.iter().copied().collect(),
                created: 0,
                destroyed: Vec::new(),
                answer: Outcome::Injected,
                frames: Vec::new(),
            }
        }
    }

    impl PenApi for FakeApi {
        type Handle = usize;
        type Error = &'static str;

        fn create(&mut self) -> Result<usize, &'static str> {
            self.created += 1;
            if self.creations.pop_front().unwrap_or(true) {
                Ok(self.created)
            } else {
                Err("refused")
            }
        }

        fn destroy(&mut self, handle: usize) {
            self.destroyed.push(handle);
        }

        fn screen(&self) -> VirtualScreen {
            SCREEN
        }

        fn inject(
            &mut self,
            handle: usize,
            frame: PenSample,
            _: PointerKind,
            _: VirtualScreen,
            _: Option<CaptureGeometry>,
        ) -> Outcome {
            assert!(!self.destroyed.contains(&handle), "used a destroyed device");
            self.frames.push((handle, frame.phase));
            self.answer
        }
    }

    fn pen(phase: PenPhase) -> Injection {
        Injection::Pen(PenSample {
            phase,
            x: 100,
            y: 100,
            x_norm: 6_000,
            y_norm: 6_000,
            pressure: if matches!(phase, PenPhase::Down | PenPhase::Move) {
                512
            } else {
                0
            },
            tilt_x: 0,
            tilt_y: 0,
        })
    }

    /// Deliver a batch as `windows_inject::inject` does; returns what went
    /// to the mouse.
    fn send(device: &mut PenDevice<FakeApi>, phases: &[PenPhase], now: Instant) -> Vec<Injection> {
        let mut mouse = Vec::new();
        for &phase in phases {
            let Injection::Pen(sample) = pen(phase) else {
                unreachable!()
            };
            mouse.extend(device.deliver(sample, SCREEN, None, now));
        }
        mouse
    }

    /// Lose the device: a stuck frame makes the session reset recreate it,
    /// and Windows refuses the new one.
    fn lose_device(device: &mut PenDevice<FakeApi>) {
        send(device, &[PenPhase::Down], Instant::now());
        device.slot.api.answer = Outcome::NotReady;
        send(device, &[PenPhase::Move], Instant::now());
        device.reset();
        device.slot.api.answer = Outcome::Injected;
        assert!(!device.available());
    }

    /// Everything logged at info or above while `f` runs.
    fn logged(f: impl FnOnce()) -> String {
        use std::sync::{Arc, Mutex};
        #[derive(Clone, Default)]
        struct Buffer(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for Buffer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let text = buffer.0.lock().unwrap().clone();
        String::from_utf8(text).unwrap()
    }

    #[test]
    fn a_failed_recreation_is_retried_at_the_next_stroke() {
        // The reviewer's case: creation fails once, then would succeed.
        let mut device = PenDevice::new(FakeApi::new(&[true, false]));
        lose_device(&mut device);
        let lost = Instant::now();
        assert_eq!(device.slot.api.created, 2);
        // Refresh ticks and resets do not ask Windows; samples go to the
        // mouse until a stroke starts, and a move presses for the stroke the
        // lost device ended.
        for _ in 0..100 {
            assert!(device.flush(None).is_empty());
            device.reset();
        }
        let (x, y) = super::super::desktop_pixel_to_abs(100, 100, SCREEN);
        assert_eq!(
            send(&mut device, &[PenPhase::Move, PenPhase::Up], lost),
            [
                Injection::MoveAbs { x, y },
                Injection::LeftDown { x, y },
                Injection::MoveAbs { x, y },
                Injection::LeftUp { x, y },
            ]
        );
        assert_eq!(device.slot.api.created, 2);
        // A stroke within a second of the refusal still uses the mouse.
        let soon = lost + Duration::from_millis(500);
        assert_eq!(
            send(&mut device, &[PenPhase::Down], soon),
            [Injection::MoveAbs { x, y }, Injection::LeftDown { x, y }]
        );
        send(&mut device, &[PenPhase::Up], soon);
        assert_eq!(device.slot.api.created, 2);
        // The next stroke after that gets a new device and draws natively.
        device.slot.api.frames.clear();
        let later = lost + RETRY_INTERVAL + Duration::from_millis(100);
        let strokes = [PenPhase::Down, PenPhase::Move, PenPhase::Up];
        assert!(send(&mut device, &strokes, later).is_empty());
        assert_eq!(device.slot.api.created, 3);
        assert_eq!(
            device.slot.api.frames,
            [(3, PenPhase::Down), (3, PenPhase::Move), (3, PenPhase::Up)]
        );
        assert_eq!(device.slot.api.destroyed, [1]);
    }

    #[test]
    fn a_persistent_refusal_uses_the_mouse_with_bounded_retries_and_one_log_line() {
        let start = Instant::now();
        let (x, y) = super::super::desktop_pixel_to_abs(100, 100, SCREEN);
        let mut device = None;
        let refused = logged(|| {
            device = Some(PenDevice::new(FakeApi::new(&[false; 200])));
            let device = device.as_mut().unwrap();
            // Ten strokes and hovers a second for ten seconds.
            for tenth in 1..=100 {
                let now = start + Duration::from_millis(100 * tenth);
                let phases = [
                    PenPhase::Hover,
                    PenPhase::Down,
                    PenPhase::Move,
                    PenPhase::Up,
                ];
                let mouse = send(device, &phases, now);
                assert!(mouse.contains(&Injection::LeftDown { x, y }));
            }
        });
        let mut device = device.unwrap();
        // The startup attempt, then at most one a second.
        assert!(device.slot.api.created <= 11, "{}", device.slot.api.created);
        assert!(device.slot.api.frames.is_empty());
        assert_eq!(
            refused.matches("pen input unavailable").count(),
            1,
            "{refused}"
        );
        // Recovery is logged once, and a new streak logs again once.
        device.slot.api.creations = [true].into();
        let later = start + Duration::from_secs(20);
        let recovered = logged(|| {
            assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], later).is_empty());
        });
        assert_eq!(
            recovered.matches("available again").count(),
            1,
            "{recovered}"
        );
        assert_eq!(recovered.matches("unavailable").count(), 0, "{recovered}");
        device.slot.api.creations = [false; 50].into();
        let again = logged(|| {
            lose_device(&mut device);
            for second in 1..=5 {
                send(
                    &mut device,
                    &[PenPhase::Down],
                    later + Duration::from_secs(second),
                );
            }
        });
        assert_eq!(again.matches("pen input unavailable").count(), 1, "{again}");
    }

    #[test]
    fn a_device_is_never_asked_for_in_the_middle_of_a_stroke() {
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let soon = Instant::now();
        let later = soon + RETRY_INTERVAL * 2;
        // A mouse stroke starts within a second of the refusal.
        send(&mut device, &[PenPhase::Down], soon);
        // Moves, ups, cancels and leaves never create a device.
        let ends = [
            PenPhase::Move,
            PenPhase::Up,
            PenPhase::Cancel,
            PenPhase::Leave,
            PenPhase::Move,
        ];
        send(&mut device, &ends, later);
        assert_eq!(device.slot.api.created, 1);
        // A down does; a cancel before it in the same batch still goes to
        // the mouse, releasing the button the mouse stroke held.
        let mouse = send(&mut device, &[PenPhase::Cancel, PenPhase::Down], later);
        assert!(matches!(
            mouse.as_slice(),
            [Injection::MoveAbs { .. }, Injection::LeftUp { .. }]
        ));
        assert_eq!(device.slot.api.created, 2);
        assert_eq!(device.slot.api.frames, [(2, PenPhase::Down)]);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_session_started_without_the_device_draws_with_the_mouse_then_recovers() {
        use super::super::{recorder, InputRelay, KIND_PENCIL, PHASE_BEGAN, PHASE_ENDED};
        use eternal_wire::v2::control::InputEvent;
        let output = CaptureGeometry {
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
        };
        // A v2 Drawing mode stroke from `session`, as the relay resolves it.
        let mut relay = InputRelay::default();
        let mut stroke = |session, first_id| {
            for (id, phase) in [(first_id, PHASE_BEGAN), (first_id + 1, PHASE_ENDED)] {
                let event = InputEvent {
                    input_ver: 2,
                    kind: KIND_PENCIL,
                    phase,
                    buttons: 1,
                    event_id: id,
                    x_norm: 6_000,
                    y_norm: 6_000,
                    pressure_x1000: 500,
                    scroll_dx: 0,
                    scroll_dy: 0,
                    keycode: 0,
                    modifiers: 0,
                    client_time_us: 0,
                    tilt_x: 10,
                    tilt_y: 0,
                };
                relay.relay(session, &event, output);
            }
            recorder::take()
        };
        // Deliver as `windows_inject::inject` does.
        let deliver = |device: &mut PenDevice<FakeApi>, batch: &[Injection], now| {
            let mut mouse = Vec::new();
            for &injection in batch {
                let Injection::Pen(sample) = injection else {
                    panic!("expected only pen input, got {injection:?}");
                };
                mouse.extend(device.deliver(sample, SCREEN, Some(output), now));
            }
            mouse
        };
        let mut device = PenDevice::new(FakeApi::new(&[true, false]));
        lose_device(&mut device);
        let lost = Instant::now();
        // A new session starts while the device is missing; its reset does
        // not ask Windows. Its first stroke, within a second of the refusal,
        // is injected as mouse input.
        device.reset();
        let first = stroke(2, 1);
        assert!(matches!(
            first.as_slice(),
            [
                Injection::Pen(PenSample {
                    phase: PenPhase::Down,
                    ..
                }),
                Injection::Pen(PenSample {
                    phase: PenPhase::Up,
                    ..
                })
            ]
        ));
        let Injection::Pen(down) = first[0] else {
            unreachable!()
        };
        let (x, y) = super::super::desktop_pixel_to_abs(down.x, down.y, SCREEN);
        assert_eq!(
            deliver(&mut device, &first, lost + Duration::from_millis(200)),
            [
                Injection::MoveAbs { x, y },
                Injection::LeftDown { x, y },
                Injection::MoveAbs { x, y },
                Injection::LeftUp { x, y },
            ]
        );
        assert_eq!(device.slot.api.created, 2);
        // Its next stroke asks again, gets a device and draws natively.
        device.slot.api.frames.clear();
        let second = stroke(2, 3);
        assert!(deliver(&mut device, &second, lost + RETRY_INTERVAL * 2).is_empty());
        assert_eq!(device.slot.api.created, 3);
        assert_eq!(
            device.slot.api.frames,
            [(3, PenPhase::Down), (3, PenPhase::Up)]
        );
    }

    #[test]
    fn a_stroke_that_loses_the_device_at_its_cancel_still_presses_the_mouse() {
        // The reviewer's case: the previous up was lost, so the relay sends
        // Cancel, Down. Windows refuses the cancel, then refuses to recreate
        // the device that would end the old contact. Without the relay's
        // cancel, PenContact ends the old contact itself, inside the down.
        let (x, y) = super::super::desktop_pixel_to_abs(100, 100, SCREEN);
        for ends_first in [true, false] {
            let mut device = PenDevice::new(FakeApi::new(&[true, false]));
            let now = Instant::now();
            send(&mut device, &[PenPhase::Down], now);
            device.slot.api.answer = Outcome::Rejected;
            let mut strokes = vec![PenPhase::Down, PenPhase::Move, PenPhase::Up];
            if ends_first {
                strokes.insert(0, PenPhase::Cancel);
            }
            let mouse = send(&mut device, &strokes, now);
            assert!(!device.available());
            assert_eq!(device.slot.api.created, 2);
            assert_eq!(
                mouse,
                [
                    Injection::MoveAbs { x, y },
                    Injection::LeftDown { x, y },
                    Injection::MoveAbs { x, y },
                    Injection::MoveAbs { x, y },
                    Injection::LeftUp { x, y },
                ],
                "cancel first: {ends_first}"
            );
        }
    }

    #[test]
    fn a_queued_down_survives_a_refused_recreation() {
        // Sol's case: an old contact is held; Windows is not ready for the
        // relay's cancel and the next down, then rejects the cancel and
        // refuses to recreate the device. The queued down, found waiting
        // when the device is lost by a later sample or by a refresh tick,
        // must still press the mouse for its stroke.
        let (x, y) = super::super::desktop_pixel_to_abs(100, 100, SCREEN);
        for flush_on_tick in [false, true] {
            let mut device = PenDevice::new(FakeApi::new(&[true, false]));
            let now = Instant::now();
            send(&mut device, &[PenPhase::Down], now);
            device.slot.api.answer = Outcome::NotReady;
            assert!(send(&mut device, &[PenPhase::Cancel, PenPhase::Down], now).is_empty());
            assert_eq!(device.contact.pending().unwrap().phase, PenPhase::Cancel);
            device.slot.api.answer = Outcome::Rejected;
            let mut mouse = Vec::new();
            if flush_on_tick {
                mouse.extend(tick(&mut device));
            }
            mouse.extend(send(&mut device, &[PenPhase::Move, PenPhase::Up], now));
            assert!(!device.available());
            assert_eq!(device.slot.api.created, 2);
            assert_eq!(
                mouse,
                [
                    Injection::MoveAbs { x, y },
                    Injection::LeftDown { x, y },
                    Injection::MoveAbs { x, y },
                    Injection::MoveAbs { x, y },
                    Injection::LeftUp { x, y },
                ],
                "flush on tick: {flush_on_tick}"
            );
        }
    }

    /// A refresh tick; returns what went to the mouse.
    fn tick<A: PenApi>(device: &mut PenDevice<A>) -> Vec<Injection> {
        device.flush(None)
    }

    /// What reached Windows or the mouse, in order, for the random streams.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Seen {
        /// A native frame Windows accepted, and the stroke it belongs to.
        Native(PenPhase, i32),
        /// A native frame Windows rejected, which ends its contact.
        Refused,
        /// The device was destroyed, ending its contact.
        Destroyed,
    }

    struct Rng(u64);

    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    /// Windows for the random streams: not ready for 3 frames in 10,
    /// rejecting 1 in 10 moves, cancels and leaves, and refusing half the
    /// devices, until `ready`. A down is never rejected, so every stroke
    /// can be drawn on one path or the other.
    struct RandomApi {
        rng: Rng,
        ready: bool,
        created: usize,
        seen: Vec<Seen>,
    }

    impl PenApi for RandomApi {
        type Handle = usize;
        type Error = &'static str;

        fn create(&mut self) -> Result<usize, &'static str> {
            self.created += 1;
            if self.ready || self.rng.below(2) == 0 {
                Ok(self.created)
            } else {
                Err("refused")
            }
        }

        fn destroy(&mut self, _: usize) {
            self.seen.push(Seen::Destroyed);
        }

        fn screen(&self) -> VirtualScreen {
            SCREEN
        }

        fn inject(
            &mut self,
            _: usize,
            frame: PenSample,
            _: PointerKind,
            _: VirtualScreen,
            _: Option<CaptureGeometry>,
        ) -> Outcome {
            let roll = if self.ready { 9 } else { self.rng.below(10) };
            let outcome = match (roll, frame.phase) {
                (0..=2, _) => Outcome::NotReady,
                (3, PenPhase::Move | PenPhase::Cancel | PenPhase::Leave) => Outcome::Rejected,
                _ => Outcome::Injected,
            };
            self.seen.push(match outcome {
                Outcome::Injected => Seen::Native(frame.phase, frame.y),
                Outcome::NotReady => return outcome,
                Outcome::Rejected => Seen::Refused,
            });
            outcome
        }
    }

    #[test]
    fn random_strokes_reach_windows_or_the_mouse_exactly_once() {
        // Relay-shaped streams (a lost up shows as the relay's cancel before
        // the next down or hover) with Windows not ready, rejecting frames
        // and refusing or granting devices at random, refresh ticks, and
        // session resets after which a stroke carries on. Each sample's y is
        // its stroke number; hovers use 0.
        let abs_y: Vec<u16> = (0..=60)
            .map(|y| super::super::desktop_pixel_to_abs(0, y, SCREEN).1)
            .collect();
        let stroke_of = |abs: u16| abs_y.iter().position(|&y| y == abs).unwrap() as i32;
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let (mut lost_streams, mut overflowed) = (0, 0);
        for stream in 0..2000 {
            let mut device = PenDevice::new(RandomApi {
                rng: Rng(rng.below(u64::MAX) | 1),
                ready: false,
                created: 0,
                seen: Vec::new(),
            });
            let mut now = Instant::now();
            let mut output: Vec<Result<Seen, Injection>> = Vec::new();
            let mut collect = |device: &mut PenDevice<RandomApi>, mouse: Vec<Injection>| {
                output.extend(device.slot.api.seen.drain(..).map(Ok));
                output.extend(mouse.into_iter().map(Err));
            };
            let (mut stroke, mut in_contact, mut hovering) = (0, false, false);
            let mut strokes = Vec::new();
            let (mut most_waiting, mut lost) = (0, false);
            // Strokes a reset may have dropped samples of, by design.
            let mut reset_before = 0;
            let sample = |phase, stroke: i32, rng: &mut Rng| PenSample {
                phase,
                x: rng.below(1920) as i32,
                y: stroke,
                x_norm: 0,
                y_norm: 0,
                pressure: if matches!(phase, PenPhase::Down | PenPhase::Move) {
                    512
                } else {
                    0
                },
                tilt_x: 0,
                tilt_y: 0,
            };
            for _ in 0..rng.below(40) {
                let mut batch = Vec::new();
                match (in_contact, rng.below(10)) {
                    (false, 0..=4) if stroke < 60 => {
                        stroke += 1;
                        strokes.push(stroke);
                        batch.push(sample(PenPhase::Down, stroke, &mut rng));
                        (in_contact, hovering) = (true, false);
                    }
                    (false, 0..=7) => {
                        batch.push(sample(PenPhase::Hover, 0, &mut rng));
                        hovering = true;
                    }
                    (false, _) if hovering => {
                        batch.push(sample(PenPhase::Leave, 0, &mut rng));
                        hovering = false;
                    }
                    (false, _) => {}
                    (true, 0..=5) => batch.push(sample(PenPhase::Move, stroke, &mut rng)),
                    (true, 6..=7) => {
                        batch.push(sample(PenPhase::Up, stroke, &mut rng));
                        in_contact = false;
                    }
                    (true, 8) if stroke < 60 => {
                        // The up was lost: the relay cancels before the next down.
                        batch.push(sample(PenPhase::Cancel, stroke, &mut rng));
                        stroke += 1;
                        strokes.push(stroke);
                        batch.push(sample(PenPhase::Down, stroke, &mut rng));
                    }
                    (true, _) => {
                        batch.push(sample(PenPhase::Cancel, stroke, &mut rng));
                        in_contact = false;
                    }
                }
                now += Duration::from_millis(rng.below(400));
                for sample in batch {
                    let mouse = device.deliver(sample, SCREEN, None, now);
                    collect(&mut device, mouse);
                    most_waiting = most_waiting.max(device.contact.waiting());
                    lost |= !device.available();
                }
                if rng.below(2) == 0 {
                    let mouse = tick(&mut device);
                    collect(&mut device, mouse);
                    lost |= !device.available();
                }
                if rng.below(25) == 0 {
                    // The session resets; the stroke in progress carries on.
                    device.reset();
                    collect(&mut device, Vec::new());
                    lost |= !device.available();
                    reset_before = stroke;
                }
            }
            // Windows becomes ready; the next tick delivers what waits.
            device.slot.api.ready = true;
            let mouse = tick(&mut device);
            collect(&mut device, mouse);
            // Replay what reached Windows and the mouse.
            let (mut native, mut mouse) = (None, None);
            let mut drawn = std::collections::BTreeSet::new();
            let mut mouse_downs = std::collections::BTreeSet::new();
            for seen in &output {
                match *seen {
                    Ok(Seen::Native(PenPhase::Down, s)) => {
                        native = Some(s);
                        drawn.insert(s);
                    }
                    Ok(Seen::Native(PenPhase::Up | PenPhase::Cancel, s)) => {
                        assert_eq!(
                            native,
                            Some(s),
                            "stream {stream}: native up without its down: {output:?}"
                        );
                        native = None;
                    }
                    Ok(Seen::Native(..)) => {}
                    Ok(Seen::Refused) => native = None,
                    Ok(Seen::Destroyed) => native = None,
                    Err(Injection::LeftDown { y, .. }) => {
                        let s = stroke_of(y);
                        assert_eq!(
                            mouse, None,
                            "stream {stream}: mouse pressed twice: {output:?}"
                        );
                        assert!(
                            mouse_downs.insert(s),
                            "stream {stream}: stroke {s} pressed twice: {output:?}"
                        );
                        mouse = Some(s);
                        drawn.insert(s);
                    }
                    Err(Injection::LeftUp { y, .. }) => {
                        assert_eq!(
                            mouse,
                            Some(stroke_of(y)),
                            "stream {stream}: mouse up without its down: {output:?}"
                        );
                        mouse = None;
                    }
                    Err(_) => {}
                }
            }
            // A full queue drops waiting samples by design; only then may a
            // stroke go undrawn.
            if most_waiting >= super::super::pen_contact::QUEUE_LIMIT {
                overflowed += 1;
            } else {
                for s in strokes.iter().filter(|&&s| s > reset_before) {
                    assert!(
                        drawn.contains(s),
                        "stream {stream}: stroke {s} was dropped: {output:?}"
                    );
                }
            }
            lost_streams += usize::from(lost);
            if !in_contact {
                assert_eq!(
                    (native, mouse),
                    (None, None),
                    "stream {stream}: left pressed: {output:?}"
                );
            }
        }
        // Many streams lose the device; few fill the queue.
        assert!(lost_streams > 200, "{lost_streams}");
        assert!(overflowed < 20, "{overflowed}");
    }
}
