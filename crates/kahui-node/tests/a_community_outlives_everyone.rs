//! Bob starts a community. Everyone who started it leaves. It carries on.
//!
//! This is the scenario the whole project exists for, written out literally:
//!
//! > Bob lives in California. Jane lives in Melbourne. Bob downloads Kāhui and
//! > creates an "Art" server. Bob shares the link to Jane. Jane is able to join.
//! > Sam from the UK later joins, and the chat is synced to him. Juan from
//! > Colombia joins, same thing. Bob and Jane leave — the Art server survives
//! > because Sam and Juan are still there. Sam also leaves. The Art server still
//! > survives because Juan is still there.
//!
//! Nobody browses for anything. The link is passed around outside Kāhui, the way
//! a Discord invite gets passed around on Facebook or Reddit, and there is no
//! directory of communities to search — see `docs/discovery.md`.
//!
//! What the test pins down is the part a Discord link gets for free and this one
//! has to earn. A Discord link keeps working because Discord's servers are
//! always up. A Kāhui link names a community rather than a machine, so it keeps
//! working as long as *anybody* still holds that community — which is why the
//! last section here matters most: somebody joins "Art" long after every person
//! who was there at the start has gone.

use std::path::Path;
use std::time::Duration;

use kahui_node::{ChannelId, Invite, NetConfig, Node, NodeConfig, NodeHandle, Reachability};
use kahui_proto::CommunityId;
use tempfile::TempDir;

const PATIENCE: Duration = Duration::from_secs(120);

/// An ordinary person's machine.
///
/// Everyone here is dialable, which is the common case once a node has asked the
/// router to open a port or found it has a public IPv6 address. Somebody who
/// cannot be dialled is covered in `finding_strangers.rs`.
async fn person(dir: &Path, name: &str, knows: Vec<String>) -> NodeHandle {
    Node::spawn(NodeConfig {
        data_dir: dir.to_path_buf(),
        display_name: Some(name.to_string()),
        net: NetConfig {
            listen: vec!["/ip4/127.0.0.1/tcp/0".parse().unwrap()],
            // Nobody here is on the same network as anybody else. California,
            // Melbourne, the UK and Colombia do not share a broadcast domain.
            enable_mdns: false,
            heartbeat: Duration::from_millis(250),
            enable_relay: true,
            enable_port_mapping: false,
            enable_dht: true,
            lan_reachable: true,
        },
        presence_interval: Duration::from_millis(400),
        sync_interval: Duration::from_millis(500),
        reachability: Some(Reachability::Direct),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 6)]
async fn the_art_server_outlives_everybody_who_started_it() {
    let root = TempDir::new().unwrap();
    let dir = |name: &str| root.path().join(name);

    // --- Zoe, who has nothing to do with any of this -----------------------
    //
    // Somebody else who happens to run Kāhui, in a community of her own. She is
    // not infrastructure, is never told about Art, and never joins it. She is
    // here because a network is other people, and everybody below is on it the
    // way a real user would be: they have run into somebody at some point.
    let zoe = person(&dir("zoe"), "zoe", vec![]).await;
    let zoe_addr = address_of(&zoe, "zoe").await;
    zoe.create_community("Bakers", "Nothing to do with Art")
        .await
        .expect("zoe has her own community");

    // --- Bob, in California, downloads Kāhui and starts a community ---------
    //
    // There is no server to sign in to and nothing to configure; the community
    // exists the moment he makes it.
    let bob = person(&dir("bob"), "bob", vec![zoe_addr.clone()]).await;
    let bob_addr = address_of(&bob, "bob").await;

    let art = bob
        .create_community("Art", "Drawing and complaining about drawing")
        .await
        .expect("bob starts Art");
    let general = bob.channels(art).await.unwrap()[0].id;
    bob.post(art, general, "bob: opening the doors")
        .await
        .unwrap();

    // The link he pastes into a message somewhere. Nobody searched for this.
    let link = bob.invite(art).await.expect("bob gets a link").to_link();

    // --- Jane, in Melbourne, is sent the link ------------------------------
    let jane = person(&dir("jane"), "jane", vec![zoe_addr.clone()]).await;
    jane.join(Invite::decode(&link).unwrap())
        .await
        .expect("jane joins from the link");
    jane.post(art, general, "jane: kia ora from melbourne")
        .await
        .unwrap();

    eventually("bob and jane to see each other", || async {
        texts(&bob, art, general).await.len() == 2 && texts(&jane, art, general).await.len() == 2
    })
    .await;

    // --- Sam, in the UK, is sent the same link later -----------------------
    //
    // He was not there for any of it, so everything said before he arrived has
    // to reach him from the people who were.
    let sam = person(&dir("sam"), "sam", vec![zoe_addr.clone()]).await;
    sam.join(Invite::decode(&link).unwrap())
        .await
        .expect("sam joins from the same link");

    eventually("sam to receive the history he missed", || async {
        texts(&sam, art, general).await.len() == 2
    })
    .await;
    sam.post(art, general, "sam: what did i miss")
        .await
        .unwrap();

    // --- Juan, in Colombia, joins too --------------------------------------
    let juan = person(&dir("juan"), "juan", vec![zoe_addr.clone()]).await;
    juan.join(Invite::decode(&link).unwrap())
        .await
        .expect("juan joins from the same link");

    eventually("juan to receive the whole conversation", || async {
        texts(&juan, art, general).await.len() == 3
    })
    .await;
    juan.post(art, general, "juan: buenas").await.unwrap();

    eventually("everybody to agree on what was said", || async {
        let expected = texts(&bob, art, general).await;
        expected.len() == 4
            && texts(&jane, art, general).await == expected
            && texts(&sam, art, general).await == expected
            && texts(&juan, art, general).await == expected
    })
    .await;

    // --- Bob and Jane leave ------------------------------------------------
    //
    // Bob founded it. Nothing about the community was ever his: there is no
    // owner flag, and no state of his that anybody else was depending on.
    let sam_addr = address_of(&sam, "sam").await;
    bob.shutdown().await.ok();
    jane.shutdown().await.ok();

    sam.post(art, general, "sam: still here without bob")
        .await
        .expect("sam can still post");

    eventually("juan to hear sam after the founder left", || async {
        texts(&juan, art, general).await.len() == 5
    })
    .await;

    // --- Sam leaves too ----------------------------------------------------
    sam.shutdown().await.ok();

    juan.post(art, general, "juan: last one here")
        .await
        .expect("juan can still post");

    let alone = texts(&juan, art, general).await;
    assert_eq!(alone.len(), 6, "juan should hold the whole conversation");
    assert!(alone.contains(&"bob: opening the doors".to_string()));
    assert!(alone.contains(&"jane: kia ora from melbourne".to_string()));

    // --- And the community is still joinable -------------------------------
    //
    // The real test of survival, and the part a link to somebody's machine
    // could never pass. Mia has the same link that has been going round, but
    // every address in it belongs to somebody who has gone. What still means
    // something is the community's id, so she asks the network who holds it and
    // Juan answers.
    //
    // The only person she knows is Zoe, who is not in Art, has never heard of
    // Art, and cannot introduce her to anybody in it. Nobody hands Mia a route
    // to Juan: she has to ask the network who holds the community, and Juan is
    // the answer.
    let mia = person(&dir("mia"), "mia", vec![zoe_addr]).await;
    assert!(
        !mia.communities().await.unwrap().iter().any(|c| c.id == art),
        "mia should not know about Art before she joins it"
    );
    assert!(
        !zoe.communities().await.unwrap().iter().any(|c| c.id == art),
        "zoe must never have joined Art, or she could have introduced them"
    );

    // Worth stating rather than assuming: the link was written when Bob was the
    // only member, so every address in it is his, and he is gone. There is no
    // route to Juan anywhere in it.
    let juan_peer = juan.status().await.unwrap().peer_id;
    let decoded = Invite::decode(&link).unwrap();
    assert!(
        !decoded.peers.iter().any(|peer| peer.peer_id == juan_peer),
        "the link must not contain a way to reach juan, or this proves nothing"
    );

    mia.join(decoded)
        .await
        .expect("mia joins from a link whose addresses are all dead");

    eventually(
        "mia to receive a conversation nobody who started it is left for",
        || async { texts(&mia, art, general).await.len() == 6 },
    )
    .await;

    assert_eq!(
        texts(&mia, art, general).await,
        alone,
        "mia should end up with exactly what juan has"
    );

    // Nothing was left behind that pointed at a machine of ours, or anybody's.
    assert!(
        !sam_addr.is_empty() && !bob_addr.is_empty(),
        "addresses existed, and none of them mattered in the end"
    );

    // And Zoe, who carried a lookup for a community she is not in, still knows
    // nothing about it. Holding part of the routing table is not membership.
    assert!(
        !zoe.communities().await.unwrap().iter().any(|c| c.id == art),
        "zoe should still not be in Art"
    );

    for node in [&zoe, &juan, &mia] {
        node.shutdown().await.ok();
    }
}
