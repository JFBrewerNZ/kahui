//! # kahui-node
//!
//! A running Kahui node: identity, storage and networking, wired together and
//! driven by one task.
//!
//! This is the layer every client is meant to sit on. A terminal client, a
//! Tauri desktop app, a WebAssembly web client and a mobile FFI shim should all
//! talk to a [`NodeHandle`] and differ only in how they render what comes out
//! of it. Nothing above this line needs to know what gossipsub is.
//!
//! ## What a node does on its own
//!
//! Starting one is enough. It loads or creates its keypair, opens its database,
//! rejoins the communities it already knows, dials the peers it last saw, and
//! starts catching up. There is no login, no registration, no bootstrap server
//! and no configuration that points at anybody's infrastructure — a node that
//! has been offline for a month finds its way back using only what is on its
//! own disk.
//!
//! ```no_run
//! use kahui_node::{Node, NodeConfig};
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let node = Node::spawn(NodeConfig {
//!     data_dir: "./alice".into(),
//!     display_name: Some("alice".into()),
//!     ..Default::default()
//! })
//! .await?;
//!
//! let community = node.create_community("Kahui", "Hosted by its members").await?;
//! println!("invite: {}", node.invite(community).await?.encode());
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod api;
pub mod chain;
mod engine;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kahui_proto::{CommunityId, Identity, IdentityError};
use kahui_store::{RedbStore, Store, StoreError};
use tokio::sync::{broadcast, mpsc};

pub use api::{
    CommunityStatus, Message, NodeEvent, NodeHandle, Reachability, Status, DEFAULT_COMMAND_TIMEOUT,
    DEFAULT_JOIN_TIMEOUT,
};
pub use kahui_net::{Invite, InviteError, InvitePeer, NetConfig, NetError};
pub use kahui_proto::{ChannelId, EventId, UserId};
pub use kahui_store::{ChannelSummary, CommunitySummary, MemberSummary};

/// Filename of the node's database inside its data directory.
pub const DATABASE_FILE: &str = "kahui.redb";

/// Meta key holding this node's Ed25519 secret.
pub(crate) const META_IDENTITY: &str = "identity.secret";
/// Meta key holding the operator's chosen display name.
pub(crate) const META_DISPLAY_NAME: &str = "identity.display_name";

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Net(#[from] NetError),

    #[error(transparent)]
    Invite(#[from] InviteError),

    #[error("identity: {0}")]
    Identity(#[from] IdentityError),

    #[error("data directory: {0}")]
    Io(#[from] std::io::Error),

    #[error("this node does not know community {0}")]
    UnknownCommunity(CommunityId),

    #[error("no channel named #{0}")]
    UnknownChannel(String),

    #[error("the node is not running")]
    NotRunning,

    #[error("timed out waiting for the node")]
    Timeout,

    #[error("{0}")]
    Engine(String),
}

/// How a node is started.
#[derive(Clone, Debug)]
pub struct NodeConfig {
    /// Where this node keeps its key and its copy of history. One directory per
    /// node, which is also how several nodes are run side by side for testing.
    pub data_dir: PathBuf,
    /// Display name. Defaults to whatever is already stored, or a name derived
    /// from the node's own id on first run.
    pub display_name: Option<String>,
    pub net: NetConfig,
    /// Extra multiaddresses to dial at startup, beyond remembered peers.
    pub bootstrap: Vec<String>,
    /// How often to announce our addresses to each community.
    pub presence_interval: Duration,
    /// How often to ask peers whether we are missing anything.
    pub sync_interval: Duration,
    /// Capacity of the event broadcast channel.
    pub event_buffer: usize,
    /// Decide our own reachability instead of waiting for peers to work it out.
    ///
    /// AutoNAT is usually right, but it needs peers willing to dial us back and
    /// a little time to ask them. Somebody who knows they are behind
    /// carrier-grade NAT can skip straight to finding a relay; somebody running
    /// a node on a server with an open port can skip the probing entirely.
    pub reachability: Option<Reachability>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        NodeConfig {
            data_dir: PathBuf::from(".kahui"),
            display_name: None,
            net: NetConfig::default(),
            bootstrap: Vec::new(),
            // Frequent enough that a new member meshes with everyone within a
            // few seconds, cheap enough to leave running all day.
            presence_interval: Duration::from_secs(3),
            sync_interval: Duration::from_secs(5),
            event_buffer: 1024,
            reachability: None,
        }
    }
}

/// Entry point for starting a node.
pub struct Node;

impl Node {
    /// Opens the node's storage, loads or creates its identity, and starts the
    /// engine task.
    ///
    /// Must be called from inside a Tokio runtime. The returned handle is the
    /// only way to talk to the node; dropping every clone of it shuts the
    /// engine down.
    pub async fn spawn(config: NodeConfig) -> Result<NodeHandle, NodeError> {
        std::fs::create_dir_all(&config.data_dir)?;
        let store: Arc<dyn Store> = Arc::new(RedbStore::open(config.data_dir.join(DATABASE_FILE))?);

        let identity = load_or_create_identity(store.as_ref())?;
        let display_name = resolve_display_name(store.as_ref(), &identity, &config)?;

        let swarm = kahui_net::build_swarm(&identity, &config.net)?;
        let peer_id = swarm.local_peer_id().to_string();
        let user = identity.user_id();

        let (commands_tx, commands_rx) = mpsc::channel(64);
        let (events_tx, _) = broadcast::channel(config.event_buffer);

        let engine = engine::Engine::new(
            identity,
            display_name,
            store,
            swarm,
            commands_rx,
            events_tx.clone(),
            config,
        );
        tokio::spawn(engine.run());

        Ok(NodeHandle {
            commands: commands_tx,
            events: events_tx,
            user,
            peer_id,
        })
    }
}

/// Reads the node's keypair from its database, generating one on first run.
///
/// The key lives beside the history it signs. Nothing is registered anywhere,
/// and copying the data directory copies the identity with it.
fn load_or_create_identity(store: &dyn Store) -> Result<Identity, NodeError> {
    match store.meta_get(META_IDENTITY)? {
        Some(secret) => Ok(Identity::from_secret(&secret)?),
        None => {
            let identity = Identity::generate();
            store.meta_put(META_IDENTITY, &identity.secret_bytes())?;
            Ok(identity)
        }
    }
}

fn resolve_display_name(
    store: &dyn Store,
    identity: &Identity,
    config: &NodeConfig,
) -> Result<String, NodeError> {
    if let Some(name) = &config.display_name {
        store.meta_put(META_DISPLAY_NAME, name.as_bytes())?;
        return Ok(name.clone());
    }
    if let Some(stored) = store.meta_get(META_DISPLAY_NAME)? {
        return Ok(String::from_utf8_lossy(&stored).into_owned());
    }
    let generated = format!("kahui-{}", identity.user_id().short());
    store.meta_put(META_DISPLAY_NAME, generated.as_bytes())?;
    Ok(generated)
}

/// Wall clock in milliseconds since the Unix epoch.
///
/// Only ever used to stamp events for display. Ordering comes from Lamport
/// clocks precisely so that a node with a wrong clock, or a lying one, cannot
/// reorder anybody's history.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
