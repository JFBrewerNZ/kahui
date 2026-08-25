//! Invites: the only bootstrap step in the system.
//!
//! Joining a community needs two things — its id, and at least one member who
//! is currently reachable. An invite is exactly that, encoded as a string
//! someone can paste into a chat window.
//!
//! There is no invite server and no redemption step. The string is a hint about
//! where to look, not a credential: everything it points at is verified against
//! signatures once fetched, so a forged invite gets you a community that fails
//! to validate, not a compromised one. It also does not expire on its own,
//! because there is nobody to expire it.

use kahui_proto::{CommunityId, EventId};
use serde::{Deserialize, Serialize};

/// Prefix that makes an invite recognisable in the middle of a chat log.
pub const INVITE_PREFIX: &str = "kahui1";

/// URL scheme the desktop app registers, so an invite can be a link somebody
/// clicks rather than a code they copy.
pub const LINK_SCHEME: &str = "kahui";

/// What a full invite link looks like: `kahui://join/kahui1...`
pub const LINK_PREFIX: &str = "kahui://join/";

/// Format version, so future invites can carry more without older clients
/// misreading them.
const INVITE_VERSION: u8 = 1;

#[derive(Debug, thiserror::Error)]
pub enum InviteError {
    #[error("an invite starts with `{INVITE_PREFIX}`")]
    MissingPrefix,
    #[error("invite text is not valid base58: {0}")]
    BadBase58(#[from] bs58::decode::Error),
    #[error("invite contents are malformed: {0}")]
    Malformed(#[from] kahui_proto::CodecError),
    #[error("invite is version {found}, this client understands {INVITE_VERSION}")]
    UnsupportedVersion { found: u8 },
    #[error("invite names no peers to connect to")]
    NoPeers,
}

/// One member who was reachable when the invite was made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvitePeer {
    /// libp2p peer id, as text.
    pub peer_id: String,
    /// Multiaddresses, without the trailing `/p2p/<peer>` component — the
    /// dialer appends it from `peer_id`.
    pub addrs: Vec<String>,
}

/// Everything needed to find and verify a community.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    version: u8,
    /// The community's id, which is also the hash of its genesis event. A node
    /// that fetches a genesis whose hash does not match this has been lied to.
    pub community: CommunityId,
    /// Display name, so a user can see what they are joining before they do.
    pub name: String,
    /// Members to try. More than one means the invite outlives any single node.
    pub peers: Vec<InvitePeer>,
}

impl Invite {
    pub fn new(community: CommunityId, name: impl Into<String>, peers: Vec<InvitePeer>) -> Self {
        Invite {
            version: INVITE_VERSION,
            community,
            name: name.into(),
            peers,
        }
    }

    /// The genesis event this invite promises. Fetching it and finding a
    /// different hash means the invite or the peer is lying.
    pub fn expected_genesis(&self) -> EventId {
        EventId(self.community.0)
    }

    /// Encodes to a single pasteable token.
    ///
    /// base58 rather than base64: no `+`, `/` or `=` to get mangled by chat
    /// clients, and no characters that look alike when read aloud.
    pub fn encode(&self) -> String {
        let bytes = kahui_proto::codec::to_canonical(self).expect("invites are always encodable");
        format!("{INVITE_PREFIX}{}", bs58::encode(bytes).into_string())
    }

    /// The same invite as a clickable link.
    pub fn to_link(&self) -> String {
        format!("{LINK_PREFIX}{}", self.encode())
    }

    /// Reads an invite from a code or a link.
    ///
    /// Both forms are accepted because both end up in front of people: a code
    /// pasted into a chat, and a `kahui://` link clicked in a browser. Asking
    /// somebody to notice which one they have would be a pointless test.
    pub fn decode(text: &str) -> Result<Self, InviteError> {
        let text = text.trim();
        let text = text.strip_prefix(LINK_PREFIX).unwrap_or(text);
        let body = text
            .strip_prefix(INVITE_PREFIX)
            .ok_or(InviteError::MissingPrefix)?;
        let bytes = bs58::decode(body).into_vec()?;
        let invite: Invite = kahui_proto::codec::from_canonical(&bytes)?;
        if invite.version != INVITE_VERSION {
            return Err(InviteError::UnsupportedVersion {
                found: invite.version,
            });
        }
        if invite.peers.is_empty() || invite.peers.iter().all(|p| p.addrs.is_empty()) {
            return Err(InviteError::NoPeers);
        }
        Ok(invite)
    }

    /// Whether anybody outside the minter's own network could use this.
    ///
    /// An invite is only as good as the addresses in it. A node behind a router
    /// with no relay has nothing but `192.168.x.x` and loopback to offer, which
    /// works on its own network and nowhere else — so handing that invite to
    /// somebody on the internet cannot work, and it is worth saying before they
    /// find out by waiting.
    pub fn reachable_beyond_lan(&self) -> bool {
        self.peers
            .iter()
            .flat_map(|peer| peer.addrs.iter())
            .any(|addr| addr_is_global(addr))
    }

    /// Dialable multiaddresses, with the peer id appended so libp2p can verify
    /// it is talking to who it expected.
    pub fn dial_addresses(&self) -> Vec<String> {
        self.peers
            .iter()
            .flat_map(|peer| {
                peer.addrs
                    .iter()
                    .map(move |addr| format!("{addr}/p2p/{}", peer.peer_id))
            })
            .collect()
    }
}

/// True if a multiaddress string contains an IP anybody on the internet could
/// route to.
///
/// Deliberately textual: an invite carries addresses as strings, and parsing
/// them into a `Multiaddr` just to look at the first component would make this
/// crate's simplest type depend on libp2p's.
/// Whether a single address could be dialled from outside the local network.
///
/// Takes a `Multiaddr` rather than a string, for callers that already have one.
pub fn addr_is_reachable_beyond_lan(addr: &libp2p::Multiaddr) -> bool {
    addr_is_global(&addr.to_string())
}

pub(crate) fn addr_is_global(addr: &str) -> bool {
    addr.split('/')
        .filter_map(|part| part.parse::<std::net::IpAddr>().ok())
        .any(|ip| match ip {
            std::net::IpAddr::V4(v4) => {
                !(v4.is_private()
                    || v4.is_loopback()
                    || v4.is_link_local()
                    || v4.is_broadcast()
                    || v4.is_documentation()
                    || v4.is_unspecified()
                    // 100.64.0.0/10, what carrier-grade NAT hands out.
                    || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1])))
            }
            std::net::IpAddr::V6(v6) => {
                !(v6.is_loopback()
                    || v6.is_unspecified()
                    // fc00::/7, the IPv6 equivalent of 192.168.x.x. Windows
                    // hands these out readily, so this is the common case.
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    // fe80::/10, which is meaningless off the local link.
                    || (v6.segments()[0] & 0xffc0) == 0xfe80)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_globally_routable_ipv6_counts() {
        // A machine can easily hold four IPv6 addresses and be reachable at
        // none of them, which is exactly the case on the network this was
        // written on.
        assert!(addr_is_global("/ip6/2404:4408:1234::1/tcp/4001"));
        // fc00::/7 — private, the IPv6 equivalent of 192.168.x.x.
        assert!(!addr_is_global(
            "/ip6/fddd:7047:7de2:428c:d301:add8:8933:f072/tcp/4001"
        ));
        // fe80::/10 — link-local, meaningless one hop away.
        assert!(!addr_is_global("/ip6/fe80::8f62:f8f0:9a56:e8ca/tcp/4001"));
        assert!(!addr_is_global("/ip6/::1/tcp/4001"));
    }

    fn sample() -> Invite {
        Invite::new(
            CommunityId::from_bytes([7; 32]),
            "Kahui",
            vec![InvitePeer {
                peer_id: "12D3KooWabc".into(),
                addrs: vec![
                    "/ip4/192.168.1.5/tcp/4001".into(),
                    "/ip4/192.168.1.5/udp/4001/quic-v1".into(),
                ],
            }],
        )
    }

    #[test]
    fn roundtrips_through_text() {
        let invite = sample();
        let text = invite.encode();
        assert!(text.starts_with(INVITE_PREFIX));
        assert_eq!(Invite::decode(&text).unwrap(), invite);
    }

    #[test]
    fn an_invite_with_only_lan_addresses_says_so() {
        let lan = Invite::new(
            CommunityId::from_bytes([1; 32]),
            "Games",
            vec![InvitePeer {
                peer_id: "12D3KooWabc".into(),
                addrs: vec![
                    "/ip4/192.168.1.143/tcp/4001".into(),
                    "/ip4/127.0.0.1/tcp/4001".into(),
                ],
            }],
        );
        assert!(!lan.reachable_beyond_lan());

        let public = Invite::new(
            CommunityId::from_bytes([1; 32]),
            "Games",
            vec![InvitePeer {
                peer_id: "12D3KooWabc".into(),
                addrs: vec!["/ip4/64.23.186.93/tcp/4001".into()],
            }],
        );
        assert!(public.reachable_beyond_lan());
    }

    #[test]
    fn carrier_grade_nat_is_not_reachable_either() {
        // 100.64.0.0/10 looks public and is not.
        assert!(!addr_is_global("/ip4/100.90.1.5/tcp/4001"));
        assert!(!addr_is_global("/ip4/10.124.0.3/tcp/4001"));
        assert!(addr_is_global("/ip4/8.8.8.8/tcp/4001"));
    }

    #[test]
    fn a_link_and_a_code_are_the_same_invite() {
        let invite = sample();
        let link = invite.to_link();
        assert!(link.starts_with("kahui://join/"));
        assert_eq!(Invite::decode(&link).unwrap(), invite);
        assert_eq!(Invite::decode(&invite.encode()).unwrap(), invite);
    }

    #[test]
    fn surrounding_whitespace_is_forgiven() {
        let text = format!("  {}\n", sample().encode());
        assert_eq!(Invite::decode(&text).unwrap(), sample());
    }

    #[test]
    fn rejects_text_that_is_not_an_invite() {
        assert!(matches!(
            Invite::decode("https://example.com/invite"),
            Err(InviteError::MissingPrefix)
        ));
        assert!(Invite::decode("kahui1not-valid-base58-0OIl").is_err());
    }

    #[test]
    fn rejects_an_invite_with_nowhere_to_connect() {
        let empty = Invite::new(CommunityId::from_bytes([7; 32]), "Kahui", Vec::new());
        assert!(matches!(
            Invite::decode(&empty.encode()),
            Err(InviteError::NoPeers)
        ));
    }

    #[test]
    fn dial_addresses_carry_the_peer_id() {
        let addrs = sample().dial_addresses();
        assert_eq!(addrs.len(), 2);
        assert!(addrs.iter().all(|a| a.ends_with("/p2p/12D3KooWabc")));
    }

    #[test]
    fn the_community_id_commits_to_the_genesis_event() {
        let invite = sample();
        assert_eq!(
            invite.expected_genesis().as_bytes(),
            invite.community.as_bytes()
        );
    }
}
