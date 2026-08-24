<script lang="ts">
  import { kahui, hueOf, initial } from "../state.svelte";

  interface Props {
    oninvite: () => void;
    onnewchannel: () => void;
    onidentity: () => void;
  }
  let { oninvite, onnewchannel, onidentity }: Props = $props();
</script>

<aside class="pane">
  <header>
    <span class="name">{kahui.community?.name ?? "Kāhui"}</span>
    <button class="invite" onclick={oninvite} disabled={!kahui.community} title="Invite someone">
      Invite
    </button>
  </header>

  <div class="channels">
    <div class="section-label row">
      <span>Channels</span>
      <button class="tiny" onclick={onnewchannel} disabled={!kahui.community} title="New channel">
        +
      </button>
    </div>

    {#each kahui.channels as channel (channel.id)}
      {@const unread = kahui.unread[channel.id] ?? 0}
      <button
        class="channel"
        class:active={channel.id === kahui.channelId}
        onclick={() => kahui.selectChannel(channel.id).catch((e) => kahui.report(e))}
      >
        <span class="hash">#</span>
        <span class="label">{channel.name}</span>
        {#if unread > 0}<span class="badge">{unread}</span>{/if}
      </button>
    {/each}

    {#if kahui.communities.length === 0}
      <p class="empty muted">No communities yet.</p>
    {/if}
  </div>

  <!-- Who you are, and whether anyone can hear you. -->
  <footer>
    <button class="me" onclick={onidentity} title="Change your display name">
      <span class="avatar" style="background: hsl({hueOf(kahui.me)} 62% 66%)">
        {initial(kahui.status?.display_name ?? "?")}
      </span>
      <span class="who">
        <span class="handle">{kahui.status?.display_name ?? "…"}</span>
        <span class="peers faint" title={kahui.standing.detail}>
          <span class="dot" class:live={kahui.standing.good}></span>
          {kahui.peerCount}
          {kahui.peerCount === 1 ? "peer" : "peers"}
          · {kahui.standing.label}
        </span>
      </span>
    </button>
  </footer>
</aside>

<style>
  .pane {
    width: 232px;
    flex: none;
    background: var(--sidebar);
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  header {
    height: 48px;
    flex: none;
    padding: 0 0.5rem 0 0.9rem;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    border-bottom: 1px solid var(--rail);
  }
  .name {
    font-weight: 620;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .invite {
    flex: none;
    font-size: 0.78rem;
    font-weight: 600;
    padding: 0.25rem 0.55rem;
    border-radius: 6px;
    color: var(--star);
  }
  .invite:hover:not(:disabled) {
    background: var(--raised);
  }
  .invite:disabled {
    color: var(--faint);
    cursor: default;
  }

  .channels {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 0.5rem 0.6rem;
  }

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .tiny {
    color: var(--faint);
    font-size: 1rem;
    line-height: 1;
    padding: 0 0.25rem;
  }
  .tiny:hover:not(:disabled) {
    color: var(--text);
  }

  .channel {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.36rem 0.5rem;
    border-radius: 6px;
    color: var(--dim);
    text-align: left;
  }
  .channel:hover {
    background: var(--raised);
    color: var(--text);
  }
  .channel.active {
    background: var(--hover);
    color: var(--text);
  }
  .hash {
    color: var(--faint);
    font-weight: 600;
  }
  .label {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .badge {
    flex: none;
    min-width: 1.15rem;
    padding: 0 0.3rem;
    border-radius: 999px;
    background: var(--star);
    color: #1a1405;
    font-size: 0.7rem;
    font-weight: 700;
    text-align: center;
  }

  .empty {
    font-size: 0.85rem;
    padding: 0.5rem;
    line-height: 1.55;
  }

  footer {
    flex: none;
    padding: 0.5rem;
    background: var(--rail);
  }
  .me {
    width: 100%;
    display: flex;
    align-items: center;
    gap: 0.55rem;
    padding: 0.35rem;
    border-radius: 6px;
    text-align: left;
  }
  .me:hover {
    background: var(--raised);
  }
  .avatar {
    width: 32px;
    height: 32px;
    font-size: 0.85rem;
  }
  .who {
    min-width: 0;
    display: flex;
    flex-direction: column;
    line-height: 1.25;
  }
  .handle {
    font-size: 0.88rem;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .peers {
    font-size: 0.74rem;
    display: flex;
    align-items: center;
    gap: 0.3rem;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--faint);
  }
  .dot.live {
    background: var(--online);
  }
</style>
