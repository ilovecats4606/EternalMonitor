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

use super::{PenPhase, PenSample, VirtualScreen};

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
    /// A state-changing frame that hit ERROR_NOT_READY. It is retried, as
    /// the same frame, before anything else.
    pending: Option<PenSample>,
}

impl PenContact {
    #[cfg(test)]
    pub fn state(&self) -> PointerState {
        self.state
    }

    pub fn pending(&self) -> Option<PenSample> {
        self.pending
    }

    /// Bring Windows to the state `sample` asks for. `inject` performs one
    /// frame and reports its outcome, given the state the frame starts from.
    pub fn deliver(
        &mut self,
        sample: PenSample,
        inject: &mut impl FnMut(PenSample, PointerKind) -> Outcome,
    ) {
        if self.pending.is_some() && self.flush(inject) == Some(Outcome::NotReady) {
            // Windows is still not ready. A frame planned now would start from
            // the wrong state; the refresh tick or the next sample retries.
            if !matches!(sample.phase, PenPhase::Move | PenPhase::Hover) {
                self.pending = Some(self.merge_pending(sample));
            }
            return;
        }
        // A rejection cancels the contact; resume it from this same, freshly
        // mapped sample once rather than waiting for the next one.
        for _ in 0..2 {
            let mut rejected = false;
            for frame in self.plan(sample) {
                match self.send(frame, inject) {
                    Outcome::Injected => {}
                    Outcome::NotReady => return,
                    Outcome::Rejected => {
                        rejected = true;
                        break;
                    }
                }
            }
            if !rejected || !matches!(sample.phase, PenPhase::Move | PenPhase::Hover) {
                return;
            }
        }
    }

    /// Retry a pending frame. Returns its outcome, or `None` with nothing
    /// pending. Call on every refresh tick so an end of contact is not left
    /// waiting for the next Pencil sample.
    pub fn flush(
        &mut self,
        inject: &mut impl FnMut(PenSample, PointerKind) -> Outcome,
    ) -> Option<Outcome> {
        let frame = self.pending.take()?;
        if !self.valid(frame) {
            // Superseded: a later accepted frame already moved Windows on.
            return None;
        }
        Some(self.send(frame, inject))
    }

    /// While Windows refuses everything, keep only the latest edge, but never
    /// replace a pending end of contact with the start of another.
    fn merge_pending(&self, sample: PenSample) -> PenSample {
        match (self.pending, sample.phase) {
            (Some(pending), PenPhase::Down | PenPhase::Hover)
                if matches!(pending.phase, PenPhase::Up | PenPhase::Cancel) =>
            {
                pending
            }
            _ => sample,
        }
    }

    fn send(
        &mut self,
        frame: PenSample,
        inject: &mut impl FnMut(PenSample, PointerKind) -> Outcome,
    ) -> Outcome {
        let outcome = inject(frame, self.state.kind());
        match outcome {
            Outcome::Injected => {
                self.pending = None;
                self.state = match frame.phase {
                    PenPhase::Down | PenPhase::Move => PointerState::Contact(frame),
                    PenPhase::Hover => PointerState::Hover(frame),
                    PenPhase::Up | PenPhase::Cancel | PenPhase::Leave => PointerState::Out,
                };
            }
            // Moves and hovers are superseded by the next sample or refresh.
            Outcome::NotReady => {
                if !matches!(frame.phase, PenPhase::Move | PenPhase::Hover) {
                    self.pending = Some(frame);
                }
            }
            Outcome::Rejected => {
                self.pending = None;
                self.state = PointerState::Out;
            }
        }
        outcome
    }

    /// Whether `frame` is a legal transition from the current state.
    fn valid(&self, frame: PenSample) -> bool {
        match (self.state, frame.phase) {
            (PointerState::Out | PointerState::Hover(_), PenPhase::Down | PenPhase::Hover) => true,
            (PointerState::Hover(_), PenPhase::Leave) => true,
            (PointerState::Contact(_), PenPhase::Move) => true,
            (PointerState::Contact(last), PenPhase::Up | PenPhase::Cancel) => {
                (last.x, last.y) == (frame.x, frame.y)
            }
            _ => false,
        }
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
            x_norm: 0,
            y_norm: 0,
            pressure: if matches!(phase, PenPhase::Down | PenPhase::Move) {
                512
            } else {
                0
            },
            tilt_x: 0,
            tilt_y: 0,
        }
    }

    /// Windows as documented: legal transitions are applied, anything else is
    /// rejected and cancels the contact. Tests can script NOT_READY results
    /// and cancel the contact behind the injector's back (a display change).
    #[derive(Default)]
    struct FakeWindows {
        state: PointerState,
        not_ready: usize,
        frames: Vec<(PenPhase, i32, i32, Outcome)>,
        illegal: Vec<(PenPhase, PointerKind)>,
    }

    impl FakeWindows {
        fn inject(&mut self, frame: PenSample, from: PointerKind) -> Outcome {
            assert_eq!(from, self.belief_of(from), "injector state diverged");
            let outcome = if self.not_ready > 0 {
                self.not_ready -= 1;
                Outcome::NotReady
            } else if (PenContact {
                state: self.state,
                pending: None,
            })
            .valid(frame)
            {
                self.state = match frame.phase {
                    PenPhase::Down | PenPhase::Move => PointerState::Contact(frame),
                    PenPhase::Hover => PointerState::Hover(frame),
                    _ => PointerState::Out,
                };
                Outcome::Injected
            } else {
                self.illegal.push((frame.phase, self.state.kind()));
                self.state = PointerState::Out;
                Outcome::Rejected
            };
            self.frames.push((frame.phase, frame.x, frame.y, outcome));
            outcome
        }

        /// The injector may only lag Windows after a silent cancellation.
        fn belief_of(&self, injector: PointerKind) -> PointerKind {
            if self.state == PointerState::Out {
                injector
            } else {
                self.state.kind()
            }
        }

        fn take(&mut self) -> Vec<(PenPhase, i32, i32, Outcome)> {
            std::mem::take(&mut self.frames)
        }
    }

    fn deliver(contact: &mut PenContact, windows: &mut FakeWindows, sample: PenSample) {
        contact.deliver(sample, &mut |frame, from| windows.inject(frame, from));
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
        windows.state = PointerState::Out;
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
        windows.state = PointerState::Out;
        windows.take();
        let mut refuse = |frame: PenSample, _| {
            windows
                .frames
                .push((frame.phase, frame.x, frame.y, Rejected));
            Rejected
        };
        contact.deliver(pen(Move, 20, 10), &mut refuse);
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
        windows.state = PointerState::Out;
        // The move is rejected, then the replacement DOWN is not ready.
        windows.take();
        let mut first = true;
        contact.deliver(pen(Move, 20, 10), &mut |frame, from| {
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
        assert_eq!(contact.pending().map(|p| p.phase), Some(Down));
        // A refresh tick retries the same frame; the stroke then continues.
        contact.flush(&mut |frame, from| windows.inject(frame, from));
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
        // retried first and stays pending; the new down cannot start over it.
        windows.not_ready = 1;
        deliver(&mut contact, &mut windows, pen(Down, 40, 40));
        assert_eq!(contact.pending().map(|p| p.phase), Some(Up));
        contact.flush(&mut |frame, from| windows.inject(frame, from));
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
            ]
        );
        assert!(windows.illegal.is_empty());
        assert_eq!(contact.state(), PointerState::Out);
    }

    #[test]
    fn release_while_lost_injects_nothing() {
        let (mut contact, mut windows) = (PenContact::default(), FakeWindows::default());
        for sample in [pen(Up, 5, 5), pen(Cancel, 5, 5), pen(Leave, 5, 5)] {
            deliver(&mut contact, &mut windows, sample);
        }
        deliver(&mut contact, &mut windows, pen(Down, 5, 5));
        windows.state = PointerState::Out;
        windows.take();
        let mut refuse = |_: PenSample, _| Rejected;
        contact.deliver(pen(Move, 6, 5), &mut refuse);
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
        windows.state = PointerState::Out;
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
}
