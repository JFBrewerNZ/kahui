/**
 * A stand-in node, for working on the interface in a plain browser.
 *
 * `npm run dev` and open http://localhost:1420 and you get the window with
 * plausible content in it, without compiling Rust or starting a real node.
 * Useful for CSS and layout work, where a rebuild per change is intolerable.
 *
 * This is development-only in the strict sense: the whole module sits behind
 * `import.meta.env.DEV`, so the production bundle never contains it. Nothing
 * here is ever a fallback in a shipped app — if the real node is missing there,
 * that is a failure worth showing, not one worth papering over.
 */

import type {
  ChannelSummary,
  CommunitySummary,
  Id,
  InviteText,
  MemberSummary,
  Message,
  NodeEvent,
  Ready,
  Status,
} from "./api";

const hex = (seed: string) =>
  Array.from(seed.padEnd(32, seed))
    .slice(0, 32)
    .map((c) => c.charCodeAt(0).toString(16).padStart(2, "0"))
    .join("");

const ME = hex("jamon");
const ALICE = hex("alice");
const BOB = hex("bob");

const AOTEAROA = hex("aotearoa");
const BOATBUILDERS = hex("boats");
const GENERAL = hex("general");
const RANDOM = hex("random");

const communities: CommunitySummary[] = [
  {
    id: AOTEAROA,
    name: "Aotearoa",
    description: "A Kāhui community",
    founder: ALICE,
    created_ms: Date.now() - 86_400_000 * 3,
  },
  {
    id: BOATBUILDERS,
    name: "Boat Builders",
    description: "",
    founder: ME,
    created_ms: Date.now() - 86_400_000,
  },
];

const channels: ChannelSummary[] = [
  { id: GENERAL, name: "general", topic: "Everything else", creator: ALICE, created_ms: 0 },
  { id: RANDOM, name: "random", topic: "", creator: BOB, created_ms: 0 },
];

const members: MemberSummary[] = [
  { id: ALICE, display_name: "alice", joined_ms: 0 },
  { id: BOB, display_name: "bob", joined_ms: 1 },
  { id: ME, display_name: "jamon", joined_ms: 2 },
];

const yesterday = Date.now() - 86_400_000;
const now = Date.now();

const messages: Message[] = [
  ["alice", ALICE, "kia ora koutou — first message in the new place", yesterday, 4],
  ["alice", ALICE, "the whole history lives on every machine here, which still feels strange to me", yesterday + 40_000, 5],
  ["bob", BOB, "morena. so if you close your laptop, nothing breaks?", yesterday + 300_000, 6],
  ["alice", ALICE, "nothing breaks. you and I are connected directly", yesterday + 340_000, 7],
  ["jamon", ME, "tested it last night — killed the founder's node mid-conversation and the rest of us kept talking", now - 3_600_000, 12],
  ["bob", BOB, "and it caught up when it came back?", now - 1_800_000, 13],
  ["jamon", ME, "caught up on everything it missed, in one round trip", now - 1_740_000, 14],
].map(([author_name, author, body, timestamp_ms, lamport], i) => ({
  id: hex(`m${i}`),
  community: AOTEAROA,
  channel: GENERAL,
  author: author as Id,
  author_name: author_name as string,
  body: body as string,
  timestamp_ms: timestamp_ms as number,
  lamport: lamport as number,
}));

const status: Status = {
  user: ME,
  display_name: "jamon",
  peer_id: "12D3KooWEL3F5St2KdExjTbHrARKnfwAo4n2uuYwEGeNj3NGPkNQ",
  listen_addrs: ["/ip4/192.168.1.143/tcp/47101", "/ip4/127.0.0.1/udp/47101/quic-v1"],
  connected_peers: ["12D3KooW…alice", "12D3KooW…bob"],
  communities: communities.map((c) => ({
    id: c.id,
    name: c.name,
    events: 11,
    members: 3,
    channels: 2,
  })),
};

const wait = <T>(value: T) => new Promise<T>((r) => setTimeout(() => r(value), 60));

export const mockApi = {
  status: () => wait(status),
  communities: () => wait(communities),
  channels: (community: Id) => wait(community === AOTEAROA ? channels : [channels[0]]),
  members: (community: Id) => wait(community === AOTEAROA ? members : [members[2]]),
  onlineMembers: (_community: Id) => wait([ALICE, BOB]),
  history: (_community: Id, channel: Id) =>
    wait(channel === GENERAL ? messages : ([] as Message[])),
  post: async (_c: Id, _ch: Id, _b: string) => {},
  createCommunity: (_name: string) => wait(BOATBUILDERS),
  joinCommunity: (_invite: string) => wait(AOTEAROA),
  createChannel: (_c: Id, _name: string) => wait(RANDOM),
  makeInvite: (_c: Id) =>
    wait<InviteText>({
      token:
        "kahui1XEcg4xA41QNfJis19rYRdWU6SPqQuN1dGbfR1HAq3o4Q9nSATPx7qva2efab5SzXF1rQBQ3y9cB5BnnNP2ZtBwQ5qoMicjTq5nk38RKZ1SySvUq6NcDWSGqdJ9dhZW8MDoBAouVYuW3TNMX9JffeTtNMN6xidiK3P89KhopiSpiLzXA46Mi8AyDGTzgmiVvu7KcAUm3YGZDtvVNTV5xTSjCsypipsYrVQ2BXisWq9oAHmZnJtNvGfTwKBjMytSKeqwtCy4VaJigAUFXJ6Htcf4ANcPW2uYD7bxB2ZkeDE1R4Fz",
      communityName: "Aotearoa",
      peerCount: 3,
    }),
  setDisplayName: async (_name: string) => {},
  syncNow: () => wait(2),
  dial: async (_addr: string) => {},
};

export const mockReady: Ready = {
  status,
  dataDir: "C:\\Users\\you\\AppData\\Local\\Kahui\\Kahui\\data",
};

/** No events arrive in preview; the interface is static by design here. */
export const mockListen = (_handler: (event: NodeEvent) => void) =>
  Promise.resolve(() => {});
