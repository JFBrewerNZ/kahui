# Hosting from home

The whole point of Kāhui is that a community lives on its members' own machines.
No server to rent, nothing of ours in the middle. This page is about making that
work from a desktop behind a home router.

## The short version

Run `kahui doctor`. It tells you whether people can reach you, and what to change
if they cannot. In the desktop app the same check is under **Settings → Can people
reach you?**

```
router    192.168.1.1
this pc   192.168.1.143
port      4001

PCP       no — Gateway did not respond within the timeout
NAT-PMP   no — Gateway did not respond within the timeout
UPnP      no — 501 Action Failed (answered by 192.168.1.2, which is not your router)
IPv6      no public address

Your router will not open a port. Forward TCP and UDP 4001 to 192.168.1.143,
or connect to a peer who can relay for you.
```

## What happens on its own

A node tries, in this order, without being asked:

| Route | What it is |
| --- | --- |
| **IPv6** | If your connection has a public IPv6 address there is usually no NAT at all, and other IPv6 peers dial you directly. Nothing to configure. |
| **PCP** | RFC 6887. The newer of the two protocols for asking a router to open a port. |
| **NAT-PMP** | RFC 6886. Older, and on plenty of routers it is the only one enabled. |
| **UPnP-IGD** | The one most people have heard of. Tried with a timed lease first, then a permanent one, because many routers only support permanent mappings and answer `501` to anything else. |
| **A relay** | If none of the above works, the network is asked for somebody who *is* reachable, and they carry your traffic until a direct connection can be punched through. You are not asked to find them. |

Router support for the first four is a lottery, so all of them get asked. Any one
succeeding is enough.

## When none of it works

Some routers refuse every method. The one this was developed against does: it
ignores PCP and NAT-PMP entirely, and the only device on the network answering a
UPnP search turns out not to be the router at all. There is no software fix for
that from the inside.

Two things still work.

### Forward the port yourself

Two minutes, once, and then it keeps working.

1. Open your router's admin page — usually the "router" address `kahui doctor`
   printed, in a browser.
2. Find **Port forwarding** (sometimes under Advanced, NAT, or Virtual Server).
3. Forward **TCP 4001** and **UDP 4001** to the "this pc" address `kahui doctor`
   printed.
4. Run `kahui doctor` again to confirm.

Kāhui listens on port 4001 by default and keeps doing so across restarts, which is
what makes a manual rule worth setting. Use `--port` if you need a different one,
and forward that instead.

Give your machine a static DHCP lease while you are in there, or the address the
rule points at can change.

### Let the network carry you

Usually nothing to do. Once your node has found the network at all, it looks up
somebody reachable, takes a reservation from them, and hosts from behind your
router as normal. **Settings → Network** says whether it has.

It only needs one way in, once ever, and takes the first of these that works: a
peer from a previous run, somebody on your own network, an invite you were sent,
or `seeds.txt` in your data directory. If you are the very first person you know
to run Kāhui and nobody has invited you, that last one is where to put an address
— `kahui seeds --add /ip4/…/tcp/4001/p2p/12D3KooW…`, or **Settings → Connect to a
peer**.

After that it is remembered for good, and everything else is found rather than
configured.

## The part that cannot be fixed

A node has to know one address to begin with. Not a member's, not a server's —
anybody's. There is no way around that: finding a stranger on the internet with no
prior information is not something IP can do, which is why every peer-to-peer
system ships starting points. Tox and Briar ship bootstrap lists; Tailscale and
iroh run relays; Scuttlebutt calls them pubs.

What Kāhui does differently is refuse to make that a service. A starting point here
is an address in a text file, it can be any user's machine, and once you have met
anybody at all you never need it again. See [finding each other](discovery.md).

## Checking it from outside

The honest test is another machine on another connection. Failing that, `kahui
doctor` on both ends and an invite passed between them will tell you soon enough:
`kahui inspect <invite>` says whether the addresses in it are usable from the
internet or only from your own network.
