//! Finding people you have never met, without anybody running a service.
//!
//! Everything else in this crate assumes you already know somebody. Gossip
//! needs a mesh, sync needs a peer to ask, and a relay reservation needs a
//! relay you can already dial. That assumption is what made hosting from home
//! awkward: a node behind a router is perfectly reachable *through* somebody
//! else, but it had no way to find anybody to be reached through.
//!
//! ## The shape of the answer
//!
//! Kāhui runs its own Kademlia distributed hash table. Two facts make it fit
//! the problem unusually well:
//!
//! 1. **A node that can be dialled is exactly a node that can route.** Kademlia
//!    needs a population of dialable nodes to hold the routing table. Circuit
//!    relay needs a population of dialable nodes to carry for everyone else.
//!    They are the same population, so a node that joins the table as a server
//!    is announcing itself as a relay in the same breath.
//!
//! 2. **A node that cannot be dialled is still a full participant.** It runs the
//!    DHT in client mode: it can ask any question, it just does not store
//!    answers. So the machine behind the worst router in the country can still
//!    look up a community and find somebody to carry for it.
//!
//! What this buys is the thing that was missing. A new node asks the DHT "who
//! is reachable?", takes a reservation from one of them, and is now dialable
//! itself. It asks "who has community X?" and gets its members. Nobody
//! configured anything, nobody ran a server, and no address was passed by hand.
//!
//! ## The one thing that cannot be conjured
//!
//! A DHT query has to be sent *somewhere*. There is no way for a program to
//! find a stranger on the internet with no prior information — not a limitation
//! of this design but of IP itself, which is why BitTorrent, Bitcoin, Tox and
//! every other such network ships a list of starting points.
//!
//! So Kāhui needs one address to begin with, once, ever. It gets it from
//! whichever of these answers first:
//!
//! - **A peer it already met.** Remembered across restarts, so this covers
//!   every run after the first.
//! - **The local network,** via mDNS, which needs nothing at all.
//! - **An invite.** Invites carry a few reachable nodes alongside the community,
//!   so being invited *is* being bootstrapped. This is how the property spreads
//!   through a social graph without anybody thinking about it.
//! - **The seed list,** a plain text file of addresses in the data directory
//!   that anybody can edit, ship, or publish.
//!
//! Note what is absent: no name server, no tracker, no rendezvous service, and
//! nothing owned by this project. A seed is data — an address in a file — not a
//! service, and losing every seed costs you nothing so long as you have met one
//! peer since.

use std::path::Path;

use kahui_proto::CommunityId;
use libp2p::{kad, Multiaddr};

/// Kāhui's own DHT protocol.
///
/// Deliberately not IPFS's `/ipfs/kad/1.0.0`. Sharing that table would mean
/// depending on a network this project does not run and cannot vouch for, and
/// filling a stranger's routing table with peers that speak none of their
/// protocols. A small table of Kāhui nodes is worth far more than a large table
/// of nodes with nothing to say.
pub const KAD_PROTOCOL: &str = "/kahui/kad/1.0.0";

/// The name of the seed file inside the data directory.
pub const SEED_FILE: &str = "seeds.txt";

/// The key every reachable node advertises itself under.
///
/// One well-known key, so "who can carry for me?" is a single lookup rather
/// than a search. Kademlia spreads the providers for a key across the nodes
/// nearest it, so this does not concentrate load anywhere in particular.
pub fn relay_key() -> kad::RecordKey {
    kad::RecordKey::new(&b"kahui.relays.v1")
}

/// The key a community's members advertise themselves under.
///
/// A community id is already a 32-byte hash of its founding event, so it makes
/// a well-distributed DHT key with no further work. This is what lets an invite
/// contain no addresses at all: the id *is* the lookup.
pub fn community_key(community: &CommunityId) -> kad::RecordKey {
    let mut key = Vec::with_capacity(14 + 32);
    // Domain-separated so a community id can never collide with another kind of
    // key this project might add later.
    key.extend_from_slice(b"kahui.community.v1");
    key.extend_from_slice(community.as_bytes());
    kad::RecordKey::new(&key)
}

/// Addresses to try when this node knows nobody at all.
///
/// Empty by default, and that is a deliberate choice rather than an oversight.
/// A list compiled into the binary is a list this project chose, and a network
/// that only works because of nodes we picked is not the network described in
/// the README. The mechanism is here and documented; who goes in it is for
/// whoever builds or runs a copy to decide.
///
/// In practice most nodes never reach this function: they have met somebody
/// before, or they were invited, or there is somebody on their own network.
pub fn compiled_seeds() -> Vec<Multiaddr> {
    Vec::new()
}

/// Reads the seed file, creating it with an explanation if it is missing.
///
/// Plain text, one address per line, `#` for comments. Deliberately the dullest
/// possible format: somebody should be able to paste an address into it from a
/// forum post without installing anything or reading a schema.
pub fn load_seeds(data_dir: &Path) -> Vec<Multiaddr> {
    let path = data_dir.join(SEED_FILE);

    if !path.exists() {
        // Best effort. A node with no seed file still works; it just has one
        // fewer way to find its first peer.
        let _ = std::fs::write(&path, SEED_FILE_TEMPLATE);
    }

    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };

    parse_seeds(&text)
}

/// Pulls addresses out of a seed file's text.
///
/// Anything unparseable is skipped rather than fatal. A typo in a seed file
/// should cost you that one seed, not the ability to start.
pub fn parse_seeds(text: &str) -> Vec<Multiaddr> {
    text.lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .filter(|line| !line.is_empty())
        .filter_map(|line| match line.parse::<Multiaddr>() {
            Ok(addr) => Some(addr),
            Err(err) => {
                tracing::warn!(line, %err, "skipping a seed that is not an address");
                None
            }
        })
        .collect()
}

/// Adds an address to the seed file, leaving anything already there alone.
///
/// Returns whether it was new.
pub fn add_seed(data_dir: &Path, addr: &Multiaddr) -> std::io::Result<bool> {
    let path = data_dir.join(SEED_FILE);
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| SEED_FILE_TEMPLATE.into());

    if parse_seeds(&existing).iter().any(|known| known == addr) {
        return Ok(false);
    }

    let mut next = existing;
    if !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&addr.to_string());
    next.push('\n');
    std::fs::write(&path, next)?;
    Ok(true)
}

/// What a fresh seed file says, so somebody opening it knows what to do.
const SEED_FILE_TEMPLATE: &str = "\
# Kahui seed addresses — one per line, # for comments.
#
# A node only needs one of these, once, ever: after it has met anybody it
# remembers them, and after that it finds everyone else through the DHT.
#
# You do not normally need to touch this file. It matters when you are the
# first Kahui node on a network and nobody has invited you yet.
#
# Example:
#   /ip4/203.0.113.4/tcp/4001/p2p/12D3KooWExample
#   /ip6/2404:4408::1/udp/4001/quic-v1/p2p/12D3KooWExample
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_community_key_is_derived_from_its_id() {
        let a = community_key(&CommunityId::from_bytes([1; 32]));
        let b = community_key(&CommunityId::from_bytes([2; 32]));
        assert_ne!(a, b, "different communities must not share a key");
        assert_eq!(a, community_key(&CommunityId::from_bytes([1; 32])));
    }

    #[test]
    fn a_community_key_cannot_collide_with_the_relay_key() {
        // Both are made from bytes we choose, so this is worth pinning down.
        let community = community_key(&CommunityId::from_bytes([0; 32]));
        assert_ne!(community, relay_key());
    }

    #[test]
    fn seeds_survive_comments_blank_lines_and_typos() {
        let seeds = parse_seeds(
            "\
# a comment
/ip4/203.0.113.4/tcp/4001

  /ip6/2404:4408::1/udp/4001/quic-v1  # trailing comment
not-an-address
",
        );
        let rendered: Vec<String> = seeds.iter().map(|a| a.to_string()).collect();
        assert_eq!(
            rendered,
            vec![
                "/ip4/203.0.113.4/tcp/4001",
                "/ip6/2404:4408::1/udp/4001/quic-v1"
            ],
            "a bad line should cost one seed, not the file"
        );
    }

    #[test]
    fn a_missing_seed_file_is_written_with_an_explanation() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_seeds(dir.path()).is_empty());

        let written = std::fs::read_to_string(dir.path().join(SEED_FILE)).unwrap();
        assert!(written.contains("one per line"));
        // The template must not accidentally seed anybody with an example.
        assert!(
            parse_seeds(&written).is_empty(),
            "the examples in the template must stay commented out"
        );
    }

    #[test]
    fn adding_a_seed_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let addr: Multiaddr = "/ip4/203.0.113.4/tcp/4001".parse().unwrap();

        assert!(add_seed(dir.path(), &addr).unwrap(), "first add is new");
        assert!(!add_seed(dir.path(), &addr).unwrap(), "second add is not");
        assert_eq!(load_seeds(dir.path()), vec![addr]);
    }

    #[test]
    fn the_compiled_list_is_empty_on_purpose() {
        // If this ever fails, somebody has decided which nodes the project
        // blesses. That is a real decision and deserves a conversation, not a
        // quiet commit.
        assert!(compiled_seeds().is_empty());
    }
}
