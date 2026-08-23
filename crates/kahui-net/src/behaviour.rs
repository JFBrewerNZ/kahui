//! The libp2p behaviour stack a Kahui node runs.
//!
//! Four protocols, each doing one job:
//!
//! | protocol | job |
//! |---|---|
//! | `gossipsub` | fan new events out to everyone online, one topic per community |
//! | `request-response` | direct sync, for catching up on what gossip missed |
//! | `identify` | learn our own public address, and our peers' |
//! | `mdns` | find members on the same LAN with no configuration at all |
//!
//! Nothing here is a server. Every node runs the identical stack and every node
//! both serves and consumes history, which is why a community survives losing
//! any particular member, including the one who created it.

use std::time::Duration;

use kahui_proto::{CommunityId, Identity};
use libp2p::identity::Keypair;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{
    gossipsub, identify, mdns, noise,
    request_response::{self, ProtocolSupport},
    tcp, yamux, Multiaddr, StreamProtocol, Swarm, SwarmBuilder,
};

use crate::wire::{topic_name, GossipMessage, SyncRequest, SyncResponse, SYNC_PROTOCOL};
use crate::NetError;

/// Advertised by `identify`, so a peer can see what it is talking to.
pub const AGENT_VERSION: &str = concat!("kahui/", env!("CARGO_PKG_VERSION"));
/// Protocol name for `identify`.
pub const IDENTIFY_PROTOCOL: &str = "/kahui/id/1.0.0";

/// How long an idle connection is kept.
///
/// Worth being generous: an open connection is what makes a sync round trip
/// cheap, and holding one to every member of a small community costs almost
/// nothing.
const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(300);

/// How a node listens and discovers.
#[derive(Clone, Debug)]
pub struct NetConfig {
    /// Addresses to listen on. Port 0 asks the OS to choose.
    pub listen: Vec<Multiaddr>,
    /// Announce and discover on the local network. Convenient on a LAN,
    /// pointless on a cloud VM, and easy to turn off when testing without it.
    pub enable_mdns: bool,
    /// Gossipsub heartbeat. Drives mesh maintenance and gossip fan-out.
    pub heartbeat: Duration,
}

impl Default for NetConfig {
    fn default() -> Self {
        NetConfig {
            listen: vec![
                "/ip4/0.0.0.0/tcp/0"
                    .parse()
                    .expect("hardcoded multiaddr is valid"),
                "/ip4/0.0.0.0/udp/0/quic-v1"
                    .parse()
                    .expect("hardcoded multiaddr is valid"),
            ],
            enable_mdns: true,
            heartbeat: Duration::from_secs(1),
        }
    }
}

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub sync: request_response::cbor::Behaviour<SyncRequest, SyncResponse>,
    pub identify: identify::Behaviour,
    pub mdns: Toggle<mdns::tokio::Behaviour>,
}

impl Behaviour {
    fn new(key: &Keypair, config: &NetConfig) -> Result<Self, NetError> {
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(config.heartbeat)
            // Strict validation means gossipsub itself checks the sending
            // peer's signature. Events carry their own author signature on top;
            // this is defence at the transport layer, not a substitute.
            .validation_mode(gossipsub::ValidationMode::Strict)
            // Content-address messages so the same event arriving from three
            // peers is recognised as one message and forwarded once.
            .message_id_fn(|message: &gossipsub::Message| {
                gossipsub::MessageId::from(kahui_proto::Hash32::digest(&message.data).to_bytes())
            })
            // Kahui communities start at two or three members. The defaults are
            // tuned for large topics and would leave a mesh that small
            // permanently under-provisioned.
            .mesh_n_low(1)
            .mesh_n(4)
            .mesh_n_high(8)
            .mesh_outbound_min(0)
            // Publish to every known subscriber, not just the mesh. On a small
            // network this is both cheaper and more reliable.
            .flood_publish(true)
            .build()
            .map_err(|e| NetError::Config(e.to_string()))?;

        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Signed(key.clone()),
            gossipsub_config,
        )
        .map_err(|e| NetError::Config(e.to_string()))?;

        let sync = request_response::cbor::Behaviour::new(
            [(StreamProtocol::new(SYNC_PROTOCOL), ProtocolSupport::Full)],
            request_response::Config::default().with_request_timeout(Duration::from_secs(30)),
        );

        let identify = identify::Behaviour::new(
            identify::Config::new(IDENTIFY_PROTOCOL.to_string(), key.public())
                .with_agent_version(AGENT_VERSION.to_string()),
        );

        let mdns = if config.enable_mdns {
            Toggle::from(Some(mdns::tokio::Behaviour::new(
                mdns::Config::default(),
                key.public().to_peer_id(),
            )?))
        } else {
            Toggle::from(None)
        };

        Ok(Behaviour {
            gossipsub,
            sync,
            identify,
            mdns,
        })
    }

    /// Starts receiving a community's traffic.
    pub fn subscribe(&mut self, community: &CommunityId) -> Result<bool, NetError> {
        let topic = gossipsub::IdentTopic::new(topic_name(community));
        self.gossipsub
            .subscribe(&topic)
            .map_err(|e| NetError::Subscribe(e.to_string()))
    }

    /// Publishes to a community's topic.
    ///
    /// `NoPeersSubscribedToTopic` is normal, not exceptional: a node that is
    /// alone, or the first to start, has nobody to publish to. The event is
    /// already in its own store, and peers will pick it up by sync when they
    /// appear.
    pub fn publish(
        &mut self,
        community: &CommunityId,
        message: &GossipMessage,
    ) -> Result<(), gossipsub::PublishError> {
        let topic = gossipsub::IdentTopic::new(topic_name(community));
        self.gossipsub.publish(topic, message.encode()).map(|_| ())
    }
}

/// Builds a ready-to-run swarm: transports, encryption, behaviours, listeners.
///
/// TCP and QUIC are both enabled. QUIC connects faster and traverses NAT
/// better; TCP is the fallback where UDP is blocked.
///
/// Must be called from inside a Tokio runtime: binding the listeners registers
/// sockets with the reactor.
pub fn build_swarm(identity: &Identity, config: &NetConfig) -> Result<Swarm<Behaviour>, NetError> {
    let keypair = crate::peer::libp2p_keypair(identity);

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )
        .map_err(|e| NetError::Build(Box::new(e)))?
        .with_quic()
        .with_behaviour(|key| {
            Behaviour::new(key, config)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        })
        .map_err(|e| NetError::Build(Box::new(e)))?
        .with_swarm_config(|c| c.with_idle_connection_timeout(IDLE_CONNECTION_TIMEOUT))
        .build();

    for addr in &config.listen {
        swarm.listen_on(addr.clone())?;
    }

    Ok(swarm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_listens_on_both_transports() {
        let config = NetConfig::default();
        assert_eq!(config.listen.len(), 2);
        let rendered: Vec<String> = config.listen.iter().map(|a| a.to_string()).collect();
        assert!(rendered.iter().any(|a| a.contains("/tcp/")));
        assert!(rendered.iter().any(|a| a.contains("/quic-v1")));
    }

    #[tokio::test]
    async fn a_swarm_builds_and_listens() {
        let identity = Identity::generate();
        let swarm = build_swarm(&identity, &NetConfig::default()).expect("swarm builds");
        assert_eq!(
            *swarm.local_peer_id(),
            crate::peer::peer_id_of(&identity.user_id()).unwrap()
        );
    }

    #[tokio::test]
    async fn mdns_can_be_turned_off() {
        let config = NetConfig {
            enable_mdns: false,
            ..NetConfig::default()
        };
        assert!(build_swarm(&Identity::generate(), &config).is_ok());
    }
}
