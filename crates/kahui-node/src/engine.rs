//! The engine task: one loop that owns the swarm and the store.
//!
//! Everything that can change a node's state funnels through here — commands
//! from the UI, gossip from peers, sync responses, timers — which means state
//! needs no locks and events are applied in a single, well-defined order.
//!
//! ## The three mechanisms that keep a community alive
//!
//! **Gossip** delivers new events to whoever is online. **Sync** repairs what
//! gossip missed, by frontier exchange. **Presence** tells members where to
//! find each other, so the graph fills in and stops depending on whoever issued
//! the invite. The last one is what makes a founder's node ordinary: by the
//! time it disconnects, everyone else already has direct paths to everyone
//! else.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use futures::StreamExt;
use kahui_net::behaviour::Behaviour;
use kahui_net::{
    peer_id_of, BehaviourEvent, GossipMessage, Invite, InvitePeer, PortMapUpdate, Presence,
    SyncRequest, SyncResponse, MAX_SYNC_BATCH,
};
use kahui_proto::{ChannelId, CommunityId, Event, EventId, Identity, Payload, SignedEvent, UserId};
use kahui_store::{Inserted, PeerRecord, Store};
use libp2p::autonat::{self, NatStatus};
use libp2p::kad;
use libp2p::multiaddr::Protocol;
use libp2p::request_response::OutboundRequestId;
use libp2p::swarm::SwarmEvent;
use libp2p::{
    dcutr, gossipsub, identify, mdns, relay, request_response, upnp, Multiaddr, PeerId, Swarm,
};
use tokio::sync::{broadcast, mpsc};
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use crate::api::{Command, CommunityStatus, Message, NodeEvent, Reachability, Status};
use crate::{chain, now_ms, NodeConfig, NodeError};

/// How many events we hold back waiting for a predecessor before we start
/// dropping them. Anything dropped is recovered by the next sync round, so this
/// is a memory bound rather than a correctness one.
const MAX_ORPHANS: usize = 2048;

/// Peers asked for history in one anti-entropy round. Asking everyone every
/// time is wasteful; any one peer with the events is enough.
const SYNC_FANOUT: usize = 4;

/// Relays to hold a reservation with at once.
///
/// One is enough to be reachable. A second is insurance against that member
/// closing their laptop, which is a thing members do.
const RELAY_TARGET: usize = 2;

/// Remembered relays to reconnect to at startup.
const RELAY_MEMORY: usize = 8;

/// How many unanswered hash table lookups to keep track of before giving up on
/// the bookkeeping. Far above anything a real node reaches.
const MAX_DHT_QUERIES: usize = 256;

/// Members named in an invite, beyond ourselves.
///
/// An invite that names several members keeps working after any one of them
/// goes offline — including the node that issued it.
const INVITE_PEERS: usize = 3;

/// Which loop input woke us.
///
/// The select produces one of these and ends, so the handlers below can take
/// `&mut self` freely instead of fighting borrows held by the other branches.
enum Tick {
    Command(Command),
    Swarm(Box<SwarmEvent<BehaviourEvent>>),
    Presence,
    AntiEntropy,
    PortMap(PortMapUpdate),
    PortMapStopped,
    Closed,
}

/// A question asked of the distributed hash table.
#[derive(Clone, Copy, Debug)]
enum DhtQuery {
    /// "Who out there can be dialled, and is willing to carry for somebody?"
    Relays,
    /// "Who holds this community?" — how an invite with no addresses in it
    /// still finds its members.
    Community(CommunityId),
    /// Filling in the routing table on startup. Nothing to do with the answer;
    /// the point is the traffic it generates.
    Warmup,
}

/// Events waiting on a predecessor that has not arrived yet.
///
/// Gossip has no ordering guarantee, so a message can easily overtake the one
/// before it. Rather than reject and re-fetch, we hold it for the moment and
/// let it fall into place — the common case costs nothing, and sync remains the
/// backstop for everything else.
#[derive(Default)]
struct OrphanBuffer {
    waiting: HashMap<EventId, Vec<SignedEvent>>,
    /// Insertion order, for evicting the oldest when full.
    arrivals: VecDeque<(EventId, EventId)>,
    held: HashSet<EventId>,
}

impl OrphanBuffer {
    /// The event this one cannot be applied without: the author's previous
    /// event, or the community's genesis if we do not even have that.
    fn dependency(event: &SignedEvent) -> EventId {
        event
            .event
            .prev_self
            .unwrap_or_else(|| EventId::from_bytes(*event.community_id().as_bytes()))
    }

    fn park(&mut self, event: SignedEvent) {
        let id = event.id();
        if !self.held.insert(id) {
            return;
        }
        let dep = Self::dependency(&event);
        self.waiting.entry(dep).or_default().push(event);
        self.arrivals.push_back((dep, id));
        while self.arrivals.len() > MAX_ORPHANS {
            if let Some((dep, id)) = self.arrivals.pop_front() {
                if let Some(bucket) = self.waiting.get_mut(&dep) {
                    bucket.retain(|e| e.id() != id);
                    if bucket.is_empty() {
                        self.waiting.remove(&dep);
                    }
                }
                self.held.remove(&id);
            }
        }
    }

    /// Removes and returns everything that was waiting on `id`.
    fn take(&mut self, id: EventId) -> Vec<SignedEvent> {
        let events = self.waiting.remove(&id).unwrap_or_default();
        for event in &events {
            self.held.remove(&event.id());
        }
        self.arrivals.retain(|(dep, _)| *dep != id);
        events
    }

    fn len(&self) -> usize {
        self.held.len()
    }
}

pub(crate) struct Engine {
    identity: Identity,
    me: UserId,
    display_name: String,
    store: Arc<dyn Store>,
    swarm: Swarm<Behaviour>,
    commands: mpsc::Receiver<Command>,
    events: broadcast::Sender<NodeEvent>,
    config: NodeConfig,

    /// Communities we participate in: subscribed, synced and announced.
    joined: HashSet<CommunityId>,
    /// Callers blocked on a join, waiting for enough history to verify the
    /// community before we announce membership.
    pending_joins: HashMap<CommunityId, Vec<crate::api::Reply<CommunityId>>>,
    orphans: OrphanBuffer,
    /// Sync requests we are waiting on, so responses can be matched to the
    /// community they were about.
    inflight: HashMap<OutboundRequestId, (PeerId, CommunityId)>,
    listen_addrs: Vec<Multiaddr>,

    /// Whether anybody can dial us, as far as our peers can tell.
    reachability: Reachability,
    /// Peers that speak the relay protocol, and addresses we could dial them
    /// on. Learned from `identify`, so no announcement is needed.
    relay_candidates: HashMap<PeerId, Vec<Multiaddr>>,
    /// Circuit listeners we have opened, by the member carrying them. Kept so a
    /// reservation can be handed back once we no longer need it.
    relay_listeners: HashMap<PeerId, libp2p::core::transport::ListenerId>,
    /// Members who have accepted and are actually carrying for us.
    relayed_by: HashSet<PeerId>,

    /// Reports from the task asking the router to open a port, if it is running.
    ///
    /// Started once we know which port we actually listen on, because that is
    /// the number the router has to be told about.
    port_map: Option<mpsc::Receiver<PortMapUpdate>>,

    /// What each outstanding DHT lookup was for.
    ///
    /// Kademlia answers asynchronously and out of order, so the question has to
    /// be remembered alongside the query id to make sense of the answer.
    dht_queries: HashMap<kad::QueryId, DhtQuery>,

    /// Whether what this node holds has been published to the hash table.
    ///
    /// The first attempt happens when a community is created or joined, which is
    /// routinely before this node knows a single peer — and a publication with
    /// nobody to publish to goes nowhere. So it is done again the moment the
    /// routing table has anybody in it.
    dht_announced: bool,

    /// Whether we have told the DHT we can carry for other people.
    ///
    /// Tracked so that repeatedly confirming the same reachability does not
    /// republish, and so the claim can be withdrawn if it stops being true.
    offering_relay: bool,

    /// A port the router opened but would not give us an address for.
    ///
    /// Held until a peer tells us how we look from outside, at which point the
    /// two halves make a dialable address. See [`Self::claim_mapped_port`].
    mapped_port: Option<u16>,
    /// Members whose traffic we are carrying. Reachable nodes pay it forward.
    relaying_for: HashSet<PeerId>,

    /// Held until the loop has ended and the database has been closed, so that
    /// a caller awaiting shutdown knows the data directory is free.
    shutdown_reply: Option<crate::api::Reply<()>>,
}

impl Engine {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        identity: Identity,
        display_name: String,
        store: Arc<dyn Store>,
        swarm: Swarm<Behaviour>,
        commands: mpsc::Receiver<Command>,
        events: broadcast::Sender<NodeEvent>,
        config: NodeConfig,
    ) -> Self {
        let me = identity.user_id();
        Engine {
            identity,
            me,
            display_name,
            store,
            swarm,
            commands,
            events,
            config,
            joined: HashSet::new(),
            pending_joins: HashMap::new(),
            orphans: OrphanBuffer::default(),
            inflight: HashMap::new(),
            listen_addrs: Vec::new(),
            reachability: Reachability::Unknown,
            relay_candidates: HashMap::new(),
            relay_listeners: HashMap::new(),
            relayed_by: HashSet::new(),
            relaying_for: HashSet::new(),
            port_map: None,
            dht_queries: HashMap::new(),
            dht_announced: false,
            offering_relay: false,
            mapped_port: None,
            shutdown_reply: None,
        }
    }

    pub(crate) async fn run(mut self) {
        self.bootstrap();

        let mut presence = tokio::time::interval(self.config.presence_interval);
        let mut anti_entropy = tokio::time::interval(self.config.sync_interval);
        presence.set_missed_tick_behavior(MissedTickBehavior::Delay);
        anti_entropy.set_missed_tick_behavior(MissedTickBehavior::Delay);

        loop {
            // Each branch borrows a different field, and the select yields an
            // owned `Tick`, so by the time a handler runs nothing else is
            // borrowed from `self`.
            let tick = tokio::select! {
                command = self.commands.recv() => match command {
                    Some(command) => Tick::Command(command),
                    None => Tick::Closed,
                },
                event = self.swarm.select_next_some() => Tick::Swarm(Box::new(event)),
                _ = presence.tick() => Tick::Presence,
                _ = anti_entropy.tick() => Tick::AntiEntropy,
                // Only ever ready once the mapper is running. `pending()` keeps
                // this branch inert rather than making the whole loop optional.
                update = async {
                    match self.port_map.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => match update {
                    Some(update) => Tick::PortMap(update),
                    None => Tick::PortMapStopped,
                },
            };

            match tick {
                Tick::Command(command) => {
                    if self.handle_command(command) {
                        break;
                    }
                }
                Tick::Swarm(event) => self.handle_swarm(*event),
                Tick::PortMap(update) => self.handle_port_map(update),
                Tick::PortMapStopped => self.port_map = None,
                Tick::Presence => {
                    self.announce_presence();
                    self.redial_known_peers();
                    self.dht_upkeep();
                    if self.relayed_by.is_empty() {
                        self.dial_known_relays();
                    }
                    // A member who could carry for us may have just arrived, or
                    // the one who was carrying may have gone.
                    self.seek_relay();
                }
                Tick::AntiEntropy => self.anti_entropy(),
                Tick::Closed => break,
            }
        }

        // Tear down in order, and only then answer the shutdown request. A
        // caller that awaited shutdown can immediately reopen the data
        // directory -- which is exactly what restarting a node does.
        let Engine {
            swarm,
            store,
            events,
            shutdown_reply,
            ..
        } = self;
        let _ = events.send(NodeEvent::Stopped);
        drop(swarm);
        drop(store);
        if let Some(reply) = shutdown_reply {
            let _ = reply.send(Ok(()));
        }
    }

    // -- startup ----------------------------------------------------------

    /// Rejoins everything this node was part of before it was last shut down.
    ///
    /// A node comes back knowing its communities and its peers' last known
    /// addresses, so it can reconnect and catch up entirely on its own. Nothing
    /// external has to remember anything on its behalf.
    fn bootstrap(&mut self) {
        // If the operator has told us what our situation is, believe them and
        // act on it now rather than waiting for probes to come back.
        if let Some(reachability) = self.config.reachability {
            self.reachability = reachability;
            self.apply_reachability();
            self.announce_reachability();
        }

        // Reachable nodes we have met before, whatever community that was in.
        // This is what lets somebody behind a router create a community and
        // hand out an invite that works: they are already relayed before the
        // community exists.
        self.dial_known_relays();

        // And everybody else, through the hash table.
        self.dht_bootstrap();

        let communities = match self.store.communities() {
            Ok(communities) => communities,
            Err(err) => {
                self.warn(format!("could not read local communities: {err}"));
                return;
            }
        };

        for community in communities {
            self.joined.insert(community.id);
            if let Err(err) = self.swarm.behaviour_mut().subscribe(&community.id) {
                self.warn(format!("could not subscribe to {}: {err}", community.name));
            }
            self.dht_provide(&community.id);
            self.dial_stored_peers(&community.id);
        }

        for addr in self.config.bootstrap.clone() {
            match addr.parse::<Multiaddr>() {
                Ok(addr) => {
                    let _ = self.swarm.dial(addr);
                }
                Err(err) => self.warn(format!("bootstrap address {addr}: {err}")),
            }
        }
    }

    // -- the distributed hash table -----------------------------------------
    //
    // The point of all of this is that a node needs no configuration and no
    // address from a person. It finds somebody reachable, gets carried by them,
    // and is then reachable itself. See `kahui_net::discovery`.

    /// Puts everything we know into the routing table and starts a lookup.
    ///
    /// Kademlia cannot answer a question until it knows somebody to ask, so
    /// every address this node has any claim to goes in first: peers from past
    /// runs, members of communities we hold, the seed file, and anything given
    /// on the command line. Which of them answers does not matter — one is
    /// enough, and after that the table fills itself.
    fn dht_bootstrap(&mut self) {
        let mut known = 0;

        for record in self.store.relays().unwrap_or_default() {
            known += self.dht_add_record(&record);
        }
        for community in self.store.communities().unwrap_or_default() {
            for record in self.store.peers(&community.id).unwrap_or_default() {
                known += self.dht_add_record(&record);
            }
        }

        // Seeds carry no peer id, so they cannot go straight into the routing
        // table — dialling them is what teaches us who they are, and identify
        // does the rest.
        let seeds = kahui_net::load_seeds(&self.config.data_dir);
        let seeds = seeds.into_iter().chain(kahui_net::compiled_seeds());
        for addr in seeds {
            if self.swarm.dial(addr.clone()).is_ok() {
                known += 1;
                debug!(%addr, "dialling a seed");
            }
        }

        if known == 0 {
            // Not fatal, and not even unusual on a first run. mDNS may find
            // somebody, an invite may arrive, or a peer may dial us.
            debug!("nobody to ask yet; waiting to be found");
            return;
        }

        self.dht_ask(DhtQuery::Warmup, |kad| Some(kad.bootstrap().ok()?));
    }

    /// Teaches the routing table about one remembered peer.
    fn dht_add_record(&mut self, record: &PeerRecord) -> usize {
        let Ok(peer) = PeerId::from_bytes(&record.peer_id) else {
            return 0;
        };
        if peer == *self.swarm.local_peer_id() {
            return 0;
        }

        let mut added = 0;
        for addr in &record.addrs {
            if let Ok(addr) = addr.parse::<Multiaddr>() {
                if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
                    kad.add_address(&peer, addr);
                    added = 1;
                }
            }
        }
        added
    }

    /// Runs a lookup and files what it was for.
    fn dht_ask<F>(&mut self, what: DhtQuery, ask: F)
    where
        F: FnOnce(&mut kad::Behaviour<kad::store::MemoryStore>) -> Option<kad::QueryId>,
    {
        let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() else {
            return;
        };
        if let Some(id) = ask(kad) {
            self.dht_queries.insert(id, what);
        }
    }

    /// Says, to anybody who asks the table, that this node holds a community.
    ///
    /// Every member does this, whether or not it can be dialled directly: a
    /// node being carried by a relay publishes its circuit address, so it is
    /// found and reached through that. This is what makes an invite able to
    /// contain a community id and nothing else.
    fn dht_provide(&mut self, community: &CommunityId) {
        let key = kahui_net::community_key(community);
        if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
            if let Err(err) = kad.start_providing(key) {
                debug!(%err, "could not announce a community to the hash table");
            }
        }
    }

    /// Asks the table who holds a community.
    fn dht_find(&mut self, community: CommunityId) {
        let key = kahui_net::community_key(&community);
        debug!(community = %community.short(), "asking the network who has this");
        self.dht_ask(DhtQuery::Community(community), |kad| {
            Some(kad.get_providers(key))
        });
    }

    /// Asks the table who can carry for us.
    fn dht_find_relays(&mut self) {
        debug!("asking the network who can carry for us");
        self.dht_ask(DhtQuery::Relays, |kad| {
            Some(kad.get_providers(kahui_net::relay_key()))
        });
    }

    /// Offers to carry for other people, and joins the routing table properly.
    ///
    /// Only a node that can actually be dialled does this. A node that claims a
    /// place in the table and then cannot be reached is worse than absent: every
    /// query routed through it stalls.
    fn dht_offer_relay(&mut self) {
        if self.offering_relay {
            return;
        }
        self.offering_relay = true;

        if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
            kad.set_mode(Some(kad::Mode::Server));
            if let Err(err) = kad.start_providing(kahui_net::relay_key()) {
                debug!(%err, "could not offer to carry for others");
                self.offering_relay = false;
                return;
            }
        }
        info!("reachable, so now carrying part of the network for everybody else");
    }

    /// Stops claiming to be reachable, because we are not any more.
    fn dht_withdraw_relay(&mut self) {
        if !self.offering_relay {
            return;
        }
        self.offering_relay = false;

        if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
            kad.stop_providing(&kahui_net::relay_key());
            kad.set_mode(Some(kad::Mode::Client));
        }
        debug!("no longer reachable, so no longer offering to carry");
    }

    /// Acts on an answer from the table.
    fn handle_kad(&mut self, event: kad::Event) {
        match event {
            // Somebody dialable turned up. Worth remembering: this is how a node
            // that has met nobody today still starts tomorrow with somewhere to
            // go.
            kad::Event::RoutingUpdated {
                peer, addresses, ..
            } => {
                let addrs: Vec<Multiaddr> = addresses.into_vec();
                debug!(%peer, count = addrs.len(), "the routing table learned somebody");
                self.note_relay_candidate(peer, addrs);
                // There is now somewhere for a publication to go.
                self.dht_announce_everything();
            }

            kad::Event::OutboundQueryProgressed {
                id, result, step, ..
            } => {
                self.handle_kad_result(id, result);
                // Kademlia reports progress repeatedly and then finishes. Only
                // the last word means the question is answered for good.
                if step.last {
                    self.dht_queries.remove(&id);
                }
            }

            kad::Event::ModeChanged { new_mode } => {
                debug!(%new_mode, "hash table mode changed");
            }

            _ => {}
        }
    }

    /// Acts on the outcome of a lookup we started.
    fn handle_kad_result(&mut self, id: kad::QueryId, result: kad::QueryResult) {
        let providers = match result {
            kad::QueryResult::GetProviders(Ok(kad::GetProvidersOk::FoundProviders {
                providers,
                ..
            })) => providers,

            kad::QueryResult::GetProviders(Ok(
                kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. },
            )) => {
                // Whether this found anything was decided by the FoundProviders
                // events that came before it.
                return;
            }

            kad::QueryResult::StartProviding(Err(err)) => {
                debug!(%err, "could not publish to the hash table");
                return;
            }

            kad::QueryResult::Bootstrap(Err(_)) => {
                // Nobody answered, which on a first run is ordinary.
                return;
            }

            _ => return,
        };

        let Some(what) = self.dht_queries.get(&id).copied() else {
            return;
        };

        for peer in providers {
            if peer == *self.swarm.local_peer_id() {
                continue;
            }
            match what {
                DhtQuery::Relays => {
                    debug!(%peer, "the network offered somebody who can carry");
                    // Dial it: a reservation needs a live connection, and
                    // identify will confirm it really speaks the relay protocol.
                    let _ = self.swarm.dial(peer);
                }
                DhtQuery::Community(community) => {
                    debug!(%peer, community = %community.short(), "found a member");
                    let _ = self.swarm.dial(peer);
                }
                DhtQuery::Warmup => {}
            }
        }
    }

    /// How many nodes are in the routing table.
    ///
    /// The honest measure of whether this node is on the network at all, as
    /// distinct from being connected to a handful of peers it was told about.
    fn dht_peer_count(&mut self) -> usize {
        self.swarm
            .behaviour_mut()
            .kad
            .as_mut()
            .map(|kad| kad.kbuckets().map(|bucket| bucket.num_entries()).sum())
            .unwrap_or(0)
    }

    /// Publishes everything this node holds, now that somebody is listening.
    ///
    /// Cheap and idempotent, but pointless to repeat: Kademlia republishes on
    /// its own schedule once the first announcement has landed.
    fn dht_announce_everything(&mut self) {
        if self.dht_announced || !self.swarm.behaviour().kad.is_enabled() {
            return;
        }
        self.dht_announced = true;

        for community in self.joined.iter().copied().collect::<Vec<_>>() {
            self.dht_provide(&community);
        }
        if self.reachability == Reachability::Direct {
            // Re-offer: the first attempt at start-up had no audience either.
            self.offering_relay = false;
            self.dht_offer_relay();
        }
    }

    /// Asks again for anything still outstanding.
    ///
    /// A lookup made before the routing table had anybody in it comes back
    /// empty, and on a cold start that is the normal case: the node has just
    /// dialled its first peer and does not know anybody yet. Repeating the
    /// question once the table has filled is the difference between joining by
    /// community id reliably and joining by it only when the timing is lucky.
    fn dht_upkeep(&mut self) {
        if self.swarm.behaviour().kad.is_enabled() {
            let waiting: Vec<CommunityId> = self.pending_joins.keys().copied().collect();
            for community in waiting {
                self.dht_find(community);
            }

            if self.reachability != Reachability::Direct && self.relay_listeners.is_empty() {
                self.dht_find_relays();
            }

            // A node that started alone and has since met somebody still needs
            // to say what it holds.
            self.dht_announce_everything();
        }

        // Bound what an unanswered question can cost us. Queries time out inside
        // Kademlia, so a lingering entry here is bookkeeping rather than a leak,
        // but it should still not grow without limit on a long-running node.
        if self.dht_queries.len() > MAX_DHT_QUERIES {
            self.dht_queries.clear();
        }
    }

    /// Records somebody who might be able to carry for us, and acts on it.
    ///
    /// Two quite different sources end up here — a peer whose `identify` says it
    /// speaks the relay protocol, and a peer the hash table returned for the
    /// relay key — because what we do about them is identical.
    fn note_relay_candidate(&mut self, peer: PeerId, addrs: Vec<Multiaddr>) {
        if peer == *self.swarm.local_peer_id() || addrs.is_empty() {
            return;
        }
        self.relay_candidates.insert(peer, addrs);
        self.seek_relay();
    }

    /// Files away a node that has carried for us, so the next run can go
    /// straight back to it.
    fn remember_relay(&mut self, peer: PeerId) {
        let addrs: Vec<String> = self
            .relay_candidates
            .get(&peer)
            .map(|addrs| addrs.iter().map(|addr| addr.to_string()).collect())
            .unwrap_or_default();
        if addrs.is_empty() {
            return;
        }
        let record = PeerRecord {
            peer_id: peer.to_bytes(),
            addrs,
            last_seen_ms: now_ms(),
        };
        if let Err(err) = self.store.remember_relay(&record) {
            debug!(%err, "could not remember a relay");
        }
    }

    /// Reconnects to relays remembered from previous runs.
    ///
    /// Kept alongside the hash table rather than replaced by it: a node that
    /// has been here before should not have to look anything up.
    fn dial_known_relays(&mut self) {
        let relays = match self.store.relays() {
            Ok(relays) => relays,
            Err(err) => {
                debug!(%err, "could not read remembered relays");
                return;
            }
        };
        for record in relays.into_iter().take(RELAY_MEMORY) {
            let Ok(peer) = PeerId::from_bytes(&record.peer_id) else {
                continue;
            };
            if peer == *self.swarm.local_peer_id() || self.swarm.is_connected(&peer) {
                continue;
            }
            for addr in &record.addrs {
                if let Ok(addr) = addr.parse::<Multiaddr>() {
                    self.swarm.add_peer_address(peer, addr);
                }
            }
            let _ = self.swarm.dial(peer);
        }
    }

    fn dial_stored_peers(&mut self, community: &CommunityId) {
        let peers = self.store.peers(community).unwrap_or_default();
        for record in peers {
            let Ok(peer) = PeerId::from_bytes(&record.peer_id) else {
                continue;
            };
            if peer == *self.swarm.local_peer_id() || self.swarm.is_connected(&peer) {
                continue;
            }
            for addr in &record.addrs {
                if let Ok(addr) = addr.parse::<Multiaddr>() {
                    self.swarm.add_peer_address(peer, addr);
                }
            }
            let _ = self.swarm.dial(peer);
        }
    }

    // -- periodic work ----------------------------------------------------

    /// Tells every community where to find us.
    fn announce_presence(&mut self) {
        for community in self.joined.clone() {
            self.announce_presence_to(community);
        }
    }

    /// Tells one community where to find us.
    fn announce_presence_to(&mut self, community: CommunityId) {
        let addrs = self.public_addrs();
        if addrs.is_empty() {
            return;
        }
        let event_count = self
            .store
            .frontier(&community)
            .map(|f| f.total_events())
            .unwrap_or(0);
        let message = GossipMessage::Presence(Presence {
            user: self.me,
            addrs,
            event_count,
            announced_at_ms: now_ms(),
        });
        let _ = self.swarm.behaviour_mut().publish(&community, &message);
    }

    fn redial_known_peers(&mut self) {
        for community in self.joined.clone() {
            self.dial_stored_peers(&community);
        }
    }

    /// Asks a handful of connected peers whether we are missing anything.
    ///
    /// Gossip covers the live case and this covers everything else: a node that
    /// was asleep, a dropped packet, a partition that healed. It is also what
    /// makes a returning node catch up without anyone doing anything special.
    fn anti_entropy(&mut self) {
        let peers: Vec<PeerId> = self.swarm.connected_peers().copied().collect();
        for community in self.joined.clone() {
            for peer in peers.iter().take(SYNC_FANOUT) {
                self.request_sync(*peer, community);
            }
        }
    }

    // -- swarm ------------------------------------------------------------

    fn handle_swarm(&mut self, event: SwarmEvent<BehaviourEvent>) {
        match event {
            SwarmEvent::NewListenAddr { address, .. } => {
                if !self.listen_addrs.contains(&address) {
                    self.listen_addrs.push(address.clone());
                }

                // A relay reservation arrives as a listen address. Announcing it
                // as an external one is what puts it in front of the hash table,
                // and so what lets somebody behind a router be found and dialled
                // by a stranger.
                if address.iter().any(|part| part == Protocol::P2pCircuit) {
                    self.swarm.add_external_address(address.clone());
                }

                self.start_port_mapping(&address);
                self.emit(NodeEvent::Listening {
                    addr: address.to_string(),
                });
            }

            SwarmEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established,
                ..
            } => {
                self.remember_peer(peer_id, &[endpoint.get_remote_address().clone()]);
                // A peer reachable over both TCP and QUIC establishes several
                // connections. Only the first means "this peer is now here";
                // the rest are the same peer arriving again by another road.
                if num_established.get() == 1 {
                    self.emit(NodeEvent::PeerConnected {
                        peer: peer_id.to_string(),
                    });
                    // Catch up immediately rather than waiting for the timer:
                    // this is the path a returning node takes.
                    for community in self.joined.clone() {
                        self.request_sync(peer_id, community);
                    }
                }
            }

            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established: 0,
                ..
            } => {
                self.emit(NodeEvent::PeerDisconnected {
                    peer: peer_id.to_string(),
                });
            }

            SwarmEvent::Behaviour(BehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            })) => {
                self.handle_gossip(propagation_source, message);
            }

            // Somebody has just joined a community we are in. This is the
            // moment their address becomes worth knowing and ours worth
            // sending -- and, unlike a raw connection, it means gossipsub is
            // ready to carry the announcement.
            //
            // Waiting for the periodic announcement instead would leave a new
            // member reachable only through whoever invited them until the next
            // tick, which is exactly the single point of failure the design is
            // meant to avoid.
            SwarmEvent::Behaviour(BehaviourEvent::Gossipsub(gossipsub::Event::Subscribed {
                peer_id,
                topic,
            })) => {
                if let Some(community) = kahui_net::wire::community_from_topic(topic.as_str()) {
                    if self.joined.contains(&community) {
                        self.announce_presence_to(community);
                        self.request_sync(peer_id, community);
                    }
                }
            }

            SwarmEvent::Behaviour(BehaviourEvent::Sync(request_response::Event::Message {
                peer,
                message,
                ..
            })) => self.handle_sync(peer, message),

            SwarmEvent::Behaviour(BehaviourEvent::Sync(
                request_response::Event::OutboundFailure {
                    peer,
                    request_id,
                    error,
                    ..
                },
            )) => {
                self.inflight.remove(&request_id);
                debug!(%peer, %error, "sync request failed");
            }

            SwarmEvent::Behaviour(BehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
                ..
            })) => {
                for addr in &info.listen_addrs {
                    self.swarm.add_peer_address(peer_id, addr.clone());
                }

                // Everybody we meet goes into the routing table, so that having
                // met one node is enough to find every other. This is the step
                // that turns a single seed into a network.
                if info
                    .protocols
                    .iter()
                    .any(|p| p.as_ref() == kahui_net::KAD_PROTOCOL)
                {
                    for addr in dialable(&info.listen_addrs) {
                        if let Some(kad) = self.swarm.behaviour_mut().kad.as_mut() {
                            kad.add_address(&peer_id, addr);
                        }
                    }
                }

                self.claim_mapped_port(&info.observed_addr);

                // A peer that speaks the relay protocol can carry for us if we
                // turn out to need it. `identify` already tells us who does, so
                // nobody has to advertise anything.
                if info.protocols.contains(&relay::HOP_PROTOCOL_NAME) {
                    self.note_relay_candidate(peer_id, dialable(&info.listen_addrs));
                }

                // What a peer sees as our address is a claim, not a fact --
                // being able to send does not mean anyone can dial back.
                // AutoNAT settles it below, and only then is the address
                // treated as ours.
                if self.reachability == Reachability::Direct {
                    self.swarm.add_external_address(info.observed_addr);
                }
            }

            // The router agreed to forward a port. This is the best possible
            // outcome: genuinely reachable, with nobody else involved.
            SwarmEvent::Behaviour(BehaviourEvent::Kad(event)) => self.handle_kad(event),

            SwarmEvent::Behaviour(BehaviourEvent::Upnp(upnp::Event::NewExternalAddr(addr))) => {
                debug!(%addr, "the router opened a port for us");
                self.swarm.add_external_address(addr);
                self.reachability_confirmed(Reachability::Direct);
            }

            SwarmEvent::Behaviour(BehaviourEvent::Upnp(upnp::Event::ExpiredExternalAddr(addr))) => {
                debug!(%addr, "the router took the port away again");
                self.swarm.remove_external_address(&addr);
                // Back to not knowing. AutoNAT will settle it, and a relay will
                // be found if the answer turns out to be no.
                self.reachability_confirmed(Reachability::Unknown);
            }

            SwarmEvent::Behaviour(BehaviourEvent::Upnp(
                upnp::Event::GatewayNotFound | upnp::Event::NonRoutableGateway,
            )) => {
                // Perfectly ordinary: plenty of routers have UPnP switched off,
                // and some networks have no router to ask. The relay path
                // covers it.
                debug!("no router willing to forward a port; relying on peers instead");
            }

            // Somebody answered the question of whether we are reachable.
            SwarmEvent::Behaviour(BehaviourEvent::Autonat(autonat::Event::StatusChanged {
                new,
                ..
            })) => self.reachability_changed(new),

            // A member agreed to carry for us.
            SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
                relay::client::Event::ReservationReqAccepted { relay_peer_id, .. },
            )) => {
                if self.relayed_by.insert(relay_peer_id) {
                    // This one actually carried for us, which is a far better
                    // recommendation than merely speaking the protocol. Keep it
                    // for good and for every community: relaying has nothing to
                    // do with membership, and somebody who has met one willing
                    // node should never go back to being unreachable.
                    self.remember_relay(relay_peer_id);
                    self.announce_reachability();
                    // Peers only learn our new circuit address when we say so.
                    self.announce_presence();
                }
            }

            // We are carrying for a member who cannot be dialled.
            SwarmEvent::Behaviour(BehaviourEvent::Relay(
                relay::Event::ReservationReqAccepted { src_peer_id, .. },
            )) => {
                self.relaying_for.insert(src_peer_id);
            }

            SwarmEvent::Behaviour(BehaviourEvent::Relay(
                relay::Event::ReservationClosed { src_peer_id }
                | relay::Event::ReservationTimedOut { src_peer_id },
            )) => {
                self.relaying_for.remove(&src_peer_id);
            }

            // The relayed connection became a direct one, and the member who
            // was carrying it is out of the path.
            SwarmEvent::Behaviour(BehaviourEvent::Dcutr(dcutr::Event {
                remote_peer_id,
                result,
            })) => match result {
                Ok(_) => self.emit(NodeEvent::HolePunched {
                    peer: remote_peer_id.to_string(),
                }),
                Err(err) => debug!(%remote_peer_id, %err, "hole punch did not get through"),
            },

            SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Discovered(peers))) => {
                for (peer, addr) in peers {
                    self.swarm
                        .behaviour_mut()
                        .gossipsub
                        .add_explicit_peer(&peer);
                    self.swarm.add_peer_address(peer, addr);
                    if !self.swarm.is_connected(&peer) {
                        let _ = self.swarm.dial(peer);
                    }
                }
            }

            SwarmEvent::Behaviour(BehaviourEvent::Mdns(mdns::Event::Expired(peers))) => {
                for (peer, _) in peers {
                    self.swarm
                        .behaviour_mut()
                        .gossipsub
                        .remove_explicit_peer(&peer);
                }
            }

            _ => {}
        }
    }

    fn handle_gossip(&mut self, source: PeerId, message: gossipsub::Message) {
        let Some(community) = kahui_net::wire::community_from_topic(message.topic.as_str()) else {
            return;
        };
        match GossipMessage::decode(&message.data) {
            Ok(GossipMessage::Event(event)) => {
                self.ingest(*event, Some(source));
            }
            Ok(GossipMessage::Presence(presence)) => self.handle_presence(community, presence),
            Err(err) => debug!(%source, %err, "undecodable gossip message"),
        }
    }

    /// Learns where another member is, and connects to them directly.
    ///
    /// This is the step that turns a star around the inviter into a mesh. Once
    /// it has happened, losing any one member — including the founder — costs
    /// the community nothing.
    fn handle_presence(&mut self, community: CommunityId, presence: Presence) {
        if presence.user == self.me {
            return;
        }
        let Some(peer) = peer_id_of(&presence.user) else {
            return;
        };

        let addrs: Vec<Multiaddr> = presence
            .addrs
            .iter()
            .filter_map(|addr| addr.parse().ok())
            .collect();

        let record = PeerRecord {
            peer_id: peer.to_bytes(),
            addrs: addrs.iter().map(|a| a.to_string()).collect(),
            last_seen_ms: now_ms(),
        };
        if let Err(err) = self.store.remember_peer(&community, &record) {
            debug!(%err, "could not remember peer");
        }

        if !self.swarm.is_connected(&peer) {
            for addr in addrs {
                self.swarm.add_peer_address(peer, addr);
            }
            let _ = self.swarm.dial(peer);
        }
    }

    fn remember_peer(&mut self, peer: PeerId, addrs: &[Multiaddr]) {
        let record = PeerRecord {
            peer_id: peer.to_bytes(),
            addrs: addrs.iter().map(|a| a.to_string()).collect(),
            last_seen_ms: now_ms(),
        };
        for community in self.joined.clone() {
            let _ = self.store.remember_peer(&community, &record);
        }
    }

    // -- reachability -------------------------------------------------------

    /// Records what our peers have concluded about whether we can be dialled.
    ///
    /// This is the one thing a node genuinely cannot work out alone: sending a
    /// packet proves nothing about whether anybody can send one back. AutoNAT
    /// asks peers to try, which is why the answer is only trusted from here.
    fn reachability_changed(&mut self, status: NatStatus) {
        if self.config.reachability.is_some() {
            // The operator said. Peers guessing otherwise does not change it.
            return;
        }
        let next = match &status {
            NatStatus::Public(addr) => {
                self.swarm.add_external_address(addr.clone());
                Reachability::Direct
            }
            NatStatus::Private => Reachability::BehindNat,
            NatStatus::Unknown => Reachability::Unknown,
        };
        self.reachability_confirmed(next);
    }

    /// Asks the router to open the port we just started listening on.
    ///
    /// Waits for a real listen address because until then we do not know the
    /// number: with an OS-assigned port there is nothing to ask for yet.
    ///
    /// Only ever done once. The task renews its own mappings, and a second one
    /// would fight the first over the same port.
    fn start_port_mapping(&mut self, address: &Multiaddr) {
        if self.port_map.is_some() || !self.config.net.enable_port_mapping {
            return;
        }

        let Some(port) = address.iter().find_map(|part| match part {
            Protocol::Tcp(port) | Protocol::Udp(port) => Some(port),
            _ => None,
        }) else {
            return;
        };
        if port == 0 {
            return;
        }

        // Loopback goes nowhere, and there is no router in front of it.
        if address.iter().any(|part| match part {
            Protocol::Ip4(ip) => ip.is_loopback(),
            Protocol::Ip6(ip) => ip.is_loopback(),
            _ => false,
        }) {
            return;
        }

        debug!(port, "asking the router to open a port");
        self.port_map = Some(kahui_net::keep_open(port));
    }

    /// Acts on what the router said.
    fn handle_port_map(&mut self, update: PortMapUpdate) {
        match update {
            PortMapUpdate::Opened { external, protocol } => {
                // The best possible outcome: dialable by anyone, with no third
                // party involved at all.
                info!(
                    protocol = protocol.label(),
                    addrs = external.len(),
                    "the router opened a port; hosting works from here"
                );
                for addr in external {
                    self.swarm.add_external_address(addr);
                }
                self.reachability_confirmed(Reachability::Direct);
                self.announce_presence();
            }
            PortMapUpdate::MappedWithoutAddress { port, protocol } => {
                // Half an answer. The port is open, but we still have to learn
                // our own public address before we can tell anybody about it.
                debug!(
                    protocol = protocol.label(),
                    port, "the router opened a port without saying where"
                );
                self.mapped_port = Some(port);
            }
            PortMapUpdate::Refused(why) => {
                // Common and survivable. Relaying covers it.
                debug!(%why, "the router would not open a port");
            }
        }
    }

    /// Turns a mapped port plus a peer's view of us into a dialable address.
    ///
    /// A router that opens a port but refuses to name its own public address is
    /// a real and fairly common combination. The missing half is exactly what
    /// every peer we talk to can see, so `identify`'s observed address supplies
    /// it — with one substitution: the port it observed is whatever the NAT
    /// assigned our outbound connection, which is not the port we listen on.
    /// The IP is what we needed.
    fn claim_mapped_port(&mut self, observed: &Multiaddr) {
        let Some(port) = self.mapped_port else {
            return;
        };

        let Some(ip) = observed.iter().find_map(|part| match part {
            Protocol::Ip4(ip) => Some(std::net::IpAddr::V4(ip)),
            Protocol::Ip6(ip) => Some(std::net::IpAddr::V6(ip)),
            _ => None,
        }) else {
            return;
        };

        let addrs = kahui_net::portmap::addrs_for(ip, Some(port), Some(port));
        // A peer on our own network sees our private address, which tells us
        // nothing about being reachable from outside. Only a global one counts.
        if !addrs.iter().all(kahui_net::addr_is_reachable_beyond_lan) {
            return;
        }

        info!(%ip, port, "a peer told us our address; the mapped port is usable");
        // Done: no need to keep asking every peer we meet.
        self.mapped_port = None;
        for addr in addrs {
            self.swarm.add_external_address(addr);
        }
        self.reachability_confirmed(Reachability::Direct);
        self.announce_presence();
    }

    /// Acts on a change of reachability, whatever established it.
    ///
    /// A forwarded port and a successful AutoNAT probe mean the same thing and
    /// deserve the same response, so both arrive here.
    fn reachability_confirmed(&mut self, next: Reachability) {
        if self.config.reachability.is_some() || next == self.reachability {
            return;
        }
        self.reachability = next;
        self.apply_reachability();
        self.announce_reachability();
        self.announce_presence();
    }

    /// Does whatever being reachable, or not, implies.
    ///
    /// Separate from [`Self::reachability_confirmed`] because an operator who
    /// states their situation with `--reachable` must get exactly the same
    /// consequences as one whose situation was discovered by probing. Skipping
    /// them left such a node in the hash table's client mode: able to ask
    /// questions, unable to hold an answer, and so invisible to everybody
    /// looking for it.
    fn apply_reachability(&mut self) {
        match self.reachability {
            Reachability::BehindNat => {
                self.dht_withdraw_relay();
                // Ask the whole network, not just the peers we happen to hold.
                self.dht_find_relays();
                self.seek_relay();
            }
            // Dialable on our own account, so stop occupying a slot somebody
            // else may need — and start offering one, since being reachable is
            // precisely what makes a node useful to everybody else.
            Reachability::Direct => {
                self.release_relays();
                self.dht_offer_relay();
            }
            Reachability::Unknown => self.dht_withdraw_relay(),
        }
    }

    /// Asks reachable members to carry for us.
    ///
    /// The relay is a member of the community, not a service: whoever can be
    /// dialled carries for whoever cannot. That is the whole mechanism, and it
    /// is why a community of people behind home routers still works as long as
    /// one of them is reachable.
    fn seek_relay(&mut self) {
        if self.reachability != Reachability::BehindNat {
            return;
        }
        if self.relay_listeners.len() >= RELAY_TARGET {
            return;
        }

        let candidates: Vec<(PeerId, Vec<Multiaddr>)> = self
            .relay_candidates
            .iter()
            .filter(|(peer, addrs)| {
                !addrs.is_empty()
                    && !self.relay_listeners.contains_key(peer)
                    && self.swarm.is_connected(peer)
            })
            .map(|(peer, addrs)| (*peer, addrs.clone()))
            .collect();

        for (peer, addrs) in candidates {
            if self.relay_listeners.len() >= RELAY_TARGET {
                break;
            }
            for addr in addrs {
                let circuit = addr.with(Protocol::P2p(peer)).with(Protocol::P2pCircuit);
                match self.swarm.listen_on(circuit.clone()) {
                    Ok(listener) => {
                        debug!(%peer, %circuit, "asked a member to relay for us");
                        self.relay_listeners.insert(peer, listener);
                        break;
                    }
                    Err(err) => debug!(%peer, %err, "could not ask that member to relay"),
                }
            }
        }
    }

    fn release_relays(&mut self) {
        for (peer, listener) in std::mem::take(&mut self.relay_listeners) {
            self.swarm.remove_listener(listener);
            debug!(%peer, "gave back a relay reservation");
        }
        self.relayed_by.clear();
    }

    fn announce_reachability(&self) {
        self.emit(NodeEvent::ReachabilityChanged {
            reachability: self.reachability,
            relayed_by: self.relayed_by.iter().next().map(|peer| peer.to_string()),
        });
    }

    // -- sync -------------------------------------------------------------

    fn request_sync(&mut self, peer: PeerId, community: CommunityId) {
        if peer == *self.swarm.local_peer_id() || !self.joined.contains(&community) {
            return;
        }
        if self
            .inflight
            .values()
            .any(|entry| *entry == (peer, community))
        {
            return;
        }
        let have = match self.store.frontier(&community) {
            Ok(have) => have,
            Err(err) => {
                self.warn(format!("could not read local frontier: {err}"));
                return;
            }
        };
        let request_id = self.swarm.behaviour_mut().sync.send_request(
            &peer,
            SyncRequest::GetDelta {
                community,
                have,
                limit: MAX_SYNC_BATCH as u32,
            },
        );
        self.inflight.insert(request_id, (peer, community));
    }

    fn handle_sync(
        &mut self,
        peer: PeerId,
        message: request_response::Message<SyncRequest, SyncResponse>,
    ) {
        match message {
            request_response::Message::Request {
                request, channel, ..
            } => {
                let response = self.serve_sync(request);
                let _ = self
                    .swarm
                    .behaviour_mut()
                    .sync
                    .send_response(channel, response);
            }
            request_response::Message::Response {
                request_id,
                response,
            } => {
                let Some((_, community)) = self.inflight.remove(&request_id) else {
                    return;
                };
                let SyncResponse::Delta { events, complete } = response else {
                    return;
                };

                let mut applied = 0;
                for event in events {
                    if self.ingest(event, Some(peer)) {
                        applied += 1;
                    }
                }
                if applied > 0 {
                    self.emit(NodeEvent::Synced {
                        peer: peer.to_string(),
                        community,
                        applied,
                    });
                    // Only chase the rest if we made progress. A peer that
                    // keeps sending events we reject cannot spin us.
                    if !complete {
                        self.request_sync(peer, community);
                    }
                }
            }
        }
    }

    /// Answers another node's request for history.
    ///
    /// Served to anyone who asks. Every event is signed, so a requester does
    /// not have to trust us and we gain nothing by lying — the worst we could
    /// do is withhold, and any other member serves the same history.
    fn serve_sync(&self, request: SyncRequest) -> SyncResponse {
        let SyncRequest::GetDelta {
            community,
            have,
            limit,
        } = request;

        if !matches!(self.store.community(&community), Ok(Some(_))) {
            return SyncResponse::UnknownCommunity;
        }
        let limit = (limit as usize).clamp(1, MAX_SYNC_BATCH);
        match self.store.delta(&community, &have, limit) {
            Ok(delta) => SyncResponse::Delta {
                events: delta.events,
                complete: delta.complete,
            },
            Err(err) => {
                warn!(%err, "could not build a sync delta");
                SyncResponse::UnknownCommunity
            }
        }
    }

    // -- applying events --------------------------------------------------

    /// Verifies and stores one event, whatever its source. Returns true if it
    /// was new.
    fn ingest(&mut self, event: SignedEvent, source: Option<PeerId>) -> bool {
        let community = event.community_id();
        let id = event.id();

        match self.store.put_event(&event) {
            Ok(Inserted::New) => {
                self.emit_for(&community, &event);
                self.resolve_dependents(id);
                self.try_complete_join(community);
                true
            }
            Ok(Inserted::Duplicate) => false,
            Err(err) if err.needs_sync() => {
                // Not a bad event, just an early one. Hold it, and ask the
                // sender for what comes before it.
                self.orphans.park(event);
                if let Some(peer) = source {
                    self.request_sync(peer, community);
                }
                false
            }
            Err(err) => {
                self.warn(format!("rejected an event: {err}"));
                false
            }
        }
    }

    /// Applies anything that was waiting on `id`, and anything waiting on
    /// those in turn.
    fn resolve_dependents(&mut self, id: EventId) {
        if self.orphans.len() == 0 {
            return;
        }
        let mut queue = vec![id];
        while let Some(dependency) = queue.pop() {
            for event in self.orphans.take(dependency) {
                let unblocked = event.id();
                let community = event.community_id();
                if matches!(self.store.put_event(&event), Ok(Inserted::New)) {
                    self.emit_for(&community, &event);
                    self.try_complete_join(community);
                    queue.push(unblocked);
                }
            }
        }
    }

    /// Authors, stores and publishes an event in this node's own chain.
    fn author(
        &mut self,
        community: CommunityId,
        payload: Payload,
    ) -> Result<SignedEvent, NodeError> {
        let tip = chain::tip(self.store.as_ref(), &community, &self.me)?;
        let event = Event::create(&self.identity, community, &tip, now_ms(), payload);
        self.store.put_event(&event)?;

        let resolved = event.community_id();
        self.emit_for(&resolved, &event);
        self.publish(&resolved, &event);
        Ok(event)
    }

    fn publish(&mut self, community: &CommunityId, event: &SignedEvent) {
        let message = GossipMessage::Event(Box::new(event.clone()));
        if let Err(err) = self.swarm.behaviour_mut().publish(community, &message) {
            // Having nobody to publish to is the ordinary state of a node that
            // just started, or just founded something. The event is already
            // stored; peers will pick it up by sync when they arrive.
            let expected = matches!(
                err,
                gossipsub::PublishError::NoPeersSubscribedToTopic
                    | gossipsub::PublishError::Duplicate
            );
            if !expected {
                debug!(%err, "could not gossip an event");
            }
        }
    }

    /// Turns a stored event into something a UI can display.
    fn emit_for(&self, community: &CommunityId, event: &SignedEvent) {
        let author = event.author();
        let node_event = match event.payload() {
            Payload::Message { channel, body } => NodeEvent::Message(Message {
                id: event.id(),
                community: *community,
                channel: *channel,
                author,
                author_name: self.display_name_of(community, &author),
                body: body.clone(),
                timestamp_ms: event.event.timestamp_ms,
                lamport: event.event.lamport,
            }),
            Payload::Join { display_name } | Payload::SetDisplayName { display_name } => {
                NodeEvent::Membership {
                    community: *community,
                    user: author,
                    display_name: display_name.clone(),
                }
            }
            Payload::CreateChannel { channel, name, .. } => NodeEvent::ChannelCreated {
                community: *community,
                channel: *channel,
                name: name.clone(),
            },
            Payload::CreateCommunity { name, .. } => NodeEvent::CommunityCreated {
                community: *community,
                name: name.clone(),
            },
        };
        self.emit(node_event);
    }

    fn display_name_of(&self, community: &CommunityId, user: &UserId) -> String {
        self.store
            .member(community, user)
            .ok()
            .flatten()
            .map(|member| member.display_name)
            // A message can legitimately arrive before the sender's join event
            // does; a short id is a better placeholder than a blank.
            .unwrap_or_else(|| user.short())
    }

    // -- commands ---------------------------------------------------------

    /// Returns true when the engine should stop.
    fn handle_command(&mut self, command: Command) -> bool {
        match command {
            Command::CreateCommunity {
                name,
                description,
                reply,
            } => {
                let _ = reply.send(self.create_community(name, description));
            }

            Command::Join { invite, reply } => self.begin_join(*invite, reply),

            Command::CreateChannel {
                community,
                name,
                topic,
                reply,
            } => {
                let channel = ChannelId::derive(&community, &name);
                let result = self
                    .author(
                        community,
                        Payload::CreateChannel {
                            channel,
                            name,
                            topic,
                        },
                    )
                    .map(|_| channel);
                let _ = reply.send(result);
            }

            Command::Post {
                community,
                channel,
                body,
                reply,
            } => {
                let result = self
                    .author(community, Payload::Message { channel, body })
                    .map(|event| event.id());
                let _ = reply.send(result);
            }

            Command::SetDisplayName {
                display_name,
                reply,
            } => {
                let _ = reply.send(self.set_display_name(display_name));
            }

            Command::BackupPhrase { reply } => {
                let _ = reply.send(Ok(self.identity.to_backup_phrase()));
            }

            Command::ReplaceIdentity { phrase, reply } => {
                let _ = reply.send(self.replace_identity(&phrase));
            }

            Command::MakeInvite { community, reply } => {
                let _ = reply.send(self.make_invite(community));
            }

            Command::Communities { reply } => {
                let _ = reply.send(self.store.communities().map_err(NodeError::from));
            }

            Command::Channels { community, reply } => {
                let _ = reply.send(self.store.channels(&community).map_err(NodeError::from));
            }

            Command::Members { community, reply } => {
                let _ = reply.send(self.store.members(&community).map_err(NodeError::from));
            }

            Command::OnlineMembers { community, reply } => {
                let _ = reply.send(self.online_members(&community));
            }

            Command::History {
                community,
                channel,
                limit,
                reply,
            } => {
                let _ = reply.send(self.history(community, channel, limit));
            }

            Command::Status { reply } => {
                let _ = reply.send(self.status());
            }

            Command::Dial { addr, reply } => {
                let result = addr
                    .parse::<Multiaddr>()
                    .map_err(|err| NodeError::Engine(format!("bad address {addr}: {err}")))
                    .and_then(|addr| {
                        self.swarm
                            .dial(addr)
                            .map_err(|err| NodeError::Engine(err.to_string()))
                    });
                let _ = reply.send(result);
            }

            Command::SyncNow { reply } => {
                let before = self.inflight.len();
                self.anti_entropy();
                let _ = reply.send(Ok(self.inflight.len().saturating_sub(before)));
            }

            Command::Shutdown { reply } => {
                // Answered after the loop ends, once the database is closed.
                self.shutdown_reply = Some(reply);
                return true;
            }
        }
        false
    }

    /// Founds a community: genesis, our own membership, and `#general`.
    ///
    /// The founder joins like anybody else. There is no owner flag, because
    /// there is nothing for one to unlock — every member's node holds the same
    /// history and serves it on the same terms.
    fn create_community(
        &mut self,
        name: String,
        description: String,
    ) -> Result<CommunityId, NodeError> {
        let genesis = self.author(
            CommunityId::ZERO,
            Payload::CreateCommunity { name, description },
        )?;
        let community = genesis.community_id();

        self.joined.insert(community);
        self.swarm.behaviour_mut().subscribe(&community)?;
        // Tell the network we hold it, so an invite need carry nothing but the
        // id and people can find us without being handed an address.
        self.dht_provide(&community);

        self.author(
            community,
            Payload::Join {
                display_name: self.display_name.clone(),
            },
        )?;
        let channel = ChannelId::derive(&community, "general");
        self.author(
            community,
            Payload::CreateChannel {
                channel,
                name: "general".into(),
                topic: "Everything else".into(),
            },
        )?;

        Ok(community)
    }

    /// Starts joining. The reply is held until the community's history has
    /// actually arrived and been verified.
    fn begin_join(&mut self, invite: Invite, reply: crate::api::Reply<CommunityId>) {
        let community = invite.community;
        self.joined.insert(community);

        if let Err(err) = self.swarm.behaviour_mut().subscribe(&community) {
            let _ = reply.send(Err(err.into()));
            return;
        }

        self.dht_provide(&community);

        for addr in invite.dial_addresses() {
            match addr.parse::<Multiaddr>() {
                Ok(addr) => {
                    let _ = self.swarm.dial(addr);
                }
                Err(err) => debug!(%err, "invite carried an unusable address"),
            }
        }

        // And ask the network, in parallel. The addresses in an invite go stale
        // — a laptop moves, a reservation lapses, the member who issued it is
        // away — whereas the community id never does. This is what makes an
        // invite work weeks after it was written.
        self.dht_find(community);

        self.pending_joins.entry(community).or_default().push(reply);
        // If we already hold this community, there is nothing to wait for.
        self.try_complete_join(community);
    }

    /// Announces membership, once we hold enough to verify the community.
    ///
    /// Waiting matters: the join event has to sit at the end of a chain we have
    /// actually seen, and its Lamport clock has to reflect real history. Firing
    /// it blind would produce an event every peer would reject.
    fn try_complete_join(&mut self, community: CommunityId) {
        if !self.pending_joins.contains_key(&community) {
            return;
        }
        if !matches!(self.store.community(&community), Ok(Some(_))) {
            return;
        }

        let already_member = matches!(self.store.member(&community, &self.me), Ok(Some(_)));
        let outcome = if already_member {
            Ok(community)
        } else {
            self.author(
                community,
                Payload::Join {
                    display_name: self.display_name.clone(),
                },
            )
            .map(|_| community)
        };

        if let Some(waiters) = self.pending_joins.remove(&community) {
            for waiter in waiters {
                let _ = waiter.send(match &outcome {
                    Ok(id) => Ok(*id),
                    Err(err) => Err(NodeError::Engine(err.to_string())),
                });
            }
        }
    }

    /// Writes a different identity to disk, for the next start.
    ///
    /// Only while nothing has been signed under the current one. After that the
    /// node's own chain is the old identity's, and quietly continuing it under
    /// a new key would produce events that verify against nobody.
    fn replace_identity(&mut self, phrase: &str) -> Result<(), NodeError> {
        if !self.store.communities()?.is_empty() {
            return Err(NodeError::Engine(
                "This device already belongs to a community, and the messages it has                  sent are signed by its current identity. Restore a key onto a fresh                  install instead."
                    .into(),
            ));
        }
        let restored = kahui_proto::Identity::from_backup_phrase(phrase)?;
        self.store
            .meta_put(crate::META_IDENTITY, &restored.secret_bytes())?;
        Ok(())
    }

    fn set_display_name(&mut self, display_name: String) -> Result<(), NodeError> {
        self.display_name = display_name.clone();
        self.store
            .meta_put(crate::META_DISPLAY_NAME, display_name.as_bytes())?;
        for community in self.joined.clone() {
            self.author(
                community,
                Payload::SetDisplayName {
                    display_name: display_name.clone(),
                },
            )?;
        }
        Ok(())
    }

    /// Builds an invite naming us and, where we know them, other members.
    ///
    /// Naming several members is the point: an invite that only worked while
    /// its author was online would put the founder back at the centre of
    /// something that is not supposed to have one.
    fn make_invite(&mut self, community: CommunityId) -> Result<Invite, NodeError> {
        let summary = self
            .store
            .community(&community)?
            .ok_or(NodeError::UnknownCommunity(community))?;

        let mut peers = vec![InvitePeer {
            peer_id: self.swarm.local_peer_id().to_string(),
            addrs: self.public_addrs(),
        }];

        for record in self.store.peers(&community)?.into_iter().take(INVITE_PEERS) {
            if let Ok(peer) = PeerId::from_bytes(&record.peer_id) {
                if !record.addrs.is_empty() {
                    peers.push(InvitePeer {
                        peer_id: peer.to_string(),
                        addrs: record.addrs,
                    });
                }
            }
        }

        // Reachable nodes we know of, whether or not they are in this community.
        //
        // They are not being named as members — they may never have heard of it
        // — but as a way into the network. Somebody receiving this invite can
        // reach them, and through them find everybody else, including members
        // whose own addresses have changed since this invite was written. It is
        // what makes being invited the same thing as being bootstrapped, so that
        // nobody has to be handed a seed address by hand.
        for record in self
            .store
            .relays()
            .unwrap_or_default()
            .into_iter()
            .take(INVITE_PEERS)
        {
            let Ok(peer) = PeerId::from_bytes(&record.peer_id) else {
                continue;
            };
            let peer = peer.to_string();
            if record.addrs.is_empty() || peers.iter().any(|known| known.peer_id == peer) {
                continue;
            }
            peers.push(InvitePeer {
                peer_id: peer,
                addrs: record.addrs,
            });
        }

        Ok(Invite::new(community, summary.name, peers))
    }

    /// Members we currently hold a connection to.
    ///
    /// Membership is a routing table: a member's network identity follows from
    /// the public key in their events, so this is a set intersection and needs
    /// nobody to have announced anything.
    fn online_members(&self, community: &CommunityId) -> Result<Vec<UserId>, NodeError> {
        let connected: HashSet<PeerId> = self.swarm.connected_peers().copied().collect();
        Ok(self
            .store
            .members(community)?
            .into_iter()
            .filter(|member| peer_id_of(&member.id).is_some_and(|peer| connected.contains(&peer)))
            .map(|member| member.id)
            .collect())
    }

    fn history(
        &self,
        community: CommunityId,
        channel: ChannelId,
        limit: usize,
    ) -> Result<Vec<Message>, NodeError> {
        let events = self.store.channel_history(&community, &channel, limit)?;
        Ok(events
            .iter()
            .filter_map(|event| match event.payload() {
                Payload::Message { channel, body } => Some(Message {
                    id: event.id(),
                    community,
                    channel: *channel,
                    author: event.author(),
                    author_name: self.display_name_of(&community, &event.author()),
                    body: body.clone(),
                    timestamp_ms: event.event.timestamp_ms,
                    lamport: event.event.lamport,
                }),
                _ => None,
            })
            .collect())
    }

    fn status(&mut self) -> Result<Status, NodeError> {
        let mut communities = Vec::new();
        for summary in self.store.communities()? {
            communities.push(CommunityStatus {
                id: summary.id,
                name: summary.name,
                events: self.store.frontier(&summary.id)?.total_events(),
                members: self.store.members(&summary.id)?.len(),
                channels: self.store.channels(&summary.id)?.len(),
            });
        }

        Ok(Status {
            user: self.me,
            display_name: self.display_name.clone(),
            peer_id: self.swarm.local_peer_id().to_string(),
            listen_addrs: self.public_addrs(),
            connected_peers: self
                .swarm
                .connected_peers()
                .map(|peer| peer.to_string())
                .collect(),
            communities,
            reachability: self.reachability,
            relayed_by: self.relayed_by.iter().next().map(|peer| peer.to_string()),
            relaying_for: self.relaying_for.len(),
            network_peers: self.dht_peer_count(),
        })
    }

    /// Addresses worth telling other people about, best first.
    ///
    /// A direct routable address is the best thing to offer. A circuit address
    /// is second: slower, and it costs a member bandwidth, but it works from
    /// anywhere. Loopback is last, since it only helps somebody already on this
    /// machine.
    fn public_addrs(&self) -> Vec<String> {
        let mut addrs: Vec<&Multiaddr> = self
            .listen_addrs
            .iter()
            .chain(self.swarm.external_addresses())
            .collect();
        addrs.sort_by_key(|addr| {
            if is_loopback(addr) {
                2
            } else if is_circuit(addr) {
                1
            } else {
                0
            }
        });
        addrs.dedup();
        addrs.iter().map(|addr| addr.to_string()).collect()
    }

    // -- plumbing ---------------------------------------------------------

    fn emit(&self, event: NodeEvent) {
        // An error here only means nobody is listening, which is fine.
        let _ = self.events.send(event);
    }

    fn warn(&self, message: String) {
        warn!("{message}");
        self.emit(NodeEvent::Warning { message });
    }
}

fn is_loopback(addr: &Multiaddr) -> bool {
    addr.iter().any(|part| match part {
        Protocol::Ip4(ip) => ip.is_loopback(),
        Protocol::Ip6(ip) => ip.is_loopback(),
        _ => false,
    })
}

/// True for an address that reaches a node through somebody else.
fn is_circuit(addr: &Multiaddr) -> bool {
    addr.iter().any(|part| matches!(part, Protocol::P2pCircuit))
}

/// Addresses we could dial a would-be relay on, best first.
///
/// Relaying through a relay is not a thing, so circuit addresses are dropped,
/// and any trailing peer id is stripped because the caller appends its own.
fn dialable(addrs: &[Multiaddr]) -> Vec<Multiaddr> {
    let mut out: Vec<Multiaddr> = addrs
        .iter()
        .filter(|addr| !is_circuit(addr))
        .map(|addr| {
            addr.iter()
                .filter(|part| !matches!(part, Protocol::P2p(_)))
                .collect::<Multiaddr>()
        })
        .collect();
    out.sort_by_key(is_loopback);
    out.dedup();
    // A handful is plenty; the rest are usually the same host on interfaces
    // nobody can reach.
    out.truncate(4);
    out
}
