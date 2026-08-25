//! `kahui` — the command line client.
//!
//! A thin shell over [`kahui_node`]. Everything it does goes through
//! [`kahui_node::NodeHandle`], the same interface a desktop, web or mobile
//! client would use, so nothing about the protocol is encoded in this binary.
//!
//! It runs a node and gives you a prompt. There is no daemon to install and no
//! service to point it at: the process you start *is* your share of the
//! communities you belong to, and closing it takes that share offline until you
//! start it again.

mod session;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use kahui_node::{NetConfig, Node, NodeConfig, Reachability};
use tracing_subscriber::EnvFilter;

/// Directory used when neither `--data-dir` nor `KAHUI_DATA_DIR` is set.
fn default_data_dir() -> PathBuf {
    directories::ProjectDirs::from("nz", "Kahui", "Kahui")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".kahui"))
}

#[derive(Parser, Debug)]
#[command(
    name = "kahui",
    version,
    about = "Kahui — an open source, decentralised alternative to Discord",
    long_about = "Kahui communities are hosted by their members. Running this \
program makes your machine part of every community you have joined: it stores \
the history, serves it to other members, and keeps working when any of them \
go offline. There is no server to sign in to.\n\nFree software under the GNU \
Affero General Public License, version 3 or later. If you run a modified \
version of this program for others to use, you must offer them its source."
)]
struct Cli {
    /// Where this node keeps its key and its copy of history.
    ///
    /// One directory per node. Point two invocations at two directories to run
    /// two independent members on one machine.
    #[arg(long, env = "KAHUI_DATA_DIR", global = true)]
    data_dir: Option<PathBuf>,

    /// Display name shown to other members.
    #[arg(long, env = "KAHUI_NAME", global = true)]
    name: Option<String>,

    /// Port to listen on. Zero lets the operating system choose.
    ///
    /// The default is fixed rather than random on purpose: a forwarded port is
    /// only useful if it is the same one next time. Zero is for running a second
    /// node on a machine that already has one.
    #[arg(long, default_value_t = kahui_net::DEFAULT_PORT, global = true)]
    port: u16,

    /// Turn off local network discovery.
    #[arg(long, global = true)]
    no_mdns: bool,

    /// Do not carry traffic for other members, and do not ask them to carry
    /// ours.
    ///
    /// Relaying is how members behind home routers stay reachable, so turning
    /// it off makes this node a worse neighbour. Reasonable on a metered
    /// connection; not otherwise.
    #[arg(long, global = true)]
    no_relay: bool,

    /// Do not ask the router to forward a port.
    ///
    /// Asking is the cheapest way to become reachable, and there is no downside
    /// beyond the request itself. Worth turning off only where the network
    /// administrator would rather you did not.
    #[arg(long, alias = "no-upnp", global = true)]
    no_port_map: bool,

    /// Treat private addresses as real ones.
    ///
    /// On an isolated network — a hall, a building, a neighbourhood mesh with
    /// no internet at all — a LAN address is the only address there is, and a
    /// node holding one is perfectly reachable by its neighbours.
    #[arg(long, global = true)]
    lan: bool,

    /// Say whether this node can be dialled, instead of working it out.
    ///
    /// Left alone, peers are asked to dial back and the answer follows from
    /// what they find. Worth setting if you already know: `direct` for a
    /// machine with an open port, `nat` behind carrier-grade NAT where probing
    /// only wastes time.
    #[arg(long, global = true, value_name = "auto|direct|nat")]
    reachable: Option<ReachabilityArg>,

    /// Dial this multiaddress at startup. May be repeated.
    #[arg(long = "connect", value_name = "MULTIADDR", global = true)]
    connect: Vec<String>,

    /// Emit newline-delimited JSON instead of prose, for scripting.
    #[arg(long, global = true)]
    json: bool,

    /// Run a session command at startup, before reading input. May be repeated.
    ///
    /// Anything valid at the prompt works here, which is how the demo script
    /// drives nodes without a human at the keyboard.
    #[arg(long = "exec", value_name = "COMMAND", global = true)]
    exec: Vec<String>,

    /// Exit after this many seconds. Without it, the node runs until you quit.
    #[arg(long = "for", value_name = "SECONDS", global = true)]
    run_for: Option<u64>,

    #[command(subcommand)]
    command: Option<Command>,
}

/// How `--reachable` is spelled on the command line.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum ReachabilityArg {
    /// Let peers work it out. The default.
    Auto,
    /// Anybody can dial us.
    Direct,
    /// A router is in the way; go and find a member to relay.
    Nat,
}

impl ReachabilityArg {
    fn resolve(self) -> Option<Reachability> {
        match self {
            ReachabilityArg::Auto => None,
            ReachabilityArg::Direct => Some(Reachability::Direct),
            ReachabilityArg::Nat => Some(Reachability::BehindNat),
        }
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Start the node and open a session. This is the default.
    Run,
    /// Print this node's identity and exit.
    Id,
    /// Show what an invite actually contains, and whether it can work.
    Inspect {
        /// A `kahui1…` code or a `kahui://join/…` link.
        invite: String,
    },
    /// Check whether people can reach this machine, and say what to change.
    Doctor,
}

impl Cli {
    fn data_dir(&self) -> PathBuf {
        self.data_dir.clone().unwrap_or_else(default_data_dir)
    }

    fn net_config(&self) -> Result<NetConfig> {
        // Both transports, and both IP versions. Costs nothing, and a peer
        // blocked from one still has a way in — IPv6 in particular often has no
        // NAT in front of it, which makes it the easiest path of the four.
        let listen = kahui_net::default_listen_addrs(self.port);
        Ok(NetConfig {
            listen,
            enable_mdns: !self.no_mdns,
            enable_relay: !self.no_relay,
            enable_port_mapping: !self.no_port_map,
            lan_reachable: self.lan,
            ..NetConfig::default()
        })
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Quiet by default; `RUST_LOG=kahui_node=debug` turns the machinery on.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let data_dir = cli.data_dir();
    let config = NodeConfig {
        data_dir: data_dir.clone(),
        display_name: cli.name.clone(),
        net: cli.net_config()?,
        bootstrap: cli.connect.clone(),
        reachability: cli.reachable.and_then(ReachabilityArg::resolve),
        ..NodeConfig::default()
    };

    let node = Node::spawn(config)
        .await
        .with_context(|| format!("starting a node in {}", data_dir.display()))?;

    match cli.command {
        Some(Command::Id) => {
            if cli.json {
                let status = node.status().await?;
                println!("{}", serde_json::to_string(&status)?);
            } else {
                println!("user id : {}", node.user());
                println!("peer id : {}", node.peer_id());
                println!("data dir: {}", data_dir.display());
            }
            node.shutdown().await.ok();
            Ok(())
        }
        Some(Command::Doctor) => {
            // The port actually bound, which is not always the one asked for:
            // a second node on one machine falls back to an OS-assigned one,
            // and reporting the wrong number would send somebody off to write
            // a firewall rule that does nothing.
            let port = node
                .status()
                .await
                .ok()
                .and_then(|status| {
                    status.listen_addrs.iter().find_map(|addr| {
                        addr.rsplit("/tcp/")
                            .next()
                            .and_then(|t| t.parse::<u16>().ok())
                    })
                })
                .unwrap_or(cli.port);
            println!("checking… (a few seconds)\n");
            let d = kahui_net::diagnose(port).await;

            let row = |label: &str, value: &str| println!("{label:<9} {value}");
            row("router", d.gateway.as_deref().unwrap_or("not found"));
            row("this pc", d.local.as_deref().unwrap_or("no address"));
            row("port", &port.to_string());
            println!();

            let outcome = |a: &kahui_net::Attempt| {
                format!("{} — {}", if a.worked() { "yes" } else { "no" }, a.detail())
            };
            row("PCP", &outcome(&d.pcp));
            row("NAT-PMP", &outcome(&d.natpmp));
            row("UPnP", &outcome(&d.upnp));
            row(
                "IPv6",
                &if d.global_ipv6.is_empty() {
                    "no public address".to_string()
                } else {
                    d.global_ipv6.join(", ")
                },
            );

            println!("\n{}", d.advice());
            node.shutdown().await.ok();
            Ok(())
        }
        Some(Command::Inspect { invite }) => {
            let invite = kahui_node::Invite::decode(&invite)?;
            println!("community : {} ({})", invite.name, invite.community.short());
            println!("peers     : {}", invite.peers.len());
            for peer in &invite.peers {
                println!("  {}", peer.peer_id);
                for addr in &peer.addrs {
                    println!("    {addr}");
                }
            }
            if invite.reachable_beyond_lan() {
                println!(
                    "
Usable from the internet."
                );
            } else {
                println!(
                    "
Local network only: every address in it is private or loopback."
                );
            }
            node.shutdown().await.ok();
            Ok(())
        }

        Some(Command::Run) | None => {
            session::run(
                node,
                session::Options {
                    json: cli.json,
                    startup_commands: cli.exec.clone(),
                    run_for: cli.run_for.map(Duration::from_secs),
                    data_dir,
                },
            )
            .await
        }
    }
}
