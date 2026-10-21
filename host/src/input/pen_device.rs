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

/// The mouse fallback, as far as Windows inserted its input.
#[derive(Debug, Default)]
struct Mouse {
    /// Windows holds the left button for the fallback.
    pressed: bool,
    /// A stroke on the mouse wants the button held: its samples stay on the
    /// mouse until it ends. Pressed but not wanted is a release Windows
    /// refused, still owed.
    wanted: bool,
    /// Where the fallback last moved the pointer, for an owed release.
    at: (u16, u16),
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
    /// resumes only where a stroke or hover starts with the mouse holding
    /// nothing, which is also where a missing device is asked for again.
    pub fn deliver(
        &mut self,
        sample: PenSample,
        screen: VirtualScreen,
        output: Option<CaptureGeometry>,
        now: Instant,
    ) {
        self.settle();
        if self.mouse.wanted || self.mouse.pressed {
            return self.mouse_for(sample, screen);
        }
        if matches!(sample.phase, PenPhase::Down | PenPhase::Hover) {
            self.revive(now);
        }
        if !self.available() {
            return self.mouse_for(sample, screen);
        }
        let mut target = Target {
            slot: &mut self.slot,
            screen,
            output,
        };
        self.contact.deliver(sample, output, &mut target);
        self.hand_over(screen);
    }

    /// The session ended or was taken over. Native input ends as
    /// `PenContact::reset` describes; a stroke on the mouse ends with a
    /// release, owed until Windows inserts it.
    pub fn reset(&mut self) {
        if self.available() {
            let mut target = Target {
                screen: self.slot.api.screen(),
                slot: &mut self.slot,
                output: None,
            };
            self.contact.reset(&mut target);
        }
        self.mouse.wanted = false;
        self.settle();
    }

    /// Called on every refresh tick: retry a release Windows refused, and
    /// deliver samples Windows was not ready for, mapped onto `output`, the
    /// current capture rect.
    pub fn flush(&mut self, output: Option<CaptureGeometry>) {
        self.settle();
        if !self.available() || self.contact.pending().is_none() {
            return;
        }
        let screen = self.slot.api.screen();
        let mut target = Target {
            slot: &mut self.slot,
            screen,
            output,
        };
        self.contact.flush(output, &mut target);
        self.hand_over(screen);
    }

    /// Whether the refresh tick has work even without a device: a release
    /// Windows refused.
    pub fn owes_release(&self) -> bool {
        self.mouse.pressed && !self.mouse.wanted
    }

    /// Send the samples `PenContact` gave up on to the mouse, oldest first.
    fn hand_over(&mut self, screen: VirtualScreen) {
        for sample in self.contact.take_abandoned() {
            self.mouse_for(sample, screen);
        }
    }

    /// Mouse input for `sample`. The button is pressed for a down or move
    /// whenever Windows does not hold it, so a stroke that left native
    /// input is pressed at its first sample on the mouse, and released for
    /// anything else only if Windows holds it. The state follows what
    /// Windows inserted, so the mouse is never pressed twice or released
    /// without a press, and a refused press or release is tried again.
    fn mouse_for(&mut self, sample: PenSample, screen: VirtualScreen) {
        let want = matches!(sample.phase, PenPhase::Down | PenPhase::Move);
        // A release still owed for an earlier stroke goes first, in the
        // same batch, so a new stroke gets its own press.
        let owed = self.owes_release();
        let mut inputs = Vec::new();
        if owed {
            let (x, y) = self.mouse.at;
            inputs.push(Injection::LeftUp { x, y });
        }
        let phase = match (want, self.mouse.pressed && !owed) {
            (true, false) => Some(PenPhase::Down),
            (true, true) => Some(PenPhase::Move),
            (false, true) => Some(PenPhase::Up),
            (false, false) if sample.phase == PenPhase::Hover => Some(PenPhase::Hover),
            (false, false) => None,
        };
        if let Some(phase) = phase {
            inputs.extend(pen_to_mouse(PenSample { phase, ..sample }, screen));
        }
        self.mouse.wanted = want;
        if !inputs.is_empty() {
            self.send_mouse(&inputs);
        }
    }

    /// Release a button Windows still holds for a stroke that has ended.
    fn settle(&mut self) {
        if self.owes_release() {
            let (x, y) = self.mouse.at;
            self.send_mouse(&[Injection::LeftUp { x, y }]);
        }
    }

    fn send_mouse(&mut self, inputs: &[Injection]) {
        let inserted = self.slot.api.send_mouse(inputs).min(inputs.len());
        for input in &inputs[..inserted] {
            match *input {
                Injection::MoveAbs { x, y } => self.mouse.at = (x, y),
                Injection::LeftDown { .. } => self.mouse.pressed = true,
                Injection::LeftUp { .. } => self.mouse.pressed = false,
                _ => {}
            }
        }
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
    fn flush_one<A: PenApi + FakeSendInput>(device: &mut PenDevice<A>) {
        device.flush(None);
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
        flush_one(device);
        std::mem::take(&mut device.slot.api.mouse)
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
        flush_one(&mut device);
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

    #[test]
    fn a_refused_release_is_owed_until_windows_takes_it() {
        // Windows refuses the device, so strokes use the mouse; then
        // SendInput refuses the release (UIPI or a desktop switch).
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        let start = Instant::now();
        send(&mut device, &[PenPhase::Down], start);
        device.slot.api.mouse_limit = 0;
        send(&mut device, &[PenPhase::Up], start);
        assert!(device.slot.api.button, "Windows still holds the button");
        // Still refused at a tick; and a stroke that could start natively
        // must not while Windows holds the button.
        tick(&mut device);
        let later = start + RETRY_INTERVAL * 2;
        send(&mut device, &[PenPhase::Down, PenPhase::Up], later);
        assert!(device.slot.api.frames.is_empty());
        assert_eq!(device.slot.api.created, 1);
        // The next tick releases it once Windows takes input again.
        device.slot.api.mouse_limit = usize::MAX;
        let (x, y) = super::super::desktop_pixel_to_abs(100, 100, SCREEN);
        assert_eq!(tick(&mut device), [Injection::LeftUp { x, y }]);
        assert!(!device.slot.api.button);
        // Then native input resumes at the next stroke.
        assert!(send(&mut device, &[PenPhase::Down, PenPhase::Up], later).is_empty());
        assert_eq!(
            device.slot.api.frames,
            [(2, PenPhase::Down), (2, PenPhase::Up)]
        );
        // A reset retries an owed release too.
        let mut device = PenDevice::new(FakeApi::new(&[false]));
        send(&mut device, &[PenPhase::Down], start);
        device.slot.api.mouse_limit = 1;
        send(&mut device, &[PenPhase::Up], start);
        assert!(device.slot.api.button, "only the move was inserted");
        device.slot.api.mouse_limit = usize::MAX;
        device.reset();
        assert!(!device.slot.api.button);
    }

    /// What reached Windows, in order, for the random streams.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Seen {
        /// A native frame Windows accepted, and the stroke it belongs to.
        Native(PenPhase, i32),
        /// A native frame Windows rejected, which ends its contact.
        Refused(PenPhase),
        /// The device was destroyed, ending its contact.
        Destroyed,
        /// A fallback mouse event, and whether SendInput inserted it.
        Mouse(Injection, bool),
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
        mouse_partial: usize,
        overflows: usize,
        streams_overflowed: usize,
        streams_losing_device: usize,
        resets: usize,
        presses_all_refused: usize,
    }

    /// Windows for the random streams, until `ready`: not ready for a
    /// quarter of the frames, or all of them while `stuck`; rejecting a
    /// tenth of the frames of every kind; refusing half the devices; and
    /// inserting all of a mouse batch, none, or only its first events.
    struct RandomApi {
        rng: Rng,
        ready: bool,
        stuck: bool,
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
                    self.seen.push(Seen::Native(frame.phase, frame.y));
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
            let inserted = match (self.ready, self.rng.below(10)) {
                (true, _) | (false, 2..) => inputs.len(),
                (false, 0) => {
                    self.coverage.mouse_refused += 1;
                    0
                }
                (false, 1) => {
                    self.coverage.mouse_partial += 1;
                    self.rng.below(inputs.len() as u64) as usize
                }
            };
            for (index, &input) in inputs.iter().enumerate() {
                self.seen.push(Seen::Mouse(input, index < inserted));
            }
            inserted
        }
    }

    #[test]
    fn random_strokes_reach_windows_or_the_mouse_exactly_once() {
        // Relay-shaped streams (a lost up shows as the relay's cancel before
        // the next down) against Windows rejecting frames of every kind,
        // staying not ready long enough to fill the queue, refusing or
        // granting devices, and inserting none or part of a mouse batch;
        // with refresh ticks, and session resets after which a stroke
        // carries on. Each sample's y is its stroke number; hovers use 0.
        let abs_y: Vec<u16> = (0..=60)
            .map(|y| super::super::desktop_pixel_to_abs(0, y, SCREEN).1)
            .collect();
        let stroke_of = |abs: u16| abs_y.iter().position(|&y| y == abs).unwrap() as i32;
        let mut rng = Rng(0x2545_f491_4f6c_dd1d);
        let mut coverage = Coverage::default();
        for stream in 0..3000 {
            let mut device = PenDevice::new(RandomApi {
                rng: Rng(rng.below(u64::MAX) | 1),
                ready: false,
                stuck: false,
                seen: Vec::new(),
                coverage: Coverage::default(),
            });
            let mut now = Instant::now();
            let (mut stroke, mut in_contact, mut hovering) = (0, false, false);
            let mut strokes = Vec::new();
            let (mut overflowed, mut lost) = (false, false);
            // Strokes a reset may have dropped samples of, by design.
            let mut reset_before = 0;
            let length = rng.below(80);
            // A third of the streams has Windows stuck for a stretch.
            let stuck_from = if rng.below(3) == 0 {
                rng.below(40)
            } else {
                u64::MAX
            };
            let stuck_until = stuck_from.saturating_add(15 + rng.below(30));
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
            for step in 0..length {
                device.slot.api.stuck = (stuck_from..stuck_until).contains(&step);
                let mut batch = Vec::new();
                match (in_contact, rng.below(10)) {
                    (false, 0..=5) if stroke < 60 => {
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
                    (true, 0..=3) => batch.push(sample(PenPhase::Move, stroke, &mut rng)),
                    (true, 4..=7) => {
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
                    let edge = !matches!(sample.phase, PenPhase::Move | PenPhase::Hover);
                    if edge && device.contact.waiting() >= super::super::pen_contact::QUEUE_LIMIT {
                        coverage.overflows += 1;
                        overflowed = true;
                    }
                    deliver_one(&mut device, Injection::Pen(sample), now);
                    lost |= !device.available();
                }
                if rng.below(2) == 0 {
                    flush_one(&mut device);
                    lost |= !device.available();
                }
                if rng.below(25) == 0 {
                    // The session resets; the stroke in progress carries on.
                    device.reset();
                    coverage.resets += 1;
                    lost |= !device.available();
                    reset_before = stroke;
                }
            }
            // Windows becomes ready; the next tick delivers what waits and
            // releases what is owed.
            device.slot.api.ready = true;
            flush_one(&mut device);
            coverage.streams_overflowed += usize::from(overflowed);
            coverage.streams_losing_device += usize::from(lost);
            // Replay what reached Windows, as Windows holds it.
            let (mut native, mut mouse) = (None, None);
            let mut drawn = std::collections::BTreeSet::new();
            let mut presses = std::collections::BTreeMap::<i32, (usize, usize)>::new();
            let output = std::mem::take(&mut device.slot.api.seen);
            for seen in &output {
                match *seen {
                    Seen::Native(PenPhase::Down, s) => {
                        assert_eq!(
                            mouse, None,
                            "stream {stream}: native and mouse together: {output:?}"
                        );
                        native = Some(s);
                        drawn.insert(s);
                    }
                    Seen::Native(PenPhase::Move, _) => {
                        assert_eq!(
                            mouse, None,
                            "stream {stream}: native and mouse together: {output:?}"
                        );
                    }
                    Seen::Native(PenPhase::Up | PenPhase::Cancel, s) => {
                        assert_eq!(
                            native,
                            Some(s),
                            "stream {stream}: native up without its down: {output:?}"
                        );
                        native = None;
                    }
                    Seen::Native(..) => {}
                    Seen::Refused(_) | Seen::Destroyed => native = None,
                    Seen::Mouse(Injection::LeftDown { y, .. }, inserted) => {
                        let attempts = presses.entry(stroke_of(y)).or_default();
                        attempts.0 += 1;
                        if inserted {
                            assert_eq!(
                                mouse, None,
                                "stream {stream}: mouse pressed twice: {output:?}"
                            );
                            assert_eq!(
                                native, None,
                                "stream {stream}: native and mouse together: {output:?}"
                            );
                            mouse = Some(stroke_of(y));
                            drawn.insert(stroke_of(y));
                        } else {
                            attempts.1 += 1;
                        }
                    }
                    Seen::Mouse(Injection::LeftUp { .. }, true) => {
                        assert!(
                            mouse.is_some(),
                            "stream {stream}: mouse up without a down: {output:?}"
                        );
                        mouse = None;
                    }
                    Seen::Mouse(..) => {}
                }
            }
            // Every stroke is drawn, unless a reset cut it or SendInput
            // refused every press it was offered.
            for s in strokes.iter().filter(|&&s| s > reset_before) {
                match presses.get(s) {
                    _ if drawn.contains(s) => {}
                    Some(&(attempts, refused)) if attempts > 0 && attempts == refused => {
                        coverage.presses_all_refused += 1;
                    }
                    _ => panic!("stream {stream}: stroke {s} was dropped: {output:?}"),
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
            coverage.not_ready += api.not_ready;
            coverage.devices_refused += api.devices_refused;
            coverage.devices_granted += api.devices_granted;
            coverage.mouse_refused += api.mouse_refused;
            coverage.mouse_partial += api.mouse_partial;
        }
        eprintln!("COVERAGE {coverage:#?}");
        for phase in ["Down", "Move", "Up", "Cancel", "Hover", "Leave"] {
            assert!(
                coverage.rejected.get(phase).is_some_and(|&n| n > 100),
                "{coverage:#?}"
            );
        }
        assert!(coverage.streams_overflowed > 300, "{coverage:#?}");
        assert!(coverage.streams_losing_device > 300, "{coverage:#?}");
        assert!(
            coverage.mouse_refused > 300 && coverage.mouse_partial > 300,
            "{coverage:#?}"
        );
    }
}
