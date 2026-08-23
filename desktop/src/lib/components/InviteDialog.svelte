<script lang="ts">
  import Modal from "./Modal.svelte";
  import { kahui } from "../state.svelte";
  import { api, errorText, type InviteText } from "../api";

  interface Props {
    onclose: () => void;
  }
  let { onclose }: Props = $props();

  let invite = $state<InviteText | null>(null);
  let problem = $state("");
  let copied = $state(false);
  let box = $state<HTMLTextAreaElement | null>(null);

  $effect(() => {
    const community = kahui.communityId;
    if (!community) return;
    api
      .makeInvite(community)
      .then((result) => (invite = result))
      .catch((err) => (problem = errorText(err)));
  });

  async function copy() {
    if (!invite) return;
    try {
      await navigator.clipboard.writeText(invite.token);
      copied = true;
      setTimeout(() => (copied = false), 1800);
    } catch {
      // Clipboard access can be refused. Selecting the text is a fine
      // fallback -- the user presses Ctrl-C themselves.
      box?.select();
      problem = "Could not reach the clipboard. The invite is selected — copy it yourself.";
    }
  }
</script>

<Modal
  title="Invite to {invite?.communityName ?? kahui.community?.name ?? 'this community'}"
  subtitle="Send this to whoever should join. It is a hint about where to find the community, not a password — everything it points at is checked against signatures once fetched."
  {onclose}
>
  {#if invite}
    <textarea
      class="field token mono"
      bind:this={box}
      readonly
      rows="5"
      value={invite.token}
      onclick={() => box?.select()}
    ></textarea>

    <p class="hint faint">
      {#if invite.peerCount > 1}
        Names {invite.peerCount} members, so it keeps working even if you go offline.
      {:else}
        Names only this node so far. Once others join, new invites will name them too and will
        keep working when you are away.
      {/if}
    </p>
  {:else if problem}
    <p class="problem">{problem}</p>
  {:else}
    <p class="muted">Building an invite…</p>
  {/if}

  <div class="actions">
    <button class="btn" onclick={onclose}>Done</button>
    <button class="btn primary" onclick={copy} disabled={!invite}>
      {copied ? "Copied" : "Copy invite"}
    </button>
  </div>
</Modal>

<style>
  .token {
    resize: none;
    font-size: 0.78rem;
    line-height: 1.55;
    word-break: break-all;
    color: var(--star);
  }
  .hint {
    font-size: 0.83rem;
    margin: 0.7rem 0 0;
    line-height: 1.55;
  }
  .problem {
    margin: 0.8rem 0 0;
    font-size: 0.85rem;
    color: var(--danger);
  }
  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 1.2rem;
  }
</style>
