//! [`Store`] backed by [redb], an embedded ACID key-value store.
//!
//! redb is pure Rust with no C dependency, which keeps a Kahui node a single
//! static binary on Windows, Linux and macOS alike, and keeps cross-compiling
//! to mobile targets uneventful.
//!
//! Every table is `bytes -> bytes`; the structure lives in [`crate::keys`],
//! where fixed-width big-endian composite keys make ordinary range scans do the
//! work an index would otherwise have to.

use std::path::Path;

use kahui_proto::codec;
use kahui_proto::{ChannelId, CommunityId, EventId, Frontier, Payload, SignedEvent, UserId};
use redb::{Database, ReadableTable, TableDefinition};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::keys::{self, ID_LEN, SEQ_LEN};
use crate::{
    ChannelSummary, CommunitySummary, Delta, Inserted, MemberSummary, PeerRecord, Result, Store,
    StoreError,
};

/// Content-addressed event bodies. `event_id -> encoded SignedEvent`
const EVENTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("events");
/// Per-author chains. `community || author || seq -> event_id`
const CHAIN: TableDefinition<&[u8], &[u8]> = TableDefinition::new("chain");
/// Causal order index. `community || lamport || event_id -> author || seq`
const ORDER: TableDefinition<&[u8], &[u8]> = TableDefinition::new("order");
/// Messages per channel. `community || channel || lamport || event_id -> ()`
const CHANNEL_LOG: TableDefinition<&[u8], &[u8]> = TableDefinition::new("channel_log");
/// `community -> CommunitySummary`
const COMMUNITIES: TableDefinition<&[u8], &[u8]> = TableDefinition::new("communities");
/// `community || channel -> ChannelRecord`
const CHANNELS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("channels");
/// `community || user -> MemberRecord`
const MEMBERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("members");
/// `community || peer_id -> PeerRecord`
const PEERS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("peers");
/// Reachable nodes willing to relay. `peer_id -> PeerRecord`, no community.
const RELAYS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("relays");
/// Node-local settings, outside the replicated log. `key -> value`
const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

/// How many addresses we keep per peer. Enough for a few interfaces and a
/// relay, not enough for a peer to use us as storage.
const MAX_PEER_ADDRS: usize = 8;

/// Channel metadata, plus the position of the event that created it so
/// concurrent creations resolve the same way everywhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChannelRecord {
    name: String,
    topic: String,
    creator: UserId,
    created_ms: u64,
    lamport: u64,
    event: EventId,
}

/// Membership, with the position of the event that last set the display name.
/// Display names are last-writer-wins over that position, so renames converge.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MemberRecord {
    display_name: String,
    joined_ms: u64,
    name_lamport: u64,
    name_event: EventId,
}

pub struct RedbStore {
    db: Database,
}

impl RedbStore {
    /// Opens, creating the file and its tables if they do not exist.
    ///
    /// Every table is created up front so read transactions never have to cope
    /// with a table that has not been written to yet.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let db = Database::create(path.as_ref())?;
        let txn = db.begin_write()?;
        {
            txn.open_table(EVENTS)?;
            txn.open_table(CHAIN)?;
            txn.open_table(ORDER)?;
            txn.open_table(CHANNEL_LOG)?;
            txn.open_table(COMMUNITIES)?;
            txn.open_table(CHANNELS)?;
            txn.open_table(MEMBERS)?;
            txn.open_table(PEERS)?;
            txn.open_table(RELAYS)?;
            txn.open_table(META)?;
        }
        txn.commit()?;
        Ok(RedbStore { db })
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    codec::to_canonical(value).map_err(|e| StoreError::Corrupt(e.to_string()))
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    codec::from_canonical(bytes).map_err(|e| StoreError::Corrupt(e.to_string()))
}

fn corrupt(what: &str) -> StoreError {
    StoreError::Corrupt(what.to_string())
}

/// Reads the lamport field out of an order-index key.
fn lamport_from_order_key(key: &[u8]) -> Result<u64> {
    key.get(ID_LEN..ID_LEN + SEQ_LEN)
        .and_then(|s| s.try_into().ok())
        .map(u64::from_be_bytes)
        .ok_or_else(|| corrupt("order key is too short"))
}

/// Reads the event id out of an order-index key.
fn event_from_order_key(key: &[u8]) -> Result<EventId> {
    key.get(ID_LEN + SEQ_LEN..)
        .and_then(EventId::try_from_slice)
        .ok_or_else(|| corrupt("order key has no event id"))
}

impl Store for RedbStore {
    fn put_event(&self, event: &SignedEvent) -> Result<Inserted> {
        // Verify before touching the disk. This is the boundary where untrusted
        // bytes from the network become trusted state, and nothing gets past it
        // without a signature that checks out.
        event.verify()?;

        let id = event.id();
        let community = event.community_id();
        let author = event.author();
        let seq = event.seq();
        let lamport = event.event.lamport;

        // Gossip and sync overlap constantly, so most events arrive twice.
        // Catch that on a read transaction rather than taking the write lock.
        if self.has_event(&id)? {
            return Ok(Inserted::Duplicate);
        }

        let txn = self.db.begin_write()?;
        {
            let mut events = txn.open_table(EVENTS)?;
            let mut chain = txn.open_table(CHAIN)?;
            let mut order = txn.open_table(ORDER)?;
            let mut communities = txn.open_table(COMMUNITIES)?;

            // Re-check under the write lock: two tasks can race here.
            if events.get(id.as_bytes().as_slice())?.is_some() {
                drop((events, chain, order, communities));
                txn.abort()?;
                return Ok(Inserted::Duplicate);
            }

            // An event only means something relative to a community we can
            // verify the root of. Without the genesis there is nothing to
            // anchor it to, so we say so and let the caller go and fetch it.
            if !event.event.is_genesis()
                && communities.get(community.as_bytes().as_slice())?.is_none()
            {
                drop((events, chain, order, communities));
                txn.abort()?;
                return Err(StoreError::UnknownCommunity(community.short()));
            }

            let own_key = keys::chain(&community, &author, seq);
            if chain.get(own_key.as_slice())?.is_some() {
                // The slot is taken by a different event: the author signed two
                // things at the same position.
                drop((events, chain, order, communities));
                txn.abort()?;
                return Err(StoreError::Equivocation {
                    author: author.short(),
                    seq,
                });
            }

            if seq > 0 {
                let prev_key = keys::chain(&community, &author, seq - 1);
                let held: Option<EventId> = chain
                    .get(prev_key.as_slice())?
                    .and_then(|g| EventId::try_from_slice(g.value()));
                match held {
                    None => {
                        drop((events, chain, order, communities));
                        txn.abort()?;
                        return Err(StoreError::MissingPrev {
                            author: author.short(),
                            seq,
                            needed_seq: seq - 1,
                        });
                    }
                    Some(held) if Some(held) != event.event.prev_self => {
                        drop((events, chain, order, communities));
                        txn.abort()?;
                        return Err(StoreError::ChainMismatch {
                            author: author.short(),
                            seq,
                        });
                    }
                    Some(_) => {}
                }
            }

            let encoded = event.encode();
            events.insert(id.as_bytes().as_slice(), encoded.as_slice())?;
            chain.insert(own_key.as_slice(), id.as_bytes().as_slice())?;

            let order_key = keys::order(&community, lamport, &id);
            let order_val = keys::order_value(&author, seq);
            order.insert(order_key.as_slice(), order_val.as_slice())?;

            apply_payload(&txn, &mut communities, event, &community, &id)?;
        }
        txn.commit()?;
        Ok(Inserted::New)
    }

    fn get_event(&self, id: &EventId) -> Result<Option<SignedEvent>> {
        let txn = self.db.begin_read()?;
        let events = txn.open_table(EVENTS)?;
        let Some(guard) = events.get(id.as_bytes().as_slice())? else {
            return Ok(None);
        };
        let event =
            SignedEvent::decode(guard.value()).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        Ok(Some(event))
    }

    fn has_event(&self, id: &EventId) -> Result<bool> {
        let txn = self.db.begin_read()?;
        let events = txn.open_table(EVENTS)?;
        Ok(events.get(id.as_bytes().as_slice())?.is_some())
    }

    fn frontier(&self, community: &CommunityId) -> Result<Frontier> {
        let txn = self.db.begin_read()?;
        let chain = txn.open_table(CHAIN)?;
        let (lo, hi) = keys::chain_bounds(community);
        let mut frontier = Frontier::new();
        // Chains have no gaps, so the last sequence seen for an author is also
        // the highest one this node can serve.
        for row in chain.range(lo.as_slice()..=hi.as_slice())? {
            let (key, _) = row?;
            let (author, seq) =
                keys::parse_chain_key(key.value()).ok_or_else(|| corrupt("chain key"))?;
            frontier.observe(author, seq);
        }
        Ok(frontier)
    }

    fn delta(&self, community: &CommunityId, have: &Frontier, limit: usize) -> Result<Delta> {
        let txn = self.db.begin_read()?;
        let order = txn.open_table(ORDER)?;
        let events = txn.open_table(EVENTS)?;
        let (lo, hi) = keys::order_bounds(community);

        let mut out = Vec::new();
        let mut complete = true;
        // Walking the order index ascending yields events in Lamport order,
        // which is a causal order: every event's predecessors come first. The
        // receiver can therefore apply the batch straight down the list, and a
        // truncated batch is still applicable as far as it goes.
        for row in order.range(lo.as_slice()..=hi.as_slice())? {
            let (key, value) = row?;
            let (author, seq) =
                keys::parse_order_value(value.value()).ok_or_else(|| corrupt("order value"))?;
            if have.covers(&author, seq) {
                continue;
            }
            if out.len() >= limit {
                complete = false;
                break;
            }
            let id = event_from_order_key(key.value())?;
            let guard = events
                .get(id.as_bytes().as_slice())?
                .ok_or_else(|| corrupt("order index points at a missing event"))?;
            out.push(
                SignedEvent::decode(guard.value())
                    .map_err(|e| StoreError::Corrupt(e.to_string()))?,
            );
        }
        Ok(Delta {
            events: out,
            complete,
        })
    }

    fn max_lamport(&self, community: &CommunityId) -> Result<u64> {
        let txn = self.db.begin_read()?;
        let order = txn.open_table(ORDER)?;
        let (lo, hi) = keys::order_bounds(community);
        let mut range = order.range(lo.as_slice()..=hi.as_slice())?;
        match range.next_back() {
            Some(row) => lamport_from_order_key(row?.0.value()),
            None => Ok(0),
        }
    }

    fn chain_head(&self, community: &CommunityId, author: &UserId) -> Result<Option<EventId>> {
        let txn = self.db.begin_read()?;
        let chain = txn.open_table(CHAIN)?;
        let (lo, hi) = keys::chain_author_bounds(community, author);
        // Chains are contiguous and keys sort by sequence, so the last row in
        // the author's range is their head.
        let mut range = chain.range(lo.as_slice()..=hi.as_slice())?;
        match range.next_back() {
            Some(row) => {
                let (_, value) = row?;
                Ok(Some(EventId::try_from_slice(value.value()).ok_or_else(
                    || corrupt("chain value is not an event id"),
                )?))
            }
            None => Ok(None),
        }
    }

    fn heads(&self, community: &CommunityId) -> Result<Vec<EventId>> {
        let frontier = self.frontier(community)?;
        let txn = self.db.begin_read()?;
        let chain = txn.open_table(CHAIN)?;
        let mut heads = Vec::with_capacity(frontier.len());
        for (author, seq) in frontier.iter() {
            let key = keys::chain(community, author, seq);
            if let Some(guard) = chain.get(key.as_slice())? {
                heads.push(
                    EventId::try_from_slice(guard.value())
                        .ok_or_else(|| corrupt("chain value is not an event id"))?,
                );
            }
        }
        Ok(heads)
    }

    fn communities(&self) -> Result<Vec<CommunitySummary>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(COMMUNITIES)?;
        let mut out = Vec::new();
        for row in table.iter()? {
            let (_, value) = row?;
            out.push(decode::<CommunitySummary>(value.value())?);
        }
        out.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.name.cmp(&b.name)));
        Ok(out)
    }

    fn community(&self, id: &CommunityId) -> Result<Option<CommunitySummary>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(COMMUNITIES)?;
        match table.get(id.as_bytes().as_slice())? {
            Some(guard) => Ok(Some(decode(guard.value())?)),
            None => Ok(None),
        }
    }

    fn channels(&self, community: &CommunityId) -> Result<Vec<ChannelSummary>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(CHANNELS)?;
        let (lo, hi) = keys::pair_bounds(community);
        let mut out = Vec::new();
        for row in table.range(lo.as_slice()..=hi.as_slice())? {
            let (key, value) = row?;
            let raw = keys::parse_pair_second(key.value()).ok_or_else(|| corrupt("channel key"))?;
            let record: ChannelRecord = decode(value.value())?;
            out.push(ChannelSummary {
                id: ChannelId::from_bytes(raw),
                name: record.name,
                topic: record.topic,
                creator: record.creator,
                created_ms: record.created_ms,
            });
        }
        out.sort_by(|a, b| a.created_ms.cmp(&b.created_ms).then(a.name.cmp(&b.name)));
        Ok(out)
    }

    fn members(&self, community: &CommunityId) -> Result<Vec<MemberSummary>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(MEMBERS)?;
        let (lo, hi) = keys::pair_bounds(community);
        let mut out = Vec::new();
        for row in table.range(lo.as_slice()..=hi.as_slice())? {
            let (key, value) = row?;
            let raw = keys::parse_pair_second(key.value()).ok_or_else(|| corrupt("member key"))?;
            let record: MemberRecord = decode(value.value())?;
            out.push(MemberSummary {
                id: UserId::from_bytes(raw),
                display_name: record.display_name,
                joined_ms: record.joined_ms,
            });
        }
        out.sort_by(|a, b| a.joined_ms.cmp(&b.joined_ms).then(a.id.cmp(&b.id)));
        Ok(out)
    }

    fn member(&self, community: &CommunityId, user: &UserId) -> Result<Option<MemberSummary>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(MEMBERS)?;
        let key = keys::pair(community, user.as_bytes());
        match table.get(key.as_slice())? {
            Some(guard) => {
                let record: MemberRecord = decode(guard.value())?;
                Ok(Some(MemberSummary {
                    id: *user,
                    display_name: record.display_name,
                    joined_ms: record.joined_ms,
                }))
            }
            None => Ok(None),
        }
    }

    fn channel_history(
        &self,
        community: &CommunityId,
        channel: &ChannelId,
        limit: usize,
    ) -> Result<Vec<SignedEvent>> {
        let txn = self.db.begin_read()?;
        let log = txn.open_table(CHANNEL_LOG)?;
        let events = txn.open_table(EVENTS)?;
        let (lo, hi) = keys::channel_log_bounds(community, channel);

        // Walk backwards so "the last 50 messages" reads 50 rows, not the whole
        // channel, then flip to oldest-first for display.
        let mut out = Vec::new();
        for row in log.range(lo.as_slice()..=hi.as_slice())?.rev() {
            if out.len() >= limit {
                break;
            }
            let (key, _) = row?;
            let id = key
                .value()
                .get(ID_LEN * 2 + SEQ_LEN..)
                .and_then(EventId::try_from_slice)
                .ok_or_else(|| corrupt("channel log key"))?;
            let guard = events
                .get(id.as_bytes().as_slice())?
                .ok_or_else(|| corrupt("channel log points at a missing event"))?;
            out.push(
                SignedEvent::decode(guard.value())
                    .map_err(|e| StoreError::Corrupt(e.to_string()))?,
            );
        }
        out.reverse();
        Ok(out)
    }

    fn peers(&self, community: &CommunityId) -> Result<Vec<PeerRecord>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(PEERS)?;
        let (lo, hi) = keys::peer_bounds(community);
        let mut out = Vec::new();
        for row in table.range(lo.as_slice()..=hi.as_slice())? {
            let (_, value) = row?;
            out.push(decode::<PeerRecord>(value.value())?);
        }
        // Most recently seen first: the peer we last heard from is the one
        // most likely to answer a dial.
        out.sort_by_key(|peer| std::cmp::Reverse(peer.last_seen_ms));
        Ok(out)
    }

    fn remember_peer(&self, community: &CommunityId, peer: &PeerRecord) -> Result<()> {
        let key = keys::peer(community, &peer.peer_id);
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(PEERS)?;
            let existing: Option<PeerRecord> = match table.get(key.as_slice())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            };

            // Newest addresses first, older ones kept as fallbacks: a peer that
            // moved networks is still reachable at the address we last saw.
            let mut addrs = peer.addrs.clone();
            if let Some(existing) = &existing {
                for addr in &existing.addrs {
                    if !addrs.contains(addr) {
                        addrs.push(addr.clone());
                    }
                }
            }
            addrs.truncate(MAX_PEER_ADDRS);

            let merged = PeerRecord {
                peer_id: peer.peer_id.clone(),
                addrs,
                last_seen_ms: peer
                    .last_seen_ms
                    .max(existing.map_or(0, |e| e.last_seen_ms)),
            };
            let encoded = encode(&merged)?;
            table.insert(key.as_slice(), encoded.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    fn relays(&self) -> Result<Vec<PeerRecord>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(RELAYS)?;
        let mut out = Vec::new();
        for row in table.iter()? {
            let (_, value) = row?;
            out.push(decode::<PeerRecord>(value.value())?);
        }
        out.sort_by_key(|peer| std::cmp::Reverse(peer.last_seen_ms));
        Ok(out)
    }

    fn remember_relay(&self, peer: &PeerRecord) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(RELAYS)?;
            let existing: Option<PeerRecord> = match table.get(peer.peer_id.as_slice())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            };
            let mut addrs = peer.addrs.clone();
            if let Some(existing) = &existing {
                for addr in &existing.addrs {
                    if !addrs.contains(addr) {
                        addrs.push(addr.clone());
                    }
                }
            }
            addrs.truncate(MAX_PEER_ADDRS);
            let merged = PeerRecord {
                peer_id: peer.peer_id.clone(),
                addrs,
                last_seen_ms: peer
                    .last_seen_ms
                    .max(existing.map_or(0, |e| e.last_seen_ms)),
            };
            let encoded = encode(&merged)?;
            table.insert(peer.peer_id.as_slice(), encoded.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    fn meta_get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(META)?;
        Ok(table.get(key)?.map(|g| g.value().to_vec()))
    }

    fn meta_put(&self, key: &str, value: &[u8]) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(META)?;
            table.insert(key, value)?;
        }
        txn.commit()?;
        Ok(())
    }
}

/// Folds an event into the materialised views.
///
/// These views are pure functions of the event set, so any two nodes holding
/// the same events derive identical channel lists, member lists and history.
/// Where events conflict, position in the causal order decides — never arrival
/// time, which differs per node.
fn apply_payload(
    txn: &redb::WriteTransaction,
    communities: &mut redb::Table<'_, &[u8], &[u8]>,
    event: &SignedEvent,
    community: &CommunityId,
    id: &EventId,
) -> Result<()> {
    let lamport = event.event.lamport;
    let author = event.author();
    let timestamp = event.event.timestamp_ms;

    match event.payload() {
        Payload::CreateCommunity { name, description } => {
            let summary = CommunitySummary {
                id: *community,
                name: name.clone(),
                description: description.clone(),
                founder: author,
                created_ms: timestamp,
            };
            let encoded = encode(&summary)?;
            communities.insert(community.as_bytes().as_slice(), encoded.as_slice())?;
        }

        Payload::CreateChannel {
            channel,
            name,
            topic,
        } => {
            let mut table = txn.open_table(CHANNELS)?;
            let key = keys::pair(community, channel.as_bytes());
            let existing: Option<ChannelRecord> = match table.get(key.as_slice())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            };
            // Two members can create the same channel concurrently. Keep the
            // causally earliest, so every node keeps the same one.
            let supersedes = match &existing {
                None => true,
                Some(prev) => (lamport, *id) < (prev.lamport, prev.event),
            };
            if supersedes {
                let record = ChannelRecord {
                    name: name.clone(),
                    topic: topic.clone(),
                    creator: author,
                    created_ms: timestamp,
                    lamport,
                    event: *id,
                };
                let encoded = encode(&record)?;
                table.insert(key.as_slice(), encoded.as_slice())?;
            }
        }

        Payload::Join { display_name } => {
            let mut table = txn.open_table(MEMBERS)?;
            let key = keys::pair(community, author.as_bytes());
            if table.get(key.as_slice())?.is_none() {
                let record = MemberRecord {
                    display_name: display_name.clone(),
                    joined_ms: timestamp,
                    name_lamport: lamport,
                    name_event: *id,
                };
                let encoded = encode(&record)?;
                table.insert(key.as_slice(), encoded.as_slice())?;
            }
        }

        Payload::SetDisplayName { display_name } => {
            let mut table = txn.open_table(MEMBERS)?;
            let key = keys::pair(community, author.as_bytes());
            let existing: Option<MemberRecord> = match table.get(key.as_slice())? {
                Some(guard) => Some(decode(guard.value())?),
                None => None,
            };
            // Last writer wins by causal position, not by who arrived first.
            let supersedes = match &existing {
                None => true,
                Some(prev) => (lamport, *id) > (prev.name_lamport, prev.name_event),
            };
            if supersedes {
                let record = MemberRecord {
                    display_name: display_name.clone(),
                    joined_ms: existing.map_or(timestamp, |e| e.joined_ms),
                    name_lamport: lamport,
                    name_event: *id,
                };
                let encoded = encode(&record)?;
                table.insert(key.as_slice(), encoded.as_slice())?;
            }
        }

        Payload::Message { channel, .. } => {
            let mut table = txn.open_table(CHANNEL_LOG)?;
            let key = keys::channel_log(community, channel, lamport, id);
            table.insert(key.as_slice(), [].as_slice())?;
        }
    }
    Ok(())
}

macro_rules! backend_error_from {
    ($($ty:ty),* $(,)?) => {
        $(
            impl From<$ty> for StoreError {
                fn from(err: $ty) -> Self {
                    StoreError::Backend(Box::new(err))
                }
            }
        )*
    };
}

backend_error_from!(
    redb::DatabaseError,
    redb::TransactionError,
    redb::TableError,
    redb::StorageError,
    redb::CommitError,
);
