# Kāhui without the internet

The scenario: a city loses its internet. People are still standing next to each other,
still have phones and laptops, still have radios in those devices that work over a few
tens of metres. Can a message get from one side of the city to the other?

The answer is yes in principle and partly yes today, and the interesting part is *why* —
because most of the work turns out to be already done, and the remaining work is not the
part people expect.

---

## 1. The protocol is already delay-tolerant

This is the thing worth understanding before anything else.

A Kāhui community is an append-only log of signed, content-addressed events. Three
consequences fall out of that, none of them designed with disaster in mind:

**Any member can carry any other member's messages.** An event proves its own authorship,
so a node can accept history from a peer it has never met, about an author it has never
met, and verify every byte of it. Nobody has to be trusted to pass a message on faithfully
— they *cannot* alter it.

**Sync is a frontier exchange, not a session.** "Here is everything I have, send me what I
am missing" is one round trip, a few dozen bytes per member, and it works identically
whether the two nodes last spoke five seconds ago or five months ago. There is no
connection state to keep, nothing to resume, nothing that expires.

**Ordering does not need a clock anybody trusts.** Lamport clocks tie-broken by event id
give every node the same transcript, no matter what order things arrived in or how long
they took. A message that takes six hours to cross town lands in the right place.

Put together, that is a [delay-tolerant network](https://en.wikipedia.org/wiki/Delay-tolerant_networking)
without having set out to build one. And it means **no routing is required**. Nobody needs
to know a path across the city. A message spreads epidemically: A syncs with B when they
are near each other, B syncs with C an hour later, and C now has A's message. The delay is
however long it takes people to move around.

This is already tested, over real sockets, in
[`crates/kahui-node/tests/carry_and_relay.rs`](../crates/kahui-node/tests/carry_and_relay.rs):
a message crosses a chain of four nodes where each one shuts down before the next appears,
so no path between the first and the last ever exists. It arrives anyway, with the whole
community — founder, channels, membership — intact behind it.

**So the missing piece is not the protocol. It is radios.**

---

## 2. What works today

| Situation | Works? | How |
|---|---|---|
| Same WiFi network | ✅ today | mDNS finds everyone with no configuration |
| Someone runs a hotspot, others join it | ✅ today | that is just a WiFi network |
| Two homes, one has a public address | ✅ today | relay + hole punching, since v0.3 |
| Two homes, neither reachable, but a mutual member is | ✅ today | that member relays for both |
| Two homes, no internet at all between them | ⚠️ manual | works if the machines can reach each other on *some* IP network |
| Across town, over Bluetooth | ❌ not yet | see below |
| Across town, hop by hop over days | ✅ in protocol, ❌ in transport | carrying is proven; the radios are not wired up |

Run a node with `--lan` on an isolated network and it stops treating private addresses as
a problem to route around — on a network with no internet, `192.168.1.5` *is* the real
address, and a node holding one is perfectly reachable by its neighbours.

The practical off-grid setup today is therefore: **a WiFi access point with no uplink.**
A router with its WAN cable unplugged, or a phone hotspot, or an old laptop running one.
Everyone in range joins it, Kāhui discovers everyone by mDNS, and the community works
completely — no internet involved at any point. Carry a laptop to the next hall and it
syncs there too, spreading whatever it picked up.

That is not a metaphor for the city mesh. It is the city mesh, with the hops done on foot.

---

## 3. Bluetooth: the honest state of it

Bluetooth is the obvious answer for hop-by-hop connectivity between neighbours, and it is
the piece that is genuinely not ready. Not because it is hard in principle, but because of
where the tooling is.

For two devices to find and talk to each other over BLE, **both need to advertise
(peripheral role) and to scan and connect (central role)**. A library that only does one
half cannot do peer-to-peer at all — it can only talk to dedicated hardware.

| Library | Central | Peripheral | Platforms |
|---|---|---|---|
| [`btleplug`](https://github.com/deviceplug/btleplug) | ✅ | ❌ *explicitly out of scope* | Windows, macOS, Linux, Android |
| [`bluest`](https://lib.rs/crates/bluest) | ✅ | ❌ | Windows, macOS, Linux |
| [`bluer`](https://lib.rs/crates/bluer) | ✅ | ✅ | **Linux only** |
| [`blew`](https://crates.io/crates/blew) | ✅ | ✅ | macOS, iOS, Android, Linux — **no Windows** |

So: **there is currently no Rust library that does peer-to-peer BLE on Windows.** Two
Windows laptops cannot talk over Bluetooth through any crate available today, and pretending
otherwise by shipping something half-working would be worse than saying so.

Two further things worth knowing before treating BLE as the answer:

- **Throughput is low.** GATT realistically manages single-digit kilobytes per second.
  L2CAP connection-oriented channels do much better and `blew` supports them. Text messages
  are tiny, so this is survivable — but syncing a year of history over BLE is not something
  to design around.
- **Range is short.** Tens of metres, less through walls. A city mesh over BLE means a
  *lot* of hops, which is exactly why the epidemic model above matters more than routing.

**Where Bluetooth actually pays off is mobile**, and that is not a coincidence. Phones are
what people carry around a city; laptops are not. And the mobile platforms have proper
peer-to-peer APIs built for this — Android's Nearby Connections and Wi-Fi Aware, iOS's
MultipeerConnectivity — which handle discovery, role negotiation and transport selection
far better than raw BLE.

So the plan is: **Bluetooth arrives with the mobile client, not before it.** Doing it on
desktop first would mean the worst platform coverage, the worst radios, and the fewest
devices that actually move around.

---

## 4. What a transport needs to provide

The useful consequence of §1 is that adding a transport is not a protocol change. Sync
needs one thing:

> a way to send some bytes to a peer and get some bytes back, occasionally, unreliably

That is it. `SyncRequest::GetDelta { community, have, limit }` in, `SyncResponse::Delta`
out. No ordering, no sessions, no liveness, no addressing scheme beyond "this thing in
front of me". A BLE characteristic can carry it. So can a serial cable, a QR code on a
screen, or a USB stick walked between two buildings.

Today that exchange is bolted to libp2p's request-response, which assumes a libp2p
connection. Making it transport-agnostic is the piece of real work that has to happen
before any of the radio options can be plugged in, and it is a refactor of one module
rather than a redesign.

Roughly, in the order they should happen:

1. **Lift sync off libp2p** — express it as a byte-pipe exchange that libp2p is merely one
   implementation of.
2. **A local-transport trait** — discover, connect, exchange, disconnect. Implementations
   for BLE, and for whatever the mobile platforms offer natively.
3. **A carrying policy** — decide how much history to hold for communities you are not a
   member of. Carrying strangers' messages is what makes a mesh work, and it is also how
   a device fills its disk. The events are signed, so there is no risk in carrying them;
   the question is purely one of storage budget.
4. **Mobile client** — where the radios and the moving-around actually are.

---

## 5. Limits worth stating

The scenario is genuinely achievable, and it will not be magic.

**Latency is human-scale.** A message crossing a city hop by hop travels at the speed of
people walking and cycling. Minutes to hours, not milliseconds. The protocol handles that
correctly — ordering does not depend on time — but a chat window that expects instant
replies will feel strange, and the interface should probably say when a message is old.

**Storage grows without bound.** Every member currently keeps every event forever. That is
fine for a year of a small community and untenable for a city. History horizons and
selective carrying are needed before this scales, and neither is built.

**Discovery is the hard part, not delivery.** Two nodes that can see each other sync
easily. Two nodes that cannot see each other need somebody to physically move between
them. A mesh is only as good as the density of people carrying it.

**Nothing here provides anonymity.** Events are signed, and signatures identify their
author to every member. Being on a mesh in an emergency is not the same as being
unobservable on one, and it should not be sold as such.

---

## 6. Where this leaves things

The part people assume is hard — getting a message across a network that never existed all
at once — is done, tested, and falls out of decisions made for other reasons. The part
people assume is easy — getting two devices in the same street to notice each other
without any infrastructure — is the actual work, and most of it is not ours to do: it
waits on platform APIs and on a mobile client to use them.

In the meantime, the honest summary is: **a Kāhui community works completely on a WiFi
network with no internet behind it, and messages spread between such networks whenever a
member moves between them.** That is a real answer to "the internet is gone", and it works
today.
