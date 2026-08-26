# Finding each other

Kāhui works if you can run it on your machine and be reached by somebody running
it on theirs, with nobody typing an address and nobody running a server. This
page is how.

## The problem was never NAT

Most of the work on reachability — port mapping, relays, hole punching — was
answering "how does a packet get to a machine behind a router?" That question has
good answers and Kāhui implements several of them.

But it was the wrong question. A node behind a router is perfectly reachable
*through somebody else*. What it could not do was **find** that somebody, so
every route out of the problem ended in "paste an address from a friend". That is
not a network anybody can join.

## Nodes that can be dialled are the network

Kāhui runs a Kademlia distributed hash table, on its own protocol
(`/kahui/kad/1.0.0`) rather than sharing anybody else's.

Two facts make it fit unusually well:

**A node that can be dialled is exactly a node that can route.** Kademlia needs
dialable nodes to hold its routing table. Circuit relay needs dialable nodes to
carry for everyone else. Those are the same set, so a node joining the table as a
server is announcing itself as a relay in the same breath. Nobody volunteers and
nobody is configured — being reachable is the whole qualification.

**A node that cannot be dialled is still a full participant.** It runs the table
in client mode: it asks any question it likes, it just does not store answers. The
machine behind the worst router in the country still looks up a community and
still finds somebody to carry for it.

What that buys:

| Question | How it is answered |
| --- | --- |
| Who can carry for me? | Reachable nodes all publish themselves under one well-known key. One lookup. |
| Who has community X? | Members publish themselves under the community's id. The id is a 32-byte hash, so it makes a good key with no further work. |
| Where is a member now? | Whatever address they last published, including a relay circuit — so a member behind a router is found and dialled like anybody else. |

## An invite is a name, not a route

Because a community id is a lookup key, it is a complete invite on its own:

```
kahui inspect kahui://join/kahui1…
community : Games (fbe320b5)
id        : fbe320b58b43b05c234e8f4973fc9f402d8a3de046010611f26ce05431b9c008
```

That id can be pasted into `/join`, into the app, or into a `kahui://join/…`
link, and it works. Invites still carry addresses because they make the first
connection immediate rather than a lookup — but they are a shortcut now, not the
mechanism, and an invite keeps working long after every address in it has changed.

## Two people who can neither of them be dialled

The hard case: Jane and Juan are both behind routers that refuse to open a port,
and the member who introduced them has gone. Neither can accept a connection, so
there is nothing to dial.

They connect anyway, and without anybody in the middle.

A NAT does not block everything — it drops packets from strangers and forwards
packets from people you have already written to. So if Jane sends to Juan at the
same moment Juan sends to Jane, each router sees an outgoing packet first and
treats the other's arriving packet as the reply. Both holes open at once. This is
hole punching, and every peer-to-peer system does it.

What normally makes it need a server is that each side has to know two things:
where the other is, and *when* to fire. Neither has to come from a server:

- **Where** was learned the first time they met. Any peer you talk to can see the
  address your router presents for you and tell you what it is, and that gets
  gossiped like anything else. Jane and Juan met once through Bob, so each holds
  the other's already.
- **When** is not communicated at all — it is *calculated*. Both sides feed the
  same two peer ids and the clock into the same function and get the same
  instant. Jane works out when to dial Juan; Juan independently works out the
  same moment; both fire. Nothing passes between them, which is just as well,
  because if it could they would not need this.

After one meeting, ever, they can re-establish a direct connection for as long as
they both keep running, with nobody in the middle.

Two honest limits. This reconnects people who have met — it is not a way for
strangers to meet. And it does not beat a *symmetric* NAT, which allocates a
different external port per destination, so the address learned through Bob is
not the one Juan would have to aim at. Most home routers are not symmetric; some
are, and for those the relay path remains the answer.

## The one thing that cannot be conjured

A lookup has to be sent somewhere. A program cannot find a stranger on the
internet with no prior information — that is IP, not Kāhui, and it is why
BitTorrent, Bitcoin, Tox and every comparable system ships starting points.

So a node needs **one address, once, ever**. It takes the first of these that
answers:

1. **A peer it has already met.** Remembered across restarts, which covers every
   run after the first.
2. **The local network,** over mDNS. Needs nothing.
3. **An invite.** Invites carry a few reachable nodes alongside the community, so
   being invited is being bootstrapped. This is how the property spreads through
   a social graph without anybody thinking about it.
4. **`seeds.txt`** in the data directory — one address per line, `#` for
   comments. `kahui seeds` shows it, `kahui seeds --add <addr>` appends.

Note what is missing: no name server, no tracker, no rendezvous, nothing owned by
this project. A seed is data in a file, not a service. Losing every seed you have
costs you nothing so long as you have met one peer since.

**The list that ships is empty, on purpose.** Addresses compiled into the binary
would be addresses this project chose, and a network that works because of nodes
we picked is not the network described in the README. The mechanism is there for
whoever builds or runs a copy to fill in.

## Checking it

`Settings → Network` in the app, or `/status` in the CLI, reports how many nodes
this one can route a lookup through. Zero means it has not found the network yet
and cannot be found by community id alone. Anything above zero means it can.

## What this does not do

**It is not anonymity.** The table maps node identities to addresses, and looking
up a community tells you who is publishing it. Anybody can watch that, exactly as
with BitTorrent. Kāhui is not a system for hiding that you are talking; it is a
system for talking without a company in between.

**It is not admission control.** Publishing a community means "I hold this", not
"I vouch for it", and a community id is a public name. Anybody who has the id can
join — the invite is a social gate, not a cryptographic one, and that was already
true before the hash table existed.

**It does not make membership discoverable.** There is no way to list communities:
you cannot ask the table what exists, only whether a specific id has holders. A
community whose id has not been shared is not findable by browsing.
