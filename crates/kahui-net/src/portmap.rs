//! Asking the router to open a port, so a home machine can be dialled.
//!
//! This is the difference between "you can host a community" and "you can host
//! a community if you own a server", and it is the whole point of the project.
//!
//! ## Why three protocols
//!
//! Routers are meant to support UPnP-IGD, PCP (RFC 6887) or NAT-PMP (RFC 6886),
//! and in practice support is a lottery. Firmware ships with one implemented,
//! one half-implemented and one switched off; some accept a mapping request on
//! one protocol and answer `501 Action Failed` on another. The correct response
//! is not to pick a favourite, it is to ask every way we can and use whichever
//! answer comes back.
//!
//! libp2p's `upnp` behaviour covers UPnP-IGD. This module covers the other two:
//! [`crab_nat`] tries PCP first and falls back to NAT-PMP, both over the same
//! UDP port on the gateway.
//!
//! ## What success buys
//!
//! A mapped port makes the node *directly* dialable. No relay, no third party,
//! no bootstrap list — a peer reads the address out of an invite and connects.
//! That is strictly better than relaying, so it is always worth trying first.
//!
//! ## What failure costs
//!
//! Nothing. Some routers have port mapping disabled by policy, carrier-grade NAT
//! has no port to map, and a corporate network will simply drop the request. The
//! attempt is cheap, times out quickly, and failure just leaves the node relying
//! on the paths it already had.

use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU16;
use std::time::Duration;

use crab_nat::{
    GatewayAddress, InternetProtocol, PortMapping, PortMappingOptions, PortMappingType,
};
use libp2p::Multiaddr;
use tokio::sync::mpsc;

/// Which protocol actually worked, for the benefit of anyone reading the logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MapProtocol {
    /// Port Control Protocol, RFC 6887. The newer of the two.
    Pcp,
    /// NAT Port Mapping Protocol, RFC 6886. Older, and often the only one on.
    NatPmp,
    /// UPnP-IGD, asked for with a permanent lease.
    ///
    /// Distinct from the UPnP the swarm attempts, which always asks for a timed
    /// lease. See [`upnp_map`] for why that difference decides whether a great
    /// many home routers say yes or no.
    Upnp,
}

impl MapProtocol {
    /// A name to show a person, not a machine.
    pub fn label(self) -> &'static str {
        match self {
            MapProtocol::Pcp => "PCP",
            MapProtocol::NatPmp => "NAT-PMP",
            MapProtocol::Upnp => "UPnP-IGD",
        }
    }
}

/// What the mapper has to say about our reachability.
#[derive(Clone, Debug)]
pub enum PortMapUpdate {
    /// The router opened a port. These addresses should reach us from anywhere.
    Opened {
        /// Dialable addresses, TCP and QUIC, built from the router's answer.
        external: Vec<Multiaddr>,
        /// Which protocol the router accepted.
        protocol: MapProtocol,
    },

    /// The router mapped a port but would not say what address it is on.
    ///
    /// Real routers do this: some answer `GetExternalIPAddress` with `501
    /// Action Failed` while still honouring `AddPortMapping`. The port is open,
    /// so throwing the result away would be silly — the missing half is the
    /// public IP, and any peer we speak to can supply that.
    MappedWithoutAddress {
        /// The port the router opened.
        port: u16,
        /// Which protocol the router accepted.
        protocol: MapProtocol,
    },

    /// The router would not open a port, and why.
    ///
    /// Worth surfacing rather than swallowing: it is the difference between
    /// telling somebody "forward a port yourself" and leaving them guessing.
    Refused(String),
}

/// Anything that stops us getting a port.
#[derive(Debug, thiserror::Error)]
pub enum PortMapError {
    #[error("no default gateway found: {0}")]
    NoGateway(String),

    #[error("the gateway has no usable address")]
    GatewayHasNoAddress,

    #[error("this machine has no address on the local network")]
    NoLocalAddress,

    #[error("port 0 cannot be mapped")]
    PortZero,

    #[error("the router refused: {0}")]
    Refused(String),
}

/// How long to wait on a router that may not be listening at all.
///
/// Short and few. A router that speaks PCP answers in milliseconds; one that
/// does not will never answer, and making a new user wait on that is rude.
const TIMEOUTS: crab_nat::TimeoutConfig = crab_nat::TimeoutConfig {
    initial_timeout: Duration::from_millis(250),
    max_retries: 3,
    max_retry_timeout: Some(Duration::from_secs(2)),
};

/// Renew this far before the mapping is due to expire.
///
/// Generous on purpose. A lapsed mapping silently drops us off the network,
/// which is much worse than a redundant renewal.
const RENEW_MARGIN: Duration = Duration::from_secs(60);

/// Retry from scratch this often after a failure.
///
/// Routers get rebooted, laptops move between networks, and a network that
/// refused an hour ago may not refuse now.
const RETRY_AFTER: Duration = Duration::from_secs(15 * 60);

/// Where we are on the local network, and where the router is.
#[derive(Clone, Copy, Debug)]
struct LocalNetwork {
    gateway: Ipv4Addr,
    client: Ipv4Addr,
}

/// Finds the router and our address on its network.
///
/// IPv4 only, deliberately. A machine with working IPv6 usually has a globally
/// routable address already and needs no mapping — see [`crate::default_listen_addrs`].
fn discover() -> Result<LocalNetwork, PortMapError> {
    let gateway = netdev::get_default_gateway().map_err(PortMapError::NoGateway)?;
    let gateway = *gateway
        .ipv4
        .first()
        .ok_or(PortMapError::GatewayHasNoAddress)?;

    let interface = netdev::get_default_interface().map_err(PortMapError::NoGateway)?;
    let client = interface
        .ipv4
        .iter()
        .map(|net| net.addr())
        .find(|addr| !addr.is_loopback())
        .ok_or(PortMapError::NoLocalAddress)?;

    Ok(LocalNetwork { gateway, client })
}

/// Asks the router to open one port for one transport.
///
/// Both protocols get a real attempt. [`PortMapping::new`] tries PCP and falls
/// back to NAT-PMP only when the gateway explicitly says "wrong version" — so a
/// router that silently ignores PCP never gets asked the other way. Since a
/// timeout is precisely the case where the older protocol might still answer,
/// we ask again ourselves rather than take silence for a no.
async fn map_one(
    net: LocalNetwork,
    protocol: InternetProtocol,
    port: NonZeroU16,
) -> Result<PortMapping, PortMapError> {
    let gateway = GatewayAddress::IpV4(net.gateway);
    let options = PortMappingOptions {
        // Ask for the same number outside as inside. Routers need not agree,
        // but when they do, an address we hand out stays valid for longer.
        external_port: Some(port),
        lifetime_seconds: Some(crab_nat::RECOMMENDED_MAPPING_LIFETIME_SECONDS),
        timeout_config: Some(TIMEOUTS),
    };

    let pcp_failure =
        match PortMapping::new(gateway, IpAddr::V4(net.client), protocol, port, options).await {
            Ok(mapping) => return Ok(mapping),
            Err(err) => err,
        };

    match crab_nat::natpmp::port_mapping(gateway, protocol, port, options).await {
        Ok(mapping) => Ok(mapping),
        // Report the first failure: PCP's message is the more informative of
        // the two, and NAT-PMP's is almost always the same timeout again.
        Err(_) => Err(PortMapError::Refused(pcp_failure.to_string())),
    }
}

/// The public address the router says we have.
///
/// PCP hands this back with the mapping. NAT-PMP requires a second question,
/// which is worth asking: without an external address, a mapped port is a door
/// with no street number.
async fn external_ip(net: LocalNetwork, mapping: &PortMapping) -> Result<IpAddr, PortMapError> {
    match mapping.mapping_type() {
        PortMappingType::Pcp { external_ip, .. } => Ok(external_ip),
        PortMappingType::NatPmp => {
            crab_nat::natpmp::external_address(GatewayAddress::IpV4(net.gateway), Some(TIMEOUTS))
                .await
                .map(IpAddr::V4)
                .map_err(|err| PortMapError::Refused(err.to_string()))
        }
    }
}

/// Which protocol answered.
fn protocol_of(mapping: &PortMapping) -> MapProtocol {
    match mapping.mapping_type() {
        PortMappingType::Pcp { .. } => MapProtocol::Pcp,
        PortMappingType::NatPmp => MapProtocol::NatPmp,
    }
}

/// Builds the addresses a peer would dial, given what the router told us.
///
/// Kept separate from the networking so it can be tested without a router.
pub fn addrs_for(ip: IpAddr, tcp_port: Option<u16>, quic_port: Option<u16>) -> Vec<Multiaddr> {
    let base = Multiaddr::from(ip);
    let mut addrs = Vec::new();

    if let Some(port) = tcp_port {
        addrs.push(base.clone().with(libp2p::multiaddr::Protocol::Tcp(port)));
    }
    if let Some(port) = quic_port {
        addrs.push(
            base.with(libp2p::multiaddr::Protocol::Udp(port))
                .with(libp2p::multiaddr::Protocol::QuicV1),
        );
    }

    addrs
}

/// How long a UPnP lease to ask for before settling for a permanent one.
const UPNP_LEASE_SECONDS: u32 = 3600;

/// Where SSDP discovery shouts when nobody answers directly.
const MULTICAST: std::net::SocketAddr = std::net::SocketAddr::new(
    IpAddr::V4(std::net::Ipv4Addr::new(239, 255, 255, 250)),
    1900,
);

/// Asks the router over UPnP-IGD, and settles for a permanent lease.
///
/// This exists because of one specific, very common failure. libp2p's own UPnP
/// support always requests a timed lease — one hour, not configurable — and a
/// large number of consumer routers, ISP-supplied ones especially, implement
/// only *permanent* mappings. Handed a lease duration they cannot honour, they
/// answer `501 Action Failed` or `725 OnlyPermanentLeasesSupported`, and the
/// whole attempt is written off as "this router has UPnP switched off".
///
/// It does not. It just wants to be asked differently. So we ask politely
/// first — a lease that expires cleans up after itself — and when the router
/// refuses, we ask for the permanent mapping it is willing to give.
///
/// Re-adding the same mapping on each launch is idempotent, so a permanent entry
/// does not accumulate.
async fn upnp_map(net: LocalNetwork, port: NonZeroU16) -> Result<PortMapUpdate, PortMapError> {
    use igd_next::{aio::tokio as igd_tokio, PortMappingProtocol, SearchOptions};

    let search = |broadcast: std::net::SocketAddr, wait: u64| async move {
        igd_tokio::search_gateway(SearchOptions {
            broadcast_address: broadcast,
            timeout: Some(Duration::from_secs(wait)),
            single_search_timeout: Some(Duration::from_secs(wait)),
            ..Default::default()
        })
        .await
    };

    // Ask the machine that actually routes our traffic before asking the network
    // at large. A multicast search returns whoever answers first, and that is
    // not always the router: a NAS or a set-top box may advertise the same
    // service and then refuse every request, which looks exactly like a router
    // with UPnP switched off. Seen on the network this was developed on, where
    // the imposter answered and the real gateway never did.
    let unicast = std::net::SocketAddr::new(IpAddr::V4(net.gateway), 1900);
    let gateway = match search(unicast, 2).await {
        Ok(gateway) => gateway,
        Err(err) => {
            tracing::debug!(%err, "the gateway did not answer directly; searching the network");
            search(MULTICAST, 3)
                .await
                .map_err(|err| PortMapError::Refused(format!("no UPnP gateway: {err}")))?
        }
    };

    let local = std::net::SocketAddr::new(IpAddr::V4(net.client), port.get());
    let mut tcp_open = false;
    let mut udp_open = false;

    for (protocol, opened) in [
        (PortMappingProtocol::TCP, &mut tcp_open),
        (PortMappingProtocol::UDP, &mut udp_open),
    ] {
        let mut added = gateway
            .add_port(protocol, port.get(), local, UPNP_LEASE_SECONDS, "Kahui")
            .await;

        if let Err(err) = &added {
            tracing::debug!(%err, "the router refused a timed lease; asking for a permanent one");
            added = gateway
                .add_port(protocol, port.get(), local, 0, "Kahui")
                .await;
        }

        match added {
            Ok(()) => *opened = true,
            Err(err) => tracing::debug!(%err, "the router refused that mapping"),
        }
    }

    if !tcp_open && !udp_open {
        return Err(PortMapError::Refused(
            "the router would not add a mapping on either transport".into(),
        ));
    }

    // Only now ask where we are. Getting this wrong is not fatal: the port is
    // open either way, and a peer's view of us supplies the missing half.
    match gateway.get_external_ip().await {
        Ok(external) => Ok(PortMapUpdate::Opened {
            external: addrs_for(
                external,
                tcp_open.then_some(port.get()),
                udp_open.then_some(port.get()),
            ),
            protocol: MapProtocol::Upnp,
        }),
        Err(err) => {
            tracing::debug!(%err, "the router opened the port but will not say its address");
            Ok(PortMapUpdate::MappedWithoutAddress {
                port: port.get(),
                protocol: MapProtocol::Upnp,
            })
        }
    }
}

/// A pair of mappings — TCP for reliability, UDP so QUIC works too — held open.
struct Held {
    tcp: Option<PortMapping>,
    udp: Option<PortMapping>,
}

impl Held {
    /// When the soonest of these needs renewing.
    fn renew_in(&self) -> Duration {
        let now = std::time::Instant::now();
        [self.tcp.as_ref(), self.udp.as_ref()]
            .into_iter()
            .flatten()
            .map(|m| m.expiration().saturating_duration_since(now))
            .min()
            .unwrap_or(RETRY_AFTER)
            .saturating_sub(RENEW_MARGIN)
            // Never spin: a router that hands out very short lifetimes should
            // not turn into a request loop.
            .max(Duration::from_secs(30))
    }
}

/// Opens both ports, and reports the addresses they can be reached at.
async fn open(net: LocalNetwork, port: NonZeroU16) -> Result<(Held, PortMapUpdate), PortMapError> {
    // TCP is the one that matters most, so its failure is the failure we report.
    let tcp = map_one(net, InternetProtocol::Tcp, port).await;
    let udp = map_one(net, InternetProtocol::Udp, port).await.ok();

    let mapped = match (&tcp, &udp) {
        (Ok(m), _) | (Err(_), Some(m)) => m,

        // Nothing is listening on the gateway's mapping port, which rules out
        // both PCP and NAT-PMP. UPnP-IGD is a different port and a different
        // protocol, so it is still very much worth asking.
        (Err(err), None) => {
            let refused = err.to_string();
            return match upnp_map(net, port).await {
                Ok(update) => Ok((
                    Held {
                        tcp: None,
                        udp: None,
                    },
                    update,
                )),
                Err(upnp_err) => {
                    tracing::debug!(%upnp_err, "UPnP would not either");
                    Err(PortMapError::Refused(refused))
                }
            };
        }
    };

    let protocol = protocol_of(mapped);
    let held = Held {
        tcp: tcp.as_ref().ok().cloned(),
        udp: udp.clone(),
    };

    // The port is open. Where it is open *at* is a second question, and for
    // NAT-PMP a second request, which can fail on its own. Losing that answer
    // is no reason to throw away a working mapping — a peer can tell us the
    // address we could not get here.
    let Ok(ip) = external_ip(net, mapped).await else {
        tracing::debug!("the router opened a port but would not say its address");
        return Ok((
            held,
            PortMapUpdate::MappedWithoutAddress {
                port: port.get(),
                protocol,
            },
        ));
    };

    let external = addrs_for(
        ip,
        tcp.as_ref().ok().map(|m| m.external_port().get()),
        udp.as_ref().map(|m| m.external_port().get()),
    );

    Ok((held, PortMapUpdate::Opened { external, protocol }))
}

/// Keeps a port open on the router for as long as this task is alive.
///
/// Returns immediately with a channel. The first message says whether it worked;
/// later messages arrive if the external address changes, which it does when a
/// router reboots or an ISP hands out a new address.
///
/// Dropping the receiver stops the task, and the mappings lapse on their own.
pub fn keep_open(port: u16) -> mpsc::Receiver<PortMapUpdate> {
    let (tx, rx) = mpsc::channel(4);

    tokio::spawn(async move {
        let Some(port) = NonZeroU16::new(port) else {
            let _ = tx
                .send(PortMapUpdate::Refused(PortMapError::PortZero.to_string()))
                .await;
            return;
        };

        let mut announced: Option<Vec<Multiaddr>> = None;
        let mut mapped_blindly = false;

        loop {
            let wait = match discover() {
                Err(err) => {
                    tracing::debug!(%err, "no router to ask for a port");
                    if announced.is_none() && !mapped_blindly {
                        let _ = tx.send(PortMapUpdate::Refused(err.to_string())).await;
                    }
                    RETRY_AFTER
                }
                Ok(net) => match open(net, port).await {
                    Ok((held, update)) => {
                        let wait = held.renew_in();
                        match &update {
                            PortMapUpdate::Opened { external, protocol } => {
                                // Only shout when something changed. A renewal
                                // that keeps the same address is not news.
                                if announced.as_ref() != Some(external) {
                                    tracing::info!(
                                        protocol = protocol.label(),
                                        ?external,
                                        "the router opened a port; this node is directly reachable"
                                    );
                                    announced = Some(external.clone());
                                    if tx.send(update.clone()).await.is_err() {
                                        return;
                                    }
                                }
                            }
                            PortMapUpdate::MappedWithoutAddress { port, protocol } => {
                                // Announce once. There is no address to compare
                                // against, so repeating it would only be noise.
                                if !mapped_blindly {
                                    mapped_blindly = true;
                                    tracing::info!(
                                        protocol = protocol.label(),
                                        port,
                                        "the router opened a port but would not say where; \
                                         waiting for a peer to tell us our address"
                                    );
                                    if tx.send(update.clone()).await.is_err() {
                                        return;
                                    }
                                }
                            }
                            PortMapUpdate::Refused(_) => {}
                        }
                        wait
                    }
                    Err(err) => {
                        tracing::debug!(%err, "the router would not open a port");
                        if announced.is_none()
                            && !mapped_blindly
                            && tx
                                .send(PortMapUpdate::Refused(err.to_string()))
                                .await
                                .is_err()
                        {
                            return;
                        }
                        RETRY_AFTER
                    }
                },
            };

            tokio::time::sleep(wait).await;
        }
    });

    rx
}

/// What one attempt at a protocol came to.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "result", content = "detail")]
pub enum Attempt {
    /// The router did what we asked.
    Worked(String),
    /// The router said no, or said nothing at all.
    Refused(String),
    /// Not attempted, because something earlier ruled it out.
    Skipped(String),
}

impl Attempt {
    /// Whether this protocol got us a port.
    pub fn worked(&self) -> bool {
        matches!(self, Attempt::Worked(_))
    }

    /// The detail, whatever the outcome.
    pub fn detail(&self) -> &str {
        match self {
            Attempt::Worked(d) | Attempt::Refused(d) | Attempt::Skipped(d) => d,
        }
    }
}

/// Everything we can find out about whether this machine can be dialled.
///
/// Exists because "it does not work" is not a diagnosis. A home user who cannot
/// host needs to know *which* of several quite different things is wrong, and
/// the answer is knowable in a few seconds without asking anybody else.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Diagnosis {
    /// The router, if we could find one.
    pub gateway: Option<String>,
    /// This machine's address on the local network.
    pub local: Option<String>,
    /// The port we tried to open.
    pub port: u16,
    /// Globally routable IPv6 addresses, which need no NAT traversal at all.
    pub global_ipv6: Vec<String>,
    /// Port Control Protocol.
    pub pcp: Attempt,
    /// NAT Port Mapping Protocol.
    pub natpmp: Attempt,
    /// UPnP-IGD.
    pub upnp: Attempt,
}

impl Diagnosis {
    /// Whether anything at all makes this machine dialable from outside.
    pub fn reachable(&self) -> bool {
        self.pcp.worked()
            || self.natpmp.worked()
            || self.upnp.worked()
            || !self.global_ipv6.is_empty()
    }

    /// What to do about it, in one or two plain sentences.
    ///
    /// Deliberately short. Somebody reading this wants to know whether they can
    /// host and, if not, what to change.
    pub fn advice(&self) -> String {
        if self.pcp.worked() || self.natpmp.worked() || self.upnp.worked() {
            return "Your router opened a port. Others can reach you directly.".into();
        }
        if !self.global_ipv6.is_empty() {
            return "You have a public IPv6 address. Peers with IPv6 can reach you directly."
                .into();
        }
        match &self.local {
            Some(local) => format!(
                "Your router will not open a port. Forward TCP and UDP {} to {}, \
                 or connect to a peer who can relay for you.",
                self.port, local
            ),
            None => "No network found.".into(),
        }
    }
}

/// Asks every question at once and reports what came back.
///
/// Leaves nothing behind: any mapping made while probing is asked for with a
/// short lease and removed again, so running this does not change how the node
/// behaves afterwards.
pub async fn diagnose(port: u16) -> Diagnosis {
    let global_ipv6 = netdev::get_default_interface()
        .map(|iface| {
            iface
                .ipv6
                .iter()
                .map(|net| net.addr())
                .filter(|ip| crate::invite::addr_is_global(&ip.to_string()))
                .map(|ip| ip.to_string())
                .collect()
        })
        .unwrap_or_default();

    let net = match discover() {
        Ok(net) => net,
        Err(err) => {
            let why = err.to_string();
            return Diagnosis {
                gateway: None,
                local: None,
                port,
                global_ipv6,
                pcp: Attempt::Skipped(why.clone()),
                natpmp: Attempt::Skipped(why.clone()),
                upnp: Attempt::Skipped(why),
            };
        }
    };

    let mut result = Diagnosis {
        gateway: Some(net.gateway.to_string()),
        local: Some(net.client.to_string()),
        port,
        global_ipv6,
        pcp: Attempt::Skipped("port 0 cannot be mapped".into()),
        natpmp: Attempt::Skipped("port 0 cannot be mapped".into()),
        upnp: Attempt::Skipped("port 0 cannot be mapped".into()),
    };

    let Some(port) = NonZeroU16::new(port) else {
        return result;
    };

    // Short lease, since this is a probe and not a mapping we intend to keep.
    let probe = PortMappingOptions {
        external_port: Some(port),
        lifetime_seconds: Some(120),
        timeout_config: Some(TIMEOUTS),
    };
    let gateway = GatewayAddress::IpV4(net.gateway);

    result.pcp = match crab_nat::pcp::port_mapping(
        crab_nat::pcp::BaseMapRequest::new(
            gateway,
            IpAddr::V4(net.client),
            InternetProtocol::Tcp,
            port,
        ),
        None,
        None,
        probe,
    )
    .await
    {
        Ok(mapping) => {
            let external = mapping.external_port();
            let _ = mapping.try_drop().await;
            Attempt::Worked(format!("mapped external port {external}"))
        }
        Err(err) => Attempt::Refused(err.to_string()),
    };

    result.natpmp =
        match crab_nat::natpmp::port_mapping(gateway, InternetProtocol::Tcp, port, probe).await {
            Ok(mapping) => {
                let external = mapping.external_port();
                let _ = mapping.try_drop().await;
                Attempt::Worked(format!("mapped external port {external}"))
            }
            Err(err) => Attempt::Refused(err.to_string()),
        };

    result.upnp = diagnose_upnp(net, port).await;
    result
}

/// The UPnP half of [`diagnose`], kept separate because it has more ways to go
/// half-right than the other two.
async fn diagnose_upnp(net: LocalNetwork, port: NonZeroU16) -> Attempt {
    use igd_next::{aio::tokio as igd_tokio, PortMappingProtocol, SearchOptions};

    let find = |broadcast: std::net::SocketAddr| async move {
        igd_tokio::search_gateway(SearchOptions {
            broadcast_address: broadcast,
            timeout: Some(Duration::from_secs(2)),
            single_search_timeout: Some(Duration::from_secs(2)),
            ..Default::default()
        })
        .await
    };

    let unicast = std::net::SocketAddr::new(IpAddr::V4(net.gateway), 1900);
    let (gateway, from_router) = match find(unicast).await {
        Ok(gateway) => (gateway, true),
        Err(_) => match find(MULTICAST).await {
            Ok(gateway) => (gateway, false),
            Err(err) => return Attempt::Refused(format!("nothing speaks UPnP here: {err}")),
        },
    };

    // Worth saying out loud. A device that is not the router answering an IGD
    // search is the single most confusing failure in this whole area: it looks
    // like the router is refusing when the router was never asked.
    let who = if from_router {
        String::new()
    } else {
        format!(
            " (answered by {}, which is not your router)",
            gateway.addr.ip()
        )
    };

    let local = std::net::SocketAddr::new(IpAddr::V4(net.client), port.get());
    let mut refusal = None;

    for lease in [120, 0] {
        match gateway
            .add_port(PortMappingProtocol::TCP, port.get(), local, lease, "Kahui")
            .await
        {
            Ok(()) => {
                let _ = gateway
                    .remove_port(PortMappingProtocol::TCP, port.get())
                    .await;
                let kind = if lease == 0 { "permanent" } else { "timed" };
                return Attempt::Worked(format!("accepted a {kind} lease{who}"));
            }
            Err(err) => refusal = Some(err.to_string()),
        }
    }

    Attempt::Refused(format!(
        "{}{who}",
        refusal.unwrap_or_else(|| "refused".into())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_dialable_addresses_for_both_transports() {
        let addrs = addrs_for("93.184.216.34".parse().unwrap(), Some(4001), Some(4001));
        let rendered: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
        assert_eq!(
            rendered,
            vec![
                "/ip4/93.184.216.34/tcp/4001",
                "/ip4/93.184.216.34/udp/4001/quic-v1"
            ]
        );
    }

    #[test]
    fn a_transport_the_router_refused_is_simply_absent() {
        // A router may map TCP and refuse UDP. That is a usable outcome, not a
        // failure, so we must not invent an address for the half that failed.
        let addrs = addrs_for("93.184.216.34".parse().unwrap(), Some(4001), None);
        assert_eq!(addrs.len(), 1);
        assert!(addrs[0].to_string().ends_with("/tcp/4001"));
    }

    #[test]
    fn the_mapped_address_is_what_an_invite_would_carry() {
        // The whole point: what comes out of here must pass the same test an
        // invite uses to decide whether it can work beyond the local network.
        let addrs = addrs_for("93.184.216.34".parse().unwrap(), Some(4001), None);
        assert!(crate::invite::addr_is_global(&addrs[0].to_string()));

        // And the converse: if a router ever reports a private address as our
        // "external" one, handing it out would be worse than saying nothing.
        let bogus = addrs_for("192.168.1.5".parse().unwrap(), Some(4001), None);
        assert!(!crate::invite::addr_is_global(&bogus[0].to_string()));
    }

    fn nothing_works(port: u16) -> Diagnosis {
        Diagnosis {
            gateway: Some("192.168.1.1".into()),
            local: Some("192.168.1.143".into()),
            port,
            global_ipv6: vec![],
            pcp: Attempt::Refused("no response".into()),
            natpmp: Attempt::Refused("no response".into()),
            upnp: Attempt::Refused("501 Action Failed".into()),
        }
    }

    #[test]
    fn advice_names_the_port_and_the_machine_to_forward_to() {
        let d = nothing_works(4001);
        assert!(!d.reachable());
        let advice = d.advice();
        // Useless advice is worse than none: it has to say which port and where.
        assert!(advice.contains("4001"), "{advice}");
        assert!(advice.contains("192.168.1.143"), "{advice}");
    }

    #[test]
    fn a_public_ipv6_address_counts_as_reachable() {
        // No NAT to traverse, so there is nothing to forward and nothing to say.
        let mut d = nothing_works(4001);
        d.global_ipv6 = vec!["2404:4408:1234::1".into()];
        assert!(d.reachable());
        assert!(d.advice().contains("IPv6"));
    }

    #[test]
    fn a_working_router_is_reported_as_working() {
        let mut d = nothing_works(4001);
        d.upnp = Attempt::Worked("accepted a timed lease".into());
        assert!(d.reachable());
        assert!(d.advice().contains("opened a port"));
    }

    #[tokio::test]
    async fn port_zero_is_refused_rather_than_mapped() {
        // Port 0 means "any port" to the OS and nothing at all to a router, so
        // this must fail fast and say so rather than sit there retrying.
        let mut rx = keep_open(0);
        let update = rx.recv().await.expect("should report the problem");
        assert!(matches!(update, PortMapUpdate::Refused(_)));
    }
}
