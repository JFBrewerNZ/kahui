//! What actually goes over the wire.
//!
//! Two channels, doing two different jobs:
//!
//! * **Gossip** carries new events to everyone who is online right now. It is
//!   best-effort — a node that is asleep, or behind a dropped packet, simply
//!   misses it.
//! * **Sync** is a direct request/response between two peers that repairs
//!   whatever gossip missed. It is the mechanism behind "shut a node down for
//!   an hour and it catches up on return".
//!
//! Neither is authoritative, because neither has to be: every event carries its
//! author's signature, so a node can accept history from any peer without
//! trusting that peer at all. The worst a hostile peer can do is decline to
//! share, and any other member will serve the same events.

use kahui_proto::{CommunityId, Frontier, SignedEvent, UserId};
use serde::{Deserialize, Serialize};

/// Direct sync protocol id, negotiated per stream.
pub const SYNC_PROTOCOL: &str = "/kahui/sync/1.0.0";

/// Gossipsub topics are namespaced by this prefix and the community id, so a
/// node only ever sees traffic for communities it has joined.
pub const TOPIC_PREFIX: &str = "/kahui/community/1.0.0/";

/// Largest number of events in one sync response.
///
/// Caps the work a peer can ask another to do in a single round trip; a node
/// with more to send says so and the requester asks again.
pub const MAX_SYNC_BATCH: usize = 128;

/// A message published to a community's gossip topic.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GossipMessage {
    /// A freshly authored event, on its way to everyone currently online.
    Event(Box<SignedEvent>),
    /// "I am here, at these addresses."
    Presence(Presence),
}

impl GossipMessage {
    pub fn encode(&self) -> Vec<u8> {
        kahui_proto::codec::to_canonical(self).expect("gossip messages are always encodable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, kahui_proto::CodecError> {
        kahui_proto::codec::from_canonical(bytes)
    }
}

/// Periodic "still here" announcement.
///
/// This is how a community meshes itself. Members initially know only whoever
/// invited them; presence tells everyone else where to find them, so within a
/// round or two every online member has a direct path to every other. By the
/// time the founder disconnects, nobody is relying on them for connectivity.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Presence {
    /// Who is announcing. Their `PeerId` follows from this.
    pub user: UserId,
    /// Multiaddresses to reach them on, as strings so this stays independent of
    /// any one libp2p version's address type.
    pub addrs: Vec<String>,
    /// Where this member's own router currently maps them.
    ///
    /// Kept apart from `addrs` because it is a different kind of claim. An entry
    /// in `addrs` says "you can dial me here". These say only "this is where my
    /// router presents me at the moment" — nobody can dial them out of the blue,
    /// because a NAT drops packets from strangers. What they are good for is
    /// [`crate::punch`]: two members who both hold each other's can open a
    /// direct connection by dialling at the same instant, with nobody in the
    /// middle. That is what lets a pair who can neither of them be dialled keep
    /// talking after the member who introduced them has gone.
    pub punch: Vec<String>,
    /// How many events they hold. A cheap hint that we may be behind; the
    /// authoritative comparison is the frontier exchanged during sync.
    pub event_count: u64,
    /// When this announcement was made, by the sender's clock.
    ///
    /// Present so that repeated announcements differ. Gossipsub identifies
    /// messages by content hash, so an unchanged presence would be treated as
    /// one already-seen message and silently dropped — and a member whose
    /// details had not changed would slowly become invisible to newcomers.
    pub announced_at_ms: u64,
}

/// Asks a peer for history.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SyncRequest {
    /// "Here is everything I have. Send me what I am missing."
    ///
    /// The whole catch-up protocol is this one message. Its size is
    /// proportional to the number of members, not to the amount of history, so
    /// it costs the same after five minutes offline as after five months.
    GetDelta {
        community: CommunityId,
        have: Frontier,
        limit: u32,
    },
}

/// A peer's answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SyncResponse {
    Delta {
        /// In causal order: apply front to back.
        events: Vec<SignedEvent>,
        /// False when the batch was truncated and the requester should ask
        /// again with an updated frontier.
        complete: bool,
    },
    /// The peer cannot serve this community. Not an error worth retrying.
    UnknownCommunity,
}

impl SyncResponse {
    pub fn event_count(&self) -> usize {
        match self {
            SyncResponse::Delta { events, .. } => events.len(),
            SyncResponse::UnknownCommunity => 0,
        }
    }
}

/// The gossipsub topic name for a community.
pub fn topic_name(community: &CommunityId) -> String {
    format!("{TOPIC_PREFIX}{}", community.to_hex())
}

/// Recovers the community from a topic name.
///
/// Gossip arrives tagged with its topic, which is how a node tells which
/// community a presence announcement is about. Returns `None` for topics that
/// are not ours, so unrelated traffic on a shared network is ignored rather
/// than misfiled.
pub fn community_from_topic(topic: &str) -> Option<CommunityId> {
    CommunityId::from_hex(topic.strip_prefix(TOPIC_PREFIX)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kahui_proto::{ChainTip, CommunityId, Event, Identity, Payload};

    #[test]
    fn gossip_messages_roundtrip() {
        let identity = Identity::generate();
        let event = Event::create(
            &identity,
            CommunityId::ZERO,
            &ChainTip::genesis(),
            0,
            Payload::CreateCommunity {
                name: "Kahui".into(),
                description: String::new(),
            },
        );
        let message = GossipMessage::Event(Box::new(event.clone()));
        let decoded = GossipMessage::decode(&message.encode()).unwrap();
        match decoded {
            GossipMessage::Event(decoded) => assert_eq!(*decoded, event),
            other => panic!("decoded as {other:?}"),
        }
    }

    #[test]
    fn presence_roundtrips() {
        let presence = Presence {
            user: Identity::generate().user_id(),
            addrs: vec!["/ip4/127.0.0.1/tcp/4001".into()],
            punch: vec!["/ip4/93.184.216.34/udp/4001/quic-v1".into()],
            event_count: 12,
            announced_at_ms: 1_700_000_000_000,
        };
        let decoded =
            GossipMessage::decode(&GossipMessage::Presence(presence.clone()).encode()).unwrap();
        match decoded {
            GossipMessage::Presence(p) => {
                assert_eq!(p.user, presence.user);
                assert_eq!(p.addrs, presence.addrs);
                // Where the sender's router puts them, which is what a peer
                // aims at when neither of them can be dialled.
                assert_eq!(p.punch, presence.punch);
                assert_eq!(p.event_count, 12);
            }
            other => panic!("decoded as {other:?}"),
        }
    }

    #[test]
    fn repeated_announcements_are_distinguishable() {
        let user = Identity::generate().user_id();
        let announce = |at| {
            GossipMessage::Presence(Presence {
                user,
                addrs: vec!["/ip4/127.0.0.1/tcp/4001".into()],
                punch: Vec::new(),
                event_count: 12,
                announced_at_ms: at,
            })
            .encode()
        };
        assert_ne!(
            announce(1_700_000_000_000),
            announce(1_700_000_003_000),
            "two announcements must not hash alike, or gossipsub drops the second"
        );
    }

    #[test]
    fn a_topic_names_the_community_it_carries() {
        let community = CommunityId::from_bytes([9; 32]);
        assert_eq!(
            community_from_topic(&topic_name(&community)),
            Some(community)
        );
    }

    #[test]
    fn topics_belonging_to_other_protocols_are_ignored() {
        assert_eq!(community_from_topic("/ipfs/announce"), None);
        assert_eq!(community_from_topic(TOPIC_PREFIX), None);
        assert_eq!(community_from_topic(&format!("{TOPIC_PREFIX}nothex")), None);
    }

    #[test]
    fn topics_are_scoped_to_one_community() {
        let a = topic_name(&CommunityId::from_bytes([1; 32]));
        let b = topic_name(&CommunityId::from_bytes([2; 32]));
        assert_ne!(a, b);
        assert!(a.starts_with(TOPIC_PREFIX));
    }
}
