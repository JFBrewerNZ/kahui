/**
 * The Rust side, as seen from the window.
 *
 * Every function here is a forward to a `#[tauri::command]`, which is itself a
 * forward to `kahui_node::NodeHandle`. Nothing about the protocol lives on this
 * side of the boundary — a web client would swap this one file for a WASM node
 * and keep the rest of the interface unchanged.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** 32-byte identifiers arrive as hex strings. */
export type Id = string;

export interface CommunitySummary {
  id: Id;
  name: string;
  description: string;
  founder: Id;
  created_ms: number;
}

export interface ChannelSummary {
  id: Id;
  name: string;
  topic: string;
  creator: Id;
  created_ms: number;
}

export interface MemberSummary {
  id: Id;
  display_name: string;
  joined_ms: number;
}

export interface Message {
  id: Id;
  community: Id;
  channel: Id;
  author: Id;
  author_name: string;
  body: string;
  timestamp_ms: number;
  /** Position in the causal order. This, not the timestamp, is what sorts. */
  lamport: number;
}

export interface CommunityStatus {
  id: Id;
  name: string;
  events: number;
  members: number;
  channels: number;
}

/** Whether anybody can dial this node. */
export type Reachability = "unknown" | "direct" | "behind_nat";

export interface Status {
  user: Id;
  display_name: string;
  peer_id: string;
  listen_addrs: string[];
  connected_peers: string[];
  communities: CommunityStatus[];
  reachability: Reachability;
  /** The member carrying our traffic, if we needed one. */
  relayed_by: string | null;
  /** How many members we are carrying for. */
  relaying_for: number;
}

export interface InviteText {
  token: string;
  communityName: string;
  peerCount: number;
}

export interface Ready {
  status: Status;
  dataDir: string;
}

/**
 * How far the node has got.
 *
 * Asked for rather than only awaited: the node usually finishes starting before
 * this window has loaded enough JavaScript to subscribe to anything, so an
 * interface that only listened would wait forever for an event that had already
 * fired.
 */
export type Startup =
  | { phase: "starting" }
  | ({ phase: "ready" } & Ready)
  | ({ phase: "failed" } & UiError);

export interface UiError {
  message: string;
  transient: boolean;
}

/** Anything the node learned. Mirrors `kahui_node::NodeEvent`. */
export type NodeEvent =
  | { type: "listening"; addr: string }
  | { type: "peer_connected"; peer: string }
  | { type: "peer_disconnected"; peer: string }
  | { type: "message"; [k: string]: unknown }
  | { type: "membership"; community: Id; user: Id; display_name: string }
  | { type: "channel_created"; community: Id; channel: Id; name: string }
  | { type: "community_created"; community: Id; name: string }
  | { type: "synced"; peer: string; community: Id; applied: number }
  | { type: "reachability_changed"; reachability: Reachability; relayed_by: string | null }
  | { type: "hole_punched"; peer: string }
  | { type: "warning"; message: string }
  | { type: "stopped" };

/**
 * A message event arrives with the message's fields inlined next to `type`,
 * because the Rust enum is `Message(Message)` with a tag. This pulls it back
 * out.
 */
export function asMessage(event: NodeEvent): Message | null {
  if (event.type !== "message") return null;
  const { type: _type, ...rest } = event as Record<string, unknown>;
  return rest as unknown as Message;
}

/** Turns whatever came back from `invoke` into something printable. */
export function errorText(err: unknown): string {
  if (typeof err === "string") return err;
  if (err && typeof err === "object" && "message" in err) {
    return String((err as UiError).message);
  }
  return String(err);
}

/**
 * True when running inside the app rather than a bare browser tab.
 *
 * Only ever consulted under `import.meta.env.DEV`. A production build has no
 * fallback: if the Rust side is missing there, something is genuinely wrong and
 * the interface should say so rather than quietly showing invented data.
 */
const insideTauri = () =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const realApi = {
  startupState: () => invoke<Startup>("startup_state"),
  setWindowTitle: (title: string) => invoke<void>("set_window_title", { title }),
  backupPhrase: () => invoke<string>("backup_phrase"),
  restoreIdentity: (phrase: string) => invoke<void>("restore_identity", { phrase }),
  status: () => invoke<Status>("status"),
  communities: () => invoke<CommunitySummary[]>("communities"),
  channels: (community: Id) => invoke<ChannelSummary[]>("channels", { community }),
  members: (community: Id) => invoke<MemberSummary[]>("members", { community }),
  onlineMembers: (community: Id) => invoke<Id[]>("online_members", { community }),
  history: (community: Id, channel: Id) => invoke<Message[]>("history", { community, channel }),
  post: (community: Id, channel: Id, body: string) =>
    invoke<void>("post", { community, channel, body }),
  createCommunity: (name: string) => invoke<Id>("create_community", { name }),
  joinCommunity: (invite: string) => invoke<Id>("join_community", { invite }),
  createChannel: (community: Id, name: string) =>
    invoke<Id>("create_channel", { community, name }),
  makeInvite: (community: Id) => invoke<InviteText>("make_invite", { community }),
  setDisplayName: (name: string) => invoke<void>("set_display_name", { name }),
  syncNow: () => invoke<number>("sync_now"),
  dial: (addr: string) => invoke<void>("dial", { addr }),
};

const realOnNodeEvent = (handler: (event: NodeEvent) => void) =>
  listen<NodeEvent>("kahui://event", (e) => handler(e.payload));

const realOnReady = (handler: (ready: Ready) => void) =>
  listen<Ready>("kahui://ready", (e) => handler(e.payload));

const realOnFailed = (handler: (err: UiError) => void) =>
  listen<UiError>("kahui://failed", (e) => handler(e.payload));

const preview = import.meta.env.DEV && !insideTauri();

export const api = preview ? (await import("./mock")).mockApi : realApi;

export const onNodeEvent = preview
  ? (await import("./mock")).mockListen
  : realOnNodeEvent;

export const onReady = preview
  ? async (handler: (ready: Ready) => void) => {
      handler((await import("./mock")).mockReady());
      return () => {};
    }
  : realOnReady;

export const onFailed = preview ? async (_h: (err: UiError) => void) => () => {} : realOnFailed;
