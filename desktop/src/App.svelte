<script lang="ts">
  import { onMount } from "svelte";
  import { kahui } from "./lib/state.svelte";
  import { api, errorText } from "./lib/api";
  import CommunityRail from "./lib/components/CommunityRail.svelte";
  import ChannelPane from "./lib/components/ChannelPane.svelte";
  import ChatPane from "./lib/components/ChatPane.svelte";
  import MemberPane from "./lib/components/MemberPane.svelte";
  import AddCommunityDialog from "./lib/components/AddCommunityDialog.svelte";
  import InviteDialog from "./lib/components/InviteDialog.svelte";
  import NewChannelDialog from "./lib/components/NewChannelDialog.svelte";
  import SettingsDialog from "./lib/components/SettingsDialog.svelte";
  import Welcome from "./lib/components/Welcome.svelte";

  type Dialog = "add" | "invite" | "channel" | "settings" | null;
  let dialog = $state<Dialog>(null);

  // A rejected promise here used to leave the window sitting on "Starting your
  // node…" forever, which says nothing to anybody. Whatever went wrong, say it.
  onMount(() => {
    kahui.init().catch((err) => {
      kahui.phase = "failed";
      kahui.fatal = `The window could not reach the node: ${errorText(err)}`;
    });
  });

  // The window title says where you are, the way a chat application's should.
  // It doubles as the one piece of state observable from outside the window,
  // which is how the app can be checked without looking at pixels.
  $effect(() => {
    const title =
      kahui.phase === "starting"
        ? "Kahui - starting"
        : kahui.phase === "failed"
          ? "Kahui - could not start"
          : kahui.fresh
            ? "Kahui - set up"
            : kahui.channel && kahui.community
              ? `#${kahui.channel.name} - ${kahui.community.name} - Kahui`
              : "Kahui";
    document.title = title;
    // The native window title is set separately, from Rust; the webview's own
    // title never reaches it.
    void api.setWindowTitle(title).catch(() => {});
  });

  // Clear a notice a few seconds after it appears, so warnings do not pile up.
  $effect(() => {
    if (!kahui.notice) return;
    const timer = setTimeout(() => (kahui.notice = ""), 6000);
    return () => clearTimeout(timer);
  });
</script>

{#if kahui.phase === "starting"}
  <div class="curtain">
    <div class="cluster"></div>
    <p class="muted">Starting your node…</p>
    <!-- Say what is happening rather than spinning silently. Opening the
         database and binding sockets takes a moment; anything beyond a few
         seconds is worth remarking on. -->
    {#if kahui.startupNote}
      <p class="reason">{kahui.startupNote}</p>
    {:else if kahui.waiting >= 8}
      <p class="reason">
        This is taking longer than usual. If another copy of Kāhui is open, or the command
        line client is running, close it — they share one identity and only one can hold it
        at a time.
      </p>
    {/if}
  </div>
{:else if kahui.phase === "failed"}
  <div class="curtain">
    <h1>Kāhui could not start</h1>
    <p class="reason">{kahui.fatal}</p>
  </div>
{:else if kahui.fresh}
  <Welcome />
{:else}
  <main>
    <CommunityRail onadd={() => (dialog = "add")} />
    <ChannelPane
      oninvite={() => (dialog = "invite")}
      onnewchannel={() => (dialog = "channel")}
      onidentity={() => (dialog = "settings")}
    />
    <ChatPane />
    {#if kahui.community}
      <MemberPane />
    {/if}
  </main>

  {#if dialog === "add"}
    <AddCommunityDialog onclose={() => (dialog = null)} />
  {:else if dialog === "invite"}
    <InviteDialog onclose={() => (dialog = null)} />
  {:else if dialog === "channel"}
    <NewChannelDialog onclose={() => (dialog = null)} />
  {:else if dialog === "settings"}
    <SettingsDialog onclose={() => (dialog = null)} />
  {/if}

  {#if kahui.busy}
    <div class="banner busy">{kahui.busy}</div>
  {:else if kahui.notice}
    <button class="banner notice" onclick={() => (kahui.notice = "")}>
      {kahui.notice}
    </button>
  {/if}
{/if}

<style>
  main {
    display: flex;
    height: 100%;
    overflow: hidden;
  }

  .curtain {
    height: 100%;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.9rem;
    padding: 2rem;
    text-align: center;
  }

  .curtain h1 {
    margin: 0;
    font-size: 1.3rem;
    font-weight: 620;
  }

  .reason {
    margin: 0;
    max-width: 32rem;
    color: var(--dim);
    line-height: 1.6;
  }

  /* A pulsing cluster, rather than a spinner: the node is finding its peers,
     not loading a page. */
  .cluster {
    width: 46px;
    height: 46px;
    border-radius: 50%;
    background: var(--star);
    animation: breathe 1.6s ease-in-out infinite;
  }
  @keyframes breathe {
    0%,
    100% {
      transform: scale(0.72);
      opacity: 0.35;
    }
    50% {
      transform: scale(1);
      opacity: 1;
    }
  }

  .banner {
    position: fixed;
    left: 50%;
    bottom: 1.2rem;
    transform: translateX(-50%);
    max-width: min(40rem, calc(100vw - 3rem));
    padding: 0.6rem 1rem;
    border-radius: 999px;
    font-size: 0.86rem;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.5);
    z-index: 60;
  }
  .busy {
    background: var(--raised);
    border: 1px solid var(--line);
    color: var(--dim);
  }
  .notice {
    background: var(--raised);
    border: 1px solid var(--danger);
    color: var(--text);
    text-align: left;
  }
</style>
