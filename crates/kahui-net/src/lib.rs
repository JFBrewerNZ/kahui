//! # kahui-net
//!
//! The peer-to-peer layer: how nodes find each other, hand each other events,
//! and repair what they missed.
//!
//! There is no server here, and no place to add one. A node is only ever a peer
//! among peers; the founder of a community runs exactly the same code as
//! somebody who joined ten minutes ago, and has exactly the same standing.
//!
//! ## How a community stays alive
//!
//! 1. An [`Invite`] carries a community id and a few members' addresses.
//! 2. A joining node dials one of them and subscribes to the community's gossip
//!    topic.
//! 3. Members periodically announce [`Presence`], so everyone learns everyone
//!    else's address and the community meshes into a full graph rather than a
//!    star around whoever issued the invite.
//! 4. New events are gossiped live; anything missed is repaired by
//!    [`SyncRequest::GetDelta`], which asks for "everything past this frontier".
//!
//! Step 3 is the one that matters for durability. Once members are connected to
//! each other directly, no single node — the founder included — is load-bearing.

#![forbid(unsafe_code)]

pub mod behaviour;
pub mod invite;
pub mod peer;
pub mod wire;

pub use behaviour::{build_swarm, Behaviour, BehaviourEvent, NetConfig};
pub use invite::{Invite, InviteError, InvitePeer, INVITE_PREFIX, LINK_PREFIX, LINK_SCHEME};
pub use peer::{libp2p_keypair, peer_id_of};
pub use wire::{
    topic_name, GossipMessage, Presence, SyncRequest, SyncResponse, MAX_SYNC_BATCH, SYNC_PROTOCOL,
};

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error("network configuration: {0}")]
    Config(String),

    #[error("could not subscribe to a community topic: {0}")]
    Subscribe(String),

    #[error("could not listen: {0}")]
    Listen(#[from] libp2p::TransportError<std::io::Error>),

    #[error("mDNS: {0}")]
    Mdns(#[from] std::io::Error),

    #[error("not a valid multiaddress: {0}")]
    Multiaddr(#[from] libp2p::multiaddr::Error),

    #[error("building the network stack: {0}")]
    Build(#[source] Box<dyn std::error::Error + Send + Sync>),

    #[error(transparent)]
    Invite(#[from] InviteError),
}
