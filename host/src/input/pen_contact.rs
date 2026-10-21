//! What Windows currently believes about our synthetic pen, and the frames
//! that bring it to the state each sample asks for.
//!
//! Pure so that tests can drive every transition against a model of Windows;
//! `windows_inject.rs` only performs the frames this type chooses. The model
//! follows the injection state table Microsoft documents for
//! `InjectTouchInput`, which synthetic pen injection shares: a contact starts
//! with DOWN from out of range or hover, updates while in contact, and ends
//! with UP or a cancel at the last updated position; hover starts or updates
//! with an in-range UPDATE and ends with a bare UPDATE. A rejected frame
//! cancels every injected contact. ERROR_NOT_READY applies nothing; the same
//! frame must be retried.

use std::collections::VecDeque;

use super::{CaptureGeometry, PenPhase, PenSample, VirtualScreen};

/// Samples that may wait for Windows. Positions coalesce, so this bounds
/// strokes that start and end while Windows stays not ready.
const QUEUE_LIMIT: usize = 8;

/// `POINTER_FLAG_*` values from winuser.h, so frames can be built and checked
/// off Windows. `windows_inject.rs` asserts that they match the SDK's.
pub mod pointer_flag {
    pub const INRANGE: u32 = 0x0000_0002;
    pub const INCONTACT: u32 = 0x0000_0004;
    pub const FIRSTBUTTON: u32 = 0x0000_0010;
    pub const PRIMARY: u32 = 0x0000_2000;
    pub const CANCELED: u32 = 0x0000_8000;
    pub const DOWN: u32 = 0x0001_0000;
    pub const UPDATE: u32 = 0x0002_0000;
    pub const UP: u32 = 0x0004_0000;
}

/// The `POINTER_PEN_INFO` fields one injected frame sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PointerFrame {
    pub flags: u32,
    /// `ptPixelLocation`, measured from the virtual screen's top-left.
    pub location: (i32, i32),
    pub pressure: u32,
    pub tilt_x: i32,
    pub tilt_y: i32,
}

impl PointerFrame {
    pub fn new(frame: PenSample, screen: VirtualScreen) -> Self {
        use pointer_flag::*;
        let flags = match frame.phase {
            PenPhase::Down => DOWN | INRANGE | INCONTACT | FIRSTBUTTON,
            PenPhase::Move => UPDATE | INRANGE | INCONTACT | FIRSTBUTTON,
            // Finish contact completely. Supported UIKit hover starts a new
            // in-range update; unsupported devices must not leave ghost hover.
            PenPhase::Up => UP,
            PenPhase::Cancel => UP | CANCELED,
            PenPhase::Hover => UPDATE | INRANGE,
            PenPhase::Leave => UPDATE,
        };
        Self {
            flags: flags | PRIMARY,
            location: super::desktop_pixel_to_pointer_location(frame.x, frame.y, screen),
            pressure: frame.pressure,
            tilt_x: frame.tilt_x,
            tilt_y: frame.tilt_y,
        }
    }
}

/// Windows' view of our pointer after the last frame it accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerState {
    #[default]
    Out,
    Hover(PenSample),
    Contact(PenSample),
}

/// The pointer state without its sample, for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerKind {
    Out,
    Hover,
    Contact,
}

impl PointerState {
    pub fn kind(self) -> PointerKind {
        match self {
            PointerState::Out => PointerKind::Out,
            PointerState::Hover(_) => PointerKind::Hover,
            PointerState::Contact(_) => PointerKind::Contact,
        }
    }
}

/// The result of one injected frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Injected,
    /// ERROR_NOT_READY after the injector's own retry window: nothing applied.
    NotReady,
    /// Any other refusal. Windows has cancelled our contact.
    Rejected,
}

#[derive(Debug, Default)]
pub struct PenContact {
    state: PointerState,
    /// Samples Windows has not been brought to yet, oldest first: one that
    /// hit ERROR_NOT_READY and those that arrived behind it. Each is planned
    /// from the accepted state when its turn comes, so a frame deferred part
    /// way through a sample (the final move before an up) is retried before
    /// the rest of it, and mapped from its wire position onto the capture
    /// rect current at that moment.
    queue: VecDeque<PenSample>,
}

impl PenContact {
    #[cfg(test)]
    pub fn state(&self) -> PointerState {
        self.state
    }

    /// The oldest sample still waiting for Windows.
    pub fn pending(&self) -> Option<PenSample> {
        self.queue.front().copied()
    }

    /// Bring Windows to the state `sample` asks for, after anything still
    /// waiting. `output` is the current capture rect. `inject` performs one
    /// frame and reports its outcome, given the state the frame starts from.
    pub fn deliver(
        &mut self,
        sample: PenSample,
        output: Option<CaptureGeometry>,
        inject: &mut impl FnMut(PenSample, PointerKind) -> Outcome,
    ) {
        self.enqueue(sample);
        self.flush(output, inject);
    }

    /// Deliver waiting samples in order until Windows is not ready. Call on
    /// every refresh tick so an end of contact is not left waiting for the
    /// next Pencil sample.
    pub fn flush(
        &mut self,
        output: Option<CaptureGeometry>,
        inject: &mut impl FnMut(PenSample, PointerKind) -> Outcome,
    ) {
        while let Some(mut sample) = self.pending() {
            if let Some(output) = output {
                (sample.x, sample.y) =
                    super::norm_to_output_pixel(sample.x_norm, sample.y_norm, output);
            }
            if !self.bring(sample, inject) {
                return;
            }
            self.queue.pop_front();
        }
    }

    fn enqueue(&mut self, sample: PenSample) {
        let update = |sample: &PenSample| matches!(sample.phase, PenPhase::Move | PenPhase::Hover);
        match self.queue.back_mut() {
            // Only the latest position matters.
            Some(last) if update(last) && update(&sample) => *last = sample,
            _ => {
                // Drop the oldest waiting sample, but never the first, which
                // may be part way through, the first end of contact, which
                // the contact Windows has is owed, or this one.
                if self.queue.len() == QUEUE_LIMIT {
                    let owed = self
                        .queue
                        .iter()
                        .position(|s| matches!(s.phase, PenPhase::Up | PenPhase::Cancel));
                    let oldest = if owed == Some(1) { 2 } else { 1 };
                    self.queue.remove(oldest);
                }
                self.queue.push_back(sample);
            }
        }
    }

    /// Plan and send the frames for one sample. Returns false if Windows was
    /// not ready, leaving the rest to be planned again from where it stopped.
    fn bring(
        &mut self,
        sample: PenSample,
        inject: &mut impl FnMut(PenSample, PointerKind) -> Outcome,
    ) -> bool {
        // A rejection cancels the contact; resume it from this same, freshly
        // mapped sample once rather than waiting for the next one.
        for _ in 0..2 {
            let mut rejected = false;
            for frame in self.plan(sample) {
                match self.send(frame, inject) {
                    Outcome::Injected => {}
                    Outcome::NotReady => return false,
                    Outcome::Rejected => {
                        rejected = true;
                        break;
                    }
                }
            }
            if !rejected || !matches!(sample.phase, PenPhase::Move | PenPhase::Hover) {
                break;
            }
        }
        true
    }

    fn send(
        &mut self,
        frame: PenSample,
        inject: &mut impl FnMut(PenSample, PointerKind) -> Outcome,
    ) -> Outcome {
        let outcome = inject(frame, self.state.kind());
        match outcome {
            Outcome::Injected => {
                self.state = match frame.phase {
                    PenPhase::Down | PenPhase::Move => PointerState::Contact(frame),
                    PenPhase::Hover => PointerState::Hover(frame),
                    PenPhase::Up | PenPhase::Cancel | PenPhase::Leave => PointerState::Out,
                };
            }
            Outcome::NotReady => {}
            Outcome::Rejected => self.state = PointerState::Out,
        }
        outcome
    }

    /// The frames that take Windows from its current state to `sample`'s.
    fn plan(&self, sample: PenSample) -> Vec<PenSample> {
        let at = |phase, pressure, from: PenSample| PenSample {
            phase,
            pressure,
            ..from
        };
        match (sample.phase, self.state) {
            // A new contact or hover while one is held: end the old one where
            // Windows last saw it.
            (PenPhase::Down | PenPhase::Hover, PointerState::Contact(last)) => {
                vec![at(PenPhase::Cancel, 0, last), sample]
            }
            (PenPhase::Down | PenPhase::Hover, _) => vec![sample],
            (PenPhase::Move, PointerState::Contact(_)) => vec![sample],
            // No contact to update (it was cancelled or never started):
            // start one here.
            (PenPhase::Move, _) => vec![at(PenPhase::Down, sample.pressure, sample)],
            (PenPhase::Up, PointerState::Contact(last)) => {
                if (last.x, last.y) == (sample.x, sample.y) {
                    vec![sample]
                } else {
                    // Windows requires UP at the last updated position.
                    vec![at(PenPhase::Move, last.pressure, sample), sample]
                }
            }
            (PenPhase::Cancel, PointerState::Contact(last)) => vec![at(PenPhase::Cancel, 0, last)],
            (PenPhase::Leave, PointerState::Hover(last)) => vec![at(PenPhase::Leave, 0, last)],
            (PenPhase::Leave, PointerState::Contact(last)) => vec![at(PenPhase::Cancel, 0, last)],
            // Nothing is in contact or in range: there is nothing to end.
            (PenPhase::Up | PenPhase::Cancel | PenPhase::Leave, _) => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pen(phase: PenPhase, x: i32, y: i32) -> PenSample {
        PenSample {
            phase,
            x,
            y,
            x_norm: (x - OUTPUT.left) as u16,
            y_norm: (y - OUTPUT.top) as u16,
            pressure: if matches!(phase, PenPhase::Down | PenPhase::Move) {
                512
            } else {
                0
            },
            tilt_x: 0,
            tilt_y: 0,
        }
    }

    /// A capture rect on which wire positions are desktop pixels offset by
    /// its origin, with room for negative desktop coordinates.
    const OUTPUT: CaptureGeometry = CaptureGeometry {
        left: -8192,
        top: -8192,
        width: 65_536,
        height: 65_536,
    };
    const SCREEN: VirtualScreen = VirtualScreen {
        left: -8192,
        top: -8192,
        width: 65_536,
        height: 65_536,
    };

    /// Microsoft's documented transitions for one injected pointer
    /// (`InjectTouchInput`'s table, which synthetic pen injection shares),
    /// keyed on the flags of the frame Windows receives. Written from the
    /// documentation rather than from `PenContact`, so it checks the planner.
    /// Button and primary flags do not affect the transition.
    const TRANSITIONS: &[(PointerKind, u32, PointerKind)] = {
        use pointer_flag::*;
        use PointerKind::{Contact, Hover, Out};
        &[
            (Out, DOWN | INRANGE | INCONTACT, Contact),
            (Out, UPDATE | INRANGE, Hover),
            (Hover, UPDATE | INRANGE, Hover),
            (Hover, DOWN | INRANGE | INCONTACT, Contact),
            (Hover, UPDATE, Out),
            (Contact, UPDATE | INRANGE | INCONTACT, Contact),
            // An up must be at the last updated location; see `inject`.
            (Contact, UP, Out),
            (Contact, UP | INRANGE, Hover),
            (Contact, UP | CANCELED, Out),
        ]
    };

    /// Windows as documented: a frame in the table is applied, anything else
    /// is rejected and cancels the contact. Tests can script NOT_READY
    /// results and cancel the contact behind the injector's back (a display
    /// change). Positions are where Windows resolves each frame's location
    /// on the virtual screen current at injection.
    struct FakeWindows {
        state: PointerKind,
        /// Desktop pixel of the last applied frame.
        at: (i32, i32),
        screen: VirtualScreen,
        not_ready: usize,
        frames: Vec<(PenPhase, i32, i32, Outcome)>,
        illegal: Vec<(PenPhase, PointerKind)>,
    }

    impl Default for FakeWindows {
        fn default() -> Self {
            Self {
                state: PointerKind::Out,
                at: (0, 0),
                screen: SCREEN,
                not_ready: 0,
                frames: Vec::new(),
                illegal: Vec::new(),
            }
        }
    }

    impl FakeWindows {
        fn inject(&mut self, frame: PenSample, from: PointerKind) -> Outcome {
            assert_eq!(from, self.belief_of(from), "injector state diverged");
            let pointer = PointerFrame::new(frame, self.screen);
            let at = (
                pointer.location.0 + self.screen.left,
                pointer.location.1 + self.screen.top,
            );
            let flags = pointer.flags & !(pointer_flag::PRIMARY | pointer_flag::FIRSTBUTTON);
            let next = TRANSITIONS
                .iter()
                .find(|&&(state, f, _)| state == self.state && f == flags)
                .map(|&(_, _, next)| next)
                .filter(|_| flags & pointer_flag::UP == 0 || at == self.at);
            let outcome = if self.not_ready > 0 {
                self.not_ready -= 1;
                Outcome::NotReady
            } else if let Some(next) = next {
                self.state = next;
                self.at = at;
                Outcome::Injected
            } else {
                self.illegal.push((frame.phase, self.state));
                self.state = PointerKind::Out;
                Outcome::Rejected
            };
            self.frames.push((frame.phase, at.0, at.1, outcome));
            outcome
        }

        /// Windows ends the contact without telling the injector.
        fn cancel(&mut self) {
            self.state = PointerKind::Out;
        }

        /// The injector may only lag Windows after a silent cancellation.
        fn belief_of(&self, injector: PointerKind) -> PointerKind {
            if self.state == PointerKind::Out {
                injector
            } else {
                self.state
            }
        }

        fn take(&mut self) -> Vec<(PenPhase, i32, i32, Outcome)> {
            std::mem::take(&mut self.frames)
        }
    }

    fn deliver(contact: &mut PenContact, windows: &mut FakeWindows, sample: PenSample) {
        contact.deliver(sample, Some(OUTPUT), &mut |frame, from| {
            windows.inject(frame, from)
        });
    }

    use Outcome::{Injected, NotReady, Rejected};
    use PenPhase::{Cancel, Down, Hover, Leave, Move, Up};

    #[test]
    fn a_normal_stroke_injects_exactly_its_samples() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        for sample in [
            pen(Hover, 10, 10),
            pen(Down, 10, 10),
            pen(Move, 12, 10),
            pen(Up, 12, 10),
            pen(Hover, 12, 11),
            pen(Leave, 12, 11),
        ] {
            deliver(&mut contact, &mut windows, sample);
        }
        assert_eq!(
            windows.take(),
            [
                (Hover, 10, 10, Injected),
                (Down, 10, 10, Injected),
                (Move, 12, 10, Injected),
                (Up, 12, 10, Injected),
                (Hover, 12, 11, Injected),
                (Leave, 12, 11, Injected),
            ]
        );
        assert_eq!(contact.state(), PointerState::Out);
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn a_cancelled_contact_resumes_at_the_same_sample() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        // A display change cancels the contact without telling us.
        windows.cancel();
        windows.take();
        deliver(&mut contact, &mut windows, pen(Move, 20, 10));
        assert_eq!(
            windows.take(),
            [(Move, 20, 10, Rejected), (Down, 20, 10, Injected)]
        );
        deliver(&mut contact, &mut windows, pen(Move, 21, 10));
        deliver(&mut contact, &mut windows, pen(Up, 21, 10));
        assert_eq!(
            windows.take(),
            [(Move, 21, 10, Injected), (Up, 21, 10, Injected)]
        );
    }

    #[test]
    fn a_rejected_resume_waits_for_the_next_sample_without_looping() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.cancel();
        windows.take();
        let mut refuse = |frame: PenSample, _| {
            windows
                .frames
                .push((frame.phase, frame.x, frame.y, Rejected));
            Rejected
        };
        contact.deliver(pen(Move, 20, 10), Some(OUTPUT), &mut refuse);
        assert_eq!(
            windows.take(),
            [(Move, 20, 10, Rejected), (Down, 20, 10, Rejected)]
        );
        assert_eq!(contact.state(), PointerState::Out);
        deliver(&mut contact, &mut windows, pen(Move, 22, 10));
        assert_eq!(windows.take(), [(Down, 22, 10, Injected)]);
    }

    #[test]
    fn not_ready_during_resume_keeps_the_replacement_down_pending() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.cancel();
        // The move is rejected, then the replacement DOWN is not ready.
        windows.take();
        let mut first = true;
        contact.deliver(pen(Move, 20, 10), Some(OUTPUT), &mut |frame, from| {
            if first {
                first = false;
                windows.inject(frame, from)
            } else {
                windows
                    .frames
                    .push((frame.phase, frame.x, frame.y, NotReady));
                NotReady
            }
        });
        assert_eq!(contact.state(), PointerState::Out);
        // The move waits; planned again from Out, it retries the same down.
        assert_eq!(contact.pending().map(|p| p.phase), Some(Move));
        // A refresh tick retries the same frame; the stroke then continues.
        contact.flush(Some(OUTPUT), &mut |frame, from| windows.inject(frame, from));
        deliver(&mut contact, &mut windows, pen(Move, 21, 10));
        deliver(&mut contact, &mut windows, pen(Up, 21, 10));
        assert_eq!(
            windows.take(),
            [
                (Move, 20, 10, Rejected),
                (Down, 20, 10, NotReady),
                (Down, 20, 10, Injected),
                (Move, 21, 10, Injected),
                (Up, 21, 10, Injected),
            ]
        );
        assert_eq!(windows.illegal, [(Move, PointerKind::Out)]);
    }

    #[test]
    fn not_ready_on_an_edge_retries_that_frame_before_the_next_sample() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.not_ready = 1;
        deliver(&mut contact, &mut windows, pen(Up, 10, 10));
        assert_eq!(contact.state().kind(), PointerKind::Contact);
        // The next stroke must not start until the up has landed.
        deliver(&mut contact, &mut windows, pen(Down, 30, 30));
        windows.not_ready = 1;
        deliver(&mut contact, &mut windows, pen(Up, 30, 30));
        // Still unready when the next stroke arrives: the pending up is
        // retried first and stays pending; the new down waits behind it.
        windows.not_ready = 1;
        deliver(&mut contact, &mut windows, pen(Down, 40, 40));
        assert_eq!(contact.pending().map(|p| p.phase), Some(Up));
        contact.flush(Some(OUTPUT), &mut |frame, from| windows.inject(frame, from));
        assert_eq!(
            windows.take(),
            [
                (Down, 10, 10, Injected),
                (Up, 10, 10, NotReady),
                (Up, 10, 10, Injected),
                (Down, 30, 30, Injected),
                (Up, 30, 30, NotReady),
                (Up, 30, 30, NotReady),
                (Up, 30, 30, Injected),
                (Down, 40, 40, Injected),
            ]
        );
        assert!(windows.illegal.is_empty());
        assert_eq!(contact.state().kind(), PointerKind::Contact);
        assert!(contact.pending().is_none());
    }

    #[test]
    fn release_while_lost_injects_nothing() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        for sample in [pen(Up, 5, 5), pen(Cancel, 5, 5), pen(Leave, 5, 5)] {
            deliver(&mut contact, &mut windows, sample);
        }
        deliver(&mut contact, &mut windows, pen(Down, 5, 5));
        windows.cancel();
        windows.take();
        let mut refuse = |_: PenSample, _| Rejected;
        contact.deliver(pen(Move, 6, 5), Some(OUTPUT), &mut refuse);
        deliver(&mut contact, &mut windows, pen(Up, 6, 5));
        deliver(&mut contact, &mut windows, pen(Cancel, 6, 5));
        assert!(windows.take().is_empty());
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn geometry_change_mid_stroke_and_during_keepalive_follows_the_new_position() {
        // The relay remaps a held contact against the current capture rect
        // before refreshing it; the injector must move there, and resume
        // there if the display change cancelled the contact.
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 3739, 819));
        deliver(&mut contact, &mut windows, pen(Move, 3739, 819));
        windows.cancel();
        windows.take();
        deliver(&mut contact, &mut windows, pen(Move, -1181, 819));
        assert_eq!(
            windows.take(),
            [(Move, -1181, 819, Rejected), (Down, -1181, 819, Injected)]
        );
        deliver(&mut contact, &mut windows, pen(Move, -1181, 819));
        // An up reported elsewhere is preceded by a move to it.
        deliver(&mut contact, &mut windows, pen(Up, -1100, 800));
        assert_eq!(
            windows.take(),
            [
                (Move, -1181, 819, Injected),
                (Move, -1100, 800, Injected),
                (Up, -1100, 800, Injected),
            ]
        );
        assert_eq!(windows.illegal, [(Move, PointerKind::Out)]);
    }

    #[test]
    fn a_new_stroke_or_hover_over_a_held_contact_cancels_it_first() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 1, 1));
        deliver(&mut contact, &mut windows, pen(Down, 9, 9));
        deliver(&mut contact, &mut windows, pen(Hover, 9, 9));
        deliver(&mut contact, &mut windows, pen(Leave, 9, 9));
        // Leave without any hover (a lost hover datagram) is skipped.
        deliver(&mut contact, &mut windows, pen(Leave, 9, 9));
        assert_eq!(
            windows.take(),
            [
                (Down, 1, 1, Injected),
                (Cancel, 1, 1, Injected),
                (Down, 9, 9, Injected),
                (Cancel, 9, 9, Injected),
                (Hover, 9, 9, Injected),
                (Leave, 9, 9, Injected),
            ]
        );
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn frames_carry_the_documented_flags_relative_to_the_virtual_screen() {
        use pointer_flag::*;
        let screen = VirtualScreen {
            left: -2732,
            top: -2048,
            width: 6172,
            height: 3488,
        };
        let flags = |phase| {
            let frame = PointerFrame::new(pen(phase, -1, 0), screen);
            assert_eq!(frame.location, (2731, 2048));
            frame.flags & !PRIMARY
        };
        assert_eq!(flags(Down), DOWN | INRANGE | INCONTACT | FIRSTBUTTON);
        assert_eq!(flags(Move), UPDATE | INRANGE | INCONTACT | FIRSTBUTTON);
        assert_eq!(flags(Up), UP);
        assert_eq!(flags(Cancel), UP | CANCELED);
        assert_eq!(flags(Hover), UPDATE | INRANGE);
        assert_eq!(flags(Leave), UPDATE);
        let frame = PointerFrame::new(pen(Down, 0, 0), screen);
        assert_ne!(frame.flags & PRIMARY, 0);
        assert_eq!((frame.pressure, frame.tilt_x, frame.tilt_y), (512, 0, 0));
    }

    #[test]
    fn a_release_survives_a_deferred_final_position_update() {
        // The tip lifted away from the last move: the up needs a move first,
        // and Windows is not ready for that move.
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.not_ready = 1;
        deliver(&mut contact, &mut windows, pen(Up, 20, 10));
        assert_eq!(contact.pending().map(|p| p.phase), Some(Up));
        // The relay holds no pen now; only the refresh tick finishes it.
        contact.flush(Some(OUTPUT), &mut |frame, from| windows.inject(frame, from));
        assert_eq!(
            windows.take(),
            [
                (Down, 10, 10, Injected),
                (Move, 20, 10, NotReady),
                (Move, 20, 10, Injected),
                (Up, 20, 10, Injected),
            ]
        );
        assert_eq!(contact.state(), PointerState::Out);
        assert!(contact.pending().is_none());
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn a_later_stroke_cannot_discard_the_release_windows_is_owed() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        for sample in [pen(Up, 10, 10), pen(Down, 20, 10), pen(Up, 20, 10)] {
            windows.not_ready = 1;
            deliver(&mut contact, &mut windows, sample);
        }
        contact.flush(Some(OUTPUT), &mut |frame, from| windows.inject(frame, from));
        assert_eq!(
            windows.take(),
            [
                (Down, 10, 10, Injected),
                (Up, 10, 10, NotReady),
                (Up, 10, 10, NotReady),
                (Up, 10, 10, NotReady),
                (Up, 10, 10, Injected),
                (Down, 20, 10, Injected),
                (Up, 20, 10, Injected),
            ]
        );
        assert_eq!(contact.state(), PointerState::Out);
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn a_full_queue_keeps_the_owed_release_and_the_latest_sample() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        deliver(&mut contact, &mut windows, pen(Down, 10, 10));
        windows.take();
        windows.not_ready = usize::MAX;
        // A move waits ahead of the release; neither may be dropped.
        deliver(&mut contact, &mut windows, pen(Move, 12, 10));
        deliver(&mut contact, &mut windows, pen(Up, 12, 10));
        for x in 20..40 {
            deliver(&mut contact, &mut windows, pen(Down, x, 10));
            deliver(&mut contact, &mut windows, pen(Move, x, 11));
            deliver(&mut contact, &mut windows, pen(Up, x, 11));
            assert!(contact.queue.len() <= QUEUE_LIMIT);
        }
        windows.not_ready = 0;
        windows.take();
        contact.flush(Some(OUTPUT), &mut |frame, from| windows.inject(frame, from));
        let frames = windows.take();
        assert_eq!(
            frames[..2],
            [(Move, 12, 10, Injected), (Up, 12, 10, Injected)]
        );
        assert_eq!(frames.last(), Some(&(Up, 39, 11, Injected)));
        assert_eq!(contact.state(), PointerState::Out);
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn waiting_samples_land_where_the_display_is_when_they_are_sent() {
        // The reviewer's case: a 2360x1640 output moves from left=2560 to
        // left=-2360 while a down, and later an up, wait for Windows.
        let before = CaptureGeometry {
            left: 2560,
            top: 0,
            width: 2360,
            height: 1640,
        };
        let after = CaptureGeometry {
            left: -2360,
            ..before
        };
        let at = |phase, output| {
            let (x, y) = super::super::norm_to_output_pixel(32_768, 32_768, output);
            PenSample {
                x_norm: 32_768,
                y_norm: 32_768,
                ..pen(phase, x, y)
            }
        };
        assert_eq!((at(Down, before).x, at(Down, after).x), (3739, -1181));
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        windows.not_ready = 1;
        contact.deliver(at(Down, before), Some(before), &mut |frame, from| {
            windows.inject(frame, from)
        });
        // The keepalive flushes onto the new rect, then refreshes the hold.
        contact.flush(Some(after), &mut |frame, from| windows.inject(frame, from));
        contact.deliver(at(Move, after), Some(after), &mut |frame, from| {
            windows.inject(frame, from)
        });
        // An up waiting across the move back is sent where the contact is
        // now, after a move to it.
        windows.not_ready = 1;
        contact.deliver(at(Up, after), Some(after), &mut |frame, from| {
            windows.inject(frame, from)
        });
        contact.flush(Some(before), &mut |frame, from| windows.inject(frame, from));
        assert_eq!(
            windows.take(),
            [
                (Down, 3739, 819, NotReady),
                (Down, -1181, 819, Injected),
                (Move, -1181, 819, Injected),
                (Up, -1181, 819, NotReady),
                (Move, 3739, 819, Injected),
                (Up, 3739, 819, Injected),
            ]
        );
        assert!(windows.illegal.is_empty());
    }

    #[test]
    fn random_strokes_end_every_contact_in_order_on_the_current_display() {
        // Relay-shaped streams with Windows not ready at random, refresh
        // ticks between samples and the display moving or changing mode.
        let outputs = [
            OUTPUT,
            CaptureGeometry {
                left: 2560,
                top: 0,
                width: 2360,
                height: 1640,
            },
            CaptureGeometry {
                left: -2732,
                top: -2048,
                width: 2732,
                height: 2048,
            },
        ];
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut random = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..2000 {
            let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
            let mut output = outputs[0];
            let mut accepted = Vec::new();
            let mut intent = PointerKind::Out;
            let mut last = None;
            for index in 0..random(40) as u16 {
                if random(10) == 0 {
                    output = outputs[random(3) as usize];
                }
                use PointerKind as K;
                let phase = match (intent, random(10)) {
                    (K::Out, 0..=5) | (K::Hover, 0..=3) => Down,
                    (K::Out | K::Hover, 0..=7) => Hover,
                    (K::Hover, _) => Leave,
                    (K::Out, _) => Hover,
                    (K::Contact, 0..=5) => Move,
                    (K::Contact, 6..=8) => Up,
                    (K::Contact, _) => Cancel,
                };
                intent = match phase {
                    Down | Move => K::Contact,
                    Hover => K::Hover,
                    Up | Cancel | Leave => K::Out,
                };
                // The index records each sample's order in y_norm.
                let (x_norm, y_norm) = (random(65_536) as u16, index);
                let (x, y) = super::super::norm_to_output_pixel(x_norm, y_norm, output);
                let sample = PenSample {
                    x_norm,
                    y_norm,
                    ..pen(phase, x, y)
                };
                last = Some(sample);
                let tick = random(2) == 0;
                let mut inject = |frame: PenSample, from| {
                    windows.not_ready = usize::from(random(3) == 0);
                    let outcome = windows.inject(frame, from);
                    if !matches!(frame.phase, Cancel | Leave) {
                        assert_eq!(
                            (frame.x, frame.y),
                            super::super::norm_to_output_pixel(frame.x_norm, frame.y_norm, output),
                            "{frame:?} was not mapped onto {output:?}"
                        );
                    }
                    if outcome == Injected {
                        accepted.push(frame);
                    }
                    outcome
                };
                contact.deliver(sample, Some(output), &mut inject);
                if tick {
                    contact.flush(Some(output), &mut inject);
                }
            }
            windows.not_ready = 0;
            contact.flush(Some(output), &mut |frame, from| {
                let outcome = windows.inject(frame, from);
                if outcome == Injected {
                    accepted.push(frame);
                }
                outcome
            });
            assert!(windows.illegal.is_empty(), "{:?}", windows.illegal);
            assert!(contact.pending().is_none());
            // Every contact Windows accepted ended exactly once; frames went
            // out in sample order, and ends sit where Windows had the pen.
            let mut open = false;
            let mut previous: Option<PenSample> = None;
            for frame in &accepted {
                match frame.phase {
                    Down => assert!(!std::mem::replace(&mut open, true)),
                    Up | Cancel => assert!(std::mem::replace(&mut open, false)),
                    _ => {}
                }
                if let Some(previous) = previous {
                    if matches!(frame.phase, Cancel | Leave) {
                        assert_eq!(frame.y_norm, previous.y_norm);
                    } else {
                        assert!(frame.y_norm >= previous.y_norm, "{accepted:?}");
                    }
                }
                previous = Some(*frame);
            }
            assert_eq!(windows.state, intent);
            if let (Some(last), true) = (last, intent != PointerKind::Out) {
                assert_eq!(windows.at, (last.x, last.y));
            }
        }
    }
}
