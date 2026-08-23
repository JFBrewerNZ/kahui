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

    pub fn decode(text: &str) -> Result<Self, InviteError> {
        let text = text.trim();
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

#[cfg(test)]
mod tests {
    use super::*;

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
