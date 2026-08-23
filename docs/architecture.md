# Kāhui architecture

How the thing works, and why it was built this way. For the elevator pitch and how to run
it, see the [README](../README.md).

---

## 1. The one idea

A community is an **append-only log of signed events**, replicated in full to every
member.

Everything else follows. Creating the community, opening a channel, joining, posting,
renaming yourself — each is one event, signed by its author with a key that never leaves
their machine. There is no mutable server-side state, so there is nothing for two nodes
to disagree about: two nodes holding the same set of events render the same community.

The consequence worth dwelling on: **because every event proves its own authorship, a node
can accept history from any peer without trusting that peer.** A hostile peer cannot forge
a message, cannot alter one, cannot delete one from your copy, and cannot reorder your
transcript. The worst it can do is decline to share — and any other member serves the same
events.

That is what removes the server. Not a clever protocol, just the observation that once
authenticity travels *with the data* rather than being asserted by whoever is hosting it,
hosting stops being a position of authority and becomes a chore anyone can do.

---

## 2. Events

```rust
struct Event {
    version:      u16,
    community:    CommunityId,       // zero in a genesis event
    author:       UserId,            // an Ed25519 public key
    seq:          u64,               // position in this author's chain, from 0
    prev_self:    Option<EventId>,   // the author's previous event
    lamport:      u64,               // logical clock
    parents:      Vec<EventId>,      // heads observed when authoring
    timestamp_ms: u64,               // advisory only
    payload:      Payload,
}

struct SignedEvent { event: Event, signature: [u8; 64] }
```

**Identity.** A `UserId` *is* an Ed25519 public key. There is no account, no registration
and no server to ask: an identity is created offline and is valid the moment it exists.
The same 32 secret bytes seed the libp2p transport keypair, so a member's `PeerId` is
derivable from their `UserId` — meaning **membership is a routing table**. Any node
holding a community's events can compute the network address of every member from those
events alone.

**Content addressing.** `EventId = BLAKE3(DOMAIN || postcard(event))`, and the signature
covers the same preimage. The domain tag means a Kāhui signature can never be replayed as
a signature for another protocol using the same key.

**Genesis.** A community's id is the hash of the event that created it. That event cannot
name its own id, so it carries `community = 0` and is recognised by its payload. An invite
therefore *commits* to the exact genesis event: fetch one whose hash does not match and
you have been lied to.

**Channel ids** are `BLAKE3("kahui.channel.v1" || community || lowercase(name))`. Two
members who independently create `#general` compute the same id, so their events merge
into one channel rather than forking it. Validation enforces that the id matches the name,
so a peer cannot smuggle a message into an unrelated channel by mislabelling it.

### Two chains, doing different jobs

`prev_self` and `seq` make each author's events a **hash chain**: it cannot be reordered,
and entries cannot be silently removed. This is per-author and totally ordered.

`lamport` and `parents` capture causality **across** authors. They do not form a chain;
they form a DAG.

The separation matters. The per-author chain is what makes sync cheap (§4). The Lamport
clock is what makes display order agree (§3). Conflating them would cost one or the other.

---

## 3. Ordering

Every node sorts a channel by `(lamport, event_id)`.

`lamport` is one greater than every event the author had seen when writing. So if you
wrote after seeing my message, your event sorts after mine — **on every node**. Where two
events are concurrent, their Lamport values tie and `event_id` breaks it: arbitrary, but
computed identically everywhere.

Wall-clock time is carried and displayed but **never used for ordering**. A node with a
wrong clock — or a lying one — produces a confusing timestamp and nothing worse. Ordering
by wall clock would mean trusting every member's clock, which is exactly the kind of
trusted component the design exists to avoid.

So the guarantee is precisely:

- Causally related messages are ordered correctly, everywhere.
- Concurrent messages get *an* order, the same one everywhere.
- Nobody's clock can reorder anybody's history.

The acceptance test asserts exactly this and no more: it checks all three nodes render an
*identical* sequence, and pins relative order only where causality settles it. Asserting
wall-clock order would be testing a promise the protocol does not make.

---

## 4. Sync: the frontier

Because each author's events are numbered from zero with no gaps, "everything I hold from
Alice" collapses to one number. A node's entire state for a community is therefore one
number per member — a **frontier**, a version vector.

```
SyncRequest::GetDelta { community, have: Frontier, limit }
SyncResponse::Delta   { events: Vec<SignedEvent>, complete: bool }
```

That is the whole catch-up protocol. A node that has been offline sends its frontier; a
peer replies with every event the frontier does not cover. Nothing is re-sent and nothing
is missed.

The request is **a few dozen bytes per member regardless of how much history exists**, so
catching up costs the same after five minutes offline as after five months.

**Batches arrive in causal order.** The store walks its Lamport-ordered index, so a
truncated batch is still applicable front-to-back — the receiver never holds a batch it
cannot use. `complete: false` tells it to ask again with an updated frontier.

**Serving is unprivileged.** Any node answers any request for a community it holds. It
gains nothing by lying, because the requester verifies every signature itself.

### Three mechanisms, three jobs

| Mechanism | Job | Failure mode it covers |
|---|---|---|
| Gossip (gossipsub) | Deliver new events to whoever is online | — |
| Sync (request/response) | Repair what gossip missed | Node was asleep, packet dropped, partition healed |
| Presence (gossip) | Tell members where to find each other | New member only knows their inviter |

Anti-entropy runs on a timer as well, so convergence does not depend on any single event
arriving.

---

## 5. Why the founder is not special

This is the property the milestone hinges on, and it does not come free.

A new member knows one address: whoever invited them. If nothing else happened, the
community would be a **star** centred on the inviter, and losing them would partition
everyone.

So every member announces its addresses to the community — periodically, and **immediately
when someone new subscribes to the topic**. Anyone who hears an announcement from a peer
they are not connected to dials it. Within a second or two the star fills in to a mesh.

The immediate-on-subscribe part was added after watching the demo fail: with only the
periodic announcement, a community created and torn down inside one timer period never
meshed, and killing the founder partitioned it. The timer alone was a race the design
happened to usually win.

Two consequences worth stating:

- Presence must be **distinguishable between rounds**, or gossipsub's content-hash dedupe
  treats a repeated announcement as an already-seen message and drops it — and a member
  whose details had not changed would slowly become invisible. Hence `announced_at_ms`.
- The demo **asserts** B and C are directly linked before killing A. Without that check,
  "the community survives its founder leaving" would be an accident waiting to be
  disproved rather than a claim.

**Invites name several members**, not just their author, for the same reason: an invite
that only worked while its author was online would put the founder back at the centre of
something that is not supposed to have one.

---

## 6. Storage

One `Store` trait; one embedded redb implementation.

The trait exists because storage is the layer that genuinely cannot be shared across
platforms. Desktop and server nodes use redb. A browser client will implement the same
trait over IndexedDB, and the protocol, sync logic and node engine above it do not change.

Every table is `bytes -> bytes`, with fixed-width big-endian composite keys, so byte order
equals logical order and an ordinary range scan returns exactly the wanted rows, already
sorted.

| Table | Key | Purpose |
|---|---|---|
| `events` | `event_id` | the events themselves |
| `chain` | `community‖author‖seq` | per-author chains; a prefix scan yields the frontier |
| `order` | `community‖lamport‖event_id` | causal order; sync streams this and can stop early |
| `channel_log` | `community‖channel‖lamport‖event_id` | channel history, newest-first scan for "last N" |
| `communities`, `channels`, `members` | | materialised views |
| `peers` | `community‖peer_id` | last known addresses, so a node can rejoin unaided |
| `meta` | `key` | the node's own key and settings, outside the replicated log |

### put_event is the only way in

It refuses anything it cannot fully verify, and the *kind* of refusal is load-bearing:

| Rejection | Meaning | Node's response |
|---|---|---|
| bad signature / malformed | the sender's fault | drop it |
| `MissingPrev` | a gap in the author's chain | **we are behind** — hold the event, sync |
| `UnknownCommunity` | we have never seen the genesis | **we are behind** — sync |
| `Equivocation` | two different events at one sequence | provable misbehaviour; reject |
| `ChainMismatch` | `prev_self` disagrees with what we hold | reject |

`StoreError::needs_sync()` distinguishes "this event is bad" from "this node is behind".
Rejections of the second kind are not failures — they are how a node **discovers** it is
out of date. The engine parks the event in a small orphan buffer, asks the sender for the
missing history, and replays it when the gap fills.

### Views converge because they are pure functions of the events

Where two events conflict, **position in the causal order decides, never arrival time**.
Concurrent creations of the same channel keep the causally earliest. Display names are
last-writer-wins by `(lamport, event_id)`. Two nodes that received the same events in
opposite orders still show the same thing.

---

## 7. Crate layout

```
kahui-cli      terminal client — renders events, forwards commands, knows no protocol
kahui-node     the engine: commands in, events out
kahui-net      libp2p: gossip, presence, sync, invites
kahui-store    storage trait + redb implementation
kahui-proto    identities, events, canonical encoding — no IO whatsoever
```

The layering is what makes "one network, many clients" achievable rather than aspirational:

- **`kahui-proto`** is pure functions over plain data — no sockets, no disk, no executor.
  A WebAssembly web client shares this crate byte for byte, which is what makes it the
  same network rather than a lookalike.
- **`kahui-store`** is swappable per platform.
- **`kahui-node`** exposes `NodeHandle`: clonable, thread-safe, async methods plus a
  broadcast event stream. A Tauri desktop app, a mobile FFI shim and the CLI all drive
  exactly this. Nothing above it needs to know what gossipsub is.

The node engine is a **single task owning the swarm and the store**. Commands, gossip,
sync responses and timers all funnel through one loop, so there are no locks and events
apply in one well-defined order.

---

## 8. Encoding

[postcard](https://github.com/jamesmunns/postcard): no field names, no map key ordering,
varint integers. Canonical *by construction*, which is what signatures require — one
value, one representation, always. The single rule callers must respect is that signed
structs contain no `HashMap`/`HashSet`, whose iteration order is not stable; every signed
type uses sorted `Vec`s instead.

Identifiers serialise as raw bytes in binary formats and as hex in human-readable ones, so
JSON debugging output stays readable at no cost on the wire.

**postcard is Rust-specific.** That is acceptable while every implementation is Rust, and
the encoding is deliberately isolated in one module: moving to DAG-CBOR is a change to
`codec.rs` plus a version bump, not a change to every call site.

---

## 9. Choices, with the alternatives

**Rust.** The only language where the same protocol core compiles to a native desktop app,
a WebAssembly web client, and a mobile library behind an FFI shim. Go's libp2p is more
mature but its mobile and WASM story is weaker; TypeScript would make the browser client
easy and everything else hard.

**libp2p over iroh.** iroh is genuinely better at NAT traversal (~95% vs ~70%) and much
simpler to use. It was set aside because that reliability comes partly from relay servers
operated by a company — self-hostable, but a project whose pitch is "no company-controlled
infrastructure" should not ship pointing at any by default. Worth revisiting:
[libp2p-iroh](https://github.com/rustonbsd/libp2p-iroh) offers iroh's QUIC transport under
libp2p's interfaces, which could give both.

**redb over SQLite.** Pure Rust, no C dependency, so a node is a single static binary on
every platform and cross-compiling to mobile is uneventful. SQLite would give better
ad-hoc querying, which will matter once there is full-text search.

**Per-author chains plus a Lamport DAG, over a general CRDT.** Automerge or Yjs would
work, but chat is append-only: a general-purpose sequence CRDT would pay for edit
operations nobody performs. The chain also produces the frontier, which is what makes sync
one small round trip.

**Gossipsub mesh parameters are tuned down** (`mesh_n_low = 1`, flood publish on).
Defaults assume large topics and would leave a three-member community permanently
under-provisioned.

---

## 10. Known limits

Stated plainly, because a decentralised system that overstates its guarantees is worse
than a centralised one that does not.

**Scaling.** Every member stores everything, forever. Sync scans the order index from the
beginning each round — fine for thousands of events, not for millions. Presence is O(members).
None of this is a problem at community scale; all of it needs work before it is a problem.

**Reachability.** No relay, no hole punching, no DHT. Nodes behind strict NATs will not
connect across the internet yet. Members find each other from an invite plus presence,
which suits a community but will not find arbitrary peers on the open internet.

**Trust.** Communities are open — the invite is a social gate, not a cryptographic one.
There are no roles or permissions. Equivocation is *detected* and rejected locally, but
there is no protocol for telling other members about it, so a misbehaving node is refused
rather than expelled.

**Privacy.** Events are signed but **not encrypted at rest or end-to-end**. Transport is
encrypted by libp2p's Noise handshake, so nothing is in the clear on the wire, but every
member can read all history — which is what "members host the community" means, and is
also why group encryption (MLS) is the obvious next cryptographic step.

**Deletion.** There is none. An append-only log replicated to every member is exactly the
wrong shape for "delete this message", and getting that right — tombstones, history
horizons, what a member who kept a copy is expected to do — is a real design problem, not
an implementation gap.
