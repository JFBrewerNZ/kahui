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
    spawn_with(
        dir,
        name,
        reachability,
        vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
    )
    .await
}

/// Starts a node that listens nowhere at all.
///
/// It can dial out and nothing can dial in — which is what a device on a
/// network with no route to the wider world actually looks like from the
/// outside, and a good deal stricter than being behind a router.
async fn spawn_dial_only(dir: &Path, name: &str) -> NodeHandle {
    spawn_with(dir, name, Some(Reachability::BehindNat), Vec::new()).await
}

async fn spawn_with(
    dir: &Path,
    name: &str,
    reachability: Option<Reachability>,
    listen: Vec<libp2p::Multiaddr>,
) -> NodeHandle {
    Node::spawn(NodeConfig {
        data_dir: dir.to_path_buf(),
        display_name: Some(name.to_string()),
        net: NetConfig {
            listen,
            // Off, so that finding each other has to happen the way it would
            // between strangers rather than by shouting on a shared network.
            enable_mdns: false,
            heartbeat: Duration::from_millis(300),
            enable_relay: true,
            // No router to ask in a test, and the search would only time out.
            enable_port_mapping: false,
            // Off here on purpose. These tests pin down what happens with an
            // exactly known topology; discovery is what removes the need for
            // one, and is proved separately in `finding_strangers.rs`.
            enable_dht: false,
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

/// The household case: one device has a connection, the others do not, and
/// everybody is in the same community anyway.
///
/// The two outer nodes listen on nothing at all, so neither can dial the other
/// even after learning its address — there is no address to learn. Every
/// message between them has to pass through the one node in the middle.
///
/// What is worth noticing is *how*. The middle node is not routing packets and
/// has not been configured as a gateway. It is an ordinary member that happens
/// to hold the events, and gossip and sync move them onward exactly as they
/// would to anybody else. Being the only path is not a role it was given; it is
/// a shape the network happened to take.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn devices_with_no_way_in_still_reach_each_other_through_one_that_has() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    // The one machine with a connection. It founds the community.
    let router_pc = spawn(&dir("desk"), "desk", Some(Reachability::Direct)).await;
    wait_until_listening(&router_pc, "desk").await;

    let community = router_pc
        .create_community("Whanau", "The household")
        .await
        .expect("create");
    let general = router_pc.channels(community).await.unwrap()[0].id;
    let invite = router_pc.invite(community).await.expect("invite");

    // Two devices that can only make outbound connections.
    let phone = spawn_dial_only(&dir("phone"), "phone").await;
    let tablet = spawn_dial_only(&dir("tablet"), "tablet").await;

    phone.join(invite.clone()).await.expect("phone joins");
    tablet.join(invite).await.expect("tablet joins");

    // Neither listens on anything of its own. Any address they do have was
    // given to them by the machine in the middle, and goes through it — so
    // there is no path between the phone and the tablet that avoids the desk.
    for (name, node) in [("phone", &phone), ("tablet", &tablet)] {
        let addrs = node.status().await.unwrap().listen_addrs;
        assert!(
            addrs.iter().all(|addr| addr.contains("p2p-circuit")),
            "{name} should be reachable only through another member, got {addrs:?}"
        );
    }

    // The desk took them both on without being asked to.
    eventually("the desk to be carrying for both devices", || async {
        router_pc
            .status()
            .await
            .map(|s| s.relaying_for >= 2)
            .unwrap_or(false)
    })
    .await;

    // A message from one goes to the other, through the machine in the middle.
    phone
        .post(community, general, "dinner in ten")
        .await
        .unwrap();

    eventually("the tablet to hear the phone", || async {
        transcript(&tablet, community, general)
            .await
            .contains(&"phone: dinner in ten".to_string())
    })
    .await;

    // And back the other way.
    tablet.post(community, general, "coming").await.unwrap();
    eventually("the phone to hear the tablet", || async {
        transcript(&phone, community, general)
            .await
            .contains(&"tablet: coming".to_string())
    })
    .await;

    // All three hold the same history, and each holds all of it.
    let expected = vec![
        "phone: dinner in ten".to_string(),
        "tablet: coming".to_string(),
    ];
    for (name, node) in [("desk", &router_pc), ("phone", &phone), ("tablet", &tablet)] {
        eventually(&format!("{name} to hold both messages"), || async {
            transcript(node, community, general).await.len() == 2
        })
        .await;
        assert_eq!(
            transcript(node, community, general).await,
            expected,
            "{name} should hold the same transcript as everyone else"
        );
    }

    for node in [&router_pc, &phone, &tablet] {
        node.shutdown().await.ok();
    }
}

/// Somebody behind a router creates a community, and other people join it.
///
/// This is the ordinary case — a person on a laptop starting a server for their
/// friends — and it used to be impossible. A node only looked for a relay among
/// peers it was already connected to, and a node that has just created a
/// community has none, so it could never become reachable and its invites named
/// addresses only its own network could use.
///
/// Meeting one reachable node, once, is now enough. It is remembered across
/// restarts and across communities, because relaying has nothing to do with
/// membership.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn somebody_behind_a_router_can_host_a_community() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    // A reachable node. Not a member of anything — just somebody who can be
    // dialled and is willing to carry.
    let helper = spawn(&dir("helper"), "helper", Some(Reachability::Direct)).await;
    wait_until_listening(&helper, "helper").await;
    let helper_addr = helper.status().await.unwrap().listen_addrs[0].clone();
    let helper_dial = format!("{helper_addr}/p2p/{}", helper.peer_id());

    // Someone on a laptop behind a router. They meet the helper once.
    let host = spawn(&dir("host"), "host", Some(Reachability::BehindNat)).await;
    host.dial(helper_dial.clone())
        .await
        .expect("meet the helper");

    eventually("the laptop to be carried by somebody", || async {
        host.status()
            .await
            .map(|s| s.relayed_by.is_some())
            .unwrap_or(false)
    })
    .await;

    // Now they start a community. Nobody is in it and nobody has been invited.
    let community = host.create_community("Games", "").await.expect("create");
    let general = host.channels(community).await.unwrap()[0].id;

    // The invite names a way in that does not require reaching the laptop
    // directly.
    let invite = host.invite(community).await.expect("invite");
    assert!(
        invite
            .dial_addresses()
            .iter()
            .any(|a| a.contains("p2p-circuit")),
        "the invite should offer a relayed route, got {:?}",
        invite.dial_addresses()
    );

    // And somebody else can use it.
    let guest = spawn(&dir("guest"), "guest", Some(Reachability::Direct)).await;
    guest
        .join(invite)
        .await
        .expect("guest joins a community hosted behind a router");

    host.post(community, general, "anyone can host this")
        .await
        .unwrap();
    eventually("the guest to hear the host", || async {
        transcript(&guest, community, general)
            .await
            .contains(&"host: anyone can host this".to_string())
    })
    .await;

    for node in [&helper, &host, &guest] {
        node.shutdown().await.ok();
    }
}

/// The laptop does not have to be told about the helper twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_met_once_is_remembered_across_restarts() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    let helper = spawn(&dir("helper"), "helper", Some(Reachability::Direct)).await;
    wait_until_listening(&helper, "helper").await;
    let helper_addr = helper.status().await.unwrap().listen_addrs[0].clone();

    let host = spawn(&dir("host"), "host", Some(Reachability::BehindNat)).await;
    host.dial(format!("{helper_addr}/p2p/{}", helper.peer_id()))
        .await
        .expect("meet the helper");
    eventually("the first reservation", || async {
        host.status()
            .await
            .map(|s| s.relayed_by.is_some())
            .unwrap_or(false)
    })
    .await;

    host.shutdown().await.expect("stop");
    drop(host);

    // Same data directory, nothing dialled by hand this time.
    let host = spawn(&dir("host"), "host", Some(Reachability::BehindNat)).await;
    eventually("the laptop to find its relay again on its own", || async {
        host.status()
            .await
            .map(|s| s.relayed_by.is_some())
            .unwrap_or(false)
    })
    .await;

    for node in [&helper, &host] {
        node.shutdown().await.ok();
    }
}
