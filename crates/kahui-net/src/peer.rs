//! Bridging Kahui identities and libp2p identities.
//!
//! A member has exactly one keypair. Its public half is their [`UserId`] in the
//! event log and, expressed as a libp2p [`PeerId`], their address on the
//! network. One key, two views.
//!
//! The useful consequence: membership is a routing table. Any node that holds a
//! community's events can compute the `PeerId` of every member from their
//! events alone and dial them directly — no directory, no tracker, nobody to
//! ask. That is what lets a community keep running when the node that founded
//! it goes away.

use kahui_proto::{Identity, UserId};
use libp2p::identity::{ed25519, Keypair, PublicKey};
use libp2p::PeerId;

/// Builds the libp2p transport keypair from a Kahui identity.
///
/// Same 32 secret bytes, so the resulting `PeerId` is the one every other node
/// derives from this member's `UserId`.
pub fn libp2p_keypair(identity: &Identity) -> Keypair {
    let mut secret = identity.secret_bytes();
    // `ed25519_from_bytes` zeroes the buffer it is given, which is why this
    // takes a mutable copy rather than borrowing the identity's key.
    Keypair::ed25519_from_bytes(&mut secret).expect("32 bytes is always a valid Ed25519 secret key")
}

/// The libp2p `PeerId` for a member, derived from their public key.
///
/// Returns `None` when the bytes do not decode to a point on the curve. Note
/// that roughly half of all 32-byte strings do decode, so getting `Some` back
/// says nothing about whether anybody holds the matching private key — that is
/// established by signature verification, not here. A `UserId` reaching this
/// function has always come attached to an event that already verified.
pub fn peer_id_of(user: &UserId) -> Option<PeerId> {
    let key = ed25519::PublicKey::try_from_bytes(user.as_bytes()).ok()?;
    Some(PeerId::from_public_key(&PublicKey::from(key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transport_identity_and_the_event_identity_agree() {
        let identity = Identity::generate();
        let from_keypair = PeerId::from_public_key(&libp2p_keypair(&identity).public());
        let from_user_id = peer_id_of(&identity.user_id()).expect("valid key");
        assert_eq!(
            from_keypair, from_user_id,
            "a member's PeerId must be derivable from their UserId alone"
        );
    }

    #[test]
    fn different_identities_get_different_peer_ids() {
        let a = peer_id_of(&Identity::generate().user_id()).unwrap();
        let b = peer_id_of(&Identity::generate().user_id()).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn every_real_identity_maps_to_a_peer_id() {
        // The mapping has to be total for real identities: a member whose
        // PeerId could not be computed would be unreachable to the rest of the
        // community even though their events validate fine.
        for _ in 0..64 {
            assert!(peer_id_of(&Identity::generate().user_id()).is_some());
        }
    }

    #[test]
    fn bytes_that_are_not_a_curve_point_have_no_peer_id() {
        assert!(peer_id_of(&UserId::from_bytes([0xab; 32])).is_none());
    }

    #[test]
    fn a_decodable_but_invented_user_id_is_still_nobody() {
        // Plenty of arbitrary byte strings are valid curve points, so getting a
        // PeerId back proves nothing. Authenticity comes from signatures, and
        // an invented identity cannot produce one.
        let invented = UserId::from_bytes([0xff; 32]);
        assert!(peer_id_of(&invented).is_some());
        assert!(!kahui_proto::identity::verify(
            &invented,
            b"anything",
            &[0u8; 64]
        ));
    }
}
