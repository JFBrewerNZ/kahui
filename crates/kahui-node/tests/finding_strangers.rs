//! Two people who have never met, and were never given each other's address.
//!
//! Everything else in this repository tests what happens once nodes know about
//! each other. This file tests the step before that, which is the one that
//! decided whether Kāhui was usable by ordinary people: can somebody run the
//! program on their own machine and be reached by somebody else running it on
//! theirs, with nobody typing an address and nobody running a server?
//!
//! The answer is the distributed hash table. Every node that can be dialled
//! joins its routing table and offers to carry for the ones that cannot, so the
//! reachable members of the network *are* its infrastructure. A node needs one
//! entry point, once — here that is a single node standing in for "somebody,
//! anybody, who is reachable" — and everything after that is discovery.
//!
//! What is deliberately *not* passed between the two strangers: any address,
//! any peer id, any relay. Only a community id, which is a public name for a
//! thing rather than a route to a machine.

use std::path::Path;
use std::time::Duration;

use kahui_node::{ChannelId, Invite, NetConfig, Node, NodeConfig, NodeHandle, Reachability};
use kahui_proto::CommunityId;
use tempfile::TempDir;

/// Discovery involves several round trips and a retry, so it needs more room
/// than a direct dial. Still short enough that a genuine failure fails.
const PATIENCE: Duration = Duration::from_secs(60);

/// A node that behaves like somebody's desktop: it can dial out, and whether it
/// can be dialled is decided by `reachability`.
async fn spawn(
    dir: &Path,
    name: &str,
    listen: Vec<&str>,
    reachability: Reachability,
    seeds: Vec<String>,
) -> NodeHandle {
    Node::spawn(NodeConfig {
        data_dir: dir.to_path_buf(),
        display_name: Some(name.to_string()),
        net: NetConfig {
            listen: listen.iter().map(|a| a.parse().unwrap()).collect(),
            // The whole point is that nobody is on the same network. Local
            // discovery would make this test pass for the wrong reason.
            enable_mdns: false,
            heartbeat: Duration::from_millis(250),
            enable_relay: true,
            enable_port_mapping: false,
            enable_dht: true,
            lan_reachable: true,
        },
        presence_interval: Duration::from_millis(400),
        sync_interval: Duration::from_millis(500),
        reachability: Some(reachability),
        bootstrap: seeds,
        ..Default::default()
    })
    .await
    .unwrap_or_else(|err| panic!("node {name} failed to start: {err}"))
}

async fn wait_until_listening(node: &NodeHandle, name: &str) -> String {
    for _ in 0..200 {
        if let Ok(status) = node.status().await {
            if let Some(addr) = status
                .listen_addrs
                .iter()
                .find(|a| a.contains("/tcp/") && !a.contains("p2p-circuit"))
            {
                return format!("{addr}/p2p/{}", status.peer_id);
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{name} never started listening");
}

async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    while tokio::time::Instant::now() < deadline {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
    panic!("timed out waiting for {what}");
}

/// Reads a channel's messages, whatever order they arrived in.
async fn texts(node: &NodeHandle, community: CommunityId, channel: ChannelId) -> Vec<String> {
    node.history(community, channel, 64)
        .await
        .map(|msgs| msgs.into_iter().map(|m| m.body).collect())
        .unwrap_or_default()
}

/// The headline case: somebody hosts from their own machine, and a stranger
/// joins knowing only the community's id.
///
/// `hub` is not infrastructure and holds nothing. It is an ordinary Kāhui node
/// that happens to be reachable, which in a real network is any member with a
/// cooperative router, a public IPv6 address, or a machine that stays on. Alice
/// and Bob each know it and nothing else; in particular neither has ever been
/// told anything about the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_strangers_meet_knowing_only_a_community_id() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    // Somebody, somewhere, who can be dialled.
    let hub = spawn(
        &dir("hub"),
        "hub",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![],
    )
    .await;
    let hub_addr = wait_until_listening(&hub, "hub").await;

    // Alice hosts from her own machine. She knows the hub; that is all.
    let alice = spawn(
        &dir("alice"),
        "alice",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![hub_addr.clone()],
    )
    .await;
    wait_until_listening(&alice, "alice").await;

    let community = alice
        .create_community("Kōrero", "Two strangers")
        .await
        .expect("alice creates a community");
    let general = alice.channels(community).await.unwrap()[0].id;
    alice
        .post(community, general, "anybody out there")
        .await
        .expect("post");

    // Bob knows the hub, and the community's id. Nothing else — no address of
    // Alice's, no peer id, not even the invite she generated.
    let bob = spawn(
        &dir("bob"),
        "bob",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![hub_addr],
    )
    .await;

    let blind_invite = Invite::new(community, "Kōrero", vec![]);
    assert!(
        blind_invite.dial_addresses().is_empty(),
        "this invite must contain no route to anybody, or the test proves nothing"
    );

    bob.join(blind_invite).await.expect("bob joins by id alone");

    eventually(
        "bob to find a community nobody gave him an address for",
        || async { !texts(&bob, community, general).await.is_empty() },
    )
    .await;

    assert_eq!(
        texts(&bob, community, general).await,
        vec!["anybody out there"]
    );

    // And it is a real membership, not a one-way read.
    bob.post(community, general, "kia ora")
        .await
        .expect("bob posts");
    eventually("alice to hear back", || async {
        texts(&alice, community, general).await.len() == 2
    })
    .await;

    for node in [&hub, &alice, &bob] {
        node.shutdown().await.ok();
    }
}

/// The case that started all of this: the host is behind a router.
///
/// Alice listens on nothing, so no address of hers exists to be handed out even
/// in principle. She is found because she takes a relay reservation from a node
/// the hash table offered her, publishes the resulting circuit address, and is
/// then discoverable through it like anybody else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_behind_a_router_is_found_without_anybody_knowing_where_she_is() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    let hub = spawn(
        &dir("hub"),
        "hub",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![],
    )
    .await;
    let hub_addr = wait_until_listening(&hub, "hub").await;

    // No listen addresses at all: Alice can dial out and nothing more.
    let alice = spawn(
        &dir("alice"),
        "alice",
        vec![],
        Reachability::BehindNat,
        vec![hub_addr.clone()],
    )
    .await;

    let community = alice
        .create_community("Papakāinga", "Hosted from behind a router")
        .await
        .expect("alice creates a community");
    let general = alice.channels(community).await.unwrap()[0].id;

    // She should end up reachable through somebody, without having been told
    // who. That is the hash table's doing.
    eventually(
        "alice to be carried by somebody she was never given",
        || async {
            alice
                .status()
                .await
                .map(|s| s.listen_addrs.iter().any(|a| a.contains("p2p-circuit")))
                .unwrap_or(false)
        },
    )
    .await;

    alice
        .post(community, general, "hosting from home")
        .await
        .expect("post");

    let bob = spawn(
        &dir("bob"),
        "bob",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![hub_addr],
    )
    .await;

    bob.join(Invite::new(community, "Papakāinga", vec![]))
        .await
        .expect("bob joins by id alone");

    eventually(
        "bob to reach a host who has no address of her own",
        || async { !texts(&bob, community, general).await.is_empty() },
    )
    .await;

    assert_eq!(
        texts(&bob, community, general).await,
        vec!["hosting from home"]
    );

    for node in [&hub, &alice, &bob] {
        node.shutdown().await.ok();
    }
}

/// A node that can be dialled starts carrying part of the network by itself.
///
/// Nobody switches this on. Being reachable is the whole qualification, which
/// is what keeps the amount of infrastructure in the system at zero while still
/// having some: the members who can host, host.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reachable_node_offers_itself_without_being_asked() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    let hub = spawn(
        &dir("hub"),
        "hub",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![],
    )
    .await;
    let hub_addr = wait_until_listening(&hub, "hub").await;

    let stuck = spawn(
        &dir("stuck"),
        "stuck",
        vec![],
        Reachability::BehindNat,
        vec![hub_addr],
    )
    .await;

    // The hub was never configured as a relay and was never asked to be one.
    eventually("the hub to end up carrying for somebody", || async {
        hub.status()
            .await
            .map(|s| s.relaying_for > 0)
            .unwrap_or(false)
    })
    .await;

    for node in [&hub, &stuck] {
        node.shutdown().await.ok();
    }
}

/// A join made long after the node started, not moments after.
///
/// This is the ordinary case — somebody opens the app, reads a message, then
/// pastes an id — and it used to be the fragile one. A provider lookup answers
/// with peer ids and no addresses; libp2p keeps those only in the address book
/// of the query that found them. Dialling a provider therefore worked while the
/// lookup was still running and failed afterwards, which made joining depend on
/// timing nobody controls.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn joining_works_long_after_starting_up() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    let hub = spawn(
        &dir("hub"),
        "hub",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![],
    )
    .await;
    let hub_addr = wait_until_listening(&hub, "hub").await;

    let alice = spawn(
        &dir("alice"),
        "alice",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![hub_addr.clone()],
    )
    .await;
    wait_until_listening(&alice, "alice").await;
    let community = alice
        .create_community("Later", "Joined in a while")
        .await
        .unwrap();
    let general = alice.channels(community).await.unwrap()[0].id;
    alice.post(community, general, "still here").await.unwrap();

    let bob = spawn(
        &dir("bob"),
        "bob",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![hub_addr],
    )
    .await;

    // Long enough that every lookup Bob made at start-up has finished and been
    // cleaned up, so nothing is left over to make this work by accident.
    tokio::time::sleep(Duration::from_secs(4)).await;

    bob.join(Invite::by_id(community)).await.expect("bob joins");
    eventually("bob to catch up on a community he joined late", || async {
        !texts(&bob, community, general).await.is_empty()
    })
    .await;
    assert_eq!(texts(&bob, community, general).await, vec!["still here"]);

    for node in [&hub, &alice, &bob] {
        node.shutdown().await.ok();
    }
}

/// Joining by id must not silently succeed when there is nobody to find.
///
/// A join that "works" and then sits empty forever is worse than one that
/// fails, because there is nothing to tell the person what went wrong.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn joining_a_community_that_does_not_exist_does_not_pretend_to_work() {
    let dir = TempDir::new().unwrap();

    let lonely = spawn(
        dir.path(),
        "lonely",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![],
    )
    .await;

    let nowhere = CommunityId::from_bytes([9; 32]);
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        lonely.join(Invite::new(nowhere, "Nowhere", vec![])),
    )
    .await;

    match outcome {
        // Still waiting is the correct behaviour: the community may yet be
        // found, and nothing has been claimed.
        Err(_) => {}
        Ok(Ok(_)) => panic!("joined a community that nobody holds"),
        // An outright error is fine too, as long as it is not a false success.
        Ok(Err(_)) => {}
    }

    assert!(
        lonely.communities().await.unwrap().is_empty(),
        "nothing should have been created locally"
    );

    lonely.shutdown().await.ok();
}
