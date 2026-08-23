//! Reading the store to work out where the next event goes.
//!
//! Authoring is the one place a node writes to its own chain, and getting it
//! wrong is unrecoverable: a gap makes the event unacceptable to every peer,
//! and a reused sequence number is indistinguishable from equivocation. So it
//! is derived from persisted state every time, never from a counter held in
//! memory that a crash could desynchronise.

use kahui_proto::{ChainTip, CommunityId, UserId};
use kahui_store::{Store, StoreError};

/// Works out the author's next position in a community.
///
/// Reads three things from the store: how far this author's own chain has got,
/// how high the community's Lamport clock has climbed, and which events are
/// currently heads. Together they place the next event after everything this
/// node has seen, in a way every other node will agree with.
pub fn tip(
    store: &dyn Store,
    community: &CommunityId,
    author: &UserId,
) -> Result<ChainTip, StoreError> {
    let next_seq = store.frontier(community)?.next_seq(author);
    let prev_self = if next_seq == 0 {
        None
    } else {
        store.chain_head(community, author)?
    };

    Ok(ChainTip {
        next_seq,
        prev_self,
        // One past everything we have seen: the defining rule of a Lamport
        // clock, and what makes our event sort after its causes on every node.
        next_lamport: store.max_lamport(community)?.saturating_add(1),
        parents: store.heads(community)?,
    })
}

/// The tip for a community that does not exist yet.
pub fn genesis_tip() -> ChainTip {
    ChainTip::genesis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kahui_proto::{ChannelId, Event, Identity, Payload};
    use kahui_store::RedbStore;
    use tempfile::TempDir;

    #[test]
    fn the_tip_advances_with_every_authored_event() {
        let dir = TempDir::new().unwrap();
        let store = RedbStore::open(dir.path().join("s.redb")).unwrap();
        let identity = Identity::generate();
        let me = identity.user_id();

        let genesis = Event::create(
            &identity,
            CommunityId::ZERO,
            &genesis_tip(),
            0,
            Payload::CreateCommunity {
                name: "Kahui".into(),
                description: String::new(),
            },
        );
        store.put_event(&genesis).unwrap();
        let community = genesis.community_id();

        let after_genesis = tip(&store, &community, &me).unwrap();
        assert_eq!(after_genesis.next_seq, 1);
        assert_eq!(after_genesis.prev_self, Some(genesis.id()));
        assert_eq!(after_genesis.next_lamport, 2);
        assert_eq!(after_genesis.parents, vec![genesis.id()]);

        let second = Event::create(
            &identity,
            community,
            &after_genesis,
            1,
            Payload::CreateChannel {
                channel: ChannelId::derive(&community, "general"),
                name: "general".into(),
                topic: String::new(),
            },
        );
        store.put_event(&second).unwrap();

        let after_channel = tip(&store, &community, &me).unwrap();
        assert_eq!(after_channel.next_seq, 2);
        assert_eq!(after_channel.prev_self, Some(second.id()));
        assert_eq!(after_channel.next_lamport, 3);
    }

    #[test]
    fn a_member_who_has_not_written_yet_starts_at_zero() {
        let dir = TempDir::new().unwrap();
        let store = RedbStore::open(dir.path().join("s.redb")).unwrap();
        let founder = Identity::generate();
        let newcomer = Identity::generate();

        let genesis = Event::create(
            &founder,
            CommunityId::ZERO,
            &genesis_tip(),
            0,
            Payload::CreateCommunity {
                name: "Kahui".into(),
                description: String::new(),
            },
        );
        store.put_event(&genesis).unwrap();
        let community = genesis.community_id();

        let fresh = tip(&store, &community, &newcomer.user_id()).unwrap();
        assert_eq!(fresh.next_seq, 0);
        assert_eq!(fresh.prev_self, None, "no chain of their own yet");
        assert_eq!(
            fresh.next_lamport, 2,
            "but their clock still starts past what they have seen"
        );
    }
}
