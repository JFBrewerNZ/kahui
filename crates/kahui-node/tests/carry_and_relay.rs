//! Getting a message somewhere no single link ever reached.
//!
//! Two things are proven here, and they are the two halves of "this works
//! without the internet".
//!
//! **Carrying.** A community is an append-only log of signed events, so any
//! member holding those events can hand them on, and the recipient can verify
//! them without ever having met the author. That makes the whole thing
//! delay-tolerant for free: a message can cross a chain of people who were
//! never online at the same time. The first test walks a message down such a
//! chain, shutting down each node before the next one appears, so at no point
//! does a path exist between the first node and the last.
//!
//! **Relaying.** A node behind a router cannot be dialled, which would make it
//! a spectator: able to read and post, unable to serve history to anybody. A
//! member who *can* be dialled carries for it. The second test shows a node
//! obtaining that reservation and a third node reaching it through the circuit.

use std::future::Future;
use std::path::Path;
use std::time::{Duration, Instant};

use kahui_node::{ChannelId, NetConfig, Node, NodeConfig, NodeHandle, Reachability};
use kahui_proto::CommunityId;
use tempfile::TempDir;

const PATIENCE: Duration = Duration::from_secs(60);

/// Starts a node with discovery limited to what the protocol provides.
async fn spawn(dir: &Path, name: &str, reachability: Option<Reachability>) -> NodeHandle {
    Node::spawn(NodeConfig {
        data_dir: dir.to_path_buf(),
        display_name: Some(name.to_string()),
        net: NetConfig {
            listen: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
            // Off, so that finding each other has to happen the way it would
            // between strangers rather than by shouting on a shared network.
            enable_mdns: false,
            heartbeat: Duration::from_millis(300),
            enable_relay: true,
            lan_reachable: false,
        },
        presence_interval: Duration::from_millis(400),
        sync_interval: Duration::from_millis(600),
        reachability,
        ..Default::default()
    })
    .await
    .unwrap_or_else(|err| panic!("node {name} failed to start: {err}"))
}

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

async fn transcript(node: &NodeHandle, community: CommunityId, channel: ChannelId) -> Vec<String> {
    node.history(community, channel, 200)
        .await
        .expect("history")
        .into_iter()
        .map(|m| format!("{}: {}", m.author_name, m.body))
        .collect()
}

/// Waits until the node has a listening address, so an invite it mints is
/// actually usable.
async fn wait_until_listening(node: &NodeHandle, who: &str) {
    eventually(&format!("{who} to be listening"), || async {
        !node
            .status()
            .await
            .map(|s| s.listen_addrs.is_empty())
            .unwrap_or(true)
    })
    .await;
}

/// A message travels from the first node to the last down a chain of members,
/// with no two ends of the chain ever online together.
///
/// This is the neighbourhood case in miniature: nobody has a route to anybody
/// far away, and the message arrives anyway, carried by whoever happened to be
/// in the middle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_crosses_a_chain_that_never_existed_all_at_once() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    // --- Ana founds a community and says something, then goes away for good.
    let ana = spawn(&dir("ana"), "ana", None).await;
    wait_until_listening(&ana, "ana").await;

    let community = ana
        .create_community("Aotearoa", "Hosted by its members")
        .await
        .expect("create");
    let general = ana.channels(community).await.unwrap()[0].id;
    ana.post(community, general, "the power is out, but this still works")
        .await
        .unwrap();
    let from_ana = ana.invite(community).await.expect("invite");

    // --- Ben meets Ana, takes a copy of everything, and Ana disappears.
    let ben = spawn(&dir("ben"), "ben", None).await;
    ben.join(from_ana).await.expect("ben joins ana");
    eventually("ben to have ana's message", || async {
        transcript(&ben, community, general).await.len() == 1
    })
    .await;

    ana.shutdown().await.expect("ana stops");
    drop(ana);

    // --- Cam meets Ben. Ana is gone and Cam has never contacted her.
    let cam = spawn(&dir("cam"), "cam", None).await;
    wait_until_listening(&ben, "ben").await;
    let from_ben = ben.invite(community).await.expect("invite from ben");
    cam.join(from_ben).await.expect("cam joins ben");

    eventually(
        "cam to receive a message from someone she never met",
        || async { transcript(&cam, community, general).await.len() == 1 },
    )
    .await;
    assert_eq!(
        transcript(&cam, community, general).await,
        ["ana: the power is out, but this still works"],
        "the message should have survived the hop through Ben"
    );

    ben.shutdown().await.expect("ben stops");
    drop(ben);

    // --- Dee meets Cam. Both Ana and Ben are gone.
    let dee = spawn(&dir("dee"), "dee", None).await;
    wait_until_listening(&cam, "cam").await;
    let from_cam = cam.invite(community).await.expect("invite from cam");
    dee.join(from_cam).await.expect("dee joins cam");

    eventually("dee to receive ana's message two hops later", || async {
        transcript(&dee, community, general).await.len() == 1
    })
    .await;

    assert_eq!(
        transcript(&dee, community, general).await,
        ["ana: the power is out, but this still works"],
        "Ana's message should have reached Dee, though they were never online together"
    );

    // Dee holds the whole community, not just the one message she was told
    // about: the founder's genesis, and every join along the way.
    let members = dee.members(community).await.expect("members");
    assert_eq!(members.len(), 4, "ana, ben, cam and dee");
    assert_eq!(
        dee.communities().await.unwrap()[0].name,
        "Aotearoa",
        "including who founded it and what it is called"
    );

    // And Dee can answer back, on a chain that continues from history she was
    // handed rather than history she witnessed.
    dee.post(community, general, "received, thank you")
        .await
        .unwrap();
    eventually("cam to hear dee", || async {
        transcript(&cam, community, general).await.len() == 2
    })
    .await;

    for node in [&cam, &dee] {
        node.shutdown().await.ok();
    }
}

/// A node that cannot be dialled gets a member to carry for it, and a third
/// node reaches it through that member.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_member_is_carried_by_a_reachable_one() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    // Hemi is reachable and will do the carrying. Iwa is behind a router.
    let hemi = spawn(&dir("hemi"), "hemi", Some(Reachability::Direct)).await;
    wait_until_listening(&hemi, "hemi").await;

    let community = hemi
        .create_community("Aotearoa", "Hosted by its members")
        .await
        .expect("create");
    let general = hemi.channels(community).await.unwrap()[0].id;
    let invite = hemi.invite(community).await.expect("invite");

    let iwa = spawn(&dir("iwa"), "iwa", Some(Reachability::BehindNat)).await;
    iwa.join(invite.clone()).await.expect("iwa joins");

    // Iwa asks Hemi to carry, and Hemi agrees.
    eventually("iwa to find a member willing to relay", || async {
        iwa.status()
            .await
            .map(|s| s.relayed_by.is_some())
            .unwrap_or(false)
    })
    .await;

    let iwa_status = iwa.status().await.unwrap();
    assert_eq!(
        iwa_status.relayed_by.as_deref(),
        Some(hemi.peer_id()),
        "the member doing the carrying should be Hemi"
    );
    assert_eq!(iwa_status.reachability, Reachability::BehindNat);

    eventually("hemi to report that it is carrying for someone", || async {
        hemi.status()
            .await
            .map(|s| s.relaying_for > 0)
            .unwrap_or(false)
    })
    .await;

    // Iwa now advertises an address that goes through Hemi.
    let circuit = iwa_status
        .listen_addrs
        .iter()
        .find(|addr| addr.contains("p2p-circuit"))
        .cloned()
        .expect("iwa should advertise a circuit address once it has a relay");

    // Rangi reaches Iwa through it, having no way to dial Iwa directly.
    let rangi = spawn(&dir("rangi"), "rangi", Some(Reachability::Direct)).await;
    rangi.join(invite).await.expect("rangi joins");
    rangi
        .dial(format!("{circuit}/p2p/{}", iwa.peer_id()))
        .await
        .expect("dial iwa through the circuit");

    eventually("rangi to connect to iwa", || async {
        rangi
            .status()
            .await
            .map(|s| s.connected_peers.iter().any(|p| p == iwa.peer_id()))
            .unwrap_or(false)
    })
    .await;

    // And a message gets through the path that was opened.
    iwa.post(community, general, "kia ora from behind the router")
        .await
        .unwrap();
    eventually("rangi to hear the unreachable member", || async {
        transcript(&rangi, community, general)
            .await
            .contains(&"iwa: kia ora from behind the router".to_string())
    })
    .await;

    for node in [&hemi, &iwa, &rangi] {
        node.shutdown().await.ok();
    }
}

/// A node told it is reachable does not go looking for somebody to carry for
/// it, because occupying a relay slot it does not need takes it from a member
/// who does.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reachable_member_does_not_take_a_relay_slot() {
    let root = TempDir::new().unwrap();
    let host = spawn(
        &root.path().join("host"),
        "host",
        Some(Reachability::Direct),
    )
    .await;
    wait_until_listening(&host, "host").await;

    let community = host.create_community("Aotearoa", "").await.unwrap();
    let invite = host.invite(community).await.unwrap();

    let guest = spawn(
        &root.path().join("guest"),
        "guest",
        Some(Reachability::Direct),
    )
    .await;
    guest.join(invite).await.expect("guest joins");

    // Give it long enough that a relay request would have happened by now.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let status = guest.status().await.unwrap();
    assert_eq!(status.reachability, Reachability::Direct);
    assert!(
        status.relayed_by.is_none(),
        "no relay should have been sought"
    );
    assert!(
        !status
            .listen_addrs
            .iter()
            .any(|a| a.contains("p2p-circuit")),
        "and no circuit address should be advertised"
    );

    for node in [&host, &guest] {
        node.shutdown().await.ok();
    }
}
