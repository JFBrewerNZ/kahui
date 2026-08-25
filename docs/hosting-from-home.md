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
| **A relay** | If none of the above works, a member who *is* reachable carries your traffic until a direct connection can be punched through. |

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

### Let a member relay for you

If you would rather not touch the router, you need one reachable node — anyone in
the community whose `kahui doctor` says yes. Connect to them once:

**Settings → Connect to a peer**, then paste their address:

```
/ip4/203.0.113.4/tcp/4001/p2p/12D3KooW…
```

That node is remembered for good, across restarts and across every community, and
you can host from behind your router as normal. Your invites will advertise a route
through them until a direct connection takes over.

## The part that cannot be fixed

If two people are both behind routers that refuse everything, and they have never
met anything reachable, they cannot find each other. That is not a Kāhui
limitation — no peer-to-peer system does it, because there is nowhere for the first
packet to go. Tox and Briar ship lists of bootstrap nodes; Tailscale and iroh run
relays; Scuttlebutt calls them pubs. All of it is somebody's infrastructure.

Kāhui's position is that the somebody should be another member, not us. So one
participant has to be reachable. Forwarding a port is the cheapest way to be that
participant, and it only takes one person per community.

## Checking it from outside

The honest test is another machine on another connection. Failing that, `kahui
doctor` on both ends and an invite passed between them will tell you soon enough:
`kahui inspect <invite>` says whether the addresses in it are usable from the
internet or only from your own network.
