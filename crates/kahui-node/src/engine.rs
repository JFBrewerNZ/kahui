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
    peer_id_of, BehaviourEvent, GossipMessage, Invite, InvitePeer, Presence, SyncRequest,
    SyncResponse, MAX_SYNC_BATCH,
};
use kahui_proto::{ChannelId, CommunityId, Event, EventId, Identity, Payload, SignedEvent, UserId};
use kahui_store::{Inserted, PeerRecord, Store};
use libp2p::request_response::OutboundRequestId;
use libp2p::swarm::SwarmEvent;
use libp2p::{gossipsub, identify, mdns, request_response, Multiaddr, PeerId, Swarm};
use tokio::sync::{broadcast, mpsc};
use tokio::time::MissedTickBehavior;
use tracing::{debug, warn};

use crate::api::{Command, CommunityStatus, Message, NodeEvent, Status};
use crate::{chain, now_ms, NodeConfig, NodeError};

/// How many events we hold back waiting for a predecessor before we start
/// dropping them. Anything dropped is recovered by the next sync round, so this
/// is a memory bound rather than a correctness one.
const MAX_ORPHANS: usize = 2048;

/// Peers asked for history in one anti-entropy round. Asking everyone every
/// time is wasteful; any one peer with the events is enough.
const SYNC_FANOUT: usize = 4;

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
    Closed,
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
            };

            match tick {
                Tick::Command(command) => {
                    if self.handle_command(command) {
                        break;
                    }
                }
                Tick::Swarm(event) => self.handle_swarm(*event),
                Tick::Presence => {
                    self.announce_presence();
                    self.redial_known_peers();
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
                for addr in info.listen_addrs {
                    self.swarm.add_peer_address(peer_id, addr);
                }
                // What a peer says our address looks like from outside is the
                // only way we learn it behind NAT.
                self.swarm.add_external_address(info.observed_addr);
            }

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

        for addr in invite.dial_addresses() {
            match addr.parse::<Multiaddr>() {
                Ok(addr) => {
                    let _ = self.swarm.dial(addr);
                }
                Err(err) => debug!(%err, "invite carried an unusable address"),
            }
        }

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

    fn status(&self) -> Result<Status, NodeError> {
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
        })
    }

    /// Addresses worth telling other people about.
    ///
    /// Routable addresses come first so a peer on another machine tries those
    /// before loopback, which only helps when everyone is on this one.
    fn public_addrs(&self) -> Vec<String> {
        let mut addrs: Vec<&Multiaddr> = self
            .listen_addrs
            .iter()
            .chain(self.swarm.external_addresses())
            .collect();
        addrs.sort_by_key(|addr| is_loopback(addr));
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
    use libp2p::multiaddr::Protocol;
    addr.iter().any(|part| match part {
        Protocol::Ip4(ip) => ip.is_loopback(),
        Protocol::Ip6(ip) => ip.is_loopback(),
        _ => false,
    })
}
