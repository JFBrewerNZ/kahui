//! Store behaviour, exercised the way the network exercises it.
//!
//! These tests deliberately skip the network: two `Replica`s stand in for two
//! nodes, and handing a [`Delta`] from one to the other is exactly what the
//! sync protocol does over the wire. If convergence works here it works there,
//! and it can be tested in milliseconds without opening a socket.

use kahui_proto::{
    ChainTip, ChannelId, CommunityId, Event, Identity, Payload, SignedEvent, UserId,
};
use kahui_store::{Inserted, RedbStore, Store, StoreError};
use tempfile::TempDir;

/// One node's local state: an identity and its own private copy of history.
struct Replica {
    identity: Identity,
    store: RedbStore,
    _dir: TempDir,
}

impl Replica {
    fn new() -> Self {
        let dir = TempDir::new().expect("temp dir");
        let store = RedbStore::open(dir.path().join("kahui.redb")).expect("open store");
        Replica {
            identity: Identity::generate(),
            store,
            _dir: dir,
        }
    }

    fn me(&self) -> UserId {
        self.identity.user_id()
    }

    /// Reads back where this author's chain ended, exactly as a live node does
    /// before authoring.
    fn tip(&self, community: &CommunityId) -> ChainTip {
        let frontier = self.store.frontier(community).unwrap();
        let next_seq = frontier.next_seq(&self.me());
        let prev_self = if next_seq == 0 {
            None
        } else {
            let heads = self.store.heads(community).unwrap();
            let mine = heads.into_iter().find(|id| {
                self.store
                    .get_event(id)
                    .unwrap()
                    .is_some_and(|e| e.author() == self.me())
            });
            Some(mine.expect("own head must exist once seq > 0"))
        };
        ChainTip {
            next_seq,
            prev_self,
            next_lamport: self.store.max_lamport(community).unwrap() + 1,
            parents: self.store.heads(community).unwrap(),
        }
    }

    /// Authors an event and commits it locally, as a node does when the user
    /// types something.
    fn author(&self, community: CommunityId, payload: Payload) -> SignedEvent {
        let tip = self.tip(&community);
        let event = Event::create(&self.identity, community, &tip, 1_700_000_000_000, payload);
        assert_eq!(self.store.put_event(&event).unwrap(), Inserted::New);
        event
    }

    /// Founds a community with `#general`, the way `kahui create` does: three
    /// events, so the founder is a member like everyone else.
    fn found(&self, name: &str) -> (CommunityId, ChannelId) {
        let genesis = self.author(
            CommunityId::ZERO,
            Payload::CreateCommunity {
                name: name.into(),
                description: "test community".into(),
            },
        );
        let community = genesis.community_id();
        self.author(
            community,
            Payload::Join {
                display_name: "founder".into(),
            },
        );
        let channel = ChannelId::derive(&community, "general");
        self.author(
            community,
            Payload::CreateChannel {
                channel,
                name: "general".into(),
                topic: "Everything else".into(),
            },
        );
        (community, channel)
    }

    fn say(&self, community: CommunityId, channel: ChannelId, body: &str) -> SignedEvent {
        self.author(
            community,
            Payload::Message {
                channel,
                body: body.into(),
            },
        )
    }

    /// Applies everything `other` holds that this replica does not, the way a
    /// sync response is applied. Returns how many events landed.
    fn pull_from(&self, other: &Replica, community: &CommunityId) -> usize {
        let mut applied = 0;
        loop {
            let have = self.store.frontier(community).unwrap();
            let delta = other.store.delta(community, &have, 64).unwrap();
            if delta.is_empty() {
                break;
            }
            for event in &delta.events {
                if self.store.put_event(event).unwrap().is_new() {
                    applied += 1;
                }
            }
            if delta.complete {
                break;
            }
        }
        applied
    }

    fn bodies(&self, community: &CommunityId, channel: &ChannelId) -> Vec<String> {
        self.store
            .channel_history(community, channel, 100)
            .unwrap()
            .iter()
            .map(|e| match e.payload() {
                Payload::Message { body, .. } => body.clone(),
                other => panic!("channel history contained a {}", other.kind()),
            })
            .collect()
    }
}

#[test]
fn founding_a_community_creates_it_with_a_channel_and_a_member() {
    let alice = Replica::new();
    let (community, channel) = alice.found("Kahui");

    let summary = alice.store.community(&community).unwrap().unwrap();
    assert_eq!(summary.name, "Kahui");
    assert_eq!(summary.founder, alice.me());

    let channels = alice.store.channels(&community).unwrap();
    assert_eq!(channels.len(), 1);
    assert_eq!(channels[0].id, channel);
    assert_eq!(channels[0].name, "general");

    let members = alice.store.members(&community).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].id, alice.me());
    assert_eq!(members[0].display_name, "founder");
}

#[test]
fn channel_history_is_in_order_and_excludes_non_messages() {
    let alice = Replica::new();
    let (community, channel) = alice.found("Kahui");
    for body in ["one", "two", "three"] {
        alice.say(community, channel, body);
    }
    assert_eq!(alice.bodies(&community, &channel), ["one", "two", "three"]);
}

#[test]
fn history_limit_returns_the_most_recent_messages_oldest_first() {
    let alice = Replica::new();
    let (community, channel) = alice.found("Kahui");
    for i in 0..10 {
        alice.say(community, channel, &format!("m{i}"));
    }
    let recent = alice
        .store
        .channel_history(&community, &channel, 3)
        .unwrap();
    let bodies: Vec<_> = recent
        .iter()
        .map(|e| match e.payload() {
            Payload::Message { body, .. } => body.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(bodies, ["m7", "m8", "m9"]);
}

#[test]
fn re_inserting_an_event_is_a_quiet_no_op() {
    let alice = Replica::new();
    let (community, channel) = alice.found("Kahui");
    let event = alice.say(community, channel, "hello");
    assert_eq!(
        alice.store.put_event(&event).unwrap(),
        Inserted::Duplicate,
        "gossip and sync both deliver events, so duplicates must be harmless"
    );
}

#[test]
fn a_new_node_can_replay_a_community_from_nothing() {
    let alice = Replica::new();
    let bob = Replica::new();
    let (community, channel) = alice.found("Kahui");
    alice.say(community, channel, "kia ora");

    let applied = bob.pull_from(&alice, &community);
    assert_eq!(applied, 4, "genesis, join, channel, message");
    assert_eq!(
        bob.store.frontier(&community).unwrap(),
        alice.store.frontier(&community).unwrap()
    );
    assert_eq!(bob.bodies(&community, &channel), ["kia ora"]);
    assert_eq!(
        bob.store.community(&community).unwrap().unwrap().name,
        "Kahui"
    );
}

#[test]
fn sync_sends_only_what_the_peer_is_missing() {
    let alice = Replica::new();
    let bob = Replica::new();
    let (community, channel) = alice.found("Kahui");
    bob.pull_from(&alice, &community);

    // Bob is caught up: there is nothing left to send him.
    let have = bob.store.frontier(&community).unwrap();
    assert!(alice.store.delta(&community, &have, 64).unwrap().is_empty());

    // Alice says two more things; only those two come back.
    alice.say(community, channel, "one");
    alice.say(community, channel, "two");
    let delta = alice.store.delta(&community, &have, 64).unwrap();
    assert_eq!(delta.len(), 2);
    assert!(delta.complete);
}

#[test]
fn a_truncated_delta_is_still_applicable_and_reports_itself_incomplete() {
    let alice = Replica::new();
    let bob = Replica::new();
    let (community, channel) = alice.found("Kahui");
    for i in 0..20 {
        alice.say(community, channel, &format!("m{i}"));
    }

    let delta = alice
        .store
        .delta(&community, &bob.store.frontier(&community).unwrap(), 5)
        .unwrap();
    assert_eq!(delta.len(), 5);
    assert!(!delta.complete, "there is more history than the limit");

    // Causal order means a partial batch applies cleanly rather than erroring
    // on a missing predecessor.
    for event in &delta.events {
        bob.store.put_event(event).unwrap();
    }

    // Repeated rounds converge.
    bob.pull_from(&alice, &community);
    assert_eq!(
        bob.store.frontier(&community).unwrap(),
        alice.store.frontier(&community).unwrap()
    );
}

#[test]
fn three_replicas_converge_on_the_same_transcript() {
    let alice = Replica::new();
    let bob = Replica::new();
    let carol = Replica::new();
    let (community, channel) = alice.found("Kahui");

    for (replica, name) in [(&bob, "bob"), (&carol, "carol")] {
        replica.pull_from(&alice, &community);
        replica.author(
            community,
            Payload::Join {
                display_name: name.into(),
            },
        );
    }

    // Interleave, pulling between turns so the Lamport clocks advance the way
    // they would on a live network.
    alice.say(community, channel, "a1");
    bob.pull_from(&alice, &community);
    bob.say(community, channel, "b1");
    carol.pull_from(&bob, &community);
    carol.say(community, channel, "c1");
    alice.pull_from(&carol, &community);
    alice.say(community, channel, "a2");

    // Gossip everything everywhere.
    for _ in 0..3 {
        alice.pull_from(&bob, &community);
        alice.pull_from(&carol, &community);
        bob.pull_from(&alice, &community);
        carol.pull_from(&alice, &community);
    }

    let transcripts: Vec<Vec<String>> = [&alice, &bob, &carol]
        .iter()
        .map(|r| r.bodies(&community, &channel))
        .collect();
    assert_eq!(transcripts[0].len(), 4);
    assert_eq!(
        transcripts[0], transcripts[1],
        "every node must render the same order"
    );
    assert_eq!(transcripts[1], transcripts[2]);

    for replica in [&alice, &bob, &carol] {
        assert_eq!(replica.store.members(&community).unwrap().len(), 3);
    }
}

#[test]
fn an_event_with_a_missing_predecessor_asks_for_a_sync_instead_of_landing() {
    let alice = Replica::new();
    let bob = Replica::new();
    let (community, channel) = alice.found("Kahui");
    bob.pull_from(&alice, &community);

    alice.say(community, channel, "skipped");
    let later = alice.say(community, channel, "arrives first");

    let err = bob.store.put_event(&later).unwrap_err();
    assert!(
        matches!(err, StoreError::MissingPrev { .. }),
        "expected a gap, got {err}"
    );
    assert!(
        err.needs_sync(),
        "a gap means we are behind, not that the event is bad"
    );

    // Filling the gap makes the held-back event applicable.
    bob.pull_from(&alice, &community);
    assert_eq!(
        bob.bodies(&community, &channel),
        ["skipped", "arrives first"]
    );
}

#[test]
fn events_for_an_unknown_community_are_refused_until_its_genesis_arrives() {
    let alice = Replica::new();
    let bob = Replica::new();
    let (community, channel) = alice.found("Kahui");
    let message = alice.say(community, channel, "hello stranger");

    let err = bob.store.put_event(&message).unwrap_err();
    assert!(matches!(err, StoreError::UnknownCommunity(_)), "got {err}");
    assert!(err.needs_sync());

    bob.pull_from(&alice, &community);
    assert_eq!(bob.bodies(&community, &channel), ["hello stranger"]);
}

#[test]
fn a_forged_signature_is_refused() {
    let alice = Replica::new();
    let bob = Replica::new();
    let (community, channel) = alice.found("Kahui");
    bob.pull_from(&alice, &community);

    let mut forged = alice.say(community, channel, "genuine");
    forged.event.payload = Payload::Message {
        channel,
        body: "tampered".into(),
    };

    let err = bob.store.put_event(&forged).unwrap_err();
    assert!(matches!(err, StoreError::Invalid(_)), "got {err}");
    assert!(
        !err.needs_sync(),
        "a bad signature is the sender's problem, not a gap in our history"
    );
}

#[test]
fn signing_two_events_at_the_same_sequence_is_caught() {
    let alice = Replica::new();
    let bob = Replica::new();
    let (community, channel) = alice.found("Kahui");
    bob.pull_from(&alice, &community);

    // Alice authors two different events from the same tip: an equivocation, of
    // the sort a node forked across two machines would produce.
    let tip = alice.tip(&community);
    let first = Event::create(
        &alice.identity,
        community,
        &tip,
        1,
        Payload::Message {
            channel,
            body: "one story".into(),
        },
    );
    let second = Event::create(
        &alice.identity,
        community,
        &tip,
        2,
        Payload::Message {
            channel,
            body: "another story".into(),
        },
    );

    assert!(bob.store.put_event(&first).unwrap().is_new());
    let err = bob.store.put_event(&second).unwrap_err();
    assert!(matches!(err, StoreError::Equivocation { .. }), "got {err}");
    assert!(!err.needs_sync());
}

#[test]
fn display_name_changes_converge_on_the_causally_latest() {
    let alice = Replica::new();
    let bob = Replica::new();
    let (community, _) = alice.found("Kahui");
    bob.pull_from(&alice, &community);

    alice.author(
        community,
        Payload::SetDisplayName {
            display_name: "Alice the Founder".into(),
        },
    );
    bob.pull_from(&alice, &community);

    for replica in [&alice, &bob] {
        let members = replica.store.members(&community).unwrap();
        let me = members.iter().find(|m| m.id == alice.me()).unwrap();
        assert_eq!(me.display_name, "Alice the Founder");
    }
}

#[test]
fn state_survives_reopening_the_database() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("kahui.redb");
    let identity = Identity::generate();

    let (community, channel) = {
        let store = RedbStore::open(&path).unwrap();
        let genesis = Event::create(
            &identity,
            CommunityId::ZERO,
            &ChainTip::genesis(),
            10,
            Payload::CreateCommunity {
                name: "Persisted".into(),
                description: String::new(),
            },
        );
        store.put_event(&genesis).unwrap();
        let community = genesis.community_id();
        let channel = ChannelId::derive(&community, "general");
        let create = Event::create(
            &identity,
            community,
            &ChainTip {
                next_seq: 1,
                prev_self: Some(genesis.id()),
                next_lamport: 2,
                parents: vec![genesis.id()],
            },
            11,
            Payload::CreateChannel {
                channel,
                name: "general".into(),
                topic: String::new(),
            },
        );
        store.put_event(&create).unwrap();
        store.meta_put("display_name", b"alice").unwrap();
        (community, channel)
    };

    let reopened = RedbStore::open(&path).unwrap();
    assert_eq!(
        reopened.community(&community).unwrap().unwrap().name,
        "Persisted"
    );
    assert_eq!(reopened.channels(&community).unwrap()[0].id, channel);
    assert_eq!(
        reopened.meta_get("display_name").unwrap().as_deref(),
        Some(b"alice".as_slice())
    );
    assert_eq!(reopened.frontier(&community).unwrap().total_events(), 2);
}

#[test]
fn peer_addresses_are_remembered_and_merged() {
    use kahui_store::PeerRecord;

    let alice = Replica::new();
    let (community, _) = alice.found("Kahui");

    alice
        .store
        .remember_peer(
            &community,
            &PeerRecord {
                peer_id: vec![1, 2, 3],
                addrs: vec!["/ip4/10.0.0.1/tcp/4001".into()],
                last_seen_ms: 100,
            },
        )
        .unwrap();
    alice
        .store
        .remember_peer(
            &community,
            &PeerRecord {
                peer_id: vec![1, 2, 3],
                addrs: vec!["/ip4/10.0.0.2/tcp/4001".into()],
                last_seen_ms: 200,
            },
        )
        .unwrap();

    let peers = alice.store.peers(&community).unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].last_seen_ms, 200);
    assert_eq!(
        peers[0].addrs,
        ["/ip4/10.0.0.2/tcp/4001", "/ip4/10.0.0.1/tcp/4001"],
        "newest first, older kept as a fallback"
    );
}
