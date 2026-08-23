//! # kahui-proto
//!
//! The Kahui protocol, with no IO in it.
//!
//! This crate knows what an identity is, what an event is, how events are
//! signed and hashed, and how a node describes what it already holds. It does
//! not know about sockets, disks, threads or executors — it is all pure
//! functions over plain data.
//!
//! That boundary is the point. A desktop app, a browser client compiled to
//! WebAssembly and a mobile client behind an FFI shim can each bring their own
//! transport and their own storage while sharing *this* crate byte for byte,
//! which is what makes them the same network rather than three lookalikes.
//!
//! ## The model in one paragraph
//!
//! A community is founded by a genesis [`Event`], and its [`CommunityId`] is
//! that event's hash. Every member keeps a hash-chained log of their own signed
//! events within it. Nodes gossip new events as they are authored and reconcile
//! missed ones by exchanging a [`Frontier`]. Ordering comes from a Lamport
//! clock, tie-broken by event id, so every node renders the same transcript
//! without anyone being in charge.
//!
//! ```
//! use kahui_proto::{CommunityId, Event, Identity, Payload, PROTOCOL_VERSION};
//!
//! let founder = Identity::generate();
//! let genesis = Event {
//!     version: PROTOCOL_VERSION,
//!     community: CommunityId::ZERO, // a genesis event cannot name its own id
//!     author: founder.user_id(),
//!     seq: 0,
//!     prev_self: None,
//!     lamport: 1,
//!     parents: Vec::new(),
//!     timestamp_ms: 0,
//!     payload: Payload::CreateCommunity {
//!         name: "Kahui".into(),
//!         description: "Hosted by its members".into(),
//!     },
//! }
//! .sign(&founder);
//!
//! genesis.verify().unwrap();
//! let community: CommunityId = genesis.community_id();
//! assert_eq!(community.as_bytes(), genesis.id().as_bytes());
//! ```

#![forbid(unsafe_code)]

pub mod codec;
pub mod event;
pub mod frontier;
pub mod identity;
pub mod ids;

pub use codec::CodecError;
pub use event::{
    ChainTip, Event, Payload, SignedEvent, ValidationError, MAX_BODY_BYTES, MAX_NAME_BYTES,
    MAX_PARENTS, MAX_TOPIC_BYTES, PROTOCOL_VERSION,
};
pub use frontier::Frontier;
pub use identity::{Identity, IdentityError, SignatureBytes};
pub use ids::{ChannelId, CommunityId, EventId, Hash32, IdError, UserId};
