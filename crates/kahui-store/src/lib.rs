//! # kahui-store
//!
//! Every node keeps its own complete copy of the communities it belongs to.
//! There is no shared database, so this crate is the whole of Kahui's
//! persistence: an append-only event log plus the indexes needed to serve it
//! back as channels, members and history.
//!
//! [`Store`] is a trait rather than a concrete type because the storage engine
//! is the part that cannot be shared across platforms. Desktop and server nodes
//! use [`RedbStore`]; a browser client compiled to WebAssembly will implement
//! the same trait over IndexedDB. The protocol, the sync logic and the node
//! engine above it do not change.
//!
//! ## What the store guarantees
//!
//! [`Store::put_event`] is the only way in, and it refuses anything it cannot
//! fully verify: a bad signature, a sequence gap in the author's chain, a
//! second event at a sequence already taken, or an event for a community whose
//! genesis this node has never seen. Rejections are not failures — they are how
//! a node discovers it is behind, and [`StoreError::needs_sync`] tells the
//! caller which ones to answer by asking a peer for the missing history.

#![forbid(unsafe_code)]

pub mod keys;
mod redb_store;

pub use redb_store::RedbStore;

use kahui_proto::{
    ChannelId, CommunityId, EventId, Frontier, SignedEvent, UserId, ValidationError,
};
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The event is not internally valid. Always the sender's fault.
    #[error(transparent)]
    Invalid(#[from] ValidationError),

    /// The author's chain has a hole: we hold nothing at `needed_seq`.
    #[error(
        "event from {author} at seq {seq} needs their event at seq {needed_seq}, which is missing"
    )]
    MissingPrev {
        author: String,
        seq: u64,
        needed_seq: u64,
    },

    /// We were handed an event for a community whose genesis we have never
    /// seen, so there is nothing to anchor it to.
    #[error("unknown community {0}")]
    UnknownCommunity(String),

    /// `prev_self` points somewhere other than the event we hold at `seq - 1`.
    #[error("event from {author} at seq {seq} does not follow the chain we hold")]
    ChainMismatch { author: String, seq: u64 },

    /// Two different events signed by the same author claim the same sequence.
    /// This is provable misbehaviour, not a race.
    #[error("author {author} signed two different events at seq {seq}")]
    Equivocation { author: String, seq: u64 },

    #[error("storage backend: {0}")]
    Backend(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error("stored record is corrupt: {0}")]
    Corrupt(String),
}

impl StoreError {
    /// True when the rejection means "this node is behind", rather than "this
    /// event is bad".
    ///
    /// The node answers these by asking the peer that sent the event for the
    /// history it is missing, then replaying the event.
    pub fn needs_sync(&self) -> bool {
        matches!(
            self,
            StoreError::MissingPrev { .. } | StoreError::UnknownCommunity(_)
        )
    }
}

/// Whether [`Store::put_event`] actually changed anything.
///
/// Gossip and sync routinely deliver the same event twice; `Duplicate` is the
/// normal, quiet outcome, and the caller uses it to avoid re-broadcasting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inserted {
    New,
    Duplicate,
}

impl Inserted {
    pub fn is_new(self) -> bool {
        self == Inserted::New
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommunitySummary {
    pub id: CommunityId,
    pub name: String,
    pub description: String,
    pub founder: UserId,
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSummary {
    pub id: ChannelId,
    pub name: String,
    pub topic: String,
    pub creator: UserId,
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberSummary {
    pub id: UserId,
    pub display_name: String,
    pub joined_ms: u64,
}

/// A peer we have seen in a community, remembered so we can dial it again after
/// a restart.
///
/// This is what lets a node rejoin without anyone's help: the addresses live on
/// disk next to the history, not in a directory service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerRecord {
    /// libp2p peer id, in its binary form.
    pub peer_id: Vec<u8>,
    /// Multiaddresses, most recently advertised first.
    pub addrs: Vec<String>,
    pub last_seen_ms: u64,
}

/// A node's local view of the communities it belongs to.
///
/// Implementations must be safe to share across threads and must make
/// [`Store::put_event`] atomic: either the event and all of its index entries
/// land together, or nothing does.
pub trait Store: Send + Sync + 'static {
    /// Verifies and appends an event, updating every index in one transaction.
    fn put_event(&self, event: &SignedEvent) -> Result<Inserted>;

    fn get_event(&self, id: &EventId) -> Result<Option<SignedEvent>>;

    fn has_event(&self, id: &EventId) -> Result<bool>;

    /// What this node holds for `community`, as one sequence number per author.
    fn frontier(&self, community: &CommunityId) -> Result<Frontier>;

    /// Events this node holds that `have` does not cover, in causal order and
    /// capped at `limit`.
    ///
    /// The order matters: the receiver applies them one by one, and each needs
    /// its predecessors already in place. Returns the events plus whether that
    /// was all of them.
    fn delta(&self, community: &CommunityId, have: &Frontier, limit: usize) -> Result<Delta>;

    /// Highest Lamport value seen in this community; the basis for the next
    /// event's clock.
    fn max_lamport(&self, community: &CommunityId) -> Result<u64>;

    /// The last event in one author's chain.
    ///
    /// A node needs its own head every time it authors, so this is a direct
    /// lookup rather than a scan through [`Store::heads`].
    fn chain_head(&self, community: &CommunityId, author: &UserId) -> Result<Option<EventId>>;

    /// The latest event from each author: the causal context a new event cites.
    fn heads(&self, community: &CommunityId) -> Result<Vec<EventId>>;

    fn communities(&self) -> Result<Vec<CommunitySummary>>;

    fn community(&self, id: &CommunityId) -> Result<Option<CommunitySummary>>;

    fn channels(&self, community: &CommunityId) -> Result<Vec<ChannelSummary>>;

    fn members(&self, community: &CommunityId) -> Result<Vec<MemberSummary>>;

    /// One member, for resolving the display name on an incoming message
    /// without scanning the whole roster.
    fn member(&self, community: &CommunityId, user: &UserId) -> Result<Option<MemberSummary>>;

    /// The most recent `limit` messages in a channel, oldest first.
    fn channel_history(
        &self,
        community: &CommunityId,
        channel: &ChannelId,
        limit: usize,
    ) -> Result<Vec<SignedEvent>>;

    /// Peers last seen in this community, for reconnecting after a restart.
    fn peers(&self, community: &CommunityId) -> Result<Vec<PeerRecord>>;

    fn remember_peer(&self, community: &CommunityId, peer: &PeerRecord) -> Result<()>;

    /// Node-local settings that are not part of the replicated log: the private
    /// key, the operator's display name, and so on.
    fn meta_get(&self, key: &str) -> Result<Option<Vec<u8>>>;

    fn meta_put(&self, key: &str, value: &[u8]) -> Result<()>;
}

/// A batch of events answering a sync request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    /// In causal order: apply them front to back.
    pub events: Vec<SignedEvent>,
    /// False when `limit` cut the batch short and the peer should ask again.
    pub complete: bool,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }
}
