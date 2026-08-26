//! Two machines that cannot be dialled, connecting to each other anyway.
//!
//! ## The situation
//!
//! Jane and Juan are both behind routers that refuse to open a port. Neither can
//! accept an incoming connection, so neither can be dialled — and the usual
//! answer is that they need somebody reachable in the middle.
//!
//! They do not. What a NAT actually does is drop packets from strangers; it
//! forwards packets from people you have already written to. So if Jane sends to
//! Juan at the same moment Juan sends to Jane, each router sees an outgoing
//! packet first and treats the other's arriving packet as the reply to it. Both
//! holes open, the packets cross in flight, and the connection is direct. This
//! is hole punching, and it is what nearly every peer-to-peer system does.
//!
//! ## Why it normally needs a third party, and why it need not
//!
//! Hole punching is usually described as requiring a rendezvous server, because
//! each side has to know two things: the other's public address, and *when* to
//! fire. A server in the middle supplies both.
//!
//! But neither of those has to come from a server:
//!
//! - **The address** was learned the first time they met. Any peer you talk to
//!   can see the address your router presents on your behalf and tell you what
//!   it is; that address is then gossiped like any other. Jane and Juan met once
//!   through Bob, so each already holds the other's.
//!
//! - **The timing** does not need to be communicated at all, because both sides
//!   can *calculate* it. Given the same two peer ids and a clock, this module
//!   produces the same instants on both machines without a message passing
//!   between them. Jane works out when to dial Juan; Juan independently works
//!   out the same moment; both fire.
//!
//! So after one meeting, ever, the two of them can re-establish a direct
//! connection for as long as they both keep running — with nobody in the middle,
//! no relay, and nothing of anyone's to depend on.
//!
//! ## What it does not do
//!
//! This is not a way for two strangers to meet. It reconnects people who have
//! met, which is exactly the case that matters when the member who introduced
//! them goes offline for good.
//!
//! It also does not work against every router. A *symmetric* NAT allocates a
//! different external port for every destination, so the address Jane learned
//! through Bob is not the address Juan would have to send to, and there is
//! nothing to aim at. Most home routers are not symmetric; some are, and for
//! those this fails and the relay path remains the answer.

use std::time::Duration;

use libp2p::PeerId;

/// How often a pair gets a chance to punch.
///
/// Short enough that reconnecting feels immediate, long enough that a pair who
/// cannot punch are not hammering each other's routers.
pub const PERIOD: Duration = Duration::from_secs(2);

/// How wide the window is.
///
/// Two machines' clocks are rarely identical, and the packets have to be in
/// flight at roughly the same time. This is generous next to the clock error
/// between any two NTP-synchronised computers, and cheap: being early or late
/// within the window costs one extra packet.
pub const WINDOW: Duration = Duration::from_millis(250);

/// Whether now is a moment for these two to dial each other.
///
/// The answer is a pure function of the pair and the clock, which is the whole
/// point: both sides reach it independently and simultaneously, having exchanged
/// nothing.
///
/// The offset is derived from the pair, so different pairs punch at different
/// moments and a busy node spreads its attempts out rather than firing them all
/// on the same tick.
pub fn is_window(a: &PeerId, b: &PeerId, now_ms: u64) -> bool {
    let period = PERIOD.as_millis() as u64;
    let offset = offset_ms(a, b);
    let position = now_ms % period;

    // The window wraps around the end of the period, so measure the distance to
    // the offset the short way round.
    let distance = position
        .abs_diff(offset)
        .min(period - position.abs_diff(offset));
    distance <= WINDOW.as_millis() as u64
}

/// Where in the period this pair's window sits.
///
/// Order-independent, so Jane computing it for Juan and Juan computing it for
/// Jane get the same answer. That is the only property that matters, and it is
/// why the ids are sorted before hashing.
fn offset_ms(a: &PeerId, b: &PeerId) -> u64 {
    let (first, second) = if a <= b { (a, b) } else { (b, a) };

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"kahui.punch.v1");
    hasher.update(&first.to_bytes());
    hasher.update(&second.to_bytes());
    let digest = hasher.finalize();

    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(bytes) % PERIOD.as_millis() as u64
}

/// Whether an address is worth aiming a punch at.
///
/// Only a globally routable one: a punch works by getting a packet to the
/// other side's router, and a private address does not name anybody's router.
pub fn is_punchable(addr: &libp2p::Multiaddr) -> bool {
    is_punchable_with(addr, false)
}

/// As [`is_punchable`], but able to count private addresses.
///
/// `allow_private` follows the same reasoning as `lan_reachable` elsewhere: on
/// an isolated network the private addresses are the real ones, and refusing
/// them would leave every node concluding it has nowhere to be reached when in
/// fact they can all reach each other. It is also what makes this testable
/// without two houses and two routers.
pub fn is_punchable_with(addr: &libp2p::Multiaddr, allow_private: bool) -> bool {
    // A relayed address is somebody else's connection, not a hole to open.
    if addr
        .iter()
        .any(|part| part == libp2p::multiaddr::Protocol::P2pCircuit)
    {
        return false;
    }
    if crate::invite::addr_is_reachable_beyond_lan(addr) {
        return true;
    }
    allow_private
        && !addr.iter().any(|part| match part {
            libp2p::multiaddr::Protocol::Ip4(ip) => ip.is_unspecified(),
            libp2p::multiaddr::Protocol::Ip6(ip) => ip.is_unspecified(),
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peers() -> (PeerId, PeerId) {
        (PeerId::random(), PeerId::random())
    }

    #[test]
    fn both_sides_work_out_the_same_moment() {
        // The property the whole idea rests on. If this were order-dependent,
        // Jane and Juan would dial at different times and never meet.
        let (jane, juan) = peers();
        for now in (0..4000).step_by(37) {
            assert_eq!(
                is_window(&jane, &juan, now),
                is_window(&juan, &jane, now),
                "disagreed at {now}ms"
            );
        }
    }

    #[test]
    fn a_pair_gets_a_window_in_every_period() {
        let (jane, juan) = peers();
        let period = PERIOD.as_millis() as u64;
        let hits = (0..period)
            .filter(|ms| is_window(&jane, &juan, *ms))
            .count();
        assert!(hits > 0, "a pair that never punches would never reconnect");
        // And not so many that it is really "always".
        assert!(
            (hits as u64) < period / 2,
            "punching should be a moment, not a state"
        );
    }

    #[test]
    fn different_pairs_punch_at_different_moments() {
        // Otherwise every pair on a busy node fires on the same tick, which is
        // a burst of traffic for no extra chance of success.
        let a = PeerId::random();
        let offsets: std::collections::HashSet<u64> =
            (0..16).map(|_| offset_ms(&a, &PeerId::random())).collect();
        assert!(offsets.len() > 8, "offsets clustered: {offsets:?}");
    }

    #[test]
    fn the_window_is_wide_enough_for_clocks_that_disagree() {
        // Two machines are rarely to the millisecond. If one is 100ms fast it
        // must still land in the same window.
        let (jane, juan) = peers();
        let period = PERIOD.as_millis() as u64;
        let centre = (0..period)
            .find(|ms| is_window(&jane, &juan, *ms))
            .expect("there is a window");
        assert!(
            is_window(&jane, &juan, centre + 100),
            "100ms of clock difference should still overlap"
        );
    }

    #[test]
    fn on_an_isolated_network_private_addresses_do_name_somebody() {
        let lan: libp2p::Multiaddr = "/ip4/192.168.1.5/udp/4001/quic-v1".parse().unwrap();
        assert!(!is_punchable_with(&lan, false));
        assert!(is_punchable_with(&lan, true));

        // Never a wildcard, though: it names this machine's socket, not a
        // place anybody else could aim at.
        let any: libp2p::Multiaddr = "/ip4/0.0.0.0/udp/4001/quic-v1".parse().unwrap();
        assert!(!is_punchable_with(&any, true));
    }

    #[test]
    fn only_addresses_that_name_a_router_are_worth_punching() {
        assert!(is_punchable(
            &"/ip4/93.184.216.34/udp/4001/quic-v1".parse().unwrap()
        ));
        // Private: names nobody's router.
        assert!(!is_punchable(
            &"/ip4/192.168.1.5/udp/4001/quic-v1".parse().unwrap()
        ));
        // Relayed: already somebody else's connection.
        assert!(!is_punchable(
            &"/ip4/93.184.216.34/tcp/4001/p2p/12D3KooWDms1wiZZX7c7sJAX9zKkE5GnaCnLxwWeCEQwKoQrspUt/p2p-circuit"
                .parse()
                .unwrap()
        ));
    }
}
