//! The synthetic pen device behind `PenContact`: created at startup,
//! recreated to end a pointer Windows may still hold, and asked for again
//! after Windows refused to create one. Portable, so tests can make Windows
//! refuse; `windows_inject.rs` supplies the Win32 calls.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use super::pen_contact::{Outcome, PenContact, PenTarget, PointerKind, INPUT_STALE};
use super::{desktop_pixel_to_abs, CaptureGeometry, Injection, PenPhase, PenSample, VirtualScreen};

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
    /// Send mouse input for the fallback. Returns how many of the events,
    /// from the first, Windows inserted.
    fn send_mouse(&mut self, inputs: &[Injection]) -> usize;
}

/// One device per relay, retained across samples and destroyed at shutdown.
#[derive(Debug)]
pub struct PenDevice<A: PenApi> {
    slot: Slot<A>,
    /// What Windows believes about the pen; chooses every frame.
    contact: PenContact,
    mouse: Mouse,
}

/// A fallback mouse event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseEvent {
    Move,
    Press,
    Release,
}

/// A mouse event waiting for `SendInput`: where it goes on the desktop, so
/// it is placed on the virtual screen current when it is sent, and when
/// the host received the sample it came from.
#[derive(Debug, Clone, Copy)]
struct Waiting {
    event: MouseEvent,
    at: (i32, i32),
    received: Instant,
}

/// The mouse fallback: an ordered queue of input for `SendInput`, like the
/// native queue for ERROR_NOT_READY. `INPUT_STALE` bounds it to about a
/// second of samples, so moves need not coalesce, and every fallback event
/// otherwise reaches Windows once.
#[derive(Debug, Default)]
struct Mouse {
    /// Events Windows has not inserted, oldest first. They go before any
    /// newer input, so a refused release always precedes the next press.
    waiting: VecDeque<Waiting>,
    /// Windows holds the left button: from inserted events only.
    pressed: bool,
    /// Whether Windows will hold the button once `waiting` is inserted; new
    /// input is planned from here.
    planned: bool,
    /// A stroke on the mouse: its samples stay on the mouse until it ends.
    wanted: bool,
    /// The desktop pixel Windows last moved the pointer to, for a release
    /// kept owed when stale input is dropped.
    at: (i32, i32),
    /// Waiting input dropped by design, with why, for tests.
    #[cfg(test)]
    dropped: Vec<(MouseEvent, (i32, i32), &'static str)>,
}

#[derive(Debug)]
struct Slot<A: PenApi> {
    api: A,
    handle: Option<A::Handle>,
    /// When Windows last refused to create the device, while it is missing.
    failed_at: Option<Instant>,
    /// The device messages logged, for tests: capturing tracing output is
    /// unreliable while other test threads hit the same log sites.
    #[cfg(test)]
    reported: Vec<&'static str>,
}

impl<A: PenApi> Slot<A> {
    /// Log the first refusal of a streak and the recovery, not every retry.
    fn create(&mut self, now: Instant) {
        match self.api.create() {
            Ok(handle) => {
                if self.failed_at.take().is_some() {
                    info!("Native Windows pen input is available again");
                    #[cfg(test)]
                    self.reported.push("available again");
                }
                self.handle = Some(handle);
            }
            Err(error) => {
                if self.failed_at.is_none() {
                    warn!(%error, "Native Windows pen input unavailable; Pencil uses the mouse");
                    #[cfg(test)]
                    self.reported.push("unavailable");
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
            #[cfg(test)]
            reported: Vec::new(),
        };
        slot.create(Instant::now());
        Self {
            slot,
            contact: PenContact::default(),
            mouse: Mouse::default(),
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

    /// Deliver one pen sample to this device or to the mouse.
    ///
    /// Each sample goes to exactly one of the two, once and in order. A
    /// stroke or hover belongs to one path at a time. It moves to the mouse
    /// when native input cannot carry it (the device is gone, Windows
    /// rejects a frame, or Windows stays not ready for a whole queue):
    /// `PenContact` then hands over the sample and every one waiting behind
    /// it. A stroke on the mouse stays there until it ends, and native input
    /// resumes only where a stroke or hover starts with the mouse idle:
    /// nothing waiting and no button held. That is also where a missing
    /// device is asked for again.
    pub fn deliver(
        &mut self,
        sample: PenSample,
        screen: VirtualScreen,
        output: Option<CaptureGeometry>,
        now: Instant,
    ) {
        self.pump(now);
        if self.mouse_busy() {
            return self.mouse_for(sample, now, now);
        }
        if matches!(sample.phase, PenPhase::Down | PenPhase::Hover) {
            self.revive(now);
        }
        if !self.available() {
            return self.mouse_for(sample, now, now);
        }
        let mut target = Target {
            slot: &mut self.slot,
            screen,
            output,
        };
        self.contact.deliver(sample, output, now, &mut target);
        self.hand_over(now);
    }

    /// The session ended or was taken over. Native input ends as
    /// `PenContact::reset` describes. Waiting mouse input is dropped like
    /// native samples Windows never received, keeping a release owed while
    /// Windows holds the button.
    pub fn reset(&mut self, now: Instant) {
        if self.available() {
            let mut target = Target {
                screen: self.slot.api.screen(),
                slot: &mut self.slot,
                output: None,
            };
            self.contact.reset(&mut target);
        }
        self.drop_mouse_input("reset", now);
        self.pump(now);
    }

    /// Called on every refresh tick: retry mouse input Windows refused, and
    /// deliver samples Windows was not ready for, mapped onto `output`, the
    /// current capture rect.
    pub fn flush(&mut self, output: Option<CaptureGeometry>, now: Instant) {
        self.pump(now);
        if !self.available() || self.contact.pending().is_none() {
            return;
        }
        let screen = self.slot.api.screen();
        let mut target = Target {
            slot: &mut self.slot,
            screen,
            output,
        };
        self.contact.flush(output, now, &mut target);
        self.hand_over(now);
    }

    /// Whether the refresh tick has work even without a device: mouse input
    /// Windows refused.
    pub fn mouse_waiting(&self) -> bool {
        !self.mouse.waiting.is_empty()
    }

    fn mouse_busy(&self) -> bool {
        self.mouse.wanted || self.mouse.pressed || self.mouse_waiting()
    }

    /// Send the samples `PenContact` gave up on to the mouse, oldest first,
    /// each still aged from when it was received.
    fn hand_over(&mut self, now: Instant) {
        for (sample, received) in self.contact.take_abandoned() {
            self.mouse_for(sample, received, now);
        }
    }

    /// Queue mouse input for `sample`, received at `received`, planned from
    /// the button state the waiting input leaves: a down or move presses
    /// unless the button will be held, so a stroke that left native input
    /// is pressed at its first sample on the mouse; anything else releases
    /// a button that will be held. The mouse is therefore never pressed
    /// twice or released without a press, and a new stroke's press follows
    /// the last stroke's release.
    fn mouse_for(&mut self, sample: PenSample, received: Instant, now: Instant) {
        let want = matches!(sample.phase, PenPhase::Down | PenPhase::Move);
        self.mouse.wanted = want;
        let events: &[MouseEvent] = match (want, self.mouse.planned) {
            (true, false) => &[MouseEvent::Move, MouseEvent::Press],
            (true, true) => &[MouseEvent::Move],
            (false, true) => &[MouseEvent::Move, MouseEvent::Release],
            (false, false) if sample.phase == PenPhase::Hover => &[MouseEvent::Move],
            (false, false) => &[],
        };
        self.mouse.planned = want;
        let at = (sample.x, sample.y);
        self.mouse
            .waiting
            .extend(events.iter().map(|&event| Waiting {
                event,
                at,
                received,
            }));
        self.pump(now);
    }

    /// Offer Windows the waiting input, oldest first, placed on the virtual
    /// screen as it is now. What it inserts leaves the queue and sets the
    /// button state; the rest waits for the next sample, tick or reset.
    /// Ink whose sample arrived `INPUT_STALE` ago is dropped first, so a
    /// late call never inserts it; a waiting release alone is never stale.
    fn pump(&mut self, now: Instant) {
        let oldest_ink = self
            .mouse
            .waiting
            .iter()
            .find(|waiting| waiting.event != MouseEvent::Release);
        if oldest_ink
            .is_some_and(|waiting| now.saturating_duration_since(waiting.received) >= INPUT_STALE)
        {
            self.drop_mouse_input("stale", now);
        }
        if self.mouse.waiting.is_empty() {
            return;
        }
        let screen = self.slot.api.screen();
        let inputs: Vec<_> = self
            .mouse
            .waiting
            .iter()
            .map(|waiting| {
                let (x, y) = desktop_pixel_to_abs(waiting.at.0, waiting.at.1, screen);
                match waiting.event {
                    MouseEvent::Move => Injection::MoveAbs { x, y },
                    MouseEvent::Press => Injection::LeftDown { x, y },
                    MouseEvent::Release => Injection::LeftUp { x, y },
                }
            })
            .collect();
        let inserted = self.slot.api.send_mouse(&inputs).min(inputs.len());
        for waiting in self.mouse.waiting.drain(..inserted) {
            match waiting.event {
                MouseEvent::Move => self.mouse.at = waiting.at,
                MouseEvent::Press => self.mouse.pressed = true,
                MouseEvent::Release => self.mouse.pressed = false,
            }
        }
    }

    /// Drop waiting mouse input and end any stroke on the mouse, keeping a
    /// release owed while Windows holds the button, so input that works
    /// again neither replays old ink nor leaves the button held.
    #[cfg_attr(not(test), allow(unused_variables))]
    fn drop_mouse_input(&mut self, why: &'static str, now: Instant) {
        #[cfg(test)]
        self.mouse.dropped.extend(
            self.mouse
                .waiting
                .iter()
                .map(|waiting| (waiting.event, waiting.at, why)),
        );
        self.mouse.waiting.clear();
        self.mouse.wanted = false;
        self.mouse.planned = false;
        if self.mouse.pressed {
            self.mouse.waiting.push_back(Waiting {
                event: MouseEvent::Release,
                at: self.mouse.at,
                received: now,
            });
        }
    }

    #[cfg(test)]
    fn take_dropped_mouse_input(&mut self) -> Vec<(MouseEvent, (i32, i32), &'static str)> {
        std::mem::take(&mut self.mouse.dropped)
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
    /// out), answering frames on a live device with `answers`, then
    /// `answer`, and inserting at most `mouse_limit` mouse events a call.
    #[derive(Debug)]
    struct FakeApi {
        creations: VecDeque<bool>,
        created: usize,
        destroyed: Vec<usize>,
        answer: Outcome,
        answers: VecDeque<Outcome>,
        frames: Vec<(usize, PenPhase)>,
        mouse_limit: usize,
        /// Mouse events Windows inserted.
        mouse: Vec<Injection>,
        /// Whether Windows holds the left button.
        button: bool,
        /// The virtual screen, which can change while input waits.
        screen: VirtualScreen,
    }

    impl FakeApi {
        fn new(creations: &[bool]) -> Self {
            Self {
                creations: creations.iter().copied().collect(),
                created: 0,
                destroyed: Vec::new(),
                answer: Outcome::Injected,
                answers: VecDeque::new(),
                frames: Vec::new(),
                mouse_limit: usize::MAX,
                mouse: Vec::new(),
                button: false,
                screen: SCREEN,
            }
        }
    }

    /// SendInput as the fakes model it: returns how many events, from the
    /// first, Windows inserted.
    trait FakeSendInput {
        fn record_mouse(&mut self, inputs: &[Injection]) -> usize;
    }

    impl FakeSendInput for FakeApi {
        /// Insert events from the first, up to the limit.
        fn record_mouse(&mut self, inputs: &[Injection]) -> usize {
            let inserted = inputs.len().min(self.mouse_limit);
            for &input in &inputs[..inserted] {
                match input {
                    Injection::LeftDown { .. } => {
                        assert!(!std::mem::replace(&mut self.button, true), "pressed twice")
                    }
                    Injection::LeftUp { .. } => {
                        assert!(
                            std::mem::replace(&mut self.button, false),
                            "released unpressed"
                        )
                    }
                    _ => {}
                }
                self.mouse.push(input);
            }
            inserted
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
            self.screen
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
            self.answers.pop_front().unwrap_or(self.answer)
        }

        fn send_mouse(&mut self, inputs: &[Injection]) -> usize {
            self.record_mouse(inputs)
        }
    }

    fn pen(phase: PenPhase) -> Injection {
        pen_at(phase, 100)
    }

    fn pen_at(phase: PenPhase, y: i32) -> Injection {
        Injection::Pen(PenSample {
            phase,
            x: 100,
            y,
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

    /// Deliver one sample as `windows_inject::inject` does.
    fn deliver_one<A: PenApi + FakeSendInput>(
        device: &mut PenDevice<A>,
        sample: Injection,
        now: Instant,
    ) {
        let Injection::Pen(sample) = sample else {
            unreachable!()
        };
        device.deliver(sample, SCREEN, None, now);
    }

    /// A refresh tick, as `keep_pen_alive` does.
    fn flush_one<A: PenApi + FakeSendInput>(device: &mut PenDevice<A>, now: Instant) {
        device.flush(None, now);
    }

    /// A session reset, as `InputRelay::reset` does.
    fn reset_one<A: PenApi + FakeSendInput>(device: &mut PenDevice<A>, now: Instant) {
        device.reset(now);
    }

    /// Deliver a batch; returns the mouse input Windows inserted for it.
    fn send(device: &mut PenDevice<FakeApi>, phases: &[PenPhase], now: Instant) -> Vec<Injection> {
        for &phase in phases {
            deliver_one(device, pen(phase), now);
        }
        std::mem::take(&mut device.slot.api.mouse)
    }

    /// A refresh tick; returns the mouse input Windows inserted for it.
    fn tick(device: &mut PenDevice<FakeApi>) -> Vec<Injection> {
        flush_one(device, Instant::now());
        std::mem::take(&mut device.slot.api.mouse)
    }

    /// A refresh tick at `now`; returns the mouse input Windows inserted.
    fn tick_at(device: &mut PenDevice<FakeApi>, now: Instant) -> Vec<Injection> {
        flush_one(device, now);
        std::mem::take(&mut device.slot.api.mouse)
    }

    /// Lose the device: a stuck frame makes the session reset recreate it,
    /// and Windows refuses the new one.
    fn lose_device(device: &mut PenDevice<FakeApi>) {
        send(device, &[PenPhase::Down], Instant::now());
        device.slot.api.answer = Outcome::NotReady;
        send(device, &[PenPhase::Move], Instant::now());
        reset_one(device, Instant::now());
        device.slot.api.answer = Outcome::Injected;
        assert!(!device.available());
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
            assert!(tick(&mut device).is_empty());
            reset_one(&mut device, Instant::now());
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
        let mut device = PenDevice::new(FakeApi::new(&[false; 200]));
        // Ten strokes and hovers a second for ten seconds.
        for tenth in 1..=100 {
            let now = start + Duration::from_millis(100 * tenth);
            let phases = [
                PenPhase::Hover,
                PenPhase::Down,
                PenPhase::Move,
                PenPhase::Up,
            ];
            let mouse = send(&mut device, &phases, now);
            assert!(mouse.contains(&Injection::LeftDown { x, y }));
        }
        // The startup attempt, then at most one a second.
        assert!(device.slot.api.created <= 11, "{}", device.slot.api.created);
        assert!(device.slot.api.frames.is_empty());
        assert_eq!(std::mem::take(&mut device.slot.reported), ["unavailable"]);
        // Recovery is logged once, and a new streak logs again once.
        device.slot.api.creations = [true].into();
        let later = start + Duration::from_secs(20);
        assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], later).is_empty());
        assert_eq!(
            std::mem::take(&mut device.slot.reported),
            ["available again"]
        );
        device.slot.api.creations = [false; 50].into();
        lose_device(&mut device);
        for second in 1..=5 {
            send(
                &mut device,
                &[PenPhase::Down],
                later + Duration::from_secs(second),
            );
        }
        assert_eq!(device.slot.reported, ["unavailable"]);
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
            for &injection in batch {
                deliver_one(device, injection, now);
            }
            std::mem::take(&mut device.slot.api.mouse)
        };
        let mut device = PenDevice::new(FakeApi::new(&[true, false]));
        lose_device(&mut device);
        let lost = Instant::now();
        // A new session starts while the device is missing; its reset does
        // not ask Windows. Its first stroke, within a second of the refusal,
        // is injected as mouse input.
        reset_one(&mut device, Instant::now());
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

    /// The left-button edges Windows inserted, with the stroke (the
    /// sample's y) each was for.
    fn presses(mouse: &[Injection]) -> Vec<(bool, i32)> {
        let stroke = |abs| {
            (0..=200)
                .find(|&y| super::super::desktop_pixel_to_abs(0, y, SCREEN).1 == abs)
                .unwrap()
        };
        mouse
            .iter()
            .filter_map(|input| match *input {
                Injection::LeftDown { y, .. } => Some((true, stroke(y))),
                Injection::LeftUp { y, .. } => Some((false, stroke(y))),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_full_queue_hands_every_waiting_stroke_to_the_mouse() {
        // Sol's case: Windows holds stroke 0 and is not ready for its
        // cancel and four taps behind it, then rejects the cancel and
        // refuses a new device. No tap may be lost.
        let mut device = PenDevice::new(FakeApi::new(&[true, false]));
        let now = Instant::now();
        deliver_one(&mut device, pen_at(PenPhase::Down, 10), now);
        device.slot.api.answer = Outcome::NotReady;
        deliver_one(&mut device, pen_at(PenPhase::Cancel, 10), now);
        for stroke in 11..=14 {
            deliver_one(&mut device, pen_at(PenPhase::Down, stroke), now);
            deliver_one(&mut device, pen_at(PenPhase::Up, stroke), now);
        }
        device.slot.api.answer = Outcome::Rejected;
        flush_one(&mut device, now);
        let mouse = std::mem::take(&mut device.slot.api.mouse);
        assert_eq!(
            presses(&mouse),
            [11, 12, 13, 14]
                .into_iter()
                .flat_map(|s| [(true, s), (false, s)])
                .collect::<Vec<_>>(),
            "{mouse:?}"
        );
        assert!(!device.slot.api.button);
    }

    #[test]
    fn a_rejected_down_draws_its_tap_on_the_mouse() {
        // Windows rejects the down (E_INVALIDARG, as field hosts logged).
        let mut device = PenDevice::new(FakeApi::new(&[]));
        let now = Instant::now();
        device.slot.api.answers = [Outcome::Rejected].into();
        let mouse = send(&mut device, &[PenPhase::Down, PenPhase::Up], now);
        assert_eq!(presses(&mouse), [(true, 100), (false, 100)]);
        // A move whose fresh start is rejected too continues on the mouse.
        device.slot.api.frames.clear();
        send(&mut device, &[PenPhase::Down], now);
        device.slot.api.answers = [Outcome::Rejected, Outcome::Rejected].into();
        let mouse = send(
            &mut device,
            &[PenPhase::Move, PenPhase::Move, PenPhase::Up],
            now,
        );
        assert_eq!(presses(&mouse), [(true, 100), (false, 100)]);
        assert_eq!(
            device.slot.api.frames,
            [
                (1, PenPhase::Down),
                (1, PenPhase::Move),
                (1, PenPhase::Down)
            ]
        );
        // The next stroke is native again.
        device.slot.api.frames.clear();
        assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], now).is_empty());
        assert_eq!(
            device.slot.api.frames,
            [(1, PenPhase::Down), (1, PenPhase::Up)]
        );
        assert!(!device.slot.api.button);
    }

    fn pen_xy(phase: PenPhase, x: i32, y: i32) -> Injection {
        let Injection::Pen(sample) = pen_at(phase, y) else {
            unreachable!()
        };
        Injection::Pen(PenSample { x, ..sample })
    }

    fn abs(x: i32, y: i32) -> (u16, u16) {
        super::super::desktop_pixel_to_abs(x, y, SCREEN)
    }

    #[test]
    fn a_refused_release_comes_before_the_next_strokes_press() {
        // Sol's case: SendInput refuses stroke 1's release, its retry and
        // stroke 2's press, then takes stroke 2's move. Stroke 1 must be
        // released before stroke 2 is pressed, not joined to it.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let now = Instant::now();
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), now);
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Up, 11, 1), now);
        flush_one(&mut device, now);
        deliver_one(&mut device, pen_xy(PenPhase::Down, 20, 2), now);
        device.slot.api.mouse_limit = usize::MAX;
        deliver_one(&mut device, pen_xy(PenPhase::Move, 21, 2), now);
        deliver_one(&mut device, pen_xy(PenPhase::Up, 22, 2), now);
        let at = |x, y| {
            let (x, y) = abs(x, y);
            (Injection::MoveAbs { x, y }, x, y)
        };
        let (m10, x10, y10) = at(10, 1);
        let (m11, x11, y11) = at(11, 1);
        let (m20, x20, y20) = at(20, 2);
        let (m21, _, _) = at(21, 2);
        let (m22, x22, y22) = at(22, 2);
        assert_eq!(
            device.slot.api.mouse,
            [
                m10,
                Injection::LeftDown { x: x10, y: y10 },
                m11,
                Injection::LeftUp { x: x11, y: y11 },
                m20,
                Injection::LeftDown { x: x20, y: y20 },
                m21,
                m22,
                Injection::LeftUp { x: x22, y: y22 },
            ]
        );
        assert!(!device.slot.api.button);
    }

    #[test]
    fn a_tap_whose_press_was_refused_is_pressed_before_its_release() {
        // Sol's case: SendInput inserts only the move of a tap's down, then
        // recovers before the up.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let now = Instant::now();
        device.slot.api.mouse_limit = 1;
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), now);
        device.slot.api.mouse_limit = usize::MAX;
        deliver_one(&mut device, pen_xy(PenPhase::Up, 10, 1), now);
        let (x, y) = abs(10, 1);
        assert_eq!(
            device.slot.api.mouse,
            [
                Injection::MoveAbs { x, y },
                Injection::LeftDown { x, y },
                Injection::MoveAbs { x, y },
                Injection::LeftUp { x, y },
            ]
        );
        assert!(!device.slot.api.button);
    }

    #[test]
    fn a_refused_move_in_a_stroke_is_retried_in_order() {
        // Sol's case: SendInput refuses a move between a down and an up it
        // takes.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let now = Instant::now();
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), now);
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Move, 15, 1), now);
        device.slot.api.mouse_limit = usize::MAX;
        deliver_one(&mut device, pen_xy(PenPhase::Up, 20, 1), now);
        let moves: Vec<_> = device
            .slot
            .api
            .mouse
            .iter()
            .filter_map(|input| match *input {
                Injection::MoveAbs { x, y } => Some((x, y)),
                _ => None,
            })
            .collect();
        assert_eq!(moves, [abs(10, 1), abs(15, 1), abs(20, 1)]);
        assert_eq!(presses(&device.slot.api.mouse), [(true, 1), (false, 1)]);
    }

    #[test]
    fn native_input_waits_for_a_refused_release() {
        // Windows refused the device, so a stroke uses the mouse, and then
        // SendInput refuses its release. A stroke that could start natively
        // must wait on the mouse until the release is inserted.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let start = Instant::now();
        send(&mut device, &[PenPhase::Down], start);
        device.slot.api.mouse_limit = 0;
        send(&mut device, &[PenPhase::Up], start);
        assert!(device.slot.api.button);
        let later = start + RETRY_INTERVAL;
        send(&mut device, &[PenPhase::Down, PenPhase::Up], later);
        assert!(device.slot.api.frames.is_empty());
        assert_eq!(device.slot.api.created, 1);
        device.slot.api.mouse_limit = usize::MAX;
        let mouse = tick(&mut device);
        assert_eq!(presses(&mouse), [(false, 100), (true, 100), (false, 100)]);
        // With the mouse idle, the next stroke gets a device again.
        let later = start + RETRY_INTERVAL * 2;
        assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], later).is_empty());
        assert_eq!(
            device.slot.api.frames,
            [(2, PenPhase::Down), (2, PenPhase::Up)]
        );
    }

    #[test]
    fn stale_mouse_input_is_dropped_but_the_release_stays_owed() {
        // SendInput refuses everything for over a second, as under UIPI or
        // on the secure desktop, while a stroke on the mouse ends.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let start = Instant::now();
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Move, 11, 1), start);
        let half = start + INPUT_STALE / 2;
        deliver_one(&mut device, pen_xy(PenPhase::Up, 12, 1), half);
        // Past the limit the old stroke's ink is dropped; the release is not.
        let stale = start + INPUT_STALE;
        flush_one(&mut device, stale);
        assert_eq!(
            device.take_dropped_mouse_input(),
            [
                (MouseEvent::Move, (11, 1), "stale"),
                (MouseEvent::Move, (12, 1), "stale"),
                (MouseEvent::Release, (12, 1), "stale"),
            ]
        );
        // A release left waiting alone is never dropped.
        for second in 2..5 {
            flush_one(&mut device, start + INPUT_STALE * second);
        }
        assert!(device.take_dropped_mouse_input().is_empty());
        // A new stroke waits behind it, and input that works again releases
        // first, without replaying the old stroke.
        deliver_one(
            &mut device,
            pen_xy(PenPhase::Down, 20, 2),
            start + INPUT_STALE * 5,
        );
        device.slot.api.mouse_limit = usize::MAX;
        flush_one(&mut device, start + INPUT_STALE * 5);
        let (x10, y10) = abs(10, 1);
        let (x20, y20) = abs(20, 2);
        assert_eq!(
            device.slot.api.mouse,
            [
                Injection::MoveAbs { x: x10, y: y10 },
                Injection::LeftDown { x: x10, y: y10 },
                Injection::LeftUp { x: x10, y: y10 },
                Injection::MoveAbs { x: x20, y: y20 },
                Injection::LeftDown { x: x20, y: y20 },
            ]
        );
        // A reset drops waiting input the same way.
        device.slot.api.mouse_limit = 0;
        deliver_one(
            &mut device,
            pen_xy(PenPhase::Up, 21, 2),
            start + INPUT_STALE * 5,
        );
        reset_one(&mut device, start + INPUT_STALE * 5);
        assert!(device
            .take_dropped_mouse_input()
            .iter()
            .all(|&(_, _, why)| why == "reset"));
        device.slot.api.mouse_limit = usize::MAX;
        flush_one(&mut device, start + INPUT_STALE * 5);
        assert!(!device.slot.api.button);
    }

    /// What reached Windows, in order, for the random streams.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Seen {
        /// A native frame Windows accepted: phase, then the sample's x and
        /// y, its number and its stroke's.
        Native(PenPhase, i32, i32, Instant),
        /// A native frame Windows rejected, which ends its contact.
        Refused(PenPhase),
        /// The device was destroyed, ending its contact.
        Destroyed,
        /// A fallback mouse event, and whether SendInput inserted it.
        Mouse(Injection, bool, Instant),
        /// The test reset the session here.
        Reset,
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

    /// How often each kind of failure happened across the random streams.
    #[derive(Debug, Default)]
    struct Coverage {
        rejected: std::collections::BTreeMap<String, usize>,
        not_ready: usize,
        devices_refused: usize,
        devices_granted: usize,
        mouse_refused: usize,
        /// Batches SendInput cut short, by how many events it inserted.
        mouse_partial_after: std::collections::BTreeMap<usize, usize>,
        overflows: usize,
        streams_overflowed: usize,
        native_stale: usize,
        streams_losing_device: usize,
        streams_mouse_blocked: usize,
        resets: usize,
        dropped: std::collections::BTreeMap<&'static str, usize>,
        samples_checked: usize,
        ups_checked: usize,
    }

    /// Windows for the random streams, until `ready`: not ready for a
    /// quarter of the frames, or all of them while `stuck`; rejecting a
    /// tenth of the frames of every kind; refusing half the devices; and
    /// inserting all of a mouse batch, none (a tenth), or only its first
    /// events (a fifth), or none at all while `blocked`.
    struct RandomApi {
        rng: Rng,
        ready: bool,
        stuck: bool,
        blocked: bool,
        /// The time of the call in progress.
        now: Instant,
        seen: Vec<Seen>,
        coverage: Coverage,
    }

    impl PenApi for RandomApi {
        type Handle = ();
        type Error = &'static str;

        fn create(&mut self) -> Result<(), &'static str> {
            if self.ready || self.rng.below(2) == 0 {
                self.coverage.devices_granted += 1;
                Ok(())
            } else {
                self.coverage.devices_refused += 1;
                Err("refused")
            }
        }

        fn destroy(&mut self, _: ()) {
            self.seen.push(Seen::Destroyed);
        }

        fn screen(&self) -> VirtualScreen {
            SCREEN
        }

        fn inject(
            &mut self,
            _: (),
            frame: PenSample,
            _: PointerKind,
            _: VirtualScreen,
            _: Option<CaptureGeometry>,
        ) -> Outcome {
            let roll = if self.ready { 19 } else { self.rng.below(20) };
            match roll {
                _ if self.stuck && !self.ready => {
                    self.coverage.not_ready += 1;
                    Outcome::NotReady
                }
                0..=4 => {
                    self.coverage.not_ready += 1;
                    Outcome::NotReady
                }
                5..=6 => {
                    *self
                        .coverage
                        .rejected
                        .entry(format!("{:?}", frame.phase))
                        .or_default() += 1;
                    self.seen.push(Seen::Refused(frame.phase));
                    Outcome::Rejected
                }
                _ => {
                    self.seen
                        .push(Seen::Native(frame.phase, frame.x, frame.y, self.now));
                    Outcome::Injected
                }
            }
        }

        fn send_mouse(&mut self, inputs: &[Injection]) -> usize {
            self.record_mouse(inputs)
        }
    }

    impl FakeSendInput for RandomApi {
        fn record_mouse(&mut self, inputs: &[Injection]) -> usize {
            if inputs.is_empty() {
                return 0;
            }
            let inserted = match (self.ready, self.blocked, self.rng.below(10)) {
                (true, ..) => inputs.len(),
                (false, true, _) | (false, false, 0) => {
                    self.coverage.mouse_refused += 1;
                    0
                }
                (false, false, 1..=2) if inputs.len() > 1 => {
                    let inserted = 1 + self.rng.below(inputs.len() as u64 - 1) as usize;
                    *self
                        .coverage
                        .mouse_partial_after
                        .entry(inserted)
                        .or_default() += 1;
                    inserted
                }
                _ => inputs.len(),
            };
            for (index, &input) in inputs.iter().enumerate() {
                self.seen
                    .push(Seen::Mouse(input, index < inserted, self.now));
            }
            inserted
        }
    }

    #[test]
    fn random_strokes_reach_windows_or_the_mouse_exactly_once() {
        // Relay-shaped streams (a lost up shows as the relay's cancel before
        // the next down) against Windows rejecting frames of every kind,
        // staying not ready long enough to fill the queue, refusing or
        // granting devices, and SendInput inserting none or part of a batch
        // or refusing everything for a while; with refresh ticks, and
        // session resets after which a stroke carries on.
        //
        // Every sample has its own x (its number) and its stroke's y, so
        // each native frame and mouse event names the sample it came from.
        // Replaying what Windows took must give each sample's input once,
        // in sample order, on one path, except for samples dropped by
        // design: positions coalesced behind a native retry, and input
        // waiting when a reset or the staleness limit dropped it.
        let to_pixel = |count: i32, axis: fn((u16, u16)) -> u16| {
            (0..count)
                .map(|p| (axis(abs(p, p.min(1079))), p))
                .collect::<std::collections::HashMap<u16, i32>>()
        };
        let pixel_x = to_pixel(1920, |a| a.0);
        let pixel_y = to_pixel(1080, |a| a.1);
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut coverage = Coverage::default();
        for stream in 0..3000 {
            let start = Instant::now();
            let mut device = PenDevice::new(RandomApi {
                rng: Rng(rng.below(u64::MAX) | 1),
                ready: false,
                stuck: false,
                blocked: false,
                now: start,
                seen: Vec::new(),
                coverage: Coverage::default(),
            });
            let mut now = start;
            let (mut stroke, mut in_contact, mut hovering) = (0, false, false);
            let mut samples: Vec<PenSample> = Vec::new();
            // When each sample was delivered, by its number less one.
            let mut times: Vec<Instant> = Vec::new();
            let mut exempt = std::collections::BTreeSet::new();
            // Strokes a reset or the staleness limit cut short.
            let mut cut_strokes = std::collections::BTreeSet::new();
            let (mut overflowed, mut lost) = (false, false);
            let length = rng.below(100);
            // Some streams have Windows stuck, or SendInput refusing
            // everything, for a stretch.
            let stretch = |rng: &mut Rng, one_in| {
                if rng.below(one_in) == 0 {
                    let from = rng.below(60);
                    from..from + 5 + rng.below(25)
                } else {
                    u64::MAX..u64::MAX
                }
            };
            let stuck = stretch(&mut rng, 2);
            let blocked = stretch(&mut rng, 3);
            coverage.streams_mouse_blocked += usize::from(blocked.start < length);
            // Record what one call dropped by design, checking why.
            let note_drops = |device: &mut PenDevice<RandomApi>,
                              resetting: bool,
                              times: &[Instant],
                              exempt: &mut std::collections::BTreeSet<i32>,
                              cut_strokes: &mut std::collections::BTreeSet<i32>,
                              coverage: &mut Coverage| {
                let api_now = device.slot.api.now;
                let waited = |id: i32| api_now.saturating_duration_since(times[id as usize - 1]);
                // A drop takes everything waiting once the oldest is stale.
                let mut oldest_stale = None;
                for (sample, why) in device.contact.take_dropped() {
                    match why {
                        "coalesced" => assert!(
                            matches!(sample.phase, PenPhase::Move | PenPhase::Hover),
                            "stream {stream}: coalesced {sample:?}"
                        ),
                        "reset" => {
                            assert!(resetting, "stream {stream}: reset drop outside a reset")
                        }
                        "stale" => {
                            assert!(!resetting, "stream {stream}: stale drop in a reset");
                            oldest_stale = oldest_stale.max(Some(waited(sample.x)));
                            coverage.native_stale += 1;
                        }
                        _ => panic!("stream {stream}: dropped {sample:?} for {why}"),
                    }
                    *coverage.dropped.entry(why).or_default() += 1;
                    exempt.insert(sample.x);
                    if why != "coalesced" {
                        cut_strokes.insert(sample.y);
                    }
                }
                for (event, (id, stroke), why) in device.take_dropped_mouse_input() {
                    match why {
                        "reset" => {
                            assert!(resetting, "stream {stream}: reset drop outside a reset")
                        }
                        "stale" => assert!(!resetting, "stream {stream}: stale drop in a reset"),
                        _ => panic!("stream {stream}: dropped {event:?} for {why}"),
                    }
                    *coverage.dropped.entry(why).or_default() += 1;
                    if why == "stale" && event != MouseEvent::Release {
                        oldest_stale = oldest_stale.max(Some(waited(id)));
                    }
                    exempt.insert(id);
                    cut_strokes.insert(stroke);
                }
                if let Some(waited) = oldest_stale {
                    assert!(
                        waited >= INPUT_STALE,
                        "stream {stream}: stale after {waited:?}"
                    );
                }
            };
            for step in 0..length {
                device.slot.api.stuck = stuck.contains(&step);
                device.slot.api.blocked = blocked.contains(&step);
                let mut phases = Vec::new();
                match (in_contact, rng.below(10)) {
                    (false, 0..=5) if stroke < 1000 => {
                        stroke += 1;
                        phases.push((PenPhase::Down, stroke));
                        (in_contact, hovering) = (true, false);
                    }
                    (false, 0..=7) => {
                        phases.push((PenPhase::Hover, 0));
                        hovering = true;
                    }
                    (false, _) if hovering => {
                        phases.push((PenPhase::Leave, 0));
                        hovering = false;
                    }
                    (false, _) => {}
                    (true, 0..=3) => phases.push((PenPhase::Move, stroke)),
                    (true, 4..=7) => {
                        phases.push((PenPhase::Up, stroke));
                        in_contact = false;
                    }
                    (true, 8) if stroke < 1000 => {
                        // The up was lost: the relay cancels before the next down.
                        phases.push((PenPhase::Cancel, stroke));
                        stroke += 1;
                        phases.push((PenPhase::Down, stroke));
                    }
                    (true, _) => {
                        phases.push((PenPhase::Cancel, stroke));
                        in_contact = false;
                    }
                }
                // Mostly bursts that can fill the queue, sometimes a stall
                // long enough to make what waits stale.
                now += Duration::from_millis(if rng.below(16) == 0 {
                    300 + rng.below(1200)
                } else {
                    rng.below(40)
                });
                device.slot.api.now = now;
                for (phase, stroke) in phases {
                    let id = samples.len() as i32 + 1;
                    let Injection::Pen(sample) = pen_xy(phase, id, stroke) else {
                        unreachable!()
                    };
                    samples.push(sample);
                    times.push(now);
                    let edge = !matches!(phase, PenPhase::Move | PenPhase::Hover);
                    if edge && device.contact.waiting() >= super::super::pen_contact::QUEUE_LIMIT {
                        coverage.overflows += 1;
                        overflowed = true;
                    }
                    deliver_one(&mut device, Injection::Pen(sample), now);
                    note_drops(
                        &mut device,
                        false,
                        &times,
                        &mut exempt,
                        &mut cut_strokes,
                        &mut coverage,
                    );
                    lost |= !device.available();
                }
                if rng.below(2) == 0 {
                    flush_one(&mut device, now);
                    note_drops(
                        &mut device,
                        false,
                        &times,
                        &mut exempt,
                        &mut cut_strokes,
                        &mut coverage,
                    );
                    lost |= !device.available();
                }
                if rng.below(25) == 0 {
                    // The session resets; the stroke in progress carries on.
                    device.slot.api.seen.push(Seen::Reset);
                    reset_one(&mut device, now);
                    note_drops(
                        &mut device,
                        true,
                        &times,
                        &mut exempt,
                        &mut cut_strokes,
                        &mut coverage,
                    );
                    coverage.resets += 1;
                    lost |= !device.available();
                }
            }
            // Everything works again; the next tick delivers what waits.
            device.slot.api.ready = true;
            device.slot.api.now = now;
            flush_one(&mut device, now);
            note_drops(
                &mut device,
                false,
                &times,
                &mut exempt,
                &mut cut_strokes,
                &mut coverage,
            );
            coverage.streams_overflowed += usize::from(overflowed);
            coverage.streams_losing_device += usize::from(lost);

            // Replay what Windows took, in order.
            let output = std::mem::take(&mut device.slot.api.seen);
            let (mut native, mut mouse) = (None, None);
            // Strokes drawn on either path, and those Windows or a reset
            // ended early (a rejected frame, a destroyed device, a session
            // reset) with no press since: their up has nothing left to end.
            let mut drawn = std::collections::BTreeSet::new();
            let mut cut = std::collections::BTreeSet::new();
            let mut last_sample = 0;
            let mut taken = std::collections::BTreeMap::<(i32, &'static str), usize>::new();
            let mut take = |id: i32, kind: &'static str| {
                assert!(
                    id >= last_sample,
                    "stream {stream}: sample {id} {kind} after sample {last_sample}: {output:?}"
                );
                last_sample = id;
                *taken.entry((id, kind)).or_default() += 1;
            };
            // Nothing but an end may reach Windows `INPUT_STALE` after its
            // sample arrived: native cancels and leaves end the pointer where
            // Windows had it, and a mouse release may be owed for longer.
            let fresh = |id: i32, at: Instant, what: &str| {
                let age = at.saturating_duration_since(times[id as usize - 1]);
                assert!(
                    age < INPUT_STALE,
                    "stream {stream}: sample {id} {what} injected {age:?} after it arrived: {output:?}"
                );
            };
            for seen in &output {
                match *seen {
                    Seen::Native(phase, x, y, at) => {
                        if !matches!(phase, PenPhase::Cancel | PenPhase::Leave) {
                            fresh(x, at, "native frame");
                        }
                        if matches!(phase, PenPhase::Down | PenPhase::Move) {
                            assert_eq!(
                                mouse, None,
                                "stream {stream}: native and mouse together: {output:?}"
                            );
                        }
                        match phase {
                            PenPhase::Down => {
                                native = Some(y);
                                drawn.insert(y);
                                cut.remove(&y);
                            }
                            PenPhase::Up | PenPhase::Cancel => {
                                assert_eq!(
                                    native,
                                    Some(y),
                                    "stream {stream}: native up without its down: {output:?}"
                                );
                                native = None;
                            }
                            _ => {}
                        }
                        // A cancel or leave ends the pointer where Windows
                        // last saw it, an earlier sample's place.
                        match phase {
                            PenPhase::Down => take(x, "down"),
                            PenPhase::Move => take(x, "move"),
                            PenPhase::Up => take(x, "up"),
                            PenPhase::Hover => take(x, "hover"),
                            PenPhase::Cancel | PenPhase::Leave => {}
                        }
                    }
                    Seen::Refused(_) | Seen::Destroyed => {
                        if let Some(stroke) = native.take() {
                            cut.insert(stroke);
                        }
                    }
                    // A reset ends whatever is held; the frames or release
                    // that follow end it.
                    Seen::Reset => cut.extend(native.iter().chain(mouse.iter()).copied()),
                    Seen::Mouse(_, false, _) => {}
                    Seen::Mouse(Injection::MoveAbs { x, .. }, true, at) => {
                        fresh(pixel_x[&x], at, "mouse move");
                        take(pixel_x[&x], "mouse move")
                    }
                    Seen::Mouse(Injection::LeftDown { x, y }, true, at) => {
                        fresh(pixel_x[&x], at, "mouse press");
                        assert_eq!(
                            mouse, None,
                            "stream {stream}: mouse pressed twice: {output:?}"
                        );
                        assert_eq!(
                            native, None,
                            "stream {stream}: native and mouse together: {output:?}"
                        );
                        mouse = Some(pixel_y[&y]);
                        drawn.insert(pixel_y[&y]);
                        cut.remove(&pixel_y[&y]);
                        take(pixel_x[&x], "mouse press");
                    }
                    Seen::Mouse(Injection::LeftUp { .. }, true, _) => {
                        // A release kept owed after a drop sits at the last
                        // place the mouse went, an earlier sample's.
                        assert!(
                            mouse.is_some(),
                            "stream {stream}: mouse up without a down: {output:?}"
                        );
                        mouse = None;
                    }
                    Seen::Mouse(..) => unreachable!(),
                }
            }
            // Exactly once, on one path, for every sample not dropped by
            // design. An up, cancel or leave has nothing to deliver when
            // what it ends is already gone; the replay above checks that
            // nothing is ended twice and nothing stays held. An up whose
            // stroke is still live must land at its own position, where the
            // Pencil lifted: natively an up frame there, on the mouse a move
            // there before the release.
            let once = |id, kind| taken.get(&(id, kind)).copied().unwrap_or(0);
            for sample in &samples {
                let id = sample.x;
                let native_frames =
                    once(id, "down") + once(id, "move") + once(id, "up") + once(id, "hover");
                let mouse_input = once(id, "mouse move") + once(id, "mouse press");
                for kind in ["down", "move", "up", "hover", "mouse move", "mouse press"] {
                    assert!(
                        once(id, kind) <= 1,
                        "stream {stream}: sample {id} {kind} twice: {output:?}"
                    );
                }
                assert!(
                    native_frames == 0 || mouse_input == 0,
                    "stream {stream}: sample {id} on both paths: {output:?}"
                );
                let stroke = sample.y;
                if sample.phase == PenPhase::Up
                    && drawn.contains(&stroke)
                    && !cut.contains(&stroke)
                    && !cut_strokes.contains(&stroke)
                {
                    coverage.ups_checked += 1;
                    assert!(
                        once(id, "up") + once(id, "mouse move") == 1,
                        "stream {stream}: up {id} of stroke {stroke} missed its position: {output:?}"
                    );
                }
                if exempt.contains(&id)
                    || !matches!(
                        sample.phase,
                        PenPhase::Down | PenPhase::Move | PenPhase::Hover
                    )
                {
                    continue;
                }
                coverage.samples_checked += 1;
                assert!(
                    native_frames + mouse_input > 0,
                    "stream {stream}: sample {id} {:?} was lost: {output:?}",
                    sample.phase
                );
                if sample.phase == PenPhase::Down && mouse_input > 0 {
                    assert_eq!(
                        once(id, "mouse press"),
                        1,
                        "stream {stream}: down {id} not pressed: {output:?}"
                    );
                }
            }
            if !in_contact {
                assert_eq!(
                    (native, mouse),
                    (None, None),
                    "stream {stream}: left pressed: {output:?}"
                );
            }
            let api = &device.slot.api.coverage;
            for (phase, count) in &api.rejected {
                *coverage.rejected.entry(phase.clone()).or_default() += count;
            }
            for (inserted, count) in &api.mouse_partial_after {
                *coverage.mouse_partial_after.entry(*inserted).or_default() += count;
            }
            coverage.not_ready += api.not_ready;
            coverage.devices_refused += api.devices_refused;
            coverage.devices_granted += api.devices_granted;
            coverage.mouse_refused += api.mouse_refused;
        }
        eprintln!("COVERAGE {coverage:#?}");
        for phase in ["Down", "Move", "Up", "Cancel", "Hover", "Leave"] {
            assert!(
                coverage.rejected.get(phase).is_some_and(|&n| n > 100),
                "{coverage:#?}"
            );
        }
        for why in ["coalesced", "reset", "stale"] {
            assert!(
                coverage.dropped.get(why).is_some_and(|&n| n > 50),
                "{coverage:#?}"
            );
        }
        assert!(coverage.ups_checked > 10_000, "{coverage:#?}");
        assert!(coverage.streams_overflowed > 200, "{coverage:#?}");
        assert!(coverage.native_stale > 300, "{coverage:#?}");
        assert!(coverage.streams_losing_device > 300, "{coverage:#?}");
        assert!(coverage.mouse_refused > 300, "{coverage:#?}");
        assert!(
            (1..=3).all(|k| coverage
                .mouse_partial_after
                .get(&k)
                .is_some_and(|&n| n > 20)),
            "{coverage:#?}"
        );
    }

    #[test]
    fn a_tap_windows_was_not_ready_for_is_not_injected_seconds_later() {
        // Claude's case: Windows is not ready for a tap, and the next tick
        // comes five seconds later with Windows ready again.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        device.slot.api.answer = Outcome::NotReady;
        let start = Instant::now();
        send(&mut device, &[PenPhase::Down, PenPhase::Up], start);
        device.slot.api.answer = Outcome::Injected;
        device.slot.api.frames.clear();
        let late = start + Duration::from_secs(5);
        flush_one(&mut device, late);
        assert!(device.slot.api.frames.is_empty());
        assert!(device.slot.api.mouse.is_empty());
        let dropped = device.contact.take_dropped();
        assert_eq!(
            dropped
                .iter()
                .map(|&(s, why)| (s.phase, why))
                .collect::<Vec<_>>(),
            [(PenPhase::Down, "stale"), (PenPhase::Up, "stale")]
        );
        // The pointer Windows may hold went with the old device, and the
        // next tap starts cleanly on a new one.
        assert_eq!(device.slot.api.destroyed, [1]);
        assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], late).is_empty());
        assert_eq!(
            device.slot.api.frames,
            [(2, PenPhase::Down), (2, PenPhase::Up)]
        );
    }

    #[test]
    fn taps_windows_was_never_ready_for_are_not_replayed_as_clicks() {
        // Claude's case: taps ten seconds apart while Windows stays not
        // ready. They must not fill the queue and come out as mouse clicks
        // when the next tap overflows it.
        let mut device = PenDevice::new(FakeApi::new(&[]));
        device.slot.api.answer = Outcome::NotReady;
        let start = Instant::now();
        for tap in 0..5 {
            let at = start + Duration::from_secs(10 * tap);
            assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], at).is_empty());
        }
        // Each tap dropped the one before it, with a new device.
        let dropped = device.contact.take_dropped();
        assert_eq!(dropped.len(), 8);
        assert!(dropped.iter().all(|&(_, why)| why == "stale"));
        assert_eq!(device.slot.api.destroyed, [1, 2, 3, 4]);
        // Windows becomes ready: only the last tap reaches it.
        device.slot.api.answer = Outcome::Injected;
        device.slot.api.frames.clear();
        assert!(tick_at(&mut device, start + Duration::from_millis(40_100)).is_empty());
        assert_eq!(
            device.slot.api.frames,
            [(5, PenPhase::Down), (5, PenPhase::Up)]
        );
    }

    #[test]
    fn refused_mouse_input_lands_where_the_virtual_screen_is_on_retry() {
        // SendInput refuses a press; a monitor is added to the left before
        // the retry. The pen's desktop position must map onto the new
        // virtual screen, not the one it was queued under.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let start = Instant::now();
        device.slot.api.mouse_limit = 0;
        deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
        let wider = VirtualScreen {
            left: -1920,
            top: 0,
            width: 3840,
            height: 1080,
        };
        device.slot.api.screen = wider;
        device.slot.api.mouse_limit = usize::MAX;
        flush_one(&mut device, start);
        let (x, y) = super::super::desktop_pixel_to_abs(10, 1, wider);
        assert_ne!((x, y), abs(10, 1));
        assert_eq!(
            device.slot.api.mouse,
            [Injection::MoveAbs { x, y }, Injection::LeftDown { x, y }]
        );
    }

    #[test]
    fn expired_input_is_never_inserted_when_insertion_recovers() {
        // Sol's case: SendInput refuses a stroke, then the next tick comes
        // two seconds later (sleep or load) with insertion working again.
        // Only a release owed for a button Windows holds may be inserted.
        let (x, y) = abs(10, 1);
        for press_inserted in [false, true] {
            let mut device = PenDevice::new(FakeApi::new(&[false]));
            let start = Instant::now();
            device.slot.api.mouse_limit = if press_inserted { 2 } else { 0 };
            deliver_one(&mut device, pen_xy(PenPhase::Down, 10, 1), start);
            device.slot.api.mouse_limit = 0;
            deliver_one(&mut device, pen_xy(PenPhase::Up, 12, 1), start);
            let inserted_before = std::mem::take(&mut device.slot.api.mouse);
            device.slot.api.mouse_limit = usize::MAX;
            flush_one(&mut device, start + INPUT_STALE * 2);
            let owed: &[Injection] = if press_inserted {
                &[Injection::LeftUp { x, y }]
            } else {
                &[]
            };
            assert_eq!(
                device.slot.api.mouse, owed,
                "press inserted: {press_inserted}, before: {inserted_before:?}"
            );
            assert!(!device.slot.api.button);
        }
    }
}
