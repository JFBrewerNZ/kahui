//! The milestone, proven end to end.
//!
//! Three real nodes, three real databases, real sockets on loopback. Nothing is
//! stubbed: these run the same engine, gossip and sync code as the CLI.
//!
//! Between them the tests below cover the five things this milestone claims:
//!
//! 1. node A creates a community and `#general`
//! 2. nodes B and C join it
//! 3. all three exchange signed messages peer to peer
//! 4. A shuts down while B and C keep chatting
//! 5. A returns and catches up on what it missed
//!
//! plus the sixth that makes the rest meaningful — each node persists its own
//! state locally, and can be restarted from it alone.
//!
//! mDNS is off throughout. Nodes find each other from an invite and then from
//! each other's presence announcements, which is the mechanism that has to work
//! on a real network; leaving mDNS on would let LAN discovery paper over a
//! failure in it, and would let a stray node on the developer's network join
//! the test.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kahui_node::{ChannelId, Invite, NetConfig, Node, NodeConfig, NodeHandle};
use kahui_proto::CommunityId;
use tempfile::TempDir;

/// Long enough to absorb a slow CI machine, short enough that a genuine hang
/// fails the run rather than stalling it.
const PATIENCE: Duration = Duration::from_secs(45);

/// Starts a node in `dir` with discovery limited to what the protocol provides.
async fn spawn(dir: &Path, name: &str) -> NodeHandle {
    Node::spawn(NodeConfig {
        data_dir: dir.to_path_buf(),
        display_name: Some(name.to_string()),
        net: NetConfig {
            // Loopback only: nothing here should touch the developer's network.
            listen: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
            enable_mdns: false,
            // Nor their router. A loopback address would be skipped anyway, but
            // this test should not depend on that to keep its promise.
            enable_port_mapping: false,
            heartbeat: Duration::from_millis(300),
            ..NetConfig::default()
        },
        // Brisk timers: the point is to watch convergence happen, not to wait
        // for it. The defaults are gentler.
        presence_interval: Duration::from_millis(400),
        sync_interval: Duration::from_millis(600),
        ..Default::default()
    })
    .await
    .unwrap_or_else(|err| panic!("node {name} failed to start: {err}"))
}

/// Polls until `check` passes, or fails the test with a useful description.
async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = Instant::now() + PATIENCE;
    loop {
        if check().await {
            return;
        }
        if Instant::now() >= deadline {
            panic!("timed out after {PATIENCE:?} waiting for: {what}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A node's view of a channel, as plain strings.
async fn transcript(node: &NodeHandle, community: CommunityId, channel: ChannelId) -> Vec<String> {
    node.history(community, channel, 200)
        .await
        .expect("history")
        .into_iter()
        .map(|message| format!("{}: {}", message.author_name, message.body))
        .collect()
}

/// Waits for every node to hold `expected`, then checks the two properties that
/// actually matter: every node renders the *same* sequence, and that sequence
/// contains exactly the expected messages.
///
/// It deliberately does not pin the order of concurrent messages. Two members
/// who post without having seen each other's message are genuinely concurrent,
/// and Kahui breaks that tie by event id — identical on every node, but
/// unrelated to who pressed enter first. Asserting wall-clock order here would
/// be demanding a guarantee the protocol does not make, and could not make
/// without a clock somebody has to be trusted to keep.
///
/// Returns the agreed transcript, so callers can assert relative order in the
/// cases where causality does settle it.
async fn converge_on(
    nodes: &[(&str, &NodeHandle)],
    community: CommunityId,
    channel: ChannelId,
    expected: &[&str],
) -> Vec<String> {
    let mut wanted: Vec<String> = expected.iter().map(|s| s.to_string()).collect();
    for (name, node) in nodes {
        eventually(
            &format!("{name} to see {} messages", wanted.len()),
            || async { transcript(node, community, channel).await.len() == wanted.len() },
        )
        .await;
    }

    let (first_name, first) = nodes[0];
    let agreed = transcript(first, community, channel).await;
    for (name, node) in &nodes[1..] {
        assert_eq!(
            transcript(node, community, channel).await,
            agreed,
            "{name} and {first_name} disagree about the order of history"
        );
    }

    let mut got = agreed.clone();
    got.sort();
    wanted.sort();
    assert_eq!(got, wanted, "{first_name} holds the wrong set of messages");
    agreed
}

/// Asserts `earlier` precedes `later`.
///
/// Kahui guarantees this exactly when the author of `later` had already seen
/// `earlier` — that is what the Lamport clock encodes. It says nothing about
/// messages written concurrently.
fn assert_causally_before(transcript: &[String], earlier: &str, later: &str) {
    let position = |needle: &str| {
        transcript
            .iter()
            .position(|line| line == needle)
            .unwrap_or_else(|| panic!("{needle:?} is missing from {transcript:?}"))
    };
    assert!(
        position(earlier) < position(later),
        "{later:?} was written after seeing {earlier:?}, so it must sort after it"
    );
}

async fn wait_for_members(node: &NodeHandle, community: CommunityId, count: usize, who: &str) {
    eventually(&format!("{who} to see {count} members"), || async {
        node.members(community).await.map(|m| m.len()).unwrap_or(0) == count
    })
    .await;
}

/// Founds a community on `founder` and returns its id, `#general` and an
/// invite.
async fn found(founder: &NodeHandle) -> (CommunityId, ChannelId, Invite) {
    // An invite is only useful once we have an address to put in it.
    eventually("the founder to be listening", || async {
        !founder
            .status()
            .await
            .map(|s| s.listen_addrs.is_empty())
            .unwrap_or(true)
    })
    .await;

    let community = founder
        .create_community("Kahui", "Hosted by its members")
        .await
        .expect("create community");

    let channels = founder.channels(community).await.expect("channels");
    assert_eq!(channels.len(), 1, "a new community starts with #general");
    assert_eq!(channels[0].name, "general");

    let invite = founder.invite(community).await.expect("invite");
    (community, channels[0].id, invite)
}

struct Nodes {
    _root: TempDir,
    a_dir: PathBuf,
    b_dir: PathBuf,
    c_dir: PathBuf,
}

impl Nodes {
    fn new() -> Self {
        let root = TempDir::new().expect("temp dir");
        let (a, b, c) = (
            root.path().join("a"),
            root.path().join("b"),
            root.path().join("c"),
        );
        Nodes {
            _root: root,
            a_dir: a,
            b_dir: b,
            c_dir: c,
        }
    }
}

/// The milestone in one run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_community_outlives_the_node_that_created_it() {
    let dirs = Nodes::new();

    // 1. A creates a community and #general.
    let alice = spawn(&dirs.a_dir, "alice").await;
    let (community, general, invite) = found(&alice).await;

    // The invite is a string a human could paste into a chat window.
    let encoded = invite.encode();
    assert!(encoded.starts_with("kahui1"));
    let invite = Invite::decode(&encoded).expect("invite survives a round trip through text");
    assert_eq!(invite.community, community);

    // 2. B and C join from that invite.
    let bob = spawn(&dirs.b_dir, "bob").await;
    let carol = spawn(&dirs.c_dir, "carol").await;
    assert_eq!(
        bob.join(invite.clone()).await.expect("bob joins"),
        community
    );
    assert_eq!(carol.join(invite).await.expect("carol joins"), community);

    // Joining means the history was fetched and verified, not merely that a
    // peer answered: B and C know the community's name and its channel.
    for (name, node) in [("bob", &bob), ("carol", &carol)] {
        let communities = node.communities().await.expect("communities");
        assert_eq!(communities.len(), 1, "{name} should hold one community");
        assert_eq!(communities[0].name, "Kahui");
        assert_eq!(communities[0].id, community);
        let channels = node.channels(community).await.expect("channels");
        assert_eq!(channels.len(), 1);
        assert_eq!(
            channels[0].id, general,
            "{name} derived the same channel id"
        );
    }

    let everyone = [("alice", &alice), ("bob", &bob), ("carol", &carol)];
    for (name, node) in everyone {
        wait_for_members(node, community, 3, name).await;
    }

    // 3. All three exchange messages.
    alice
        .post(community, general, "kia ora koutou")
        .await
        .unwrap();
    converge_on(&everyone, community, general, &["alice: kia ora koutou"]).await;

    bob.post(community, general, "kia ora alice").await.unwrap();
    carol.post(community, general, "carol here").await.unwrap();
    let agreed = converge_on(
        &everyone,
        community,
        general,
        &[
            "alice: kia ora koutou",
            "bob: kia ora alice",
            "carol: carol here",
        ],
    )
    .await;

    // Bob and Carol wrote concurrently, so their order is a coin toss that
    // lands the same way everywhere. Both had seen Alice's message, though, and
    // that ordering is not negotiable.
    assert_causally_before(&agreed, "alice: kia ora koutou", "bob: kia ora alice");
    assert_causally_before(&agreed, "alice: kia ora koutou", "carol: carol here");

    // Before pulling the founder out, confirm B and C are genuinely connected
    // to each other and not merely both connected to A. Without this the next
    // step would be testing nothing.
    for (name, node) in [("bob", &bob), ("carol", &carol)] {
        let other = if name == "bob" {
            carol.peer_id().to_string()
        } else {
            bob.peer_id().to_string()
        };
        eventually(&format!("{name} to connect directly to the other"), || {
            let other = other.clone();
            async move {
                node.status()
                    .await
                    .map(|s| s.connected_peers.contains(&other))
                    .unwrap_or(false)
            }
        })
        .await;
    }

    // 4. A shuts down. B and C carry on without it.
    alice.shutdown().await.expect("alice stops");
    drop(alice);

    bob.post(community, general, "still here").await.unwrap();
    carol.post(community, general, "so am i").await.unwrap();

    let survivors = [("bob", &bob), ("carol", &carol)];
    let agreed = converge_on(
        &survivors,
        community,
        general,
        &[
            "alice: kia ora koutou",
            "bob: kia ora alice",
            "carol: carol here",
            "bob: still here",
            "carol: so am i",
        ],
    )
    .await;
    // Written after the founder left, and still ordered after everything the
    // founder had said.
    assert_causally_before(&agreed, "carol: carol here", "bob: still here");
    assert_causally_before(&agreed, "carol: carol here", "carol: so am i");

    // 5. A comes back, from its own data directory, and catches up.
    let alice = spawn(&dirs.a_dir, "alice").await;

    // Its identity survived the restart: the events it signed before are still
    // attributed to it, so it is the same member, not a new one.
    assert_eq!(
        alice.members(community).await.expect("members").len(),
        3,
        "alice remembered the community without asking anyone"
    );

    // Nudge it towards its old peers; on a real network presence does this on
    // its own, but pinning it here keeps the test from depending on timing.
    for peer in [&bob, &carol] {
        let addr = peer.status().await.unwrap().listen_addrs[0].clone();
        alice
            .dial(format!("{addr}/p2p/{}", peer.peer_id()))
            .await
            .expect("dial");
    }

    let everyone = [("alice", &alice), ("bob", &bob), ("carol", &carol)];
    converge_on(
        &everyone,
        community,
        general,
        &[
            "alice: kia ora koutou",
            "bob: kia ora alice",
            "carol: carol here",
            "bob: still here",
            "carol: so am i",
        ],
    )
    .await;

    // And it can speak again, continuing its own chain from where it left off.
    alice
        .post(community, general, "what did i miss")
        .await
        .unwrap();
    let agreed = converge_on(
        &everyone,
        community,
        general,
        &[
            "alice: kia ora koutou",
            "bob: kia ora alice",
            "carol: carol here",
            "bob: still here",
            "carol: so am i",
            "alice: what did i miss",
        ],
    )
    .await;
    // Alice wrote this only after catching up, so it sorts after everything she
    // missed -- on her node and on everyone else's alike.
    assert_eq!(
        agreed.last().map(String::as_str),
        Some("alice: what did i miss"),
        "the catch-up message should land at the end of the transcript"
    );

    // 6. Every node holds the whole thing, independently.
    for (name, node) in everyone {
        let status = node.status().await.expect("status");
        let community_status = &status.communities[0];
        // 1 genesis + 3 joins + 1 channel + 6 messages. Every node holds the
        // complete set, not a cache of one: this is what "hosted by its
        // members" amounts to in practice.
        assert_eq!(
            community_status.events, 11,
            "{name} is missing part of the history",
        );
        assert_eq!(community_status.members, 3);
        assert_eq!(community_status.channels, 1);
    }

    for node in [&alice, &bob, &carol] {
        node.shutdown().await.expect("clean shutdown");
    }
}

/// A node restarted from its data directory is the same node.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_keeps_its_identity_and_history_across_restarts() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("solo");

    let node = spawn(&path, "solo").await;
    let user = node.user();
    let peer_id = node.peer_id().to_string();
    let (community, general, _) = found(&node).await;
    node.post(community, general, "written before the restart")
        .await
        .unwrap();
    node.shutdown().await.unwrap();
    drop(node);

    let node = spawn(&path, "solo").await;
    assert_eq!(
        node.user(),
        user,
        "the keypair is the identity, and it persisted"
    );
    assert_eq!(
        node.peer_id(),
        peer_id,
        "so the network identity is stable too"
    );
    assert_eq!(
        transcript(&node, community, general).await,
        ["solo: written before the restart"]
    );

    // The chain continues rather than restarting, which is what stops the
    // restart from looking like equivocation to everybody else.
    node.post(community, general, "written after")
        .await
        .unwrap();
    assert_eq!(
        transcript(&node, community, general).await,
        ["solo: written before the restart", "solo: written after"]
    );
    node.shutdown().await.unwrap();
}

/// Two members creating the same channel name independently converge on one
/// channel rather than forking into two.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_channel_creation_converges() {
    let dirs = Nodes::new();
    let alice = spawn(&dirs.a_dir, "alice").await;
    let (community, _general, invite) = found(&alice).await;

    let bob = spawn(&dirs.b_dir, "bob").await;
    bob.join(invite).await.expect("bob joins");
    wait_for_members(&alice, community, 2, "alice").await;

    // Both create #random, neither having seen the other do it.
    let from_alice = alice.create_channel(community, "random", "").await.unwrap();
    let from_bob = bob.create_channel(community, "random", "").await.unwrap();
    assert_eq!(
        from_alice, from_bob,
        "channel ids are derived from the name, so both computed the same one"
    );

    for (name, node) in [("alice", &alice), ("bob", &bob)] {
        eventually(&format!("{name} to settle on two channels"), || async {
            node.channels(community).await.map(|c| c.len()).unwrap_or(0) == 2
        })
        .await;
        let channels = node.channels(community).await.unwrap();
        let names: Vec<_> = channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["general", "random"], "{name} has one #random");
    }

    for node in [&alice, &bob] {
        node.shutdown().await.unwrap();
    }
}
