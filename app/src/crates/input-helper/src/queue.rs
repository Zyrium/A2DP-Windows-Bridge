//! The core's queue of input for the helper, and its record of what may be
//! held down in Windows.
//!
//! The BTstack thread pushes ([`SendQueue::push`], a short lock, never a
//! wait); the core's sender thread takes frames ([`SendQueue::take`]) and
//! writes them to the helper. Rules:
//!
//! - Key and button transitions are never dropped one by one: a lost key-up
//!   leaves a key held in Windows, a lost key-down only loses a keystroke
//!   but breaks the pairing of downs and ups. When the queue is full of
//!   transitions (the helper stopped reading), everything queued is
//!   dropped at once and replaced by a release of everything held
//!   ([`Item::ReleaseAll`]), so nothing stays down.
//! - Pointer moves and wheel turns are merged with the one queued before
//!   them, and when the queue is full the oldest is dropped (counted).
//! - Without a helper (none connected yet, or it died) input is dropped
//!   and counted.
//!
//! What may be held in Windows ([`SendQueue::possibly_held`]) is what the
//! helper confirmed (acks), plus every key-down queued or written but not
//! confirmed yet. When a helper goes away, however it goes, the next one
//! first releases all of that ([`SendQueue::connect`]): a helper killed so
//! hard that its own exit path never ran leaves nothing held for good.

use std::collections::VecDeque;

use win_sys::hid_guard::HeldSet;
use win_sys::input_inject::Inject;

use crate::protocol::{self, MAX_RECORDS, ToHelper};

/// Items queued at most
pub const CAPACITY: usize = 256;
/// Items written and not confirmed at most: a helper that reads but never
/// confirms is treated as hung
pub const UNCONFIRMED_MAX: usize = 4096;

/// Where a queued input came from
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Origin {
    /// Not the user's input (releases)
    None,
    /// A report of this keyboard or mouse
    Device([u8; 6]),
    /// The sink's self-test
    SelfTest,
}

/// One queued item
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Input(Inject, Origin),
    ReleaseAll,
}

fn is_motion(i: &Inject) -> bool {
    matches!(i, Inject::Move { .. } | Inject::Wheel(_) | Inject::HWheel(_))
}

fn is_down(i: &Inject) -> bool {
    matches!(i, Inject::Key { up: false, .. } | Inject::VirtualKey { up: false, .. } | Inject::Button { up: false, .. })
}

/// Two motions as one, when they are of one kind and the sum stays in the
/// protocol's bounds
fn merge(a: Inject, b: Inject) -> Option<Inject> {
    let sum = |x: i32, y: i32, m: i32| x.checked_add(y).filter(|s| s.unsigned_abs() <= m as u32);
    match (a, b) {
        (Inject::Move { dx: ax, dy: ay }, Inject::Move { dx: bx, dy: by }) => {
            Some(Inject::Move { dx: sum(ax, bx, protocol::MAX_MOVE)?, dy: sum(ay, by, protocol::MAX_MOVE)? })
        }
        (Inject::Wheel(x), Inject::Wheel(y)) => Some(Inject::Wheel(sum(x, y, protocol::MAX_WHEEL)?)),
        (Inject::HWheel(x), Inject::HWheel(y)) => Some(Inject::HWheel(sum(x, y, protocol::MAX_WHEEL)?)),
        _ => None,
    }
}

/// What a push did, for the counters
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Pushed {
    /// Inputs that will never reach Windows
    pub dropped: u64,
    /// Motions merged into one queued before them
    pub merged: u64,
    /// The queue overflowed with transitions: all were dropped and a
    /// release of everything queued instead
    pub overflowed: bool,
}

#[derive(Debug, Default)]
pub struct SendQueue {
    items: VecDeque<Item>,
    /// A helper takes input
    connected: bool,
    /// Items written to the helper and not confirmed, with their numbers
    sent: VecDeque<(u64, Item)>,
    /// The number of the next item written (from 1 per connection)
    next: u64,
    /// What the helpers confirmed is held: applied in order as acks come
    confirmed: HeldSet,
}

impl SendQueue {
    pub fn connected(&self) -> bool {
        self.connected
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Items written and not confirmed yet
    pub fn unconfirmed(&self) -> usize {
        self.sent.len()
    }

    /// Queues `injects` in order (from the BTstack thread), as no one's
    pub fn push(&mut self, injects: &[Inject]) -> Pushed {
        self.push_from(Origin::None, injects)
    }

    /// Queues `injects` of `origin` in order. Motions of different origins
    /// are never merged, so that each input is confirmed as its origin's.
    pub fn push_from(&mut self, origin: Origin, injects: &[Inject]) -> Pushed {
        let mut out = Pushed::default();
        if !self.connected {
            out.dropped = injects.len() as u64;
            return out;
        }
        for &i in injects {
            let Some(i) = protocol::clamp(i) else {
                out.dropped += 1;
                continue;
            };
            if is_motion(&i)
                && let Some(Item::Input(last, o)) = self.items.back_mut()
                && *o == origin
                && let Some(m) = merge(*last, i)
            {
                *last = m;
                out.merged += 1;
                continue;
            }
            if self.items.len() >= CAPACITY {
                match self.items.iter().position(|q| matches!(q, Item::Input(m, _) if is_motion(m))) {
                    Some(at) => {
                        self.items.remove(at);
                        out.dropped += 1;
                    }
                    None if is_motion(&i) => {
                        out.dropped += 1;
                        continue;
                    }
                    None => {
                        // Only transitions queued: all go, a release of
                        // everything takes their place (what the helper
                        // holds stays in possibly_held until the release is
                        // confirmed; the downs dropped never reach anyone)
                        out.dropped += self.items.iter().filter(|q| matches!(q, Item::Input(..))).count() as u64;
                        self.items.clear();
                        self.items.push_back(Item::ReleaseAll);
                        out.overflowed = true;
                    }
                }
            }
            self.items.push_back(Item::Input(i, origin));
        }
        out
    }

    /// Queues a release of everything the helper holds
    pub fn push_release_all(&mut self) {
        if self.connected && self.items.back() != Some(&Item::ReleaseAll) {
            self.items.push_back(Item::ReleaseAll);
        }
    }

    /// The next frame for the helper (up to [`MAX_RECORDS`] inputs, or one
    /// release); its items count as written
    pub fn take(&mut self) -> Option<ToHelper> {
        let first = *self.items.front()?;
        if first == Item::ReleaseAll {
            self.items.pop_front();
            self.mark_sent(Item::ReleaseAll);
            return Some(ToHelper::ReleaseAll);
        }
        let mut batch = Vec::with_capacity(MAX_RECORDS);
        while batch.len() < MAX_RECORDS
            && let Some(Item::Input(i, o)) = self.items.front().copied()
        {
            self.items.pop_front();
            self.mark_sent(Item::Input(i, o));
            batch.push(i);
        }
        Some(ToHelper::Inject(batch))
    }

    fn mark_sent(&mut self, item: Item) {
        self.next += 1;
        self.sent.push_back((self.next, item));
    }

    /// The helper confirmed every item up to `processed`. False: it
    /// confirmed more than was written (a protocol violation).
    pub fn ack(&mut self, processed: u64) -> bool {
        self.ack_items(processed).is_some()
    }

    /// As [`SendQueue::ack`]; the origins of the inputs confirmed now, each
    /// once, in order. None: a protocol violation.
    pub fn ack_items(&mut self, processed: u64) -> Option<Vec<Origin>> {
        if processed > self.next {
            return None;
        }
        let mut origins = Vec::new();
        while self.sent.front().is_some_and(|(n, _)| *n <= processed) {
            let Some((_, item)) = self.sent.pop_front() else { break };
            match item {
                Item::Input(i, o) => {
                    self.confirmed.apply(&[i]);
                    if !origins.contains(&o) {
                        origins.push(o);
                    }
                }
                Item::ReleaseAll => self.confirmed = HeldSet::default(),
            }
        }
        Some(origins)
    }

    /// What may be held in Windows right now: what was confirmed held,
    /// plus every key-down written or queued but not confirmed. Conservative:
    /// a key released in between is released again, which does nothing.
    pub fn possibly_held(&self) -> HeldSet {
        let mut held = self.confirmed.clone();
        let pending = self.sent.iter().map(|(_, i)| i).chain(&self.items);
        let downs: Vec<Inject> =
            pending.filter_map(|i| if let Item::Input(d, _) = i { Some(*d) } else { None }).filter(is_down).collect();
        held.apply(&downs);
        held
    }

    /// The helper is gone (died, killed, stopped, or the session changed):
    /// input is dropped from now on; the queued items are dropped (their
    /// count returned). What may be held is kept for the next helper.
    pub fn disconnect(&mut self) -> u64 {
        let held = self.possibly_held();
        let dropped = self.items.iter().filter(|i| matches!(i, Item::Input(..))).count() as u64;
        self.items.clear();
        self.sent.clear();
        self.next = 0;
        self.confirmed = held;
        self.connected = false;
        dropped
    }

    /// A new helper takes input: its first items release everything the
    /// helpers before it may have left held. The number of keys and
    /// buttons released.
    pub fn connect(&mut self) -> usize {
        self.disconnect();
        self.connected = true;
        let releases = self.confirmed.releases();
        // Counted as held by this helper until it confirms the releases
        self.items.extend(releases.iter().map(|&r| Item::Input(r, Origin::None)));
        releases.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use win_sys::input_inject::MouseButton;

    fn key(scan: u16, up: bool) -> Inject {
        Inject::Key { scan, extended: false, up }
    }

    fn mv(dx: i32, dy: i32) -> Inject {
        Inject::Move { dx, dy }
    }

    fn connected() -> SendQueue {
        let mut q = SendQueue::default();
        assert_eq!(q.connect(), 0);
        q
    }

    fn drain(q: &mut SendQueue) -> Vec<ToHelper> {
        std::iter::from_fn(|| q.take()).collect()
    }

    #[test]
    fn without_a_helper_everything_is_dropped_and_counted() {
        let mut q = SendQueue::default();
        let p = q.push(&[key(0x1E, false), mv(1, 1)]);
        assert_eq!(p.dropped, 2);
        assert!(q.is_empty() && q.take().is_none());
        q.push_release_all();
        assert!(q.is_empty());
    }

    #[test]
    fn moves_merge_and_transitions_keep_their_order() {
        let mut q = connected();
        let p = q.push(&[mv(1, 2), mv(3, 4), key(0x1E, false), mv(1, 0), Inject::Wheel(120), Inject::Wheel(120)]);
        assert_eq!(p, Pushed { dropped: 0, merged: 2, overflowed: false });
        q.push(&[key(0x1E, true)]);
        assert_eq!(
            drain(&mut q),
            [ToHelper::Inject(vec![mv(4, 6), key(0x1E, false), mv(1, 0), Inject::Wheel(240), key(0x1E, true)])]
        );
        // Not past the protocol's bound: a second record
        q.push(&[mv(protocol::MAX_MOVE, 0), mv(1, 0)]);
        assert_eq!(drain(&mut q), [ToHelper::Inject(vec![mv(protocol::MAX_MOVE, 0), mv(1, 0)])]);
        // Batches of at most MAX_RECORDS
        let many: Vec<Inject> = (0..100).map(|n| key(1 + n % 50, n % 2 == 1)).collect();
        q.push(&many);
        let frames = drain(&mut q);
        assert_eq!(frames.len(), 2);
        assert!(matches!(&frames[0], ToHelper::Inject(v) if v.len() == MAX_RECORDS));
    }

    #[test]
    fn a_full_queue_drops_moves_first_and_never_a_single_transition() {
        let mut q = connected();
        // Alternate so that moves cannot merge
        for n in 0..CAPACITY / 2 {
            q.push(&[mv(1, 0), key(0x10 + (n % 20) as u16, n % 2 == 1)]);
        }
        assert_eq!(q.len(), CAPACITY);
        // A transition pushes out the oldest move
        let p = q.push(&[key(0x1E, false)]);
        assert_eq!((p.dropped, p.overflowed), (1, false));
        assert_eq!(q.len(), CAPACITY);
        // A move after a transition: the oldest move goes
        let p = q.push(&[mv(5, 5)]);
        assert_eq!(p.dropped, 1);
        // Only transitions left once the moves are gone
        let mut t = connected();
        for n in 0..CAPACITY {
            t.push(&[key(1 + (n % 100) as u16, n % 2 == 1)]);
        }
        // A move into a queue full of transitions is the one dropped
        assert_eq!(t.push(&[mv(1, 1)]).dropped, 1);
        // A transition: everything goes, a release of everything instead,
        // and the new key after it
        let p = t.push(&[key(0x30, false)]);
        assert!(p.overflowed);
        assert_eq!(p.dropped, CAPACITY as u64);
        assert_eq!(drain(&mut t), [ToHelper::ReleaseAll, ToHelper::Inject(vec![key(0x30, false)])]);
    }

    #[test]
    fn acks_track_what_is_held() {
        let mut q = connected();
        q.push(&[key(0x2A, false), key(0x1E, false), key(0x1E, true)]);
        // Queued downs count as held before they are even written
        assert_eq!(q.possibly_held().len(), 2);
        let _ = drain(&mut q);
        assert_eq!(q.unconfirmed(), 3);
        assert!(q.ack(3));
        assert_eq!(q.unconfirmed(), 0);
        // Shift held, A pressed and let go
        assert_eq!(q.possibly_held().releases(), [key(0x2A, true)]);
        assert!(!q.ack(4), "more confirmed than written");
        q.push_release_all();
        q.push_release_all();
        assert_eq!(drain(&mut q), [ToHelper::ReleaseAll]);
        // Not confirmed yet: still held
        assert_eq!(q.possibly_held().len(), 1);
        assert!(q.ack(4));
        assert!(q.possibly_held().is_empty());
    }

    /// The helper killed with a key down, its key-up written but never
    /// confirmed: the replacement releases the key, and is counted as
    /// holding it until it confirms
    #[test]
    fn the_replacement_releases_what_the_dead_helper_may_hold() {
        let mut q = connected();
        q.push(&[key(0x1E, false), Inject::Button { button: MouseButton::Left, up: false }]);
        let _ = drain(&mut q);
        assert!(q.ack(2));
        q.push(&[key(0x1E, true), mv(3, 3)]);
        let _ = drain(&mut q);
        // Killed before it confirmed the key-up
        q.push(&[key(0x20, false)]);
        assert_eq!(q.disconnect(), 1);
        assert!(!q.connected());
        assert_eq!(q.push(&[key(0x21, false)]).dropped, 1);
        // The next helper: both keys of before and the button, released
        // first (keys, then the button); the queued D never reached anyone
        // but is released too (it might have)
        assert_eq!(q.connect(), 3);
        let first = drain(&mut q);
        assert_eq!(
            first,
            [ToHelper::Inject(vec![
                key(0x1E, true),
                key(0x20, true),
                Inject::Button { button: MouseButton::Left, up: true }
            ])]
        );
        // That helper dies too before it confirms: the next releases again
        q.disconnect();
        assert_eq!(q.connect(), 3);
        let _ = drain(&mut q);
        assert!(q.ack(3));
        assert!(q.possibly_held().is_empty());
        // A clean helper change with nothing held releases nothing
        q.disconnect();
        assert_eq!(q.connect(), 0);
    }

    /// Each input is confirmed as its origin's: motions of two devices are
    /// not merged, and an ack names the origins it covers
    #[test]
    fn acks_name_the_origins_of_what_they_confirm() {
        let a = Origin::Device([1; 6]);
        let b = Origin::Device([2; 6]);
        let mut q = connected();
        q.push_from(a, &[mv(1, 0)]);
        assert_eq!(q.push_from(b, &[mv(1, 0)]).merged, 0);
        assert_eq!(q.push_from(b, &[mv(1, 0)]).merged, 1);
        q.push_from(Origin::SelfTest, &[mv(0, 0)]);
        assert_eq!(drain(&mut q), [ToHelper::Inject(vec![mv(1, 0), mv(2, 0), mv(0, 0)])]);
        assert_eq!(q.ack_items(1), Some(vec![a]));
        assert_eq!(q.ack_items(3), Some(vec![b, Origin::SelfTest]));
        assert_eq!(q.ack_items(3), Some(vec![]));
        assert_eq!(q.ack_items(4), None);
        // Without a helper the input of a device is dropped, not confirmed
        q.disconnect();
        assert_eq!(q.push_from(a, &[key(0x1E, false)]).dropped, 1);
    }

    #[test]
    fn keys_no_keyboard_has_are_dropped() {
        let mut q = connected();
        let p = q.push(&[Inject::Key { scan: 0x1FF, extended: false, up: false }, mv(i32::MAX, 0)]);
        assert_eq!(p.dropped, 1);
        assert_eq!(drain(&mut q), [ToHelper::Inject(vec![mv(protocol::MAX_MOVE, 0)])]);
    }
}
