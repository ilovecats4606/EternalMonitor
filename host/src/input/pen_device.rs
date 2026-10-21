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
        }
    }

    pub fn available(&self) -> bool {
        self.slot.handle.is_some()
    }

    /// Where no stroke is in progress (a pen down, a hover or a new
    /// session), ask again for a device Windows refused, at most once per
    /// `RETRY_INTERVAL`.
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

    /// Keep pen samples for this device, or turn them into their mouse
    /// equivalent while it is missing. A missing device is asked for again
    /// where a stroke or hover starts; the relay has ended any earlier
    /// stroke there, so the mouse holds no button for it.
    pub fn route(
        &mut self,
        injections: &[Injection],
        screen: VirtualScreen,
        now: Instant,
    ) -> Vec<Injection> {
        let mut routed = Vec::with_capacity(injections.len());
        for &injection in injections {
            let Injection::Pen(sample) = injection else {
                routed.push(injection);
                continue;
            };
            if matches!(sample.phase, PenPhase::Down | PenPhase::Hover) {
                self.revive(now);
            }
            if self.available() {
                routed.push(injection);
            } else {
                routed.extend(pen_to_mouse(sample, screen));
            }
        }
        routed
    }

    pub fn inject(
        &mut self,
        sample: PenSample,
        screen: VirtualScreen,
        output: Option<CaptureGeometry>,
    ) {
        if !self.available() {
            return;
        }
        let mut target = Target {
            slot: &mut self.slot,
            screen,
            output,
        };
        self.contact.deliver(sample, output, &mut target);
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
    /// current capture rect. Called on every refresh tick.
    pub fn flush(&mut self, output: Option<CaptureGeometry>) {
        if !self.available() || self.contact.pending().is_none() {
            return;
        }
        let mut target = Target {
            screen: self.slot.api.screen(),
            slot: &mut self.slot,
            output,
        };
        self.contact.flush(output, &mut target);
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

    /// Route a batch and inject what stays native, as the relay and
    /// `windows_inject::inject` do; returns what went to the mouse.
    fn send(device: &mut PenDevice<FakeApi>, phases: &[PenPhase], now: Instant) -> Vec<Injection> {
        let batch: Vec<_> = phases.iter().map(|&phase| pen(phase)).collect();
        let mut mouse = Vec::new();
        for injection in device.route(&batch, SCREEN, now) {
            match injection {
                Injection::Pen(sample) => device.inject(sample, SCREEN, None),
                other => mouse.push(other),
            }
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
        // mouse until a stroke starts.
        for _ in 0..100 {
            device.flush(None);
            device.reset();
        }
        let (x, y) = super::super::desktop_pixel_to_abs(100, 100, SCREEN);
        assert_eq!(
            send(&mut device, &[PenPhase::Move, PenPhase::Up], lost),
            [
                Injection::MoveAbs { x, y },
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
        let later = Instant::now() + RETRY_INTERVAL * 2;
        // Moves, ups, cancels and leaves never create a device.
        let ends = [
            PenPhase::Move,
            PenPhase::Up,
            PenPhase::Cancel,
            PenPhase::Leave,
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
}
