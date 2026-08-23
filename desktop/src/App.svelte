<script lang="ts">
  import { onMount } from "svelte";
  import { kahui } from "./lib/state.svelte";
  import CommunityRail from "./lib/components/CommunityRail.svelte";
  import ChannelPane from "./lib/components/ChannelPane.svelte";
  import ChatPane from "./lib/components/ChatPane.svelte";
  import MemberPane from "./lib/components/MemberPane.svelte";
  import AddCommunityDialog from "./lib/components/AddCommunityDialog.svelte";
  import InviteDialog from "./lib/components/InviteDialog.svelte";
  import NewChannelDialog from "./lib/components/NewChannelDialog.svelte";
  import SettingsDialog from "./lib/components/SettingsDialog.svelte";

  type Dialog = "add" | "invite" | "channel" | "settings" | null;
  let dialog = $state<Dialog>(null);

  onMount(() => kahui.init());

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
  </div>
{:else if kahui.phase === "failed"}
  <div class="curtain">
    <h1>Kāhui could not start</h1>
    <p class="reason">{kahui.fatal}</p>
  </div>
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
