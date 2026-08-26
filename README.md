# Kāhui

[![CI](https://github.com/JFBrewerNZ/kahui/actions/workflows/ci.yml/badge.svg)](https://github.com/JFBrewerNZ/kahui/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/JFBrewerNZ/kahui?display_name=tag&label=download)](https://github.com/JFBrewerNZ/kahui/releases/latest)
[![Licence](https://img.shields.io/badge/licence-AGPL--3.0--or--later-f4c65a)](LICENSE)

**An open-source, decentralised alternative to Discord where communities are hosted by their members, not by a company.**

When you join a Kāhui server, your device becomes part of that community's network.
Members collectively store message history, share data, and keep the server alive.
There is no central database, no login server, and no company-controlled infrastructure
holding everyone's conversations.

If the Kāhui project, its website, and its developers disappeared tomorrow, existing
communities would keep running.

> **Status: milestone 3 — reachable from anywhere.** The protocol, storage, networking and
> node engine are real and tested, there is a desktop app alongside the command line
> client, and nodes behind home routers now reach each other by relaying through members
> who are reachable. No voice, roles or moderation yet.
> See [What is not built yet](#what-is-not-built-yet).

---

## Download

[**Get the latest release →**](https://github.com/JFBrewerNZ/kahui/releases/latest) ·
[project site](https://jfbrewernz.github.io/kahui/)

**The app** — a window, for using Kāhui.

| Platform | File |
|---|---|
| Windows | `.msi` or `-setup.exe` |
| macOS | `.dmg` (Apple Silicon or Intel) |
| Linux | `.AppImage` or `.deb` |

**The command line client** — one binary, no window. For servers and for scripting.

| Platform | File |
|---|---|
| Windows | `…-x86_64-pc-windows-msvc.zip` |
| Linux | `…-x86_64-unknown-linux-musl.tar.gz` — statically linked, runs on any distribution |
| macOS (Apple Silicon) | `…-aarch64-apple-darwin.tar.gz` |
| macOS (Intel) | `…-x86_64-apple-darwin.tar.gz` |

No account either way. Windows and macOS will warn that the download is unsigned — there
is no code-signing certificate, because buying one would put a company between you and
the software. Every release ships `SHA256SUMS`, and every artifact is built from its tag
by [a workflow you can read](.github/workflows/release.yml).

Both clients share one data directory, so they are the same member of the same
communities — which also means only one of them can run at a time.

---

## Try it

Everything below runs on one machine. Nothing contacts the internet.

```bash
# Prove the whole milestone in one go: three nodes, three databases, real sockets.
bash scripts/demo.sh
```

The script founds a community on node A, joins B and C to it from A's invite, has all
three exchange messages, **shuts A down** while B and C keep talking, then **restarts A**
and shows it catching up on what it missed. It fails loudly if any of that does not
happen.

To do it by hand, in three terminals:

```bash
cargo build --release

# Terminal 1 — Alice founds a community and prints an invite
./target/release/kahui --data-dir ./alice --name alice --port 4001
> /create Aotearoa
invite: kahui1XEcg4xA41QNfJis19rYRdWU6SPqQuN1dGbfR1HAq3o4Q9nSATPx7q…

# Terminal 2 — Bob joins with that invite
./target/release/kahui --data-dir ./bob --name bob --port 4002
> /join kahui1XEcg4xA41QNfJis19rYRdWU6SPqQuN1dGbfR1HAq3o4Q9nSATPx7q…
> kia ora

# Terminal 3 — Carol does the same
./target/release/kahui --data-dir ./carol --name carol --port 4003
> /join kahui1XEcg4xA41QNfJis19rYRdWU6SPqQuN1dGbfR1HAq3o4Q9nSATPx7q…
```

Now close Alice's terminal. Bob and Carol keep talking. Start Alice again with the same
`--data-dir` and she catches up.

Type `/help` at the prompt for the full command list.

---

## What this actually is

A community is an **append-only log of signed events**, replicated in full to every
member. Creating a channel, joining, posting a message — each is one event, signed by
its author with a key that never leaves their machine.

That single decision is what removes the server. Because every event proves its own
authorship, a node can accept history from *any* peer without trusting that peer. There
is nothing left for a central authority to do:

| Job a Discord server does | How Kāhui does it instead |
|---|---|
| Stores message history | Every member stores all of it |
| Says who you are | Your keypair does; there is no account |
| Decides message order | A Lamport clock every node computes identically |
| Says what is authentic | The author's signature on each event |
| Relays messages | Members gossip directly to each other |
| Serves history to newcomers | Any member who has it |

### How a node catches up

Each member keeps a hash-chained log per community, numbered from zero. So "everything I
have from Alice" is a single number, and a node's whole state is one number per member —
a *frontier*.

Catching up is one round trip: send your frontier, receive what it does not cover. The
request is a few dozen bytes per member no matter how much history exists, so it costs
the same after five minutes offline as after five months.

### Invites are links

An invite is a code (`kahui1…`) or the same thing as a link (`kahui://join/kahui1…`).
The desktop app registers the `kahui://` scheme, so clicking one opens the app on that
community — and if it is already running, the link goes to the open window rather than
starting a second copy. Both forms are accepted anywhere an invite is asked for.

### Anyone can run it, from anywhere

Most people are behind a home router, which means nothing can dial them. A node in that
position could still read and post, but it could not serve history to anybody — and
serving history is the half of the bargain that keeps a community alive when other members
go offline. A network of spectators is not a network.

So members carry for each other. Each node asks its peers to dial it back (AutoNAT); if
nobody can, it asks the network for somebody who *is* reachable, takes a relay reservation
from them, and advertises the resulting address alongside its direct ones. The moment a relayed connection exists,
[DCUtR](https://libp2p.io/docs/dcutr/) uses it to coordinate a hole punch, and the relay
drops out of the path.

Before any of that, the node tries to be reachable on its own account. It listens on IPv6
as well as IPv4 — a connection with a public IPv6 address usually has no NAT in front of it
at all — and asks the router to open a port three different ways: PCP, NAT-PMP and
UPnP-IGD. Router support is a lottery, and one that refuses one protocol often accepts
another, so all three get asked. When any of them works there is no relay and no hole
punch, just a reachable node. The rest is the fallback.

`kahui doctor` reports which of those worked, and what to change if none did.

The relay is a member of the community, not a service. Nothing is configured, nothing is
hosted, and the node doing the carrying is running exactly the same binary as everyone
else. One reachable member is enough for a community of people who are not.

**So, concretely:**

| Two people, both at home, no server anywhere | Result |
|---|---|
| Either has a public IPv6 address | ✅ dialled directly, nothing to configure |
| Either router accepts PCP, NAT-PMP or UPnP | ✅ that node is reachable; the other dials it |
| Neither does, but one forwards port 4001 by hand | ✅ same, and it survives restarts |
| Neither, but *anybody* on the network is reachable | ✅ found through the hash table, relayed, then usually hole punched |
| Nobody either of them can reach has ever been seen | ❌ nowhere to send the first packet |

The fourth row is the one that changed in 0.8, and it is the one that matters. It used to
say "a third *member*", and somebody had to introduce you to them by hand. Now every node
that can be dialled joins a Kademlia distributed hash table and offers to carry for the ones
that cannot, so a node behind a router looks up somebody reachable, gets carried by them, and
becomes reachable itself. Nobody configures anything.

That also means an invite no longer has to contain an address. A community id **is** the
invite — `kahui inspect` prints it — and unlike an address it never goes stale.

The last row is the honest limit, and it is IP's rather than Kāhui's: a program cannot find
a stranger on the internet with no prior information, which is why BitTorrent, Bitcoin and
Tox all ship starting points. Kāhui needs one address once, ever, and takes it from whichever
comes first — a peer met on a previous run, the local network, an invite (which carries
entry points), or `seeds.txt`. None of those is a service, and none of them is ours.

See [finding each other](docs/discovery.md) for how that works,
[hosting from home](docs/hosting-from-home.md) for routers that refuse everything, or
[running on a server](docs/running-on-a-server.md) if you would rather a VPS did it.

**A household where one device has a connection and the others do not** works today, and
not by routing: the connected device is an ordinary member that happens to hold the
events, and gossip and sync carry them onward exactly as they would to anybody else. It
also ends up relaying for the others without being asked. There is a test for it.
[Why there is no routing layer](docs/architecture.md#5c-paths-and-why-there-is-no-routing)
explains what that does and does not buy you.

### Why the founder is not special

New members initially know only whoever invited them. Every member periodically announces
its addresses to the community, and announces immediately when someone new subscribes, so
within a second or two everyone has a direct path to everyone else.

By the time the founder disconnects, nobody is routing through them. The demo asserts
this — it checks B and C are directly linked *before* it kills A, because otherwise
"the community survives" would be luck rather than design.

### What ordering does and does not promise

Messages you wrote after seeing someone else's always sort after theirs, on every node.
Messages written concurrently are tied, and the tie is broken by event id — identical on
every node, but unrelated to who pressed enter first.

There is no way to do better without a clock someone has to be trusted to keep, which is
the thing we are trying not to need.

---

## The stack, and why

**Rust** for the whole thing. It is the only language where the same protocol core can be
compiled into a native desktop app, a WebAssembly web client, and a mobile library behind
an FFI shim — which is exactly what "one network, many clients" requires.

| Layer | Choice | Why |
|---|---|---|
| Networking | [rust-libp2p](https://github.com/libp2p/rust-libp2p) 0.56 | Gossipsub, NAT traversal, QUIC and TCP, no default dependency on anybody's servers |
| Storage | [redb](https://github.com/cberner/redb) 2.x | Embedded, ACID, pure Rust — so a node is one static binary, and cross-compiling to mobile is uneventful |
| Signatures | Ed25519 (`ed25519-dalek`) | Small keys, fast verification, and the same key doubles as the libp2p transport identity |
| Hashing | BLAKE3 | Fast, 32-byte content addresses |
| Encoding | [postcard](https://github.com/jamesmunns/postcard) | Canonical by construction: no field names, no map ordering, so every node derives byte-identical signing input |

**[iroh](https://www.iroh.computer/) was the main alternative** and is genuinely better at
NAT traversal (~95% vs libp2p's ~70%). It was set aside because its reliability comes
partly from relay servers operated by a company. They are self-hostable, but a project
whose pitch is "no company-controlled infrastructure" should not ship pointing at any by
default. This is worth revisiting: a libp2p transport backed by iroh
([libp2p-iroh](https://github.com/rustonbsd/libp2p-iroh)) could give us both.

---

## How it is put together

Layered so that the parts a web or mobile client cannot reuse are the *only* parts it has
to replace.

```
desktop/       the app: a Svelte window over a Tauri shell
kahui-cli      the terminal client
               ^ two front ends, one node API, no protocol knowledge in either
kahui-node     the engine: commands in, events out. What every client drives.
kahui-net      libp2p: gossip, presence, the sync protocol
kahui-store    a storage trait, plus an embedded redb implementation
kahui-proto    identities, signed events, canonical encoding — no IO at all
```

The desktop app runs a full node **in its own process**. There is no local server, no
port to bind, and no second thing to install: the window talks to the node over Tauri's
IPC, and every one of its commands is a one-line forward to `NodeHandle`. That the CLI
and the app are interchangeable front ends is the test of whether the layering is real.

Each layer only knows about the ones below it.

- **`kahui-proto`** has no IO, no async, no networking — just pure functions over plain
  data. A browser client compiled to WebAssembly shares this crate byte for byte, which
  is what makes it the same network rather than a lookalike.
- **`kahui-store`** is a trait because the storage engine is the part that genuinely
  cannot be shared. Desktop uses redb; a web client will implement the same trait over
  IndexedDB, and nothing above it changes.
- **`kahui-node`** exposes `NodeHandle` — a clonable, thread-safe handle with async
  methods and an event stream. A Tauri app, a mobile FFI shim and this CLI would all
  drive exactly this.

The CLI is deliberately thin: it renders events and forwards commands. Nothing about the
protocol is encoded in it.

See [`docs/architecture.md`](docs/architecture.md) for the event format, the sync
protocol, and the reasoning behind each choice, and
[`docs/off-grid.md`](docs/off-grid.md) for what happens when there is no internet at all.

---

## Testing

```bash
cargo test --workspace     # 73 tests
bash scripts/demo.sh       # the milestone, live, in three processes
```

CI runs the tests on Linux, Windows and macOS, runs the demo on Linux, and fails the
build if a dependency appears whose licence is not AGPL-compatible.

The interesting tests are:

- **`crates/kahui-store/tests/store.rs`** — replication logic with the network removed.
  Handing a delta between two `Replica`s is exactly what sync does over the wire, so
  convergence, equivocation detection and forged-signature rejection are all provable in
  milliseconds without opening a socket.
- **`crates/kahui-node/tests/three_nodes.rs`** — the milestone over real sockets, mDNS
  off so that discovery must work the way it would on a real network.

---

## What is not built yet

Honestly, so the roadmap is not mistaken for the present tense:

- **No voice, and no video.**
- **No roles, permissions or moderation.** Communities are open; the invite is a social
  gate, not a cryptographic one. The event model has room for capability-based
  permissions, but none are enforced.
- **Reachability needs one reachable member.** Relay and hole punching are wired up, so a
  community works as long as *somebody* in it can be dialled. A community where every
  single member is behind carrier-grade NAT, with no reachable member at all, still
  cannot form. There is no fallback to anybody else's relay infrastructure, deliberately.
- **Bluetooth is not built.** See [running off-grid](docs/off-grid.md) for the honest
  state of it: the protocol is already delay-tolerant and carries messages across nodes
  that were never online together, but no Rust library does peer-to-peer BLE on Windows,
  and Bluetooth properly belongs with the mobile client.
- **No DHT.** Members find each other from an invite plus presence announcements. This is
  fine for a community; it will not scale to finding arbitrary peers on the open internet
  without adding Kademlia.
- **No deletion or history trimming.** Every node keeps everything forever. Real
  communities will need message deletion and history horizons.
- **Equivocation is detected, not punished.** A node that signs two events at the same
  sequence is rejected, but there is no protocol for telling everyone else about it.
- **`postcard` is Rust-specific.** Fine while every implementation is Rust; a
  language-neutral canonical encoding (DAG-CBOR) is the obvious replacement, and the
  encoding is isolated in one module so that swap is contained.

---

## Notes for running elsewhere

**Linux, including a cloud VM:** see [running on a server](docs/running-on-a-server.md)
for the full walkthrough — firewall, systemd unit, and how to test between a laptop and a
droplet. Short version: open your `--port` on both TCP and UDP, and pass `--no-mdns`,
since local discovery finds nothing on a cloud VM but still chatters.

A machine with a public address is worth having in a community *today*, because relay
support is not built yet and nodes behind home routers cannot accept incoming
connections. One reachable member is enough for everyone else to dial in. That member is
not a server — same events, same powers, and the community survives losing it — it is
just the member most likely to be awake.

**Windows + Git Bash:** two things bite, and `scripts/demo.sh` handles both.

- Git Bash rewrites arguments that look like Unix paths, turning `/create` into
  `C:/Program Files/Git/create`. Set `MSYS_NO_PATHCONV=1`. (The CLI detects this and says
  so, rather than posting the mangled path as a chat message.)
- `kahui.exe` is a native Windows binary, so a Unix-style path like `/c/Users/...` is read
  as *root of the current drive* and silently resolves somewhere else. Use a Windows path
  or `cygpath -w`.

---

## Licence

**GNU Affero General Public License, version 3 or later** ([LICENSE](LICENSE)).

Kāhui exists so that a community is not captured by whoever happens to host it. The AGPL
is the same idea applied to the code. Its section 13 is the part that matters: **if you
run a modified Kāhui as a network service, you must offer its source to the people using
it.** The ordinary GPL lets a company take a project, improve it privately and run it as
a service without giving anything back. The AGPL closes that.

What it does and does not do, plainly:

- ✅ Anyone may use, study, modify and share it, including a community running its own
  fork.
- ✅ Any modified version anyone runs for others must be published, so a proprietary
  fork of Kāhui cannot exist.
- ✅ Clients for other platforms must also be AGPL — there is no proprietary Kāhui app.
- ⚠️ It does **not** forbid charging money. Nothing open source can: the Open Source
  Definition explicitly bars discriminating against commercial use, so a licence can be
  open source or it can ban commercial gain, not both. What the AGPL does instead is
  remove the *point* of commercial capture — anyone charging to host Kāhui must hand
  every user the complete source, which anyone else can then run for free.

That is a deliberate trade. A genuine non-commercial licence (PolyForm Noncommercial, say)
would have cost the project the ability to call itself open source, kept it out of Linux
distributions, deterred contributors, and — awkwardly — forbidden a community co-op from
charging its own members to cover server costs.

If you want stronger than this, the usual route is to keep copyright and sell commercial
exceptions alongside the AGPL. That needs a contributor licence agreement, which is
friction on outside contributions, so it is worth deciding before the project has any.

### Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). In short: contributions are AGPL-3.0-or-later,
there is deliberately no contributor licence agreement, and every dependency must be
licence-compatible — CI enforces the last one.
