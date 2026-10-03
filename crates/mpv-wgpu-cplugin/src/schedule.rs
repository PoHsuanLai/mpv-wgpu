//! Which slot to render into and when: the plugin's bookkeeping, free of FFI.

use mpv_wgpu_protocol::SLOTS;

/// What mpv's render context says about the next frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Frame {
    /// No update, or no frame is present.
    Absent,
    /// The same frame as last time; nothing to draw, only a swap to report.
    Repeat,
    /// The same frame, drawn again.
    Redraw,
    /// A new frame.
    New,
}

/// What the plugin should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Nothing.
    Nothing,
    /// Report the swap without drawing.
    SwapOnly,
    /// Draw into a free slot, if there is one.
    Render,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owed {
    Nothing,
    Resize,
    Frame,
}

/// A draw that is owed, and the count of frames given up for want of a slot.
#[derive(Debug)]
pub(crate) struct Schedule {
    owed: Owed,
    dropped: u64,
}

impl Schedule {
    pub(crate) fn new() -> Self {
        Self {
            owed: Owed::Nothing,
            dropped: 0,
        }
    }

    /// A new ring arrived: the current picture must be drawn at the new size.
    pub(crate) fn resized(&mut self) {
        if self.owed == Owed::Nothing {
            self.owed = Owed::Resize;
        }
    }

    /// Decide from the latest `frame`. A frame that arrives while the previous
    /// one is still waiting for a slot replaces it, and the old one is dropped.
    pub(crate) fn plan(&mut self, frame: Frame) -> Action {
        match frame {
            Frame::New => {
                if self.owed == Owed::Frame {
                    self.dropped += 1;
                }
                self.owed = Owed::Frame;
            }
            Frame::Redraw => {
                if self.owed == Owed::Nothing {
                    self.owed = Owed::Frame;
                }
            }
            Frame::Repeat if self.owed == Owed::Nothing => return Action::SwapOnly,
            Frame::Repeat | Frame::Absent => {}
        }
        if self.owed == Owed::Nothing {
            Action::Nothing
        } else {
            Action::Render
        }
    }

    /// The owed draw was done.
    pub(crate) fn rendered(&mut self) {
        self.owed = Owed::Nothing;
    }

    /// Frames given up so far.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// Which slots of the current ring mpv may write.
#[derive(Debug)]
pub(crate) struct Slots {
    generation: u32,
    count: u8,
    busy: [bool; SLOTS as usize],
}

impl Slots {
    pub(crate) fn new() -> Self {
        Self {
            generation: 0,
            count: 0,
            busy: [false; SLOTS as usize],
        }
    }

    /// A new ring: every slot is free again.
    pub(crate) fn reset(&mut self, generation: u32, count: u8) {
        self.generation = generation;
        self.count = count.min(SLOTS);
        self.busy = [false; SLOTS as usize];
    }

    pub(crate) fn generation(&self) -> u32 {
        self.generation
    }

    /// Take a free slot, lowest index first.
    pub(crate) fn acquire(&mut self) -> Option<u8> {
        let index = (0..self.count).find(|&i| !self.busy[usize::from(i)])?;
        self.busy[usize::from(index)] = true;
        Some(index)
    }

    /// Free `slot` of `generation`. False when it names an older ring or a slot that is out of range.
    pub(crate) fn release(&mut self, generation: u32, slot: u8) -> bool {
        if generation != self.generation || slot >= self.count {
            return false;
        }
        self.busy[usize::from(slot)] = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_hand_out_each_index_once_until_released() {
        let mut slots = Slots::new();
        assert_eq!(slots.acquire(), None, "no ring yet");
        slots.reset(4, 3);
        assert_eq!(slots.acquire(), Some(0));
        assert_eq!(slots.acquire(), Some(1));
        assert_eq!(slots.acquire(), Some(2));
        assert_eq!(slots.acquire(), None);
        assert!(slots.release(4, 1));
        assert_eq!(slots.acquire(), Some(1));
    }

    #[test]
    fn a_stale_or_out_of_range_release_is_ignored() {
        let mut slots = Slots::new();
        slots.reset(2, 3);
        assert_eq!(slots.acquire(), Some(0));
        assert!(!slots.release(1, 0), "older generation");
        assert!(!slots.release(2, 3), "out of range");
        assert_eq!(slots.acquire(), Some(1));
        assert!(slots.release(2, 0));
        assert_eq!(slots.generation(), 2);
    }

    #[test]
    fn a_new_ring_frees_every_slot() {
        let mut slots = Slots::new();
        slots.reset(1, 3);
        for _ in 0..3 {
            assert!(slots.acquire().is_some());
        }
        slots.reset(2, 3);
        assert_eq!(slots.acquire(), Some(0));
        slots.reset(3, 9);
        assert_eq!(slots.acquire(), Some(0), "count is capped at SLOTS");
    }

    #[test]
    fn frames_map_to_actions() {
        let mut schedule = Schedule::new();
        assert_eq!(schedule.plan(Frame::Absent), Action::Nothing);
        assert_eq!(schedule.plan(Frame::Repeat), Action::SwapOnly);
        assert_eq!(schedule.plan(Frame::New), Action::Render);
        schedule.rendered();
        assert_eq!(schedule.plan(Frame::Absent), Action::Nothing);
        assert_eq!(schedule.plan(Frame::Redraw), Action::Render);
        schedule.rendered();
        assert_eq!(schedule.dropped(), 0);
    }

    #[test]
    fn a_resize_owes_a_draw_even_without_a_frame() {
        let mut schedule = Schedule::new();
        schedule.resized();
        assert_eq!(schedule.plan(Frame::Absent), Action::Render);
        assert_eq!(schedule.plan(Frame::Repeat), Action::Render);
        schedule.rendered();
        assert_eq!(schedule.plan(Frame::Repeat), Action::SwapOnly);
    }

    #[test]
    fn a_frame_waiting_for_a_slot_is_dropped_when_the_next_arrives() {
        let mut schedule = Schedule::new();
        assert_eq!(schedule.plan(Frame::New), Action::Render);
        // No slot was free; nothing was drawn. Another frame comes in.
        assert_eq!(schedule.plan(Frame::New), Action::Render);
        assert_eq!(schedule.dropped(), 1);
        // A slot frees up and the owed draw is retried without a new frame.
        assert_eq!(schedule.plan(Frame::Absent), Action::Render);
        schedule.rendered();
        assert_eq!(schedule.plan(Frame::Absent), Action::Nothing);
        assert_eq!(schedule.dropped(), 1);
    }
}
