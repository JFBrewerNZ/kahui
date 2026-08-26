//! What happens when the only reachable member leaves.
//!
//! Bob can be dialled and starts a community. Jane and Juan both sit behind
//! routers that refuse everything, so neither can be dialled by anybody — while
//! Bob is there, he carries for both of them. Then Bob leaves.
//!
//! There are two quite different questions in that, and they have two different
//! answers:
//!
//! 1. **Is anything lost?** No, and this is the part that is guaranteed. Jane
//!    and Juan each hold the entire history on their own disk. Nothing about the
//!    community lived on Bob's machine that does not also live on theirs.
//!
//! 2. **Can they still reach each other?** Only if there is some path between
//!    them, and two machines that can each only make outbound connections have
//!    none. They need somebody reachable — any member, or any other Kāhui user
//!    at all, since carrying is not membership.
//!
//! The tests below pin down both. The second one is deliberately the worst case
//! there is: Jane and Juan listen on nothing whatsoever, so no hole punch is
//! even theoretically possible. A real pair of home connections usually does
//! better than this, because a relayed connection is enough for DCUtR to
//! coordinate a hole punch and most home routers allow one — but "usually" is
//! not something a test can assert, so what is asserted here is the floor.

use std::path::Path;
use std::time::Duration;

use kahui_node::{ChannelId, Invite, NetConfig, Node, NodeConfig, NodeHandle, Reachability};
use kahui_proto::CommunityId;
use tempfile::TempDir;

const PATIENCE: Duration = Duration::from_secs(120);

async fn spawn(
    dir: &Path,
    name: &str,
    listen: Vec<&str>,
    reachability: Reachability,
    knows: Vec<String>,
) -> NodeHandle {
    Node::spawn(NodeConfig {
        data_dir: dir.to_path_buf(),
        display_name: Some(name.to_string()),
        net: NetConfig {
            listen: listen.iter().map(|a| a.parse().unwrap()).collect(),
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
        bootstrap: knows,
        ..Default::default()
    })
    .await
    .unwrap_or_else(|err| panic!("{name} could not start: {err}"))
}

async fn address_of(node: &NodeHandle, name: &str) -> String {
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
    panic!("{name} never got an address");
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

async fn texts(node: &NodeHandle, community: CommunityId, channel: ChannelId) -> Vec<String> {
    let mut said: Vec<String> = node
        .history(community, channel, 200)
        .await
        .map(|msgs| msgs.into_iter().map(|m| m.body).collect())
        .unwrap_or_default();
    said.sort();
    said
}

/// The realistic answer: yes, because somebody else on the network carries them.
///
/// Zoe is not in their community and never joins it. She is another person who
/// happens to run Kāhui and happens to be reachable — which in this design is
/// the entire qualification for being useful to strangers. When Bob goes, Jane
/// and Juan find her and carry on through her, without anybody arranging it.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn jane_and_juan_keep_talking_through_somebody_who_is_not_even_a_member() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    // Another Kāhui user, minding her own business.
    let zoe = spawn(
        &dir("zoe"),
        "zoe",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![],
    )
    .await;
    let zoe_addr = address_of(&zoe, "zoe").await;

    let bob = spawn(
        &dir("bob"),
        "bob",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![zoe_addr.clone()],
    )
    .await;
    address_of(&bob, "bob").await;

    let art = bob
        .create_community("Art", "")
        .await
        .expect("bob starts it");
    let general = bob.channels(art).await.unwrap()[0].id;
    bob.post(art, general, "bob: hello").await.unwrap();
    let link = bob.invite(art).await.unwrap().to_link();

    // Both behind routers that let nothing in. Neither can be dialled.
    let jane = spawn(
        &dir("jane"),
        "jane",
        vec![],
        Reachability::BehindNat,
        vec![zoe_addr.clone()],
    )
    .await;
    let juan = spawn(
        &dir("juan"),
        "juan",
        vec![],
        Reachability::BehindNat,
        vec![zoe_addr],
    )
    .await;

    jane.join(Invite::decode(&link).unwrap())
        .await
        .expect("jane joins");
    juan.join(Invite::decode(&link).unwrap())
        .await
        .expect("juan joins");

    eventually("everybody to be in the same conversation", || async {
        texts(&jane, art, general).await.len() == 1 && texts(&juan, art, general).await.len() == 1
    })
    .await;

    // --- Bob goes, and does not come back ---------------------------------
    bob.shutdown().await.ok();

    jane.post(art, general, "jane: bob has gone")
        .await
        .expect("jane can still post");

    eventually("juan to hear jane with bob gone", || async {
        texts(&juan, art, general).await.len() == 2
    })
    .await;

    juan.post(art, general, "juan: still here")
        .await
        .expect("juan can still post");

    eventually("jane to hear juan back", || async {
        texts(&jane, art, general).await.len() == 3
    })
    .await;

    assert_eq!(
        texts(&jane, art, general).await,
        texts(&juan, art, general).await
    );

    // And Zoe, who carried all of that, is still not in the community.
    assert!(
        !zoe.communities().await.unwrap().iter().any(|c| c.id == art),
        "carrying for somebody is not joining their community"
    );

    for node in [&zoe, &jane, &juan] {
        node.shutdown().await.ok();
    }
}

/// The worst case: nobody reachable is left at all.
///
/// Neither Jane nor Juan can be dialled and there is no third party of any kind,
/// so there is no path between them and no new message can cross. What matters
/// is what that costs: nothing is lost, both still hold every word, and the
/// moment anybody reachable turns up they are back in touch — including messages
/// written while they were apart.
#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn with_nobody_reachable_left_nothing_is_lost_and_it_resumes() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    let bob = spawn(
        &dir("bob"),
        "bob",
        vec!["/ip4/127.0.0.1/tcp/0"],
        Reachability::Direct,
        vec![],
    )
    .await;
    let bob_addr = address_of(&bob, "bob").await;

    let art = bob
        .create_community("Art", "")
        .await
        .expect("bob starts it");
    let general = bob.channels(art).await.unwrap()[0].id;
    bob.post(art, general, "bob: hello").await.unwrap();
    let link = bob.invite(art).await.unwrap().to_link();

    let jane = spawn(
        &dir("jane"),
        "jane",
        vec![],
        Reachability::BehindNat,
        vec![],
    )
    .await;
    let juan = spawn(
        &dir("juan"),
        "juan",
        vec![],
        Reachability::BehindNat,
        vec![],
    )
    .await;
    jane.join(Invite::decode(&link).unwrap())
        .await
        .expect("jane joins");
    juan.join(Invite::decode(&link).unwrap())
        .await
        .expect("juan joins");

    eventually("both to have bob's message", || async {
        texts(&jane, art, general).await.len() == 1 && texts(&juan, art, general).await.len() == 1
    })
    .await;

    // --- The only reachable person leaves ---------------------------------
    bob.shutdown().await.ok();

    // Nothing is lost. This is the guaranteed part.
    assert_eq!(texts(&jane, art, general).await, vec!["bob: hello"]);
    assert_eq!(texts(&juan, art, general).await, vec!["bob: hello"]);

    // Both can still write. A message is an event on their own chain; whether
    // anybody has read it yet is a separate matter.
    jane.post(art, general, "jane: anyone there")
        .await
        .expect("jane can still write");
    juan.post(art, general, "juan: hello?")
        .await
        .expect("juan can still write");

    // And neither of them can hear the other, which is the premise of this
    // test rather than an incidental detail. Two machines that can only make
    // outbound connections have nowhere to meet.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        texts(&jane, art, general).await.len(),
        2,
        "jane should have bob's message and her own, and nothing of juan's"
    );
    assert_eq!(texts(&juan, art, general).await.len(), 2, "juan likewise");

    // --- Somebody reachable comes back ------------------------------------
    //
    // Bob restarting is the easiest way to write this, but it need not be Bob,
    // and it need not be a member — see the test above.
    let bob = spawn(
        &dir("bob"),
        "bob",
        vec![bob_addr.rsplit_once("/p2p/").unwrap().0],
        Reachability::Direct,
        vec![],
    )
    .await;

    eventually("jane and juan to find each other again", || async {
        texts(&jane, art, general).await.len() == 3 && texts(&juan, art, general).await.len() == 3
    })
    .await;

    let settled = texts(&jane, art, general).await;
    assert_eq!(texts(&juan, art, general).await, settled);
    assert!(settled.contains(&"jane: anyone there".to_string()));
    assert!(settled.contains(&"juan: hello?".to_string()));

    for node in [&bob, &jane, &juan] {
        node.shutdown().await.ok();
    }
}
