# Running a node on a server

Everything here works today. This is the shortest path from "I have a droplet" to "I am
chatting between my laptop and a machine on the other side of the world."

## Why you would bother

Kāhui has no servers, but it does have a reachability problem, and the two are different
things.

Nodes behind a home router cannot yet accept incoming connections — relay support and
hole punching are not built (see `architecture.md` §10). Outgoing connections work fine.
So today:

| Setup | Works? |
|---|---|
| Two machines on the same network | ✅ found automatically by mDNS |
| Laptop → a machine with a public address | ✅ the laptop dials out |
| Two laptops, different homes, nothing public | ❌ needs relay support |

A cheap VPS with a public IP fixes this for a whole community: **one reachable member is
enough**, because everyone dials it, and once connected the gossip flows in every
direction. That member is not a server. It stores the same events as everyone else,
has no special powers, and can vanish without taking the community with it — it is just
the member most likely to be awake.

## Set it up

On the droplet:

```bash
# Grab the static Linux build — no glibc version to worry about.
VERSION=0.1.0
curl -LO https://github.com/JFBrewerNZ/kahui/releases/download/v$VERSION/kahui-$VERSION-x86_64-unknown-linux-musl.tar.gz
tar xzf kahui-$VERSION-x86_64-unknown-linux-musl.tar.gz
cd kahui-$VERSION-x86_64-unknown-linux-musl
chmod +x kahui

# Kahui speaks TCP and QUIC on the same port number.
sudo ufw allow 4001/tcp
sudo ufw allow 4001/udp

# mDNS finds nothing useful on a cloud VM and chatters pointlessly, so turn it off.
./kahui --data-dir ~/.kahui --name droplet --port 4001 --no-mdns
```

Then, at the prompt:

```
/create Aotearoa
```

It prints an invite. Copy it.

## Connect from your own machine

```
kahui --data-dir ./me --name jamon --port 4001
> /join kahui1XEcg4xA41QNfJis19rY…
> kia ora from the laptop
```

The message appears on the droplet. Type on the droplet, it appears on the laptop. That
is the whole thing working: two independent nodes, two databases, signed messages, no
service in between.

To make it a proper test, add a second droplet, join it to the same invite, then **kill
the first one**. The other two keep talking, and when it comes back it catches up on
everything it missed.

## Keep it running

`tmux` is enough to try things out:

```bash
tmux new -s kahui
./kahui --data-dir ~/.kahui --name droplet --port 4001 --no-mdns
# Ctrl-B then D to detach; `tmux attach -t kahui` to come back
```

For something that survives a reboot, `/etc/systemd/system/kahui.service`:

```ini
[Unit]
Description=Kahui node
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=kahui
ExecStart=/opt/kahui/kahui --data-dir /var/lib/kahui --name droplet --port 4001 --no-mdns
Restart=always
RestartSec=5

# It needs its own data directory and nothing else on the machine.
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
StateDirectory=kahui

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable --now kahui
journalctl -u kahui -f
```

Under systemd there is no terminal to type at, so the node serves history and relays
messages but cannot post. That is usually what you want from an always-on member. To
drive it interactively, run it under `tmux` instead.

## Things worth knowing

**Back up the data directory, not just the messages.** It holds the node's private key.
Copy it and you have copied that identity — which is the point, but also the risk. There
is nothing to reset if you lose it: an identity that vanishes simply stops posting, and
its past messages remain valid forever.

**Pin the port.** With `--port 0` the OS picks a new one each restart, and peers holding
your old address have to rediscover you. A fixed port makes a returning node reappear
where everyone last saw it.

**Check it is reachable.** From your laptop:

```bash
nc -vz YOUR_DROPLET_IP 4001
```

If that fails, the firewall is the first suspect — the cloud provider's as well as the
machine's own.

**Watch what it is doing.** `RUST_LOG=kahui_node=debug ./kahui …` turns on the
machinery: sync requests, presence, gossip. Ordinary output stays on stdout, logs go to
stderr, so you can separate them.
