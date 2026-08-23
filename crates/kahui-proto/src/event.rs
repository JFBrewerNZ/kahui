//! The event: the only unit of state in Kahui.
//!
//! Everything a community *is* — its name, its channels, its members, every
//! message ever posted — is an append-only sequence of signed events. There is
//! no mutable server-side state to disagree about, so two nodes that hold the
//! same set of events render exactly the same community.
//!
//! # Shape
//!
//! Each author keeps one hash-chained log per community. `seq` counts up from
//! zero and `prev_self` points at the author's previous event, so a log cannot
//! be reordered or have entries silently removed without breaking the chain.
//! That chain is also what makes sync cheap: "I have alice up to 12" is all a
//! node needs to say to receive everything it missed (see [`crate::frontier`]).
//!
//! `lamport` and `parents` capture causality *across* authors, giving every
//! node the same display order without a clock anyone has to trust.

use serde::{Deserialize, Serialize};

use crate::codec;
use crate::identity::{self, Identity, SignatureBytes};
use crate::ids::{ChannelId, CommunityId, EventId, Hash32, UserId};

/// Wire format version. Bumped only for breaking changes to [`Event`].
pub const PROTOCOL_VERSION: u16 = 1;

/// Domain separation tag. Prefixed to every signing preimage so a Kahui
/// signature can never be replayed as a signature for some other protocol that
/// happens to use the same key.
const EVENT_DOMAIN: &[u8] = b"kahui.event.v1\0";

/// Longest message body, in bytes of UTF-8.
pub const MAX_BODY_BYTES: usize = 4096;
/// Longest community, channel or display name, in bytes of UTF-8.
pub const MAX_NAME_BYTES: usize = 64;
/// Longest channel topic or community description, in bytes of UTF-8.
pub const MAX_TOPIC_BYTES: usize = 512;
/// Largest causal parent set carried by one event.
pub const MAX_PARENTS: usize = 32;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    #[error("unsupported protocol version {found} (this node speaks {PROTOCOL_VERSION})")]
    UnsupportedVersion { found: u16 },
    #[error("signature does not verify for author {0}")]
    BadSignature(String),
    #[error("event id does not match its contents")]
    IdMismatch,
    #[error("genesis event must have seq 0, no prev_self, and a zero community id")]
    MalformedGenesis,
    #[error("non-genesis event must name a community")]
    MissingCommunity,
    #[error("event at seq {seq} must reference the author's previous event")]
    MissingPrevSelf { seq: u64 },
    #[error("event at seq 0 must not reference a previous event")]
    UnexpectedPrevSelf,
    #[error("lamport clock must be at least 1")]
    ZeroLamport,
    #[error("{field} is empty")]
    Empty { field: &'static str },
    #[error("{field} is {len} bytes, limit is {max}")]
    TooLong {
        field: &'static str,
        len: usize,
        max: usize,
    },
    #[error("event carries {0} parents, limit is {MAX_PARENTS}")]
    TooManyParents(usize),
    #[error("channel id does not match its name")]
    ChannelIdMismatch,
}

/// What an event actually says.
///
/// Adding a variant is backwards compatible for readers that skip unknown
/// payloads; changing one is not, and needs a [`PROTOCOL_VERSION`] bump.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Payload {
    /// Genesis. Brings a community into existence; its event id becomes the
    /// [`CommunityId`].
    CreateCommunity { name: String, description: String },
    /// Opens a channel. `channel` must equal `ChannelId::derive(community, name)`.
    CreateChannel {
        channel: ChannelId,
        name: String,
        topic: String,
    },
    /// Announces membership. Self-signed: for this milestone communities are
    /// open, and the invite is the social gate rather than a cryptographic one.
    Join { display_name: String },
    /// Changes the author's display name.
    SetDisplayName { display_name: String },
    /// A chat message.
    Message { channel: ChannelId, body: String },
}

impl Payload {
    /// Short tag used in logs and in the JSON event stream.
    pub fn kind(&self) -> &'static str {
        match self {
            Payload::CreateCommunity { .. } => "create_community",
            Payload::CreateChannel { .. } => "create_channel",
            Payload::Join { .. } => "join",
            Payload::SetDisplayName { .. } => "set_display_name",
            Payload::Message { .. } => "message",
        }
    }
}

/// An unsigned event body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub version: u16,
    /// Zero for a genesis event, which cannot reference its own id.
    pub community: CommunityId,
    pub author: UserId,
    /// Position in the author's per-community chain, from zero.
    pub seq: u64,
    /// The author's previous event, or `None` at `seq == 0`.
    pub prev_self: Option<EventId>,
    /// Logical clock: one greater than every event the author had seen.
    pub lamport: u64,
    /// Causal context: the heads the author had observed when authoring.
    pub parents: Vec<EventId>,
    /// Wall clock at the author, in milliseconds since the Unix epoch. Purely
    /// advisory — it is displayed but never used for ordering, because a peer
    /// can put anything here.
    pub timestamp_ms: u64,
    pub payload: Payload,
}

impl Event {
    /// The exact bytes that get hashed and signed.
    ///
    /// postcard has no field names and no map ordering, so encoding is
    /// deterministic for a fixed struct layout: every node derives the same
    /// preimage, and therefore the same id, from the same event.
    pub fn preimage(&self) -> Vec<u8> {
        let body = codec::to_canonical(self).expect("Event is always encodable");
        let mut out = Vec::with_capacity(EVENT_DOMAIN.len() + body.len());
        out.extend_from_slice(EVENT_DOMAIN);
        out.extend_from_slice(&body);
        out
    }

    /// Content address of this event.
    pub fn id(&self) -> EventId {
        EventId(Hash32::digest(&self.preimage()))
    }

    /// Signs the event, producing the form that travels over the wire.
    pub fn sign(self, identity: &Identity) -> SignedEvent {
        debug_assert_eq!(
            self.author,
            identity.user_id(),
            "refusing to sign an event attributed to another author"
        );
        let signature = identity.sign(&self.preimage());
        SignedEvent {
            event: self,
            signature,
        }
    }

    /// True when this event founds a community.
    pub fn is_genesis(&self) -> bool {
        matches!(self.payload, Payload::CreateCommunity { .. })
    }

    /// The community this event belongs to. A genesis event belongs to the
    /// community it creates, whose id is the event's own id.
    pub fn community_id(&self) -> CommunityId {
        if self.is_genesis() {
            CommunityId::from(self.id())
        } else {
            self.community
        }
    }

    /// Deterministic total order, consistent with causality.
    ///
    /// Lamport clocks order causally related events correctly and leave
    /// concurrent ones tied; the event id breaks the tie the same way on every
    /// node, so all members render identical transcripts.
    pub fn order_key(&self) -> (u64, EventId) {
        (self.lamport, self.id())
    }

    /// Structural checks that need no knowledge of other events.
    pub fn validate_shape(&self) -> Result<(), ValidationError> {
        if self.version != PROTOCOL_VERSION {
            return Err(ValidationError::UnsupportedVersion {
                found: self.version,
            });
        }
        if self.lamport == 0 {
            return Err(ValidationError::ZeroLamport);
        }
        if self.parents.len() > MAX_PARENTS {
            return Err(ValidationError::TooManyParents(self.parents.len()));
        }

        if self.is_genesis() {
            if self.seq != 0 || self.prev_self.is_some() || !self.community.is_zero() {
                return Err(ValidationError::MalformedGenesis);
            }
        } else {
            if self.community.is_zero() {
                return Err(ValidationError::MissingCommunity);
            }
            match (self.seq, &self.prev_self) {
                (0, Some(_)) => return Err(ValidationError::UnexpectedPrevSelf),
                (seq, None) if seq > 0 => return Err(ValidationError::MissingPrevSelf { seq }),
                _ => {}
            }
        }

        match &self.payload {
            Payload::CreateCommunity { name, description } => {
                check_text("community name", name, MAX_NAME_BYTES, true)?;
                check_text("description", description, MAX_TOPIC_BYTES, false)?;
            }
            Payload::CreateChannel {
                channel,
                name,
                topic,
            } => {
                check_text("channel name", name, MAX_NAME_BYTES, true)?;
                check_text("topic", topic, MAX_TOPIC_BYTES, false)?;
                // The id is a pure function of the community and the name, so a
                // peer cannot smuggle a message into an unrelated channel by
                // mislabelling it.
                if *channel != ChannelId::derive(&self.community, name) {
                    return Err(ValidationError::ChannelIdMismatch);
                }
            }
            Payload::Join { display_name } | Payload::SetDisplayName { display_name } => {
                check_text("display name", display_name, MAX_NAME_BYTES, true)?;
            }
            Payload::Message { body, .. } => {
                check_text("message body", body, MAX_BODY_BYTES, true)?;
            }
        }
        Ok(())
    }
}

fn check_text(
    field: &'static str,
    value: &str,
    max: usize,
    required: bool,
) -> Result<(), ValidationError> {
    if required && value.trim().is_empty() {
        return Err(ValidationError::Empty { field });
    }
    if value.len() > max {
        return Err(ValidationError::TooLong {
            field,
            len: value.len(),
            max,
        });
    }
    Ok(())
}

/// Everything an author needs to know about a community in order to append to
/// it: where their own chain ended, and what they had seen when they wrote.
///
/// A node derives this from its store just before authoring. Keeping it a plain
/// value, rather than something the store hands out directly, is what lets the
/// authoring rules live here in the protocol crate where every client shares
/// them, instead of being reimplemented per platform.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainTip {
    /// Sequence number the next event will carry.
    pub next_seq: u64,
    /// The author's most recent event, or `None` if they have not written yet.
    pub prev_self: Option<EventId>,
    /// One greater than the highest Lamport value this node has seen.
    pub next_lamport: u64,
    /// The latest event from each author, as observed right now.
    pub parents: Vec<EventId>,
}

impl ChainTip {
    /// The tip for a community that does not exist yet.
    pub fn genesis() -> Self {
        ChainTip {
            next_seq: 0,
            prev_self: None,
            next_lamport: 1,
            parents: Vec::new(),
        }
    }
}

impl Event {
    /// Builds and signs the author's next event.
    ///
    /// Pass [`CommunityId::ZERO`] when the payload is
    /// [`Payload::CreateCommunity`], since a genesis event cannot name the
    /// community it is about to create.
    ///
    /// The parent set is truncated to [`MAX_PARENTS`], keeping the causal
    /// context useful in small communities without letting event size grow
    /// with membership.
    pub fn create(
        identity: &Identity,
        community: CommunityId,
        tip: &ChainTip,
        timestamp_ms: u64,
        payload: Payload,
    ) -> SignedEvent {
        let mut parents = tip.parents.clone();
        parents.sort_unstable();
        parents.dedup();
        parents.truncate(MAX_PARENTS);

        Event {
            version: PROTOCOL_VERSION,
            community,
            author: identity.user_id(),
            seq: tip.next_seq,
            prev_self: tip.prev_self,
            lamport: tip.next_lamport.max(1),
            parents,
            timestamp_ms,
            payload,
        }
        .sign(identity)
    }
}

/// An event plus the author's signature over it. This is what nodes gossip,
/// store and sync; an unsigned [`Event`] never leaves the process that built it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEvent {
    pub event: Event,
    #[serde(with = "crate::codec::signature_serde")]
    pub signature: SignatureBytes,
}

impl SignedEvent {
    pub fn id(&self) -> EventId {
        self.event.id()
    }

    pub fn author(&self) -> UserId {
        self.event.author
    }

    pub fn seq(&self) -> u64 {
        self.event.seq
    }

    pub fn community_id(&self) -> CommunityId {
        self.event.community_id()
    }

    pub fn payload(&self) -> &Payload {
        &self.event.payload
    }

    pub fn order_key(&self) -> (u64, EventId) {
        self.event.order_key()
    }

    /// Full standalone validation: structure, then signature.
    ///
    /// Every event is checked here before it touches the store, no matter which
    /// peer handed it over. Causal checks that need neighbouring events (does
    /// `prev_self` exist? is `seq` contiguous?) belong to the store layer.
    pub fn verify(&self) -> Result<(), ValidationError> {
        self.event.validate_shape()?;
        let preimage = self.event.preimage();
        if !identity::verify(&self.event.author, &preimage, &self.signature) {
            return Err(ValidationError::BadSignature(self.event.author.short()));
        }
        Ok(())
    }

    /// Checks that a claimed id matches, without recomputing it twice.
    pub fn verify_id(&self, expected: &EventId) -> Result<(), ValidationError> {
        if self.id() != *expected {
            return Err(ValidationError::IdMismatch);
        }
        Ok(())
    }

    pub fn encode(&self) -> Vec<u8> {
        codec::to_canonical(self).expect("SignedEvent is always encodable")
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, codec::CodecError> {
        codec::from_canonical(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn genesis(id: &Identity) -> SignedEvent {
        Event {
            version: PROTOCOL_VERSION,
            community: CommunityId::ZERO,
            author: id.user_id(),
            seq: 0,
            prev_self: None,
            lamport: 1,
            parents: Vec::new(),
            timestamp_ms: 1_700_000_000_000,
            payload: Payload::CreateCommunity {
                name: "Kahui".into(),
                description: "test".into(),
            },
        }
        .sign(id)
    }

    #[test]
    fn signed_event_verifies() {
        let id = Identity::generate();
        genesis(&id).verify().unwrap();
    }

    #[test]
    fn tampering_with_the_body_breaks_the_signature() {
        let id = Identity::generate();
        let mut ev = genesis(&id);
        ev.event.payload = Payload::CreateCommunity {
            name: "Not Kahui".into(),
            description: "test".into(),
        };
        assert!(matches!(ev.verify(), Err(ValidationError::BadSignature(_))));
    }

    #[test]
    fn community_id_of_genesis_is_its_own_id() {
        let id = Identity::generate();
        let ev = genesis(&id);
        assert_eq!(ev.community_id(), CommunityId::from(ev.id()));
    }

    #[test]
    fn encoding_is_deterministic() {
        let id = Identity::generate();
        let ev = genesis(&id);
        assert_eq!(ev.encode(), ev.encode());
        assert_eq!(SignedEvent::decode(&ev.encode()).unwrap(), ev);
    }

    #[test]
    fn rejects_channel_id_that_does_not_match_its_name() {
        let id = Identity::generate();
        let community = CommunityId(Hash32::digest(b"c"));
        let ev = Event {
            version: PROTOCOL_VERSION,
            community,
            author: id.user_id(),
            seq: 1,
            prev_self: Some(EventId(Hash32::digest(b"prev"))),
            lamport: 2,
            parents: Vec::new(),
            timestamp_ms: 0,
            payload: Payload::CreateChannel {
                channel: ChannelId::derive(&community, "random"),
                name: "general".into(),
                topic: String::new(),
            },
        }
        .sign(&id);
        assert_eq!(ev.verify(), Err(ValidationError::ChannelIdMismatch));
    }

    #[test]
    fn rejects_oversized_body() {
        let id = Identity::generate();
        let community = CommunityId(Hash32::digest(b"c"));
        let ev = Event {
            version: PROTOCOL_VERSION,
            community,
            author: id.user_id(),
            seq: 1,
            prev_self: Some(EventId(Hash32::digest(b"prev"))),
            lamport: 2,
            parents: Vec::new(),
            timestamp_ms: 0,
            payload: Payload::Message {
                channel: ChannelId::derive(&community, "general"),
                body: "x".repeat(MAX_BODY_BYTES + 1),
            },
        }
        .sign(&id);
        assert!(matches!(ev.verify(), Err(ValidationError::TooLong { .. })));
    }

    #[test]
    fn rejects_non_genesis_without_prev_self() {
        let id = Identity::generate();
        let community = CommunityId(Hash32::digest(b"c"));
        let ev = Event {
            version: PROTOCOL_VERSION,
            community,
            author: id.user_id(),
            seq: 4,
            prev_self: None,
            lamport: 5,
            parents: Vec::new(),
            timestamp_ms: 0,
            payload: Payload::Join {
                display_name: "b".into(),
            },
        }
        .sign(&id);
        assert_eq!(
            ev.verify(),
            Err(ValidationError::MissingPrevSelf { seq: 4 })
        );
    }
}
