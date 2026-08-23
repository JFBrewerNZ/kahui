//! The libp2p behaviour stack a Kahui node runs.
//!
//! | protocol | job |
//! |---|---|
//! | `gossipsub` | fan new events out to everyone online, one topic per community |
//! | `request-response` | direct sync, for catching up on what gossip missed |
//! | `identify` | learn our own address, our peers' addresses, and what they speak |
//! | `mdns` | find members on the same network with no configuration at all |
//! | `upnp` | ask the home router to open a port, so nothing else is needed |
//! | `autonat` | find out whether anybody can actually dial us |
//! | `relay` (server) | carry traffic for members who cannot be dialled |
//! | `relay` (client) | be carried, when we are one of them |
//! | `dcutr` | escape the relay by hole punching to a direct connection |
//!
//! Nothing here is a server, and the last four are what keeps that true for
//! people behind home routers. A node that *can* be reached offers to relay for
//! the ones that cannot, so a community's reachability comes from its own
//! members rather than from anybody's infrastructure. The relayed connection is
//! then treated as a stepping stone rather than a destination: `dcutr` uses it
//! to coordinate a hole punch, and the relay drops out of the path as soon as
//! that succeeds.

use std::time::Duration;

use kahui_proto::{CommunityId, Identity};
use libp2p::identity::Keypair;
use libp2p::swarm::behaviour::toggle::Toggle;
use libp2p::swarm::NetworkBehaviour;
use libp2p::{
    autonat, dcutr, gossipsub, identify, mdns, noise, relay,
    request_response::{self, ProtocolSupport},
    tcp, upnp, yamux, Multiaddr, StreamProtocol, Swarm, SwarmBuilder,
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
    /// Announce and discover on the local network.
    ///
    /// This is how neighbours find each other with no configuration, and the
    /// only discovery mechanism that keeps working with no internet at all.
    pub enable_mdns: bool,
    /// Gossipsub heartbeat. Drives mesh maintenance and gossip fan-out.
    pub heartbeat: Duration,
    /// Offer to carry traffic for members who cannot be dialled, and accept the
    /// same offer when we are one of them.
    ///
    /// On by default. A community that cannot reach its own members has not
    /// avoided depending on infrastructure; it has just failed.
    pub enable_relay: bool,
    /// Ask the router to forward a port, using UPnP.
    ///
    /// When it works this is by far the best outcome: the node becomes
    /// genuinely reachable, with no relay, no hole punch and nobody else
    /// involved at all. Most consumer routers support it and have it switched
    /// on. Some have it switched off, and a few have it switched off for good
    /// reasons, so this fails quietly and the relay path takes over.
    pub enable_upnp: bool,
    /// Whether a private address counts as being reachable.
    ///
    /// On the internet `192.168.1.5` means nothing to anybody outside the
    /// house, so the default is `false`: a node holding only private addresses
    /// concludes it needs a relay, and goes looking for one.
    ///
    /// On an isolated network — a hall, a building, a neighbourhood mesh with
    /// no internet at all — private addresses are the *only* addresses there
    /// are, and a node holding one is perfectly reachable by its neighbours.
    /// Setting this stops the node treating that as a problem to route around.
    pub lan_reachable: bool,
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
            enable_relay: true,
            enable_upnp: true,
            lan_reachable: false,
        }
    }
}

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub sync: request_response::cbor::Behaviour<SyncRequest, SyncResponse>,
    pub identify: identify::Behaviour,
    pub mdns: Toggle<mdns::tokio::Behaviour>,
    /// Tells us whether anybody can dial us. Every node answers these probes
    /// for its peers as well as asking them, so the answer costs nothing but
    /// participation.
    pub autonat: Toggle<autonat::Behaviour>,
    /// Carrying for others.
    pub relay: Toggle<relay::Behaviour>,
    /// Being carried.
    pub relay_client: Toggle<relay::client::Behaviour>,
    /// Turning a carried connection into a direct one.
    pub dcutr: Toggle<dcutr::Behaviour>,
    /// Asking the router nicely, which is better than all of the above.
    pub upnp: Toggle<upnp::tokio::Behaviour>,
}

impl Behaviour {
    fn new(
        key: &Keypair,
        relay_client: relay::client::Behaviour,
        config: &NetConfig,
    ) -> Result<Self, NetError> {
        let peer_id = key.public().to_peer_id();

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
                peer_id,
            )?))
        } else {
            Toggle::from(None)
        };

        let autonat = if config.enable_relay {
            Toggle::from(Some(autonat::Behaviour::new(
                peer_id,
                autonat::Config {
                    // On an isolated network the private addresses are the real
                    // ones. Refusing to count them would have every node
                    // concluding it is unreachable, when in fact they can all
                    // reach each other perfectly well.
                    only_global_ips: !config.lan_reachable,
                    // Laptops move between networks, and reachability moves
                    // with them, so ask again reasonably often.
                    boot_delay: Duration::from_secs(4),
                    refresh_interval: Duration::from_secs(60),
                    retry_interval: Duration::from_secs(12),
                    ..autonat::Config::default()
                },
            )))
        } else {
            Toggle::from(None)
        };

        let (relay, relay_client, dcutr) = if config.enable_relay {
            (
                Toggle::from(Some(relay::Behaviour::new(
                    peer_id,
                    relay::Config {
                        // Enough to carry a community, bounded so that being a
                        // good neighbour cannot be turned into being somebody
                        // else's free infrastructure.
                        max_reservations: 64,
                        max_circuits: 32,
                        max_circuit_duration: Duration::from_secs(30 * 60),
                        max_circuit_bytes: 512 * 1024 * 1024,
                        ..relay::Config::default()
                    },
                ))),
                Toggle::from(Some(relay_client)),
                Toggle::from(Some(dcutr::Behaviour::new(peer_id))),
            )
        } else {
            (Toggle::from(None), Toggle::from(None), Toggle::from(None))
        };

        let upnp = if config.enable_upnp {
            Toggle::from(Some(upnp::tokio::Behaviour::default()))
        } else {
            Toggle::from(None)
        };

        Ok(Behaviour {
            gossipsub,
            sync,
            identify,
            mdns,
            autonat,
            relay,
            relay_client,
            dcutr,
            upnp,
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
        // Adds a transport that can dial `/p2p-circuit` addresses, and hands
        // back the client behaviour that drives it.
        .with_relay_client(noise::Config::new, yamux::Config::default)
        .map_err(|e| NetError::Build(Box::new(e)))?
        .with_behaviour(|key, relay_client| {
            Behaviour::new(key, relay_client, config)
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

    #[test]
    fn relaying_is_on_by_default() {
        let config = NetConfig::default();
        assert!(config.enable_relay);
        // ...but a private address is not assumed to be reachable, because on
        // the internet it is not.
        assert!(!config.lan_reachable);
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

    #[tokio::test]
    async fn relaying_can_be_turned_off() {
        let config = NetConfig {
            enable_relay: false,
            ..NetConfig::default()
        };
        assert!(build_swarm(&Identity::generate(), &config).is_ok());
    }
}
