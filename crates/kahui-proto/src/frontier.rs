//! What a node already has, expressed compactly enough to put in one packet.
//!
//! Because each author's events form a gap-free chain numbered from zero,
//! "everything I hold from alice" collapses to a single number: her highest
//! sequence. A [`Frontier`] is one such number per author — a version vector.
//!
//! That is the whole of Kahui's catch-up protocol. A node that has been offline
//! sends its frontier; a peer replies with every event the frontier does not
//! cover. Nothing is re-sent, nothing is missed, and the request stays a few
//! dozen bytes per member no matter how much history exists.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ids::UserId;

/// The highest contiguous sequence number held for each author, in a single
/// community.
///
/// Entries are kept sorted by author so the encoding is canonical and two
/// frontiers over the same state compare equal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Frontier {
    entries: Vec<(UserId, u64)>,
}

impl Frontier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds a frontier from arbitrary pairs, keeping the highest sequence per
    /// author and sorting the result.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (UserId, u64)>) -> Self {
        let mut map: BTreeMap<UserId, u64> = BTreeMap::new();
        for (user, seq) in pairs {
            map.entry(user)
                .and_modify(|s| *s = (*s).max(seq))
                .or_insert(seq);
        }
        Frontier {
            entries: map.into_iter().collect(),
        }
    }

    /// Highest sequence held for `user`, or `None` if this node holds nothing
    /// from them.
    pub fn get(&self, user: &UserId) -> Option<u64> {
        self.entries
            .binary_search_by(|(u, _)| u.cmp(user))
            .ok()
            .map(|i| self.entries[i].1)
    }

    /// The sequence number the next event from `user` will carry.
    pub fn next_seq(&self, user: &UserId) -> u64 {
        self.get(user).map_or(0, |s| s + 1)
    }

    /// True if this frontier already covers `(user, seq)`.
    pub fn covers(&self, user: &UserId, seq: u64) -> bool {
        self.get(user).is_some_and(|held| held >= seq)
    }

    /// Records that `seq` is held for `user`, keeping the highest value seen.
    pub fn observe(&mut self, user: UserId, seq: u64) {
        match self.entries.binary_search_by(|(u, _)| u.cmp(&user)) {
            Ok(i) => {
                let slot = &mut self.entries[i].1;
                *slot = (*slot).max(seq);
            }
            Err(i) => self.entries.insert(i, (user, seq)),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&UserId, u64)> {
        self.entries.iter().map(|(u, s)| (u, *s))
    }

    /// Number of authors represented.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total events implied by this frontier. Handy for progress reporting and
    /// for asserting in tests that two nodes have converged.
    pub fn total_events(&self) -> u64 {
        self.entries.iter().map(|(_, seq)| seq + 1).sum()
    }
}

impl FromIterator<(UserId, u64)> for Frontier {
    fn from_iter<T: IntoIterator<Item = (UserId, u64)>>(iter: T) -> Self {
        Frontier::from_pairs(iter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::Hash32;

    fn user(tag: &[u8]) -> UserId {
        UserId(Hash32::digest(tag))
    }

    #[test]
    fn tracks_the_highest_sequence_per_author() {
        let mut f = Frontier::new();
        let alice = user(b"alice");
        assert_eq!(f.get(&alice), None);
        assert_eq!(f.next_seq(&alice), 0);

        f.observe(alice, 3);
        f.observe(alice, 1); // out of order, must not go backwards
        assert_eq!(f.get(&alice), Some(3));
        assert_eq!(f.next_seq(&alice), 4);
        assert!(f.covers(&alice, 3));
        assert!(!f.covers(&alice, 4));
    }

    #[test]
    fn entries_stay_sorted_so_the_encoding_is_canonical() {
        let users: Vec<UserId> = (0..8u8).map(|i| user(&[i])).collect();
        let mut a = Frontier::new();
        for (i, u) in users.iter().enumerate() {
            a.observe(*u, i as u64);
        }
        let mut b = Frontier::new();
        for (i, u) in users.iter().enumerate().rev() {
            b.observe(*u, i as u64);
        }
        assert_eq!(a, b);
        assert_eq!(
            crate::codec::to_canonical(&a).unwrap(),
            crate::codec::to_canonical(&b).unwrap()
        );
    }

    #[test]
    fn total_events_counts_from_zero() {
        let f = Frontier::from_pairs([(user(b"a"), 2), (user(b"b"), 0)]);
        assert_eq!(f.total_events(), 4);
    }
}
