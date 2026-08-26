//! The desktop client's Rust half.
//!
//! It runs a full [`kahui_node`] in-process and exposes it to the window. There
//! is no local server, no socket and no second process: the interface talks to
//! the node the same way the CLI does, over [`kahui_node::NodeHandle`], and
//! every command below is a thin forward to one of its methods.
//!
//! Keeping it thin is the point. Anything that belongs to the protocol lives in
//! `kahui-node` and below, where a web or mobile client shares it; the only
//! thing that should ever be desktop-specific is how it is drawn.

use std::path::PathBuf;

use kahui_node::{
    ChannelSummary, CommunitySummary, Invite, MemberSummary, Message, Node, NodeConfig, NodeError,
    NodeHandle, Status,
};
use kahui_proto::{ChannelId, CommunityId};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::RwLock;

/// Every event the node emits is forwarded to the window under this name.
const NODE_EVENT: &str = "kahui://event";
/// Fired once the node is up and commands will succeed.
const READY_EVENT: &str = "kahui://ready";
/// Fired if the node could not start at all.
const FAILED_EVENT: &str = "kahui://failed";
/// Fired when the app is opened by a `kahui://join/...` link.
const INVITE_EVENT: &str = "kahui://invite";

/// How many messages the window asks for when opening a channel.
const HISTORY_LIMIT: usize = 300;

/// An error, in a shape the window can render.
#[derive(Debug, Clone, Serialize)]
pub struct UiError {
    pub message: String,
    /// True when retrying later might work, so the interface can say "not yet"
    /// rather than "broken".
    pub transient: bool,
}

impl UiError {
    fn new(message: impl Into<String>) -> Self {
        UiError {
            message: message.into(),
            transient: false,
        }
    }

    fn transient(message: impl Into<String>) -> Self {
        UiError {
            message: message.into(),
            transient: true,
        }
    }
}

impl From<NodeError> for UiError {
    fn from(err: NodeError) -> Self {
        // A timeout usually means the node is busy syncing, not that anything
        // is wrong.
        let transient = matches!(err, NodeError::Timeout | NodeError::NotRunning);
        UiError {
            message: err.to_string(),
            transient,
        }
    }
}

impl From<kahui_node::InviteError> for UiError {
    fn from(err: kahui_node::InviteError) -> Self {
        UiError::new(format!("That invite could not be read: {err}"))
    }
}

/// How far the node has got.
///
/// Kept as state rather than only announced as an event. The window cannot
/// subscribe until its JavaScript has loaded, and the node routinely finishes
/// starting before that — so an interface that only listened would wait forever
/// for something that had already happened. Asking cannot race.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Startup {
    #[default]
    Starting,
    Ready(Ready),
    Failed(UiError),
}

/// Holds the node once it has started.
#[derive(Default)]
pub struct NodeState {
    handle: RwLock<Option<NodeHandle>>,
    startup: RwLock<Startup>,
    /// An invite link the app was launched with, held until the window asks.
    pending_invite: RwLock<Option<String>>,
}

impl NodeState {
    async fn get(&self) -> Result<NodeHandle, UiError> {
        self.handle
            .read()
            .await
            .clone()
            .ok_or_else(|| UiError::transient("Kāhui is still starting up."))
    }
}

/// Where the node keeps its key and its copy of history.
///
/// Deliberately the same directory the command line client uses, so the two are
/// the same member rather than two strangers who happen to share a machine. The
/// consequence is that only one of them can run at a time, which is what the
/// database lock error below is about.
fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("nz", "Kahui", "Kahui")
        .map(|dirs| dirs.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".kahui"))
}

/// What the window is told once the node is running.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ready {
    // (kept cloneable so startup state and the event can share one value)
    pub status: Status,
    pub data_dir: String,
}

async fn start_node(app: AppHandle) -> Result<(), UiError> {
    let dir = data_dir();
    let node = Node::spawn(NodeConfig {
        data_dir: dir.clone(),
        ..NodeConfig::default()
    })
    .await
    .map_err(|err| explain_startup_failure(err, &dir))?;

    // Subscribe before publishing the handle, so nothing that happens between
    // here and the window's first render is lost.
    let mut events = node.subscribe();
    let emitter = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => {
                    let _ = emitter.emit(NODE_EVENT, &event);
                }
                // The window is behind. Its own state is unaffected -- it can
                // always re-read from the store -- so keep going.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => break,
            }
        }
    });

    let status = node.status().await?;
    let state = app.state::<NodeState>();
    *state.handle.write().await = Some(node);

    let ready = Ready {
        status,
        data_dir: dir.display().to_string(),
    };
    // Recorded first, announced second. A window that loads late reads the
    // record; one that was already listening gets the event.
    *state.startup.write().await = Startup::Ready(ready.clone());
    let _ = app.emit(READY_EVENT, ready);
    Ok(())
}

/// Turns a startup failure into something worth reading.
///
/// The overwhelmingly common cause is a second copy already running against the
/// same data directory, and "resource busy" does not help anybody work that out.
fn explain_startup_failure(err: NodeError, dir: &std::path::Path) -> UiError {
    let text = err.to_string().to_lowercase();
    if text.contains("lock") || text.contains("busy") || text.contains("used by another") {
        return UiError::new(format!(
            "Another copy of Kāhui is already using {}. Only one can run at a time, \
             because they would be the same member of the same communities.",
            dir.display()
        ));
    }
    UiError::new(format!("Kāhui could not start: {err}"))
}

// -- commands -------------------------------------------------------------
//
// Each is a forward to NodeHandle. If one of these grows logic, it probably
// belongs in kahui-node instead, where every client gets it.

/// Reports a crash in the window to the log.
///
/// A packaged desktop app has no console, so a JavaScript error that stops the
/// interface dead is otherwise completely silent — the window simply never
/// changes, which is indistinguishable from being slow.
#[tauri::command]
async fn report_web_error(message: String) -> Result<(), UiError> {
    tracing::error!(target: "kahui_desktop_lib", "the window reported: {message}");
    Ok(())
}

/// Puts the current community and channel in the window title.
///
/// The webview's own `document.title` does not reach the native window, so this
/// has to come back through Rust. Worth having: it is what makes the window
/// findable in a task switcher full of other windows.
#[tauri::command]
async fn set_window_title(title: String, app: AppHandle) -> Result<(), UiError> {
    if let Some(window) = app.get_webview_window("main") {
        window
            .set_title(&title)
            .map_err(|err| UiError::new(err.to_string()))?;
    }
    Ok(())
}

/// The link this app was opened with, if it was opened by one.
///
/// Like startup, an arriving link can beat the window to it, so it is recorded
/// as well as announced.
#[tauri::command]
async fn pending_invite(state: State<'_, NodeState>) -> Result<Option<String>, UiError> {
    Ok(state.pending_invite.write().await.take())
}

/// Where the node has got to, asked rather than awaited.
#[tauri::command]
async fn startup_state(state: State<'_, NodeState>) -> Result<Startup, UiError> {
    let startup = state.startup.read().await.clone();
    tracing::debug!(?startup, "the window asked how startup is going");
    Ok(startup)
}

/// This node's identity, as a line of text to write down.
#[tauri::command]
async fn backup_phrase(state: State<'_, NodeState>) -> Result<String, UiError> {
    Ok(state.get().await?.backup_phrase().await?)
}

/// Adopts an identity from a backup phrase and restarts onto it.
///
/// Only possible before this device has joined anything: the events it has
/// already signed belong to the identity it signed them with.
#[tauri::command]
async fn restore_identity(
    phrase: String,
    app: AppHandle,
    state: State<'_, NodeState>,
) -> Result<(), UiError> {
    let node = state.get().await?;
    node.replace_identity(phrase.trim().to_string()).await?;

    // The key on disk is the new one now, but this node is still running on the
    // old one. Stop it and come back up as the restored identity.
    node.shutdown().await.ok();
    drop(node);
    {
        *state.handle.write().await = None;
        *state.startup.write().await = Startup::Starting;
    }
    start_node(app).await
}

#[tauri::command]
async fn status(state: State<'_, NodeState>) -> Result<Status, UiError> {
    Ok(state.get().await?.status().await?)
}

#[tauri::command]
async fn communities(state: State<'_, NodeState>) -> Result<Vec<CommunitySummary>, UiError> {
    Ok(state.get().await?.communities().await?)
}

#[tauri::command]
async fn channels(
    community: CommunityId,
    state: State<'_, NodeState>,
) -> Result<Vec<ChannelSummary>, UiError> {
    Ok(state.get().await?.channels(community).await?)
}

#[tauri::command]
async fn members(
    community: CommunityId,
    state: State<'_, NodeState>,
) -> Result<Vec<MemberSummary>, UiError> {
    Ok(state.get().await?.members(community).await?)
}

/// Which members this node can currently reach.
///
/// First-hand knowledge only. Somebody absent from this list may be perfectly
/// well and talking to another member -- there is no server keeping a roster,
/// so no node can speak for the whole community.
#[tauri::command]
async fn online_members(
    community: CommunityId,
    state: State<'_, NodeState>,
) -> Result<Vec<kahui_proto::UserId>, UiError> {
    Ok(state.get().await?.online_members(community).await?)
}

#[tauri::command]
async fn history(
    community: CommunityId,
    channel: ChannelId,
    state: State<'_, NodeState>,
) -> Result<Vec<Message>, UiError> {
    Ok(state
        .get()
        .await?
        .history(community, channel, HISTORY_LIMIT)
        .await?)
}

#[tauri::command]
async fn post(
    community: CommunityId,
    channel: ChannelId,
    body: String,
    state: State<'_, NodeState>,
) -> Result<(), UiError> {
    let body = body.trim().to_string();
    if body.is_empty() {
        return Err(UiError::new("A message needs some text in it."));
    }
    state.get().await?.post(community, channel, body).await?;
    Ok(())
}

#[tauri::command]
async fn create_community(
    name: String,
    state: State<'_, NodeState>,
) -> Result<CommunityId, UiError> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(UiError::new("Give the community a name."));
    }
    Ok(state
        .get()
        .await?
        .create_community(name, "A Kāhui community")
        .await?)
}

#[tauri::command]
async fn join_community(
    invite: String,
    state: State<'_, NodeState>,
) -> Result<CommunityId, UiError> {
    let invite = Invite::decode(invite.trim())?;
    Ok(state.get().await?.join(invite).await?)
}

#[tauri::command]
async fn create_channel(
    community: CommunityId,
    name: String,
    state: State<'_, NodeState>,
) -> Result<ChannelId, UiError> {
    let name = name.trim().trim_start_matches('#').to_lowercase();
    if name.is_empty() {
        return Err(UiError::new("Give the channel a name."));
    }
    Ok(state
        .get()
        .await?
        .create_channel(community, name, String::new())
        .await?)
}

/// An invite for a community, ready to paste to somebody.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteText {
    /// The bare code, for pasting into a box.
    pub token: String,
    /// The same thing as a `kahui://` link, for clicking.
    pub link: String,
    pub community_name: String,
    /// How many members are named in it. More than one means the invite keeps
    /// working after any of them goes offline.
    pub peer_count: usize,
    /// False when every address in it is private or loopback, so it can only be
    /// used by somebody on the same network.
    pub reachable: bool,
}

#[tauri::command]
async fn make_invite(
    community: CommunityId,
    state: State<'_, NodeState>,
) -> Result<InviteText, UiError> {
    let node = state.get().await?;
    let invite = node.invite(community).await?;
    let on_the_network = on_the_network(&node).await;
    Ok(InviteText {
        token: invite.encode(),
        link: invite.to_link(),
        community_name: invite.name.clone(),
        peer_count: invite.peers.len(),
        // Two quite different ways for this to work. Either the invite names an
        // address somebody outside can dial, or this node is on the network and
        // findable by the community id the invite carries — in which case the
        // addresses are a shortcut rather than the mechanism.
        reachable: invite.reachable_beyond_lan() || on_the_network,
    })
}

/// Whether this node can be found through the network rather than by address.
async fn on_the_network(node: &kahui_node::NodeHandle) -> bool {
    node.status()
        .await
        .map(|status| status.network_peers > 0)
        .unwrap_or(false)
}

#[tauri::command]
async fn set_display_name(name: String, state: State<'_, NodeState>) -> Result<(), UiError> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(UiError::new("Pick a name to be known by."));
    }
    state.get().await?.set_display_name(name).await?;
    Ok(())
}

#[tauri::command]
async fn sync_now(state: State<'_, NodeState>) -> Result<usize, UiError> {
    Ok(state.get().await?.sync_now().await?)
}

#[tauri::command]
async fn dial(addr: String, state: State<'_, NodeState>) -> Result<(), UiError> {
    state.get().await?.dial(addr.trim().to_string()).await?;
    Ok(())
}

/// Works out whether anybody can reach this machine, and what to do if not.
///
/// Takes a few seconds and talks to the router, so it runs when asked rather
/// than on a timer.
#[tauri::command]
async fn check_network(state: State<'_, NodeState>) -> Result<NetworkCheck, UiError> {
    // The port we actually listen on, which is the one the router has to be
    // told about. Falls back to the default when nothing is bound yet.
    let port = state
        .get()
        .await?
        .status()
        .await
        .ok()
        .and_then(|status| {
            status.listen_addrs.iter().find_map(|addr| {
                addr.rsplit("/tcp/")
                    .next()
                    .and_then(|tail| tail.parse::<u16>().ok())
            })
        })
        .unwrap_or(kahui_node::DEFAULT_PORT);

    let found = kahui_node::diagnose(port).await;
    Ok(NetworkCheck {
        reachable: found.reachable(),
        advice: found.advice(),
        rows: vec![
            Row::new(
                "Router",
                found.gateway.clone().unwrap_or_else(|| "not found".into()),
            ),
            Row::new(
                "This PC",
                found.local.clone().unwrap_or_else(|| "no address".into()),
            ),
            Row::new("Port", port.to_string()),
            Row::result("PCP", &found.pcp),
            Row::result("NAT-PMP", &found.natpmp),
            Row::result("UPnP", &found.upnp),
            Row::new(
                "IPv6",
                if found.global_ipv6.is_empty() {
                    "no public address".into()
                } else {
                    found.global_ipv6.join(", ")
                },
            ),
        ],
    })
}

/// One line of the network check.
#[derive(serde::Serialize)]
struct Row {
    label: String,
    value: String,
    /// `None` for plain facts, `Some` for things that either worked or did not.
    ok: Option<bool>,
}

impl Row {
    fn new(label: &str, value: String) -> Self {
        Row {
            label: label.into(),
            value,
            ok: None,
        }
    }

    fn result(label: &str, attempt: &kahui_node::Attempt) -> Self {
        Row {
            label: label.into(),
            value: attempt.detail().to_string(),
            ok: Some(attempt.worked()),
        }
    }
}

/// What the window shows after a network check.
#[derive(serde::Serialize)]
struct NetworkCheck {
    reachable: bool,
    advice: String,
    rows: Vec<Row>,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    tauri::Builder::default()
        // One node per data directory, so one window. Launching again — by
        // clicking a kahui:// link, or the icon — hands whatever was on the
        // command line to the copy that is already running and raises it,
        // rather than starting a second one that cannot open the database.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
            if let Some(link) = argv.iter().find(|arg| arg.starts_with("kahui://")) {
                tracing::info!(%link, "a link arrived for the running window");
                let _ = app.emit(INVITE_EVENT, link.clone());
            }
        }))
        .plugin(tauri_plugin_deep_link::init())
        .manage(NodeState::default())
        .invoke_handler(tauri::generate_handler![
            startup_state,
            pending_invite,
            set_window_title,
            report_web_error,
            backup_phrase,
            restore_identity,
            status,
            communities,
            channels,
            members,
            online_members,
            history,
            post,
            create_community,
            join_community,
            create_channel,
            make_invite,
            set_display_name,
            sync_now,
            dial,
            check_network,
        ])
        // Whether the page loaded at all, and where from. Without this a blank
        // window and a broken window look identical from out here.
        .on_page_load(|window, payload| {
            tracing::info!(
                target: "kahui_desktop_lib",
                url = %payload.url(),
                event = ?payload.event(),
                "webview page load"
            );
            // Hook the window's own failures back to this log.
            let _ = window.eval(
                "window.addEventListener('error', function (e) {                   window.__TAURI_INTERNALS__.invoke('report_web_error', {                     message: e.message + ' at ' + e.filename + ':' + e.lineno });                 });                 window.addEventListener('unhandledrejection', function (e) {                   window.__TAURI_INTERNALS__.invoke('report_web_error', {                     message: 'unhandled rejection: ' + e.reason });                 });",
            );
        })
        .setup(|app| {
            // Claim kahui:// so an invite can be a link somebody clicks. An
            // installer does this too; doing it here as well means a copy run
            // straight from a folder works the same way.
            #[cfg(desktop)]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                if let Err(err) = app.deep_link().register_all() {
                    tracing::debug!(%err, "could not claim the kahui:// scheme");
                }

                let opener = app.handle().clone();
                app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        tracing::info!(%url, "opened by a link");
                        let link = url.to_string();
                        let recorded = opener.clone();
                        let stored = link.clone();
                        tauri::async_runtime::spawn(async move {
                            *recorded.state::<NodeState>().pending_invite.write().await =
                                Some(stored);
                        });
                        let _ = opener.emit(INVITE_EVENT, link);
                    }
                });
            }

            // Starting the node takes a moment (opening the database, binding
            // sockets), so the window paints first and is told when it is ready.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(err) = start_node(handle.clone()).await {
                    // Recorded before it is announced, for the same reason as
                    // the ready case: the window may not be listening yet.
                    *handle.state::<NodeState>().startup.write().await =
                        Startup::Failed(err.clone());
                    let _ = handle.emit(FAILED_EVENT, err);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("failed to start the Kāhui window");
}
