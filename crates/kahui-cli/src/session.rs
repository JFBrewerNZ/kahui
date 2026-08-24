//! The interactive session: a prompt, and a live view of everything the node
//! learns.
//!
//! Two things happen at once here. Lines typed (or piped) in become commands,
//! and events coming out of the node become output — including events caused by
//! other members, which arrive whether or not anyone is typing.
//!
//! Every command below is a call on [`NodeHandle`]. Nothing in this file knows
//! about gossip topics, sync batches or database tables, which is the point:
//! swapping this prompt for a window is a rendering change, not a protocol one.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Result};
use kahui_node::{
    ChannelId, ChannelSummary, CommunitySummary, Invite, Message, NodeEvent, NodeHandle,
};
use kahui_proto::CommunityId;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::broadcast::error::RecvError;

/// How long to wait for a listening address before running startup commands.
const LISTEN_WAIT: Duration = Duration::from_secs(5);

/// Grace period after the first listener appears.
///
/// TCP and QUIC bind separately, and once per interface, so the addresses
/// arrive as a short burst. Pausing for the rest of it means an invite minted
/// immediately after startup names every way in, not just the first one bound.
const LISTEN_SETTLE: Duration = Duration::from_millis(400);

/// Everything valid at the prompt, without the leading slash.
const COMMANDS: &[&str] = &[
    "help",
    "quit",
    "exit",
    "status",
    "id",
    "peers",
    "create",
    "join",
    "invite",
    "communities",
    "use",
    "channels",
    "channel",
    "c",
    "newchannel",
    "members",
    "history",
    "name",
    "dial",
    "sync",
];

pub struct Options {
    /// Emit newline-delimited JSON instead of prose.
    pub json: bool,
    /// Commands to run before reading input.
    pub startup_commands: Vec<String>,
    /// Stop after this long. Used by the demo script; interactive sessions run
    /// until told to quit.
    pub run_for: Option<Duration>,
    pub data_dir: PathBuf,
}

/// Which community and channel typed messages go to.
struct Focus {
    community: CommunityId,
    community_name: String,
    channel: ChannelId,
    channel_name: String,
}

struct Session {
    node: NodeHandle,
    json: bool,
    focus: Option<Focus>,
    /// Peers currently connected, so `/peers` reflects reality without asking
    /// the engine to recount.
    peers: HashSet<String>,
}

pub async fn run(node: NodeHandle, options: Options) -> Result<()> {
    // Subscribe before anything else runs, so nothing that happens during
    // startup is missed.
    let mut events = node.subscribe();

    let mut session = Session {
        node: node.clone(),
        json: options.json,
        focus: None,
        peers: HashSet::new(),
    };

    session.banner(&options.data_dir);

    // A node with no address yet cannot mint a usable invite, and startup
    // commands very often begin with `/create`. Wait for the first listener
    // before running them, rendering whatever arrives in the meantime.
    let listening = tokio::time::timeout(LISTEN_WAIT, async {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let is_listening = matches!(event, NodeEvent::Listening { .. });
                    session.render(&event);
                    if is_listening {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }
        }
    })
    .await;
    if listening.is_err() {
        session.info("no listening address yet; invites minted now may be unusable");
    } else {
        // Drain the rest of the address burst so it is on the record before
        // any startup command runs.
        let _ = tokio::time::timeout(LISTEN_SETTLE, async {
            while let Ok(event) = events.recv().await {
                session.render(&event);
            }
        })
        .await;
    }

    // A node that already belongs to a community should come back into it. The
    // prompt's focus lives in memory, so without this a restarted node has
    // every message on disk and nowhere to type.
    session.resume().await;

    for command in &options.startup_commands {
        session.print_input(command);
        session.dispatch(command).await;
    }

    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let deadline = async {
        match options.run_for {
            Some(duration) => tokio::time::sleep(duration).await,
            // Nothing to wait for: this branch simply never fires.
            None => std::future::pending().await,
        }
    };
    tokio::pin!(deadline);

    // Once stdin closes we keep serving peers. A node started from a script
    // with its input redirected is still a full member of its communities, and
    // should stay one until it is asked to stop.
    let mut reading = true;

    loop {
        tokio::select! {
            line = stdin.next_line(), if reading => match line {
                Ok(Some(line)) => {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    if session.dispatch(&line).await {
                        break;
                    }
                }
                Ok(None) => {
                    reading = false;
                    session.info("input closed; still serving peers (Ctrl-C to stop)");
                }
                Err(err) => {
                    session.info(&format!("could not read input: {err}"));
                    reading = false;
                }
            },

            event = events.recv() => match event {
                Ok(event) => session.render(&event),
                Err(RecvError::Lagged(missed)) => {
                    session.info(&format!("display fell behind and skipped {missed} events; local history is unaffected"));
                }
                Err(RecvError::Closed) => break,
            },

            _ = tokio::signal::ctrl_c() => {
                session.info("stopping");
                break;
            }

            _ = &mut deadline => {
                session.info("time is up, stopping");
                break;
            }
        }
    }

    node.shutdown().await.ok();
    Ok(())
}

impl Session {
    // -- output -----------------------------------------------------------

    fn banner(&self, data_dir: &std::path::Path) {
        if self.json {
            self.emit(json!({
                "type": "ready",
                "user": self.node.user().to_hex(),
                "peer_id": self.node.peer_id(),
                "data_dir": data_dir.display().to_string(),
                "license": env!("CARGO_PKG_LICENSE"),
                "source": env!("CARGO_PKG_REPOSITORY"),
            }));
            return;
        }
        println!("Kahui — a community is its members. No server is running.");
        println!("  you      {}", self.node.user().short());
        println!("  peer id  {}", self.node.peer_id());
        println!("  data     {}", data_dir.display());
        // Under the AGPL, anyone running this is entitled to its source. Saying
        // where it is costs one line and is the whole point of the licence.
        println!(
            "  source   {} · {}",
            env!("CARGO_PKG_LICENSE"),
            env!("CARGO_PKG_REPOSITORY")
        );
        println!("Type /help for commands.");
    }

    fn emit(&self, value: serde_json::Value) {
        println!("{value}");
    }

    fn info(&self, message: &str) {
        if self.json {
            self.emit(json!({"type": "info", "message": message}));
        } else {
            println!("· {message}");
        }
    }

    fn problem(&self, message: &str) {
        if self.json {
            self.emit(json!({"type": "error", "message": message}));
        } else {
            println!("! {message}");
        }
    }

    fn print_input(&self, command: &str) {
        if !self.json {
            println!("> {command}");
        }
    }

    /// Renders one thing the node learned.
    fn render(&mut self, event: &NodeEvent) {
        if self.json {
            if let Ok(value) = serde_json::to_value(event) {
                self.emit(value);
            }
            return;
        }
        match event {
            NodeEvent::Message(message) => println!("{}", format_message(message)),
            NodeEvent::Membership {
                display_name, user, ..
            } => println!("· {display_name} ({}) is a member", user.short()),
            NodeEvent::ChannelCreated { name, .. } => println!("· #{name} was created"),
            NodeEvent::CommunityCreated { name, .. } => println!("· community {name} exists"),
            NodeEvent::PeerConnected { peer } => {
                self.peers.insert(peer.clone());
                println!("· connected to {}", short_peer(peer));
            }
            NodeEvent::PeerDisconnected { peer } => {
                self.peers.remove(peer);
                println!("· lost {}", short_peer(peer));
            }
            NodeEvent::Synced { peer, applied, .. } => println!(
                "· caught up on {applied} event{} from {}",
                if *applied == 1 { "" } else { "s" },
                short_peer(peer)
            ),
            NodeEvent::Listening { addr } => println!("· listening on {addr}"),
            NodeEvent::ReachabilityChanged {
                reachability,
                relayed_by,
            } => match relayed_by {
                Some(peer) => println!(
                    "· reachability: {} — {} is relaying for you",
                    reachability.as_str(),
                    short_peer(peer)
                ),
                None => println!("· reachability: {}", reachability.as_str()),
            },
            NodeEvent::HolePunched { peer } => println!(
                "· connected straight to {} — no longer going through a relay",
                short_peer(peer)
            ),
            NodeEvent::Warning { message } => println!("! {message}"),
            NodeEvent::Stopped => println!("· node stopped"),
        }
    }

    // -- dispatch ---------------------------------------------------------

    /// Runs one line. Returns true when the session should end.
    async fn dispatch(&mut self, line: &str) -> bool {
        if let Some(hint) = mangled_command_hint(line) {
            self.problem(&hint);
            return false;
        }
        if !line.starts_with('/') {
            self.post(line).await;
            return false;
        }

        let (command, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();

        let result = match command {
            "/help" | "/?" => {
                self.help();
                Ok(())
            }
            "/quit" | "/exit" => return true,
            "/status" => self.status().await,
            "/id" => self.whoami().await,
            "/peers" => self.list_peers().await,
            "/create" => self.create(rest).await,
            "/join" => self.join(rest).await,
            "/invite" => self.invite().await,
            "/communities" => self.list_communities().await,
            "/use" => self.use_community(rest).await,
            "/channels" => self.list_channels().await,
            "/channel" | "/c" => self.use_channel(rest).await,
            "/newchannel" => self.new_channel(rest).await,
            "/members" => self.list_members().await,
            "/history" => self.history(rest).await,
            "/name" => self.rename(rest).await,
            "/dial" => self.dial(rest).await,
            "/sync" => self.sync().await,
            other => Err(anyhow!("unknown command {other}; try /help")),
        };

        if let Err(err) = result {
            self.problem(&err.to_string());
        }
        false
    }

    fn help(&self) {
        if self.json {
            return;
        }
        println!(
            "\
  /create <name>        found a community, with a #general channel
  /join <invite>        join from an invite token
  /invite               print an invite for the current community
  /communities          list communities this node belongs to
  /use <name>           switch to a community
  /channels             list channels
  /channel <name>       switch channel
  /newchannel <name>    open a channel
  /members              who belongs to this community
  /history [n]          show the last n messages (default 30)
  /name <display name>  change how you appear
  /peers                who this node is connected to right now
  /dial <multiaddr>     connect to a peer directly
  /sync                 ask peers for anything missing
  /status               what this node holds
  /id                   this node's identity
  /quit                 stop the node
  anything else         post to the current channel"
        );
    }

    // -- commands ---------------------------------------------------------

    async fn post(&mut self, body: &str) {
        let Some(focus) = &self.focus else {
            self.problem("no channel selected; use /create, /join or /use first");
            return;
        };
        if let Err(err) = self
            .node
            .post(focus.community, focus.channel, body.to_string())
            .await
        {
            self.problem(&format!("could not post: {err}"));
        }
    }

    async fn create(&mut self, name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(anyhow!("usage: /create <name>"));
        }
        let community = self
            .node
            .create_community(name, "A Kahui community")
            .await?;
        self.focus_on(community).await?;
        self.info(&format!("created {name}"));
        self.invite().await
    }

    async fn join(&mut self, token: &str) -> Result<()> {
        if token.is_empty() {
            return Err(anyhow!("usage: /join <invite>"));
        }
        let invite = Invite::decode(token)?;
        let name = invite.name.clone();
        self.info(&format!("joining {name}; fetching history"));
        let community = self.node.join(invite).await?;
        self.focus_on(community).await?;
        self.info(&format!("joined {name}"));
        Ok(())
    }

    async fn invite(&mut self) -> Result<()> {
        let focus = self.require_focus()?;
        let invite = self.node.invite(focus.community).await?;
        let usable = invite.reachable_beyond_lan();

        if self.json {
            self.emit(json!({
                "type": "invite",
                "community": focus.community.to_hex(),
                "name": invite.name,
                "invite": invite.encode(),
                "link": invite.to_link(),
                "reachable": usable,
            }));
            return Ok(());
        }

        println!("invite: {}", invite.to_link());
        if !usable {
            // Handing somebody an invite that cannot possibly work, and letting
            // them discover it by waiting for a timeout, is the worst of both.
            self.problem("This only works on your network — nothing outside it can reach you.");
        }
        Ok(())
    }

    async fn list_communities(&mut self) -> Result<()> {
        let communities = self.node.communities().await?;
        if self.json {
            self.emit(json!({"type": "communities", "communities": communities}));
            return Ok(());
        }
        if communities.is_empty() {
            self.info("no communities yet; try /create or /join");
            return Ok(());
        }
        for community in communities {
            let current = self
                .focus
                .as_ref()
                .is_some_and(|f| f.community == community.id);
            println!(
                "{} {} ({})",
                if current { "*" } else { " " },
                community.name,
                community.id.short()
            );
        }
        Ok(())
    }

    async fn use_community(&mut self, needle: &str) -> Result<()> {
        let community = self.find_community(needle).await?;
        self.focus_on(community.id).await?;
        self.info(&format!("now in {}", community.name));
        Ok(())
    }

    async fn list_channels(&mut self) -> Result<()> {
        let focus = self.require_focus()?;
        let channels = self.node.channels(focus.community).await?;
        if self.json {
            self.emit(json!({"type": "channels", "channels": channels}));
            return Ok(());
        }
        for channel in channels {
            let current = self.focus.as_ref().is_some_and(|f| f.channel == channel.id);
            println!(
                "{} #{}{}",
                if current { "*" } else { " " },
                channel.name,
                if channel.topic.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", channel.topic)
                }
            );
        }
        Ok(())
    }

    async fn use_channel(&mut self, name: &str) -> Result<()> {
        let focus = self.require_focus()?;
        let channel = self.find_channel(focus.community, name).await?;
        self.set_focus(focus.community, &focus.community_name, &channel);
        self.info(&format!("now in #{}", channel.name));
        Ok(())
    }

    async fn new_channel(&mut self, name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(anyhow!("usage: /newchannel <name>"));
        }
        let focus = self.require_focus()?;
        let name = name.trim_start_matches('#');
        self.node
            .create_channel(focus.community, name, String::new())
            .await?;
        self.use_channel(name).await
    }

    async fn list_members(&mut self) -> Result<()> {
        let focus = self.require_focus()?;
        let members = self.node.members(focus.community).await?;
        if self.json {
            self.emit(json!({"type": "members", "members": members}));
            return Ok(());
        }
        for member in members {
            let me = if member.id == self.node.user() {
                " (you)"
            } else {
                ""
            };
            println!("  {} [{}]{me}", member.display_name, member.id.short());
        }
        Ok(())
    }

    async fn history(&mut self, argument: &str) -> Result<()> {
        let limit: usize = if argument.is_empty() {
            30
        } else {
            argument
                .parse()
                .map_err(|_| anyhow!("usage: /history [number of messages]"))?
        };
        let focus = self.require_focus()?;
        let messages = self
            .node
            .history(focus.community, focus.channel, limit)
            .await?;
        if self.json {
            self.emit(json!({"type": "history", "messages": messages}));
            return Ok(());
        }
        if messages.is_empty() {
            self.info("nothing here yet");
        }
        for message in messages {
            println!("{}", format_message(&message));
        }
        Ok(())
    }

    async fn rename(&mut self, name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(anyhow!("usage: /name <display name>"));
        }
        self.node.set_display_name(name).await?;
        self.info(&format!("you are now {name}"));
        Ok(())
    }

    async fn dial(&mut self, addr: &str) -> Result<()> {
        if addr.is_empty() {
            return Err(anyhow!("usage: /dial <multiaddr>"));
        }
        self.node.dial(addr).await?;
        self.info(&format!("dialling {addr}"));
        Ok(())
    }

    async fn sync(&mut self) -> Result<()> {
        let sent = self.node.sync_now().await?;
        self.info(&format!(
            "asked {sent} peer{} for anything we are missing",
            if sent == 1 { "" } else { "s" }
        ));
        Ok(())
    }

    async fn list_peers(&mut self) -> Result<()> {
        let status = self.node.status().await?;
        if self.json {
            self.emit(json!({"type": "peers", "peers": status.connected_peers}));
            return Ok(());
        }
        if status.connected_peers.is_empty() {
            self.info("not connected to anyone");
        }
        for peer in status.connected_peers {
            println!("  {peer}");
        }
        Ok(())
    }

    async fn whoami(&mut self) -> Result<()> {
        let status = self.node.status().await?;
        if self.json {
            self.emit(json!({"type": "identity", "status": status}));
            return Ok(());
        }
        println!("  name    {}", status.display_name);
        println!("  user    {}", status.user);
        println!("  peer id {}", status.peer_id);
        for addr in status.listen_addrs {
            println!("  address {addr}");
        }
        Ok(())
    }

    async fn status(&mut self) -> Result<()> {
        let status = self.node.status().await?;
        if self.json {
            self.emit(json!({"type": "status", "status": status}));
            return Ok(());
        }
        println!(
            "  {} peer{} connected",
            status.connected_peers.len(),
            if status.connected_peers.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        print!("  reachable: {}", status.reachability.as_str());
        if let Some(peer) = &status.relayed_by {
            print!(" (relayed by {})", short_peer(peer));
        }
        if status.relaying_for > 0 {
            print!(", relaying for {}", status.relaying_for);
        }
        println!();
        for community in status.communities {
            println!(
                "  {} — {} events, {} member{}, {} channel{}",
                community.name,
                community.events,
                community.members,
                if community.members == 1 { "" } else { "s" },
                community.channels,
                if community.channels == 1 { "" } else { "s" },
            );
        }
        Ok(())
    }

    // -- focus ------------------------------------------------------------

    fn require_focus(&self) -> Result<Focus> {
        self.focus
            .as_ref()
            .map(|focus| Focus {
                community: focus.community,
                community_name: focus.community_name.clone(),
                channel: focus.channel,
                channel_name: focus.channel_name.clone(),
            })
            .ok_or_else(|| anyhow!("no community selected; use /create, /join or /use first"))
    }

    /// Picks up where this node left off, if it belongs to anything already.
    async fn resume(&mut self) {
        if self.focus.is_some() {
            return;
        }
        let Ok(communities) = self.node.communities().await else {
            return;
        };
        let Some(first) = communities.into_iter().next() else {
            return;
        };
        if self.focus_on(first.id).await.is_ok() {
            if let Some(focus) = &self.focus {
                let message = format!("back in {} #{}", focus.community_name, focus.channel_name);
                self.info(&message);
            }
        }
    }

    /// Points the prompt at a community and its first channel.
    async fn focus_on(&mut self, community: CommunityId) -> Result<()> {
        let summary = self
            .node
            .communities()
            .await?
            .into_iter()
            .find(|c| c.id == community)
            .ok_or_else(|| anyhow!("this node does not hold community {}", community.short()))?;

        let channels = self.node.channels(community).await?;
        let channel = channels
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("{} has no channels yet", summary.name))?;

        self.set_focus(community, &summary.name, &channel);
        Ok(())
    }

    fn set_focus(
        &mut self,
        community: CommunityId,
        community_name: &str,
        channel: &ChannelSummary,
    ) {
        self.focus = Some(Focus {
            community,
            community_name: community_name.to_string(),
            channel: channel.id,
            channel_name: channel.name.clone(),
        });
    }

    async fn find_community(&self, needle: &str) -> Result<CommunitySummary> {
        if needle.is_empty() {
            return Err(anyhow!("usage: /use <community name or id>"));
        }
        let needle_lower = needle.to_lowercase();
        let communities = self.node.communities().await?;
        communities
            .iter()
            .find(|c| {
                c.name.to_lowercase() == needle_lower || c.id.to_hex().starts_with(&needle_lower)
            })
            .or_else(|| {
                communities
                    .iter()
                    .find(|c| c.name.to_lowercase().starts_with(&needle_lower))
            })
            .cloned()
            .ok_or_else(|| anyhow!("no community here matches {needle}"))
    }

    async fn find_channel(&self, community: CommunityId, name: &str) -> Result<ChannelSummary> {
        if name.is_empty() {
            return Err(anyhow!("usage: /channel <name>"));
        }
        let wanted = name.trim_start_matches('#').to_lowercase();
        self.node
            .channels(community)
            .await?
            .into_iter()
            .find(|channel| channel.name.to_lowercase() == wanted)
            .ok_or_else(|| anyhow!("no channel called #{wanted}"))
    }
}

/// Spots a command that a Unix-shell emulator has rewritten into a path.
///
/// Git Bash and MSYS translate arguments that look like absolute Unix paths, so
/// `--exec "/create Kahui"` reaches the program as
/// `C:/Program Files/Git/create Kahui`. Posting that as a chat message would be
/// a baffling way to fail, so say what happened instead.
fn mangled_command_hint(line: &str) -> Option<String> {
    let (prefix, tail) = line.rsplit_once('/')?;
    if !prefix.contains(":/") && !prefix.contains(":\\") {
        return None;
    }
    let command = tail.split_whitespace().next()?;
    if !COMMANDS.contains(&command) {
        return None;
    }
    Some(format!(
        "that looks like `/{command}` after your shell rewrote it as a path;          run the command with MSYS_NO_PATHCONV=1 set, or type it at the prompt"
    ))
}

/// `[12:34:56] #general alice: hello`
fn format_message(message: &Message) -> String {
    format!(
        "[{}] {}: {}",
        clock(message.timestamp_ms),
        message.author_name,
        message.body
    )
}

/// Renders a timestamp as UTC wall clock.
///
/// The author's own clock, shown as they reported it. It is never used to order
/// anything — that is what the Lamport clock is for — so a peer with a wrong
/// clock produces a confusing timestamp and nothing worse.
fn clock(timestamp_ms: u64) -> String {
    let seconds = timestamp_ms / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        (seconds / 3600) % 24,
        (seconds / 60) % 60,
        seconds % 60
    )
}

/// Peer ids are long and only the tail is worth reading at a glance.
fn short_peer(peer: &str) -> String {
    match peer.char_indices().nth_back(7) {
        Some((index, _)) => format!("…{}", &peer[index..]),
        None => peer.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_renders_utc_wall_time() {
        assert_eq!(clock(0), "00:00:00");
        assert_eq!(clock(3_661_000), "01:01:01");
        // 2023-11-14T22:13:20Z
        assert_eq!(clock(1_700_000_000_000), "22:13:20");
    }

    #[test]
    fn a_shell_mangled_command_is_recognised_not_posted() {
        let hint = mangled_command_hint("C:/Program Files/Git/create Kahui")
            .expect("should be recognised as a rewritten /create");
        assert!(hint.contains("/create"), "the hint should name the command");
        assert!(hint.contains("MSYS_NO_PATHCONV"), "and say how to fix it");
    }

    #[test]
    fn ordinary_messages_containing_paths_are_left_alone() {
        // Only a path whose last segment is an actual command is suspicious.
        assert_eq!(
            mangled_command_hint("look in C:/Program Files/Git/bin"),
            None
        );
        assert_eq!(mangled_command_hint("/create Kahui"), None);
        assert_eq!(mangled_command_hint("see you at 3/4 past"), None);
    }

    #[test]
    fn short_peer_keeps_the_distinctive_tail() {
        assert_eq!(short_peer("12D3KooWabcdefgh"), "…abcdefgh");
        assert_eq!(short_peer("short"), "short");
    }
}
