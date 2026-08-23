//! Composite key layout.
//!
//! Every key is a fixed-length concatenation of 32-byte ids and big-endian
//! integers. Fixed width plus big-endian means byte order equals logical order,
//! so an ordinary range scan over a key prefix returns exactly the rows we want,
//! already sorted — no secondary sort, no scanning the whole table.

use kahui_proto::{ChannelId, CommunityId, EventId, UserId};

pub const ID_LEN: usize = 32;
pub const SEQ_LEN: usize = 8;

/// `community || author || seq`
pub const CHAIN_KEY_LEN: usize = ID_LEN * 2 + SEQ_LEN;
/// `community || lamport || event`
pub const ORDER_KEY_LEN: usize = ID_LEN + SEQ_LEN + ID_LEN;
/// `author || seq`
pub const ORDER_VALUE_LEN: usize = ID_LEN + SEQ_LEN;
/// `community || channel || lamport || event`
pub const CHANNEL_KEY_LEN: usize = ID_LEN * 2 + SEQ_LEN + ID_LEN;
/// `community || (channel | user)`
pub const PAIR_KEY_LEN: usize = ID_LEN * 2;

/// Writes `parts` end to end into a fixed-size buffer.
macro_rules! concat_key {
    ($len:expr; $($part:expr),+ $(,)?) => {{
        let mut out = [0u8; $len];
        let mut at = 0usize;
        $(
            let part: &[u8] = $part;
            out[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        )+
        debug_assert_eq!(at, $len, "key layout does not fill its buffer");
        out
    }};
}

pub fn chain(community: &CommunityId, author: &UserId, seq: u64) -> [u8; CHAIN_KEY_LEN] {
    concat_key!(CHAIN_KEY_LEN; community.as_bytes(), author.as_bytes(), &seq.to_be_bytes())
}

/// Inclusive bounds covering every chain entry in a community.
pub fn chain_bounds(community: &CommunityId) -> ([u8; CHAIN_KEY_LEN], [u8; CHAIN_KEY_LEN]) {
    let mut lo = [0u8; CHAIN_KEY_LEN];
    let mut hi = [0xffu8; CHAIN_KEY_LEN];
    lo[..ID_LEN].copy_from_slice(community.as_bytes());
    hi[..ID_LEN].copy_from_slice(community.as_bytes());
    (lo, hi)
}

/// Inclusive bounds covering one author's chain within a community.
pub fn chain_author_bounds(
    community: &CommunityId,
    author: &UserId,
) -> ([u8; CHAIN_KEY_LEN], [u8; CHAIN_KEY_LEN]) {
    (
        chain(community, author, 0),
        chain(community, author, u64::MAX),
    )
}

pub fn order(community: &CommunityId, lamport: u64, event: &EventId) -> [u8; ORDER_KEY_LEN] {
    concat_key!(ORDER_KEY_LEN; community.as_bytes(), &lamport.to_be_bytes(), event.as_bytes())
}

pub fn order_bounds(community: &CommunityId) -> ([u8; ORDER_KEY_LEN], [u8; ORDER_KEY_LEN]) {
    (
        order(community, 0, &EventId::ZERO),
        order(community, u64::MAX, &EventId::from_bytes([0xff; ID_LEN])),
    )
}

pub fn order_value(author: &UserId, seq: u64) -> [u8; ORDER_VALUE_LEN] {
    concat_key!(ORDER_VALUE_LEN; author.as_bytes(), &seq.to_be_bytes())
}

/// Splits an order-index value back into the author and sequence it points at.
pub fn parse_order_value(bytes: &[u8]) -> Option<(UserId, u64)> {
    if bytes.len() != ORDER_VALUE_LEN {
        return None;
    }
    let author = UserId::try_from_slice(&bytes[..ID_LEN])?;
    let seq = u64::from_be_bytes(bytes[ID_LEN..].try_into().ok()?);
    Some((author, seq))
}

/// Recovers the author from a chain key, for frontier scans.
pub fn parse_chain_key(bytes: &[u8]) -> Option<(UserId, u64)> {
    if bytes.len() != CHAIN_KEY_LEN {
        return None;
    }
    let author = UserId::try_from_slice(&bytes[ID_LEN..ID_LEN * 2])?;
    let seq = u64::from_be_bytes(bytes[ID_LEN * 2..].try_into().ok()?);
    Some((author, seq))
}

pub fn channel_log(
    community: &CommunityId,
    channel: &ChannelId,
    lamport: u64,
    event: &EventId,
) -> [u8; CHANNEL_KEY_LEN] {
    concat_key!(
        CHANNEL_KEY_LEN;
        community.as_bytes(),
        channel.as_bytes(),
        &lamport.to_be_bytes(),
        event.as_bytes(),
    )
}

pub fn channel_log_bounds(
    community: &CommunityId,
    channel: &ChannelId,
) -> ([u8; CHANNEL_KEY_LEN], [u8; CHANNEL_KEY_LEN]) {
    (
        channel_log(community, channel, 0, &EventId::ZERO),
        channel_log(
            community,
            channel,
            u64::MAX,
            &EventId::from_bytes([0xff; ID_LEN]),
        ),
    )
}

/// `community || second`, used for the channel, member and peer indexes.
pub fn pair(community: &CommunityId, second: &[u8; ID_LEN]) -> [u8; PAIR_KEY_LEN] {
    concat_key!(PAIR_KEY_LEN; community.as_bytes(), second)
}

pub fn pair_bounds(community: &CommunityId) -> ([u8; PAIR_KEY_LEN], [u8; PAIR_KEY_LEN]) {
    let mut lo = [0u8; PAIR_KEY_LEN];
    let mut hi = [0xffu8; PAIR_KEY_LEN];
    lo[..ID_LEN].copy_from_slice(community.as_bytes());
    hi[..ID_LEN].copy_from_slice(community.as_bytes());
    (lo, hi)
}

/// Recovers the trailing id from a pair key.
pub fn parse_pair_second(bytes: &[u8]) -> Option<[u8; ID_LEN]> {
    if bytes.len() != PAIR_KEY_LEN {
        return None;
    }
    bytes[ID_LEN..].try_into().ok()
}

/// `community || peer_id`. Peer ids are multihashes of varying length, so this
/// key is variable width; prefix scans still work because the fixed-width
/// community id comes first.
pub fn peer(community: &CommunityId, peer_id: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(ID_LEN + peer_id.len());
    out.extend_from_slice(community.as_bytes());
    out.extend_from_slice(peer_id);
    out
}

/// Inclusive bounds covering every peer recorded for a community. The upper
/// bound is padded well past any real peer id length.
pub fn peer_bounds(community: &CommunityId) -> (Vec<u8>, Vec<u8>) {
    (peer(community, &[]), peer(community, &[0xff; 128]))
}

/// Recovers the peer id from a peer key.
pub fn parse_peer_key(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() <= ID_LEN {
        return None;
    }
    Some(bytes[ID_LEN..].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kahui_proto::Hash32;

    fn community() -> CommunityId {
        CommunityId(Hash32::digest(b"c"))
    }

    #[test]
    fn chain_keys_sort_by_sequence_within_an_author() {
        let (c, a) = (community(), UserId(Hash32::digest(b"a")));
        assert!(chain(&c, &a, 1) < chain(&c, &a, 2));
        assert!(chain(&c, &a, 9) < chain(&c, &a, 10));
    }

    #[test]
    fn chain_bounds_enclose_every_author() {
        let c = community();
        let (lo, hi) = chain_bounds(&c);
        for tag in [b"a".as_slice(), b"zzz", &[0u8; 1], &[0xff; 1]] {
            let key = chain(&c, &UserId(Hash32::digest(tag)), 7);
            assert!(lo <= key && key <= hi);
        }
    }

    #[test]
    fn bounds_exclude_other_communities() {
        let (lo, hi) = chain_bounds(&community());
        let other = chain(
            &CommunityId(Hash32::digest(b"other")),
            &UserId(Hash32::digest(b"a")),
            0,
        );
        assert!(other < lo || other > hi);
    }

    #[test]
    fn order_keys_sort_by_lamport_then_event() {
        let c = community();
        let (e1, e2) = (EventId(Hash32::digest(b"1")), EventId(Hash32::digest(b"2")));
        assert!(order(&c, 1, &e1) < order(&c, 2, &e1));
        let lo = e1.min(e2);
        let hi = e1.max(e2);
        assert!(order(&c, 5, &lo) < order(&c, 5, &hi));
    }

    #[test]
    fn key_parts_roundtrip() {
        let (c, a) = (community(), UserId(Hash32::digest(b"a")));
        assert_eq!(parse_chain_key(&chain(&c, &a, 42)), Some((a, 42)));
        assert_eq!(parse_order_value(&order_value(&a, 42)), Some((a, 42)));
        assert_eq!(
            parse_pair_second(&pair(&c, a.as_bytes())),
            Some(*a.as_bytes())
        );
    }

    #[test]
    fn peer_bounds_enclose_any_peer_id() {
        let c = community();
        let (lo, hi) = peer_bounds(&c);
        for len in [6usize, 38, 64] {
            let key = peer(&c, &vec![0x9a; len]);
            assert!(
                lo <= key && key <= hi,
                "peer id of {len} bytes escaped the bounds"
            );
        }
        let other = peer(&CommunityId(Hash32::digest(b"other")), &[0x9a; 38]);
        assert!(other < lo || other > hi);
    }

    #[test]
    fn peer_key_roundtrips() {
        let c = community();
        assert_eq!(parse_peer_key(&peer(&c, &[1, 2, 3])), Some(vec![1, 2, 3]));
        assert_eq!(parse_peer_key(&[0u8; 8]), None);
    }
}
