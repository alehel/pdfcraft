//! When each kind of selection on the pages was last made, so that a key two selections could take acts on the
//! newer: select Edit text's objects and then a picture, and the arrows move the picture; select a field and then a
//! comment, and Delete deletes the comment. The older selection stays, and the next key reaches it.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::PageSession;

/// A kind of selection that shares a key with another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Held {
    /// Edit text's objects.
    Objects,
    /// Edit text's picture.
    Picture,
    /// Comments.
    Comment,
    /// Prepare's fields.
    Field,
}

const KINDS: usize = 4;

/// What each kind of selection holds, as a fingerprint, or `None` while it holds nothing.
fn holding(session: &PageSession) -> [Option<u64>; KINDS] {
    fn print(value: impl Hash) -> u64 {
        let mut h = DefaultHasher::new();
        value.hash(&mut h);
        h.finish()
    }
    let comments = session.comments.selection();
    let prepare = &session.prepare;
    [
        session.objects.selection().map(print),
        session.image_selection.as_ref().map(|s| print((s.page, s.index))),
        (!comments.is_empty()).then(|| print(&comments)),
        prepare.selected.as_ref().map(|s| print((s, &prepare.also))),
    ]
}

/// When each kind of selection was last made or changed, counted in the passes that changed one.
#[derive(Clone, Debug, Default)]
pub(crate) struct SelectionAges {
    seen: [Option<u64>; KINDS],
    made: [u64; KINDS],
    clock: u64,
}

impl SelectionAges {
    /// Notes what the session's selections hold now: a kind holding something it didn't at the last note is the
    /// newest. Run once a pass, before the keys.
    pub(crate) fn note(&mut self, session: &PageSession) {
        self.note_holding(holding(session));
    }

    fn note_holding(&mut self, now: [Option<u64>; KINDS]) {
        let tick = self.clock.saturating_add(1);
        let mut changed = false;
        for ((made, seen), now) in self.made.iter_mut().zip(self.seen.iter()).zip(now) {
            if now.is_some() && now != *seen {
                *made = tick;
                changed = true;
            }
        }
        if changed {
            self.clock = tick;
        }
        self.seen = now;
    }

    /// `a` holds something and was made after `b`; two made in the same pass are neither.
    pub(crate) fn newer(&self, a: Held, b: Held) -> bool {
        self.seen.get(a as usize).is_some_and(Option::is_some) && self.made.get(a as usize) > self.made.get(b as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(kinds: &[(Held, u64)]) -> [Option<u64>; KINDS] {
        let mut now = [None; KINDS];
        for (kind, print) in kinds {
            now[*kind as usize] = Some(*print);
        }
        now
    }

    #[test]
    fn the_selection_made_last_is_newer_until_the_other_changes() {
        let mut ages = SelectionAges::default();
        ages.note_holding(with(&[(Held::Field, 1)]));
        ages.note_holding(with(&[(Held::Field, 1), (Held::Comment, 2)]));
        assert!(ages.newer(Held::Comment, Held::Field), "the comment was selected after the field");
        ages.note_holding(with(&[(Held::Field, 1), (Held::Comment, 2)]));
        assert!(ages.newer(Held::Comment, Held::Field), "an unchanged pass changes nothing");
        ages.note_holding(with(&[(Held::Field, 3), (Held::Comment, 2)]));
        assert!(ages.newer(Held::Field, Held::Comment), "another field selected is newer again");
    }

    #[test]
    fn a_selection_let_go_and_made_again_is_new_again() {
        let mut ages = SelectionAges::default();
        ages.note_holding(with(&[(Held::Objects, 7)]));
        ages.note_holding(with(&[(Held::Objects, 7), (Held::Picture, 1)]));
        ages.note_holding(with(&[(Held::Picture, 1)]));
        ages.note_holding(with(&[(Held::Objects, 7), (Held::Picture, 1)]));
        assert!(ages.newer(Held::Objects, Held::Picture), "the same objects selected again are the newest");
    }

    #[test]
    fn two_made_in_one_pass_are_neither_newer() {
        let mut ages = SelectionAges::default();
        ages.note_holding(with(&[(Held::Objects, 1), (Held::Picture, 2)]));
        assert!(!ages.newer(Held::Objects, Held::Picture) && !ages.newer(Held::Picture, Held::Objects));
    }
}
