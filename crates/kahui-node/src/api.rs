//! The node's public surface: commands in, events out.
//!
//! This is deliberately the *only* way to drive a node, and deliberately free
//! of anything terminal-shaped. The CLI in this repository is one consumer; a
//! Tauri desktop app, a WebAssembly web client and a mobile FFI shim are all
//! meant to be others, and none of them should need to reach past this file
//! into gossip topics or database tables.
//!
//! The shape is a plain actor. [`NodeHandle`] is cheap to clone and safe to
//! share; every method sends a command to the engine task and waits for a
//! reply, while [`NodeHandle::subscribe`] gives a live stream of everything the
//! node learns, whoever caused it.

use std::time::Duration;

use kahui_proto::{ChannelId, CommunityId, EventId, UserId};
use kahui_store::{ChannelSummary, CommunitySummary, MemberSummary};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::{Invite, NodeError};

pub(crate) type Reply<T> = oneshot::Sender<Result<T, NodeError>>;

/// How long a caller waits for the engine before giving up.
///
/// Joining is the slow one: it has to reach a peer and pull enough history to
/// verify the community before it can announce membership.
pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_JOIN_TIMEOUT: Duration = Duration::from_secs(45);

/// A chat message, resolved for display.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: EventId,
    pub community: CommunityId,
    pub channel: ChannelId,
    pub author: UserId,
    /// The author's display name at the time we render it, or a short form of
    /// their id if we have not seen them join yet.
    pub author_name: String,
    pub body: String,
    /// The author's wall clock. Advisory: shown, never used for ordering.
    pub timestamp_ms: u64,
    /// Position in the causal order. This is what actually orders the channel.
    pub lamport: u64,
}

/// Everything a node reports about itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub user: UserId,
    pub display_name: String,
    pub peer_id: String,
    pub listen_addrs: Vec<String>,
    pub connected_peers: Vec<String>,
    pub communities: Vec<CommunityStatus>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommunityStatus {
    pub id: CommunityId,
    pub name: String,
    /// Total events held. Two nodes reporting the same number for the same
    /// community are almost certainly in sync; it is the quickest way to see
    /// convergence by eye.
    pub events: u64,
    pub members: usize,
    pub channels: usize,
}

/// Something the node learned. Broadcast to every subscriber.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeEvent {
    /// A listening address became available. Emitted once per address.
    Listening {
        addr: String,
    },
    PeerConnected {
        peer: String,
    },
    PeerDisconnected {
        peer: String,
    },
    /// A message became visible, whether typed here or received from a peer.
    Message(Message),
    /// Somebody joined, or changed their display name.
    Membership {
        community: CommunityId,
        user: UserId,
        display_name: String,
    },
    ChannelCreated {
        community: CommunityId,
        channel: ChannelId,
        name: String,
    },
    CommunityCreated {
        community: CommunityId,
        name: String,
    },
    /// A sync round with `peer` applied `applied` events we did not have.
    Synced {
        peer: String,
        community: CommunityId,
        applied: usize,
    },
    /// Something went wrong that did not stop the node.
    Warning {
        message: String,
    },
    Stopped,
}

pub(crate) enum Command {
    CreateCommunity {
        name: String,
        description: String,
        reply: Reply<CommunityId>,
    },
    CreateChannel {
        community: CommunityId,
        name: String,
        topic: String,
        reply: Reply<ChannelId>,
    },
    Join {
        invite: Box<Invite>,
        reply: Reply<CommunityId>,
    },
    Post {
        community: CommunityId,
        channel: ChannelId,
        body: String,
        reply: Reply<EventId>,
    },
    SetDisplayName {
        display_name: String,
        reply: Reply<()>,
    },
    MakeInvite {
        community: CommunityId,
        reply: Reply<Invite>,
    },
    Communities {
        reply: Reply<Vec<CommunitySummary>>,
    },
    Channels {
        community: CommunityId,
        reply: Reply<Vec<ChannelSummary>>,
    },
    Members {
        community: CommunityId,
        reply: Reply<Vec<MemberSummary>>,
    },
    OnlineMembers {
        community: CommunityId,
        reply: Reply<Vec<UserId>>,
    },
    History {
        community: CommunityId,
        channel: ChannelId,
        limit: usize,
        reply: Reply<Vec<Message>>,
    },
    Status {
        reply: Reply<Status>,
    },
    Dial {
        addr: String,
        reply: Reply<()>,
    },
    /// Force an immediate sync round with every connected peer.
    SyncNow {
        reply: Reply<usize>,
    },
    Shutdown {
        reply: Reply<()>,
    },
}

/// A handle to a running node.
///
/// Cloning is cheap and every clone talks to the same engine, so a UI can hold
/// one per view without coordination.
#[derive(Clone)]
pub struct NodeHandle {
    pub(crate) commands: mpsc::Sender<Command>,
    pub(crate) events: broadcast::Sender<NodeEvent>,
    pub(crate) user: UserId,
    pub(crate) peer_id: String,
}

impl NodeHandle {
    /// This node's identity in the event log.
    pub fn user(&self) -> UserId {
        self.user
    }

    /// This node's identity on the network. Derived from the same key.
    pub fn peer_id(&self) -> &str {
        &self.peer_id
    }

    /// A live stream of everything the node learns.
    ///
    /// Late subscribers do not see past events; call this before driving the
    /// node if you need every one. A slow consumer is told it lagged rather
    /// than being allowed to stall the engine.
    pub fn subscribe(&self) -> broadcast::Receiver<NodeEvent> {
        self.events.subscribe()
    }

    async fn call<T>(
        &self,
        make: impl FnOnce(Reply<T>) -> Command,
        timeout: Duration,
    ) -> Result<T, NodeError> {
        let (tx, rx) = oneshot::channel();
        self.commands
            .send(make(tx))
            .await
            .map_err(|_| NodeError::NotRunning)?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(NodeError::NotRunning),
            Err(_) => Err(NodeError::Timeout),
        }
    }

    /// Founds a community with a `#general` channel and this node as its first
    /// member.
    pub async fn create_community(
        &self,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Result<CommunityId, NodeError> {
        let (name, description) = (name.into(), description.into());
        self.call(
            |reply| Command::CreateCommunity {
                name,
                description,
                reply,
            },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    /// Joins a community from an invite.
    ///
    /// Returns once the community's history has been verified locally and this
    /// node's membership has been announced — not merely once a peer answered.
    pub async fn join(&self, invite: Invite) -> Result<CommunityId, NodeError> {
        self.call(
            |reply| Command::Join {
                invite: Box::new(invite),
                reply,
            },
            DEFAULT_JOIN_TIMEOUT,
        )
        .await
    }

    pub async fn create_channel(
        &self,
        community: CommunityId,
        name: impl Into<String>,
        topic: impl Into<String>,
    ) -> Result<ChannelId, NodeError> {
        let (name, topic) = (name.into(), topic.into());
        self.call(
            |reply| Command::CreateChannel {
                community,
                name,
                topic,
                reply,
            },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    pub async fn post(
        &self,
        community: CommunityId,
        channel: ChannelId,
        body: impl Into<String>,
    ) -> Result<EventId, NodeError> {
        let body = body.into();
        self.call(
            |reply| Command::Post {
                community,
                channel,
                body,
                reply,
            },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    pub async fn set_display_name(&self, name: impl Into<String>) -> Result<(), NodeError> {
        let display_name = name.into();
        self.call(
            |reply| Command::SetDisplayName {
                display_name,
                reply,
            },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    /// Mints an invite naming this node as a way in.
    pub async fn invite(&self, community: CommunityId) -> Result<Invite, NodeError> {
        self.call(
            |reply| Command::MakeInvite { community, reply },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    pub async fn communities(&self) -> Result<Vec<CommunitySummary>, NodeError> {
        self.call(
            |reply| Command::Communities { reply },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    pub async fn channels(&self, community: CommunityId) -> Result<Vec<ChannelSummary>, NodeError> {
        self.call(
            |reply| Command::Channels { community, reply },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    pub async fn members(&self, community: CommunityId) -> Result<Vec<MemberSummary>, NodeError> {
        self.call(
            |reply| Command::Members { community, reply },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    /// Which members this node currently has a connection to.
    ///
    /// Presence in Kāhui is first-hand only: there is no server keeping a
    /// roster, so a node can report who *it* can reach and nothing more.
    /// A member missing from this list may well be online and talking to
    /// somebody else.
    pub async fn online_members(&self, community: CommunityId) -> Result<Vec<UserId>, NodeError> {
        self.call(
            |reply| Command::OnlineMembers { community, reply },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    pub async fn history(
        &self,
        community: CommunityId,
        channel: ChannelId,
        limit: usize,
    ) -> Result<Vec<Message>, NodeError> {
        self.call(
            |reply| Command::History {
                community,
                channel,
                limit,
                reply,
            },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    pub async fn status(&self) -> Result<Status, NodeError> {
        self.call(|reply| Command::Status { reply }, DEFAULT_COMMAND_TIMEOUT)
            .await
    }

    /// Connects to a peer by multiaddress.
    pub async fn dial(&self, addr: impl Into<String>) -> Result<(), NodeError> {
        let addr = addr.into();
        self.call(
            |reply| Command::Dial { addr, reply },
            DEFAULT_COMMAND_TIMEOUT,
        )
        .await
    }

    /// Asks every connected peer for anything we are missing. Returns the
    /// number of sync requests sent.
    pub async fn sync_now(&self) -> Result<usize, NodeError> {
        self.call(|reply| Command::SyncNow { reply }, DEFAULT_COMMAND_TIMEOUT)
            .await
    }

    /// Stops the engine and flushes state to disk.
    pub async fn shutdown(&self) -> Result<(), NodeError> {
        self.call(|reply| Command::Shutdown { reply }, DEFAULT_COMMAND_TIMEOUT)
            .await
    }
}
