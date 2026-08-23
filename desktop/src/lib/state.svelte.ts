/**
 * Everything the window knows, in one place.
 *
 * The node is the source of truth; this is a view of it that keeps itself
 * current from the event stream. Nothing here decides anything about the
 * protocol — when an event says something changed, the answer is to ask the
 * node again rather than to guess locally.
 */

import {
  api,
  asMessage,
  errorText,
  onFailed,
  onNodeEvent,
  onReady,
  type ChannelSummary,
  type CommunitySummary,
  type Id,
  type MemberSummary,
  type Message,
  type NodeEvent,
  type Status,
} from "./api";

/** How often to re-read connection state, in milliseconds. */
const STATUS_INTERVAL = 4000;

class Kahui {
  phase = $state<"starting" | "ready" | "failed">("starting");
  fatal = $state("");
  notice = $state("");

  status = $state<Status | null>(null);
  dataDir = $state("");

  communities = $state<CommunitySummary[]>([]);
  channels = $state<ChannelSummary[]>([]);
  members = $state<MemberSummary[]>([]);
  /** Members this node currently holds a connection to. */
  online = $state<Id[]>([]);
  messages = $state<Message[]>([]);

  communityId = $state<Id | null>(null);
  channelId = $state<Id | null>(null);

  /** Unread counts per channel, cleared when you look at one. */
  unread = $state<Record<Id, number>>({});
  /** True while a join is fetching history, which can take a few seconds. */
  busy = $state("");

  community = $derived(this.communities.find((c) => c.id === this.communityId) ?? null);
  channel = $derived(this.channels.find((c) => c.id === this.channelId) ?? null);
  peerCount = $derived(this.status?.connected_peers.length ?? 0);
  me = $derived(this.status?.user ?? "");

  async init() {
    await onReady(async (ready) => {
      this.status = ready.status;
      this.dataDir = ready.dataDir;
      this.phase = "ready";
      await this.loadCommunities();
      // Drop straight into a community if this node already belongs to one.
      if (!this.communityId && this.communities.length > 0) {
        await this.selectCommunity(this.communities[0].id);
      }
    });

    await onFailed((err) => {
      this.phase = "failed";
      this.fatal = err.message;
    });

    await onNodeEvent((event) => void this.apply(event));

    setInterval(() => void this.refreshStatus(), STATUS_INTERVAL);
  }

  private async refreshStatus() {
    if (this.phase !== "ready") return;
    try {
      this.status = await api.status();
    } catch {
      // A missed poll is not worth telling anybody about.
    }
  }

  /** Folds one node event into the view. */
  private async apply(event: NodeEvent) {
    switch (event.type) {
      case "message": {
        const message = asMessage(event);
        if (!message) return;
        if (message.channel === this.channelId) {
          this.insert(message);
        } else if (message.community === this.communityId || this.knows(message.community)) {
          this.unread[message.channel] = (this.unread[message.channel] ?? 0) + 1;
        }
        return;
      }

      case "membership":
        if (event.community === this.communityId) await this.loadMembers();
        return;

      case "channel_created":
        if (event.community === this.communityId) await this.loadChannels();
        return;

      case "community_created":
        await this.loadCommunities();
        return;

      case "synced":
        // A sync can backfill messages that belong *earlier* in the transcript,
        // so re-read rather than trying to patch what is on screen.
        if (event.community === this.communityId) {
          await Promise.all([this.loadMessages(), this.loadMembers(), this.loadChannels()]);
        }
        return;

      case "peer_connected":
      case "peer_disconnected":
        await this.refreshStatus();
        // Who is reachable just changed, so the roster's dimming is stale.
        await this.loadMembers();
        return;

      case "warning":
        this.notice = event.message;
        return;

      default:
        return;
    }
  }

  private knows(community: Id) {
    return this.communities.some((c) => c.id === community);
  }

  /**
   * Places a message in causal order.
   *
   * Messages do not always arrive in the order they belong in: one that was
   * synced from a peer can sort before messages already on screen. Ordering is
   * by Lamport clock, tie-broken by id — the same rule every node applies, so
   * everyone sees the same transcript.
   */
  private insert(message: Message) {
    if (this.messages.some((m) => m.id === message.id)) return;
    const key = (m: Message): [number, string] => [m.lamport, m.id];
    const at = this.messages.findIndex((m) => {
      const [al, ai] = key(m);
      const [bl, bi] = key(message);
      return al > bl || (al === bl && ai > bi);
    });
    if (at === -1) this.messages.push(message);
    else this.messages.splice(at, 0, message);
  }

  async loadCommunities() {
    this.communities = await api.communities();
  }

  async loadChannels() {
    if (!this.communityId) return;
    this.channels = await api.channels(this.communityId);
  }

  async loadMembers() {
    if (!this.communityId) return;
    const community = this.communityId;
    const [members, online] = await Promise.all([
      api.members(community),
      api.onlineMembers(community),
    ]);
    this.members = members;
    this.online = online;
  }

  async loadMessages() {
    if (!this.communityId || !this.channelId) return;
    this.messages = await api.history(this.communityId, this.channelId);
  }

  async selectCommunity(id: Id) {
    if (this.communityId === id) return;
    this.communityId = id;
    this.channelId = null;
    this.messages = [];
    await Promise.all([this.loadChannels(), this.loadMembers()]);
    if (this.channels.length > 0) await this.selectChannel(this.channels[0].id);
  }

  async selectChannel(id: Id) {
    this.channelId = id;
    delete this.unread[id];
    await this.loadMessages();
  }

  async send(body: string) {
    if (!this.communityId || !this.channelId) return;
    // The node echoes the message back through the event stream, so there is
    // nothing to add locally: what appears on screen is what was actually
    // written to the log.
    await api.post(this.communityId, this.channelId, body);
  }

  async createCommunity(name: string) {
    const id = await api.createCommunity(name);
    await this.loadCommunities();
    await this.selectCommunity(id);
    return id;
  }

  async joinCommunity(invite: string) {
    this.busy = "Fetching the community's history…";
    try {
      const id = await api.joinCommunity(invite);
      await this.loadCommunities();
      await this.selectCommunity(id);
      return id;
    } finally {
      this.busy = "";
    }
  }

  async createChannel(name: string) {
    if (!this.communityId) return;
    const id = await api.createChannel(this.communityId, name);
    await this.loadChannels();
    await this.selectChannel(id);
  }

  async rename(name: string) {
    await api.setDisplayName(name);
    this.status = await api.status();
    await this.loadMembers();
  }

  /** Surfaces a problem without derailing whatever the user was doing. */
  report(err: unknown) {
    this.notice = errorText(err);
  }
}

export const kahui = new Kahui();

/**
 * A stable colour per identifier.
 *
 * Community and member avatars need to be distinguishable at a glance; deriving
 * the hue from the id means it is the same on every machine, for free.
 */
export function hueOf(id: Id): number {
  let hash = 0;
  for (let i = 0; i < id.length; i += 1) {
    hash = (hash * 31 + id.charCodeAt(i)) % 360;
  }
  return hash;
}

/** The first letter of a name, for an avatar. */
export function initial(name: string): string {
  return [...name.trim()][0]?.toUpperCase() ?? "?";
}

/** `14:05`, in local time. */
export function clock(ms: number): string {
  return new Date(ms).toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
  });
}

/** `Tuesday, 23 August` — the separator between days in a transcript. */
export function dayLabel(ms: number): string {
  return new Date(ms).toLocaleDateString(undefined, {
    weekday: "long",
    day: "numeric",
    month: "long",
  });
}

export function sameDay(a: number, b: number): boolean {
  const x = new Date(a);
  const y = new Date(b);
  return (
    x.getFullYear() === y.getFullYear() &&
    x.getMonth() === y.getMonth() &&
    x.getDate() === y.getDate()
  );
}
