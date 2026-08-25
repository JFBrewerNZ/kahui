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

/// Port a node listens on unless told otherwise.
///
/// A *fixed* default matters more than which number it is. With a random port,
/// a home user who forwards one on their router is reachable exactly until they
/// restart, which is worse than useless — it looks like it worked.
pub const DEFAULT_PORT: u16 = 4001;

/// Everything a node listens on, given a port.
///
/// IPv6 is not an afterthought here. A great many home connections now have a
/// globally routable IPv6 address and no NAT in front of it at all, so two
/// desktops that could never reach each other over IPv4 often can over IPv6
/// with nothing configured. Leaving it out was leaving the easiest case on the
/// table.
pub fn default_listen_addrs(port: u16) -> Vec<Multiaddr> {
    [
        format!("/ip4/0.0.0.0/tcp/{port}"),
        format!("/ip4/0.0.0.0/udp/{port}/quic-v1"),
        format!("/ip6/::/tcp/{port}"),
        format!("/ip6/::/udp/{port}/quic-v1"),
    ]
    .iter()
    .filter_map(|addr| addr.parse().ok())
    .collect()
}

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
    /// involved at all.
    ///
    /// Three protocols get tried, because router support is a lottery and a
    /// router that refuses one often accepts another: UPnP-IGD here in the
    /// swarm, plus PCP and NAT-PMP from [`crate::portmap`]. All of them fail
    /// quietly, and the relay path takes over if none works.
    pub enable_port_mapping: bool,
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
            listen: default_listen_addrs(DEFAULT_PORT),
            enable_mdns: true,
            heartbeat: Duration::from_secs(1),
            enable_relay: true,
            enable_port_mapping: true,
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

        let upnp = if config.enable_port_mapping {
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

    listen_on_all(&mut swarm, &config.listen)?;
    Ok(swarm)
}

/// Binds every address it can, and falls back to an OS-chosen port.
///
/// Individual failures are expected and survivable: a machine with IPv6
/// switched off cannot bind an IPv6 address, and a fixed port may already be
/// taken by another copy. Only being unable to listen at *all* is fatal.
///
/// The fallback matters for the fixed default port. Without it, a second node on
/// one machine — which is how anybody tests this — would simply refuse to start.
fn listen_on_all(swarm: &mut Swarm<Behaviour>, addrs: &[Multiaddr]) -> Result<(), NetError> {
    // Asking for nothing is a real request, not a failure to bind. A node that
    // can only dial out — a phone behind carrier NAT, say — listens on nothing
    // and reaches the world entirely through other members.
    if addrs.is_empty() {
        return Ok(());
    }

    // Check the port before handing it to libp2p, because libp2p will not tell
    // us the truth about it. Its TCP transport sets `SO_REUSEADDR`, and on
    // Windows that lets a second process bind a port the first already holds:
    // both are told they are listening, and arriving connections go to whichever
    // the OS feels like. A silent half-working node is far worse than a loud
    // move to a different port.
    let requested = port_of(&addrs[0]);
    let addrs = if port_is_free(requested) {
        addrs.to_vec()
    } else {
        tracing::warn!(
            port = requested,
            "that port is already in use, so this node will take whatever is free; \
             any port you forwarded to it will not reach this node"
        );
        default_listen_addrs(0)
    };

    let mut bound = 0;
    let mut last_error = None;

    for addr in &addrs {
        match swarm.listen_on(addr.clone()) {
            Ok(_) => bound += 1,
            Err(err) => {
                tracing::debug!(%addr, %err, "could not listen there");
                last_error = Some(err);
            }
        }
    }

    if bound > 0 {
        return Ok(());
    }

    // Nothing bound at all, which the check above should have prevented. Try
    // once more with an OS-chosen port rather than refusing to start.
    tracing::warn!("could not bind anything; asking the OS for any free port");
    for addr in default_listen_addrs(0) {
        if swarm.listen_on(addr).is_ok() {
            bound += 1;
        }
    }

    if bound > 0 {
        Ok(())
    } else {
        Err(last_error
            .map(NetError::Listen)
            .unwrap_or_else(|| NetError::Config("no listen addresses were configured".into())))
    }
}

/// The port a listen address asks for, or zero if it names none.
fn port_of(addr: &Multiaddr) -> u16 {
    addr.iter()
        .find_map(|part| match part {
            libp2p::multiaddr::Protocol::Tcp(port) | libp2p::multiaddr::Protocol::Udp(port) => {
                Some(port)
            }
            _ => None,
        })
        .unwrap_or(0)
}

/// Whether a port is genuinely available on both transports.
///
/// Binds it plainly and lets it go. A plain socket does not set `SO_REUSEADDR`,
/// so unlike libp2p's it fails when somebody else already holds the port —
/// which is the whole point of asking.
///
/// There is a race between letting go and libp2p binding it, but it is a very
/// short one and losing it is no worse than not having checked.
fn port_is_free(port: u16) -> bool {
    if port == 0 {
        return true;
    }
    std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
        && std::net::UdpSocket::bind(("0.0.0.0", port)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_covers_both_transports_and_both_ip_versions() {
        let rendered: Vec<String> = NetConfig::default()
            .listen
            .iter()
            .map(|a| a.to_string())
            .collect();
        assert!(rendered
            .iter()
            .any(|a| a.contains("/ip4/") && a.contains("/tcp/")));
        assert!(rendered
            .iter()
            .any(|a| a.contains("/ip4/") && a.contains("/quic-v1")));
        // IPv6 often has no NAT in front of it, which makes it the easiest way
        // for two home machines to reach each other.
        assert!(rendered
            .iter()
            .any(|a| a.contains("/ip6/") && a.contains("/tcp/")));
        assert!(rendered
            .iter()
            .any(|a| a.contains("/ip6/") && a.contains("/quic-v1")));
    }

    #[test]
    fn a_port_somebody_else_holds_is_not_reported_free() {
        // This is the check that stops two nodes sharing a port on Windows,
        // where libp2p's own listener would happily bind it twice and then
        // deliver connections to whichever socket the OS picked.
        let held = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("a port");
        let port = held.local_addr().unwrap().port();
        assert!(!port_is_free(port), "port {port} is held by this test");

        drop(held);
        assert!(port_is_free(port), "and free once let go");
    }

    #[test]
    fn port_zero_is_always_free_because_it_means_any_port() {
        assert!(port_is_free(0));
    }

    #[test]
    fn a_listen_address_reports_the_port_it_asks_for() {
        assert_eq!(port_of(&"/ip4/0.0.0.0/tcp/4001".parse().unwrap()), 4001);
        assert_eq!(port_of(&"/ip6/::/udp/4001/quic-v1".parse().unwrap()), 4001);
        assert_eq!(port_of(&"/ip4/0.0.0.0/tcp/0".parse().unwrap()), 0);
    }

    #[tokio::test]
    async fn asking_to_listen_on_nothing_listens_on_nothing() {
        // Otherwise a dial-only node would quietly acquire a listener and stop
        // being dial-only, which is the whole point of the household case.
        let config = NetConfig {
            listen: vec![],
            enable_mdns: false,
            ..NetConfig::default()
        };
        let swarm = build_swarm(&Identity::generate(), &config).expect("should build");
        assert_eq!(swarm.listeners().count(), 0);
    }

    #[test]
    fn the_default_port_is_fixed_not_random() {
        // A random port would silently undo any port the user forwards.
        assert_ne!(DEFAULT_PORT, 0);
        assert!(NetConfig::default()
            .listen
            .iter()
            .all(|addr| addr.to_string().contains(&DEFAULT_PORT.to_string())));
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
